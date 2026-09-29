/**
 * Real-boundary Rust operation-execution transport-generation acceptance.
 *
 * A generated Rust `TransportGrowthOperationProvider` service is approved with
 * its optional `extend` capability declined. A caller starts its always-granted
 * `Hold` operation, whose handler parks until a `Continue` signal, so a real
 * deployment consent can grow the provider's physical transport *while one
 * execution is in flight*. The held execution must finish exactly once on the
 * generation that accepted it, and a start after cutover must be accepted and
 * complete exactly once — proving the candidate ingress was broker-ready before
 * it became the default and that accepted executions are never restarted.
 */

import { assert, assertEquals } from "@std/assert";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

/** Optional capability id declined at consent and grown later. */
const CAPABILITY_EXTEND = "runtime-trellis.transport_growth@v1::extend";

const PROVIDER_DEPLOYMENT = "tg-op-provider";

/** Short lifetimes so growth crosses a real ordinary renewal quickly. */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 62,
    refreshLeadSeconds: 25,
    refreshJitterSeconds: 0,
    minimumContextLifetimeSeconds: 32,
  },
};

/** A bounded command channel to the Rust operation provider leg. */
type OperationLeg = {
  waitFor(marker: string): Promise<void>;
  send(command: string): Promise<void>;
  executionCount(): number | undefined;
  parkCalls(): number | undefined;
};

