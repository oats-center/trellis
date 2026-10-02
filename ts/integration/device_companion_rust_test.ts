/**
 * Real-boundary Rust device companion acceptance.
 *
 * Runs the generated Rust device SDK against a live runtime: activation, native
 * device connection, the server-assigned companion connected through the
 * ordinary user login path, the companion's required RPC, an ordinary
 * authorization renewal observed from persisted issuance, and a control-plane
 * restart that reconnects without a second consent.
 *
 * The companion is never part of the native device bootstrap request; it is an
 * ordinary user login using only the durable assignment the device bootstrap
 * returned. The Rust leg prints a bounded line protocol and waits on stdin so
 * the harness can drive the renewal check on the same live connection.
 */

import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { createClient } from "@libsql/client";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";

import { base64urlEncode } from "../packages/trellis/auth/utils.ts";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type {
  ConsentCapability,
  ConsentResource,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/types/index.js";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

/**
 * Short but valid lifetimes. A context is issued with `lifetime - clock skew`
 * seconds of validity and the proactive refresh fires `lifetime - refreshLead -
 * skew` seconds after issuance, here about seven seconds, so ordinary renewal is
 * observable within a few seconds.
 */
const shortAuthorizationLifetimes = {
  contextLifetimeSeconds: 62,
  refreshLeadSeconds: 25,
  refreshJitterSeconds: 0,
  minimumContextLifetimeSeconds: 32,
};

const runtimeOptions = { authorization: shortAuthorizationLifetimes };

/** The companion participant recorded in persisted issuance state. */
const COMPANION_PARTICIPANT_ID = "runtime-trellis.Device.Companion";

/** How long the harness will watch persisted issuance for a real renewal. */
const RENEWAL_EVIDENCE_TIMEOUT_MS = 45_000;

/** One Rust leg's decoded line protocol. */
type RustLegOutput = {
  output: string;
  lines: string[];
};

/** Runs one Rust device companion leg, bounded by `deadlineMs`. */
async function runDeviceCompanionLeg(
  runtime: TrellisTestRuntime,
  rootSecret: Uint8Array,
  options: {
    provisioningSecret?: string;
    onActivation?: (flowId: string, confirmationCode: string) => Promise<void>;
    onReady?: () => Promise<void>;
    deadlineMs?: number;
  },
): Promise<RustLegOutput> {
  const env: Record<string, string> = {
    TRELLIS_URL: runtime.trellisUrl,
    DEVICE_ROOT_SECRET: base64urlEncode(rootSecret),
    CARGO_TARGET_DIR: fromFileUrl(new URL("../../target", import.meta.url)),
  };
  if (options.provisioningSecret !== undefined) {
    env.TRELLIS_PROVISIONING_SECRET = options.provisioningSecret;
  }
  const child = new Deno.Command("setsid", {
    args: rustFixtureArgv("device_companion"),
    env,
    stdin: "piped",
    stdout: "piped",
    stderr: "inherit",
  }).spawn();

  const lines: string[] = [];
  let output = "";
  let exited = false;
  const status = child.status.then((result) => {
    exited = true;
    return result;
  });
  const stdin = child.stdin.getWriter();
  const drain = (async () => {
    const reader = child.stdout.pipeThrough(new TextDecoderStream())
      .getReader();
    let buffer = "";
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      output += chunk.value;
      buffer += chunk.value;
      const parts = buffer.split("\n");
      buffer = parts.pop() ?? "";
      for (const part of parts) {
        const line = part.trim();
        if (line.length > 0) lines.push(line);
      }
    }
    const tail = buffer.trim();
    if (tail.length > 0) lines.push(tail);
  })();
  const drainSettled = drain.catch(() => {});

  const deadline = Date.now() + (options.deadlineMs ?? 90_000);
  const remainingMs = (): number => Math.max(0, deadline - Date.now());
  /** Resolves with the first emitted line that starts with any marker. */
  const waitForMarker = (markers: string[]): Promise<string> =>
    runtime.waitFor(
      () =>
        lines.find((line) => markers.some((marker) => line.startsWith(marker))),
      { timeoutMs: remainingMs(), intervalMs: 50 },
    );
  const throwOnError = (line: string): void => {
    if (line.startsWith("DEVICE_COMPANION_ERROR ")) {
      throw new Error(`Rust device companion leg failed: ${line}`);
    }
  };
  const send = async (command: string): Promise<void> => {
    await stdin.write(new TextEncoder().encode(`${command}\n`));
  };

  try {
    const first = await waitForMarker([
      "DEVICE_COMPANION_ACTIVATION ",
      "DEVICE_COMPANION_READY",
      "DEVICE_COMPANION_ERROR ",
    ]);
    throwOnError(first);
    if (first.startsWith("DEVICE_COMPANION_ACTIVATION ")) {
      if (options.onActivation === undefined) {
        throw new Error(
          `reconnecting after restart demanded a second consent: ${first}`,
        );
      }
      const [, activationUrl, confirmationCode] = first.split(" ");
      const flowId = new URL(activationUrl).searchParams.get("flowId");
      assert(flowId, `activation URL carries no flow id: ${activationUrl}`);
      await options.onActivation(flowId, confirmationCode);
    }
    if (!first.startsWith("DEVICE_COMPANION_READY")) {
      const ready = await waitForMarker([
        "DEVICE_COMPANION_READY",
        "DEVICE_COMPANION_ERROR ",
      ]);
      throwOnError(ready);
    }

    // Observe persisted renewal on the still-open connection, then prove the
    // same live companion still serves its required RPC.
    if (options.onReady !== undefined) {
      await options.onReady();
      await send("REQUIRED");
      const renewed = await waitForMarker([
        "DEVICE_COMPANION_RENEWED_OK",
        "DEVICE_COMPANION_ERROR ",
      ]);
      throwOnError(renewed);
    }

    await send("EXIT");
    const done = await waitForMarker([
      "DEVICE_COMPANION_DONE",
      "DEVICE_COMPANION_ERROR ",
    ]);
    throwOnError(done);
    await runtime.waitFor(() => (exited ? true : undefined), {
      timeoutMs: remainingMs(),
      intervalMs: 50,
    });
    const finalStatus = await status;
    assert(
      finalStatus.success,
      `Rust device companion leg failed: ${output}`,
    );
    return { output, lines };
  } finally {
    try {
      await stdin.close();
    } catch {
      // stdin may already be closed once the process exited.
    }
    if (!exited) {
      try {
        Deno.kill(-child.pid, "SIGKILL");
      } catch {
        // the process may have exited between the check and the signal.
      }
    }
    await Promise.allSettled([status, drainSettled]);
  }
}