/** Spawns the Rust operation provider leg, runs `body`, and always reaps it. */
async function withOperationProviderLeg(
  runtime: TrellisTestRuntime,
  seed: string,
  deadlineMs: number,
  body: (leg: OperationLeg) => Promise<void>,
): Promise<void> {
  const child = new Deno.Command("setsid", {
    args: rustFixtureArgv("transport_growth_operation"),
    env: { TRELLIS_URL: runtime.trellisUrl, TRELLIS_IDENTITY_SEED: seed },
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
  const deadline = Date.now() + deadlineMs;
  const remainingMs = (): number => Math.max(0, deadline - Date.now());

  const leg: OperationLeg = {
    async waitFor(marker: string): Promise<void> {
      await runtime.waitFor(
        () => {
          if (lines.some((line) => line.startsWith(marker))) return true;
          const error = lines.find((line) =>
            line.startsWith("OPERATION_PROVIDER_ERROR ")
          );
          if (error !== undefined) {
            throw new Error(`Rust operation provider leg failed: ${error}`);
          }
          return undefined;
        },
        { timeoutMs: remainingMs(), intervalMs: 50 },
      );
    },
    async send(command: string): Promise<void> {
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    },
    executionCount(): number | undefined {
      let latest: number | undefined;
      for (const line of lines) {
        if (line.startsWith("HOLD_EXECUTIONS ")) {
          const value = Number(line.slice("HOLD_EXECUTIONS ".length));
          if (Number.isFinite(value)) latest = value;
        }
      }
      return latest;
    },
    parkCalls(): number | undefined {
      let latest: number | undefined;
      for (const line of lines) {
        if (line.startsWith("PARK_CALLS ")) {
          const value = Number(line.slice("PARK_CALLS ".length));
          if (Number.isFinite(value)) latest = value;
        }
      }
      return latest;
    },
  };

  try {
    await body(leg);
    if (!lines.includes("OPERATION_PROVIDER_DONE") && !exited) {
      await leg.send("EXIT");
      await leg.waitFor("OPERATION_PROVIDER_DONE");
    }
    await runtime.waitFor(() => (exited ? true : undefined), {
      timeoutMs: remainingMs(),
      intervalMs: 50,
    });
    const finalStatus = await status;
    assert(
      finalStatus.success,
      `Rust operation provider leg failed: ${output}`,
    );
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

Deno.test(
  "Rust operation execution survives transport growth and accepts work after cutover",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract =
        participants.TransportGrowthOperationProvider.participant;
      const callerContract = participants.TransportGrowthCaller.participant;

      // The provider starts approved with the optional capability declined, so
      // it serves `Hold` on a narrower transport generation.
      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        deployment: PROVIDER_DEPLOYMENT,
        contract: providerContract,
      });
      assert(
        requested.status === "approval_required",
        "the provider deployment must require an approval",
      );
      if (requested.status !== "approval_required") return;
      await runtime.contracts.approveApply(requested.pendingId, {
        excludeCapabilities: [CAPABILITY_EXTEND],
      });
      const providerInstance = await runtime.services.createInstance({
        deployment: PROVIDER_DEPLOYMENT,
        name: "tg-op-provider",
        contract: providerContract,
      });
      const providerId = providerContract.identity;

      /** The provider's admitted physical attachments. */
      const attachments = async (): Promise<
        { connectionId: string; contextDigest: string }[]
      > => {
        const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
          items: {
            participantId: string;
            connectionId: string;
            contextDigest: string;
          }[];
        };
        return page.items.filter((item) => item.participantId === providerId);
      };

      // The caller connects through the ordinary public client path.
      const caller = await runtime.connectClient({
        name: "tg-op-caller",
        contract: callerContract,
      });

      await withOperationProviderLeg(
        runtime,
        providerInstance.seed,
        150_000,
        async (leg) => {
          await leg.waitFor("OPERATION_PROVIDER_READY");
          const before = await attachments();
          assertEquals(
            before.length,
            1,
            "a fresh operation provider has exactly one admitted attachment",
          );

          // Start one execution on the initial generation; its handler parks.
          const held = await caller.hold({}).start().orThrow();
          await runtime.waitFor(
            async () =>
              (await held.get().orThrow()).state === "running"
                ? true
                : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );

          // Grow the provider's optional capability through real consent while
          // the execution is in flight. The provider's transport must adopt a
          // wider generation with candidate intake ready before it is default.
          await runtime.contracts.apply({
            deployment: PROVIDER_DEPLOYMENT,
            contract: providerContract,
          });

          // The provider's physical transport actually grows: a new attachment
          // with a newer context is admitted while the execution stays held on
          // the original one (make-before-break).
          await runtime.waitFor(
            async () => (await attachments()).length >= 2 ? true : undefined,
            { timeoutMs: 45_000, intervalMs: 100 },
          );
          const grown = await attachments();
          assert(
            grown.some((item) =>
              item.contextDigest !== before[0].contextDigest
            ),
            "growth must admit a new attachment with a newer authorization context",
          );
          assert(
            grown.some((item) => item.connectionId === before[0].connectionId),
            "the original attachment must survive the growth",
          );

          // The held execution finishes exactly once on the generation that
          // accepted it, and its control still reaches it across the cutover.
          await held.signal("Continue", { value: "continue" }).orThrow();
          const firstTerminal = await held.wait().orThrow();
          assertEquals(firstTerminal.state, "completed");
          assertEquals(firstTerminal.output?.value, "completed-1");

          // A start after cutover is accepted by the new generation and
          // completes exactly once.
          const after = await caller.hold({}).start().orThrow();
          await runtime.waitFor(
            async () =>
              (await after.get().orThrow()).state === "running"
                ? true
                : undefined,
            { timeoutMs: 30_000, intervalMs: 50 },
          );
          await after.signal("Continue", { value: "continue" }).orThrow();
          const secondTerminal = await after.wait().orThrow();
          assertEquals(secondTerminal.state, "completed");
          assertEquals(secondTerminal.output?.value, "completed-2");

          // Exactly two handler invocations for two accepted starts: no restart
          // and no duplicate delivery across the transport cutover.
          await leg.send("COUNT");
          await runtime.waitFor(
            () => leg.executionCount() === 2 ? true : undefined,
            { timeoutMs: 15_000, intervalMs: 50 },
          );
          assertEquals(
            leg.executionCount(),
            2,
            "each accepted operation must execute exactly once",
          );
        },
      );
    }, runtimeOptions);
  },
);

Deno.test(
  "Rust concurrent held RPC callbacks survive transport cutover",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract =
        participants.TransportGrowthOperationProvider.participant;
      const callerContract = participants.TransportGrowthCaller.participant;

      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        deployment: PROVIDER_DEPLOYMENT,
        contract: providerContract,
      });
      assert(
        requested.status === "approval_required",
        "the provider deployment must require an approval",
      );
      if (requested.status !== "approval_required") return;
      await runtime.contracts.approveApply(requested.pendingId, {
        excludeCapabilities: [CAPABILITY_EXTEND],
      });
      const providerInstance = await runtime.services.createInstance({
        deployment: PROVIDER_DEPLOYMENT,
        name: "tg-op-provider",
        contract: providerContract,
      });
      const providerId = providerContract.identity;
      const attachments = async (): Promise<
        { connectionId: string; contextDigest: string }[]
      > => {
        const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
          items: {
            participantId: string;
            connectionId: string;
            contextDigest: string;
          }[];
        };
        return page.items.filter((item) => item.participantId === providerId);
      };
      const caller = await runtime.connectClient({
        name: "tg-op-caller-rpc",
        contract: callerContract,
      });

      await withOperationProviderLeg(
        runtime,
        providerInstance.seed,
        150_000,
        async (leg) => {
          await leg.waitFor("OPERATION_PROVIDER_READY");
          assertEquals((await attachments()).length, 1);

          // Fire many RPC callbacks concurrently. Each parks inside its
          // handler, so several are accepted and in flight on the broker when
          // the transport cuts over.
          const total = 12;
          const held = Array.from(
            { length: total },
            () => caller.park({}, { timeout: 90_000 }).orThrow(),
          );
          const parkDeadline = Date.now() + 30_000;
          while ((leg.parkCalls() ?? 0) < 1) {
            if (Date.now() > parkDeadline) {
              throw new Error("no Park callback was accepted");
            }
            await leg.send("PARKS");
            await new Promise((resolve) => setTimeout(resolve, 100));
          }

          // Grow the provider while three accepted callbacks are held.
          await runtime.contracts.apply({
            deployment: PROVIDER_DEPLOYMENT,
            contract: providerContract,
          });
          await runtime.waitFor(
            async () => (await attachments()).length >= 2 ? true : undefined,
            { timeoutMs: 45_000, intervalMs: 100 },
          );

          // Release them: every held or in-flight callback completes exactly
          // once, so the cutover drops no accepted or buffered request.
          await leg.send("RELEASE");
          const results = await Promise.all(held);
          assertEquals(results.length, total);
          assertEquals(
            results.map((value) => value.value).sort(),
            Array.from({ length: total }, (_, index) => `parked-${index + 1}`)
              .sort(),
          );
        },
      );
    }, runtimeOptions);
  },
);

/**
 * Grant binding as reported by the production admin surface.
 */
type GrantBinding = {
  approval: unknown;
  approvalMode: string;
  createdAt: bigint;
  expiresAt: bigint | null;
  grants: {
    format: string;
    permissions: { action: string; target: Uint8Array }[];
  };
  installedRevision: bigint;
  ownerId: string;
  ownerKind: string;
  participantId: string;
  platformPrivileges: string[];
  provenance: unknown;
  revision: bigint;
  state: string;
  updatedAt: bigint;
};

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Stable identity of one permission atom, independent of wire encoding. */
function atomKey(atom: { action: string; target: Uint8Array }): string {
  return `${atom.action}:${btoa(String.fromCharCode(...atom.target))}`;
}

/** Reads the grant binding owned by a participant. */
async function readGrantBinding(
  runtime: Runtime,
  participantId: string,
): Promise<GrantBinding> {
  const page = await runtime.callAdminRpc("authGrantsList", {
    participantId,
  }) as { items: GrantBinding[] };
  const binding = page.items.find((item) =>
    item.participantId === participantId
  );
  if (!binding) throw new Error(`no grant binding for ${participantId}`);
  return binding;
}

/**
 * The provider's optional capability is withdrawn by narrowing its grant binding
 * back to the permissions the original attachment adopted. The wider generation
 * must close while the surviving generation resumes RPC intake, under a stable
 * logical connection.
 */