Deno.test("Rust device companion activates, connects, renews, and survives restart without second consent", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.deployments.create({
      id: "device",
      kind: "device",
      reviewMode: "none",
    });
    await runtime.deployments.create({ id: "child-provider", kind: "service" });
    const childProviderIdentity = await runtime.services.createInstance({
      deployment: "child-provider",
      name: "device-child-provider",
      contract: participants.ChildProvider.participant,
    });
    const childProvider = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.ChildProvider.participant,
      seed: childProviderIdentity.seed,
    }).orThrow();
    await childProvider.handleRequired(() => Result.ok({}));
    await childProvider.handleOptional(() => Result.ok({}));
    await runtime.contracts.install({
      contract: participants.Device.Companion.participant,
    });
    await runtime.contracts.apply({
      deployment: "device",
      contract: participants.Device.participant,
    });
    await runtime.ensurePortalConsentPolicy(
      participants.Device.Companion.participant.identity,
      [
        "capability:runtime-trellis.device_child@v1::required",
        "capability:runtime-trellis.device_child@v1::optional",
        "capability:trellis.auth@v1::public",
        "capability:trellis.state@v1::public",
      ],
    );
    const provisioned = await runtime.devices.provision({
      deploymentId: "device",
      participantId: participants.Device.participant.identity,
      identityPublicKey: null,
      instanceId: null,
      idempotencyKey: crypto.randomUUID(),
    });
    const provisioningSecret = provisioned.provisioningSecret;
    assert(provisioningSecret);

    const portal = await runtime.connectClient({
      name: "device-companion-rust-portal",
      contract: participants.DevicePortal.participant,
    });

    /** Resolves the pending device review and approves the companion ceiling. */
    const approveActivation = async (
      flowId: string,
      confirmationCode: string,
    ): Promise<void> => {
      const pending = await portal.deviceUserAuthoritiesResolve({
        flowId,
        confirmationCode,
      }).start().orThrow();
      const pendingSnapshot = await runtime.waitFor(async () => {
        const snapshot = await pending.get().orThrow();
        return snapshot.progress?.companionConsent ? snapshot : undefined;
      });
      const consent = pendingSnapshot.progress?.companionConsent;
      assert(consent, "the device review reported no companion consent");
      const approved = await portal.deviceUserAuthoritiesResolve({
        flowId,
        confirmationCode,
        companionApproval: {
          mode: "capabilities" as const,
          installedRevision: consent.installedRevision,
          expectedGrantRevision: consent.expectedGrantRevision,
          decisionDigest: consent.decisionDigest,
          approvedCapabilities: consent.capabilities.filter((
            item: ConsentCapability,
          ) => item.required && item.eligible).map((
            item: ConsentCapability,
          ) => ({
            id: item.id,
            consentDigest: item.consentDigest,
          })),
          approvedResources: consent.resources.filter((item: ConsentResource) =>
            item.required && item.eligible
          ).map((item: ConsentResource) => ({
            kind: item.kind,
            name: item.name,
            commitment: item.requestedCommitment,
          })),
          companionApproved: false,
        },
      }).start().orThrow();
      const result = await runtime.waitFor(async () => {
        const snapshot = await approved.get().orThrow();
        return snapshot.state === "running" ? undefined : snapshot;
      }, { timeoutMs: 60_000 });
      assertEquals(result.state, "completed", `approval state ${result.state}`);
    };

    const rootSecret = crypto.getRandomValues(new Uint8Array(32));
    const database = createClient({
      url: `file:${
        join(runtime.workdir, "trellis", "trellis.sqlite.platform")
      }`,
    });
    /**
     * Waits until persisted issuance shows the companion's initial context plus
     * at least two scheduled renewals. A single companion login issues exactly
     * one context, so every further distinct digest is a real renewal.
     */
    const waitForPersistedRenewal = async (): Promise<void> => {
      await runtime.waitFor(async () => {
        const result = await database.execute({
          sql:
            "SELECT COUNT(DISTINCT context_digest) AS count FROM auth_authorization_contexts WHERE participant_id = ?",
          args: [COMPANION_PARTICIPANT_ID],
        });
        return Number(result.rows[0].count) >= 3 ? true : undefined;
      }, { timeoutMs: RENEWAL_EVIDENCE_TIMEOUT_MS, intervalMs: 250 });
    };

    try {
      // Phase one: activation, native device connection, and the companion user
      // login, held open until persisted renewal is observed, then asked for its
      // required RPC again on the same connection.
      const first = await runDeviceCompanionLeg(runtime, rootSecret, {
        provisioningSecret,
        onActivation: approveActivation,
        onReady: waitForPersistedRenewal,
      });
      assert(
        first.lines.includes("DEVICE_COMPANION_READY"),
        `Rust leg never reached its companion required RPC: ${first.output}`,
      );
      assert(
        first.lines.includes("DEVICE_COMPANION_RENEWED_OK"),
        `Rust companion did not serve its required RPC after renewal: ${first.output}`,
      );
    } finally {
      database.close();
    }

    try {
      // Phase two: the control plane restarts while the session state persists.
      // Reconnecting the same device must not require a second consent.
      await runtime.restartControlPlane();
      const second = await runDeviceCompanionLeg(runtime, rootSecret, {
        deadlineMs: 60_000,
      });
      assert(
        !second.lines.some((line) =>
          line.startsWith("DEVICE_COMPANION_ACTIVATION ")
        ),
        `reconnecting after restart demanded a second consent: ${second.output}`,
      );
      assert(
        second.lines.includes("DEVICE_COMPANION_READY"),
        `Rust companion required RPC failed after restart: ${second.output}`,
      );
    } finally {
      await portal.connection.close();
      await childProvider.connection.close();
    }
  }, runtimeOptions);
});