Deno.test(
  "Rust provider reduction closes the wider generation and restores survivor intake",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract =
        participants.TransportGrowthOperationProvider.participant;
      const callerContract = participants.TransportGrowthCaller.participant;

      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        deployment: PROVIDER_DEPLOYMENT,
        contract: providerContract,
      });
      assert(
        requested.status === "approval_required",
        "the provider deployment must require an approval",
      );
      if (requested.status !== "approval_required") return;
      await runtime.contracts.approveApply(requested.pendingId, {
        excludeCapabilities: [CAPABILITY_EXTEND],
      });
      const providerInstance = await runtime.services.createInstance({
        deployment: PROVIDER_DEPLOYMENT,
        name: "tg-op-provider",
        contract: providerContract,
      });
      const providerId = providerContract.identity;
      const attachments = async (): Promise<
        {
          connectionId: string;
          runtimeConnectionId: string;
          contextDigest: string;
        }[]
      > => {
        const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
          items: {
            participantId: string;
            connectionId: string;
            runtimeConnectionId: string;
            contextDigest: string;
          }[];
        };
        return page.items.filter((item) => item.participantId === providerId);
      };
      const caller = await runtime.connectClient({
        name: "tg-op-caller-reduce",
        contract: callerContract,
      });

      await withOperationProviderLeg(
        runtime,
        providerInstance.seed,
        150_000,
        async (leg) => {
          await leg.waitFor("OPERATION_PROVIDER_READY");
          const original = await attachments();
          assertEquals(original.length, 1);
          const originalRuntimeId = original[0].runtimeConnectionId;
          assert(
            typeof originalRuntimeId === "string" &&
              originalRuntimeId.length > 0,
            "the admin surface must report a logical connection identity",
          );

          // Baseline: the survivor's intake serves ordinary RPC.
          await caller.advance({}).orThrow();
          const adoptedKeys = new Set(
            (await readGrantBinding(runtime, providerId)).grants.permissions
              .map(atomKey),
          );

          // Grow the optional capability; a wider generation is adopted on the
          // same logical connection.
          await runtime.contracts.apply({
            deployment: PROVIDER_DEPLOYMENT,
            contract: providerContract,
          });
          await runtime.waitFor(
            async () => (await attachments()).length >= 2 ? true : undefined,
            { timeoutMs: 45_000, intervalMs: 100 },
          );
          await caller.advance({}).orThrow();

          // Reduce: narrow the binding back to the permissions the original
          // attachment adopted.
          const binding = await readGrantBinding(runtime, providerId);
          const kept = binding.grants.permissions.filter((atom) =>
            adoptedKeys.has(atomKey(atom))
          );
          assert(
            binding.grants.permissions.length > kept.length,
            "growth must have granted an optional-capability atom the reduction removes",
          );
          await runtime.callAdminRpc("authGrantsSet", {
            expectedRevision: binding.revision,
            expiresAt: binding.expiresAt,
            grants: { format: binding.grants.format, permissions: kept },
            idempotencyKey: crypto.randomUUID(),
            installedRevision: binding.installedRevision,
            ownerId: binding.ownerId,
            ownerKind: binding.ownerKind,
            participantId: binding.participantId,
            platformPrivileges: binding.platformPrivileges,
          });

          // The wider generation closes; the surviving generation must be
          // serving RPC intake again (not a current with dead intake) under the
          // original logical connection identity.
          const remaining = await runtime.waitFor(async () => {
            const items = await attachments();
            return items.length === 1 ? items : undefined;
          }, { timeoutMs: 60_000, intervalMs: 200 });
          assertEquals(
            remaining[0].runtimeConnectionId,
            originalRuntimeId,
            "a reduction must keep the logical connection identity",
          );
          await caller.advance({}).orThrow();
          assertEquals(
            (await attachments()).length,
            1,
            "only the surviving generation may remain",
          );
        },
      );
    }, runtimeOptions);
  },
);
