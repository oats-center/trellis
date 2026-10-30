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

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { Readable, Writable } from "node:stream";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { codecs } from "@oatscenter/trellis/generated";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { ulid } from "ulid";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
  readRuntimeServiceAttachments,
  waitForServiceRenewalWindow,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { spawnTracked, type ProcessExit } from "./_support/process.ts";

/** Optional capability id declined at consent and grown later. */
const CAPABILITY_EXTEND = "runtime-trellis.transport_growth.v1:growth.extend";

const PROVIDER_DEPLOYMENT = "tg-op-provider";

/** Short lifetimes so growth crosses a real ordinary renewal quickly. */
const runtimeOptions = {
  authorization: {
    contextLifetimeSeconds: 27,
    allowedClockSkewSeconds: 2,
  },
};

async function prepareOperationProvider(runtime: TrellisTestRuntime) {
  const instance = await runtime.registerService({
    deployment: PROVIDER_DEPLOYMENT,
    name: "tg-op-provider",
    contract: participants.TransportGrowthOperationProvider.participant,
  });
  const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
    capabilityId: CAPABILITY_EXTEND,
  });
  const grant = grants.items[0];
  assert(grant);
  await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
    grantId: grant.grantId,
    expectedRevision: grant.revision,
    idempotencyKey: ulid(),
  });
  return {
    ...instance,
    grantee: grant.principalId,
  };
}

/** A bounded command channel to the Rust operation provider leg. */
type OperationLeg = {
  waitFor(marker: string): Promise<void>;
  send(command: string): Promise<void>;
  executionCount(): number | undefined;
  parkCalls(): number | undefined;
  jobExecutions(): number | undefined;
  /** Number of completed `Tick` deliveries observed for one payload value. */
  tickDeliveries(value: string): number;
  error(): string | undefined;
};

/** Spawns the Rust operation provider leg, runs `body`, and always reaps it. */
async function withOperationProviderLeg(
  runtime: TrellisTestRuntime,
  seed: string,
  deadlineMs: number,
  body: (leg: OperationLeg) => Promise<void>,
  options: { allowTerminalError?: boolean; rawConsumer?: boolean } = {},
): Promise<void> {
  const child = spawnTracked(
    "setsid",
    rustFixtureArgv("transport_growth_operation"),
    {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: seed,
        TRELLIS_RAW_CONSUMER: options.rawConsumer ? "1" : "0",
      },
    },
  );
  const lines: string[] = [];
  let output = "";
  let stderrOutput = "";
  let observedStatus: ProcessExit | undefined;
  let exited = false;
  const status = child.exited.then((result) => {
    observedStatus = result;
    exited = true;
    return result;
  });
  const stdin = Writable.toWeb(child.stdin).getWriter();
  // Closing stdin for a child that already exited rejects (bad resource/broken
  // pipe); both the terminal-error path and the cleanup path ignore that.
  const closeStdin = async (): Promise<void> => {
    try {
      await stdin.close();
    } catch {
      // stdin may already be closed once the process exited.
    }
  };
  const drain = (async () => {
    const reader = Readable.toWeb(child.stdout)
      .pipeThrough(new TextDecoderStream())
      .getReader();
    let buffer = "";
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      output = (output + chunk.value).slice(-16_384);
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
  const stderrDrain = (async () => {
    const reader = Readable.toWeb(child.stderr)
      .pipeThrough(new TextDecoderStream())
      .getReader();
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      stderrOutput = (stderrOutput + chunk.value).slice(-16_384);
      console.error(
        chunk.value
          .trimEnd()
          .split("\n")
          .map(
            (line) =>
              `[Rust operation provider pid=${child.pid} stderr] ${line}`,
          )
          .join("\n"),
      );
    }
  })();
  const stderrDrainSettled = stderrDrain.catch(() => {});
  const deadline = Date.now() + deadlineMs;
  const remainingMs = (): number => Math.max(0, deadline - Date.now());

  const leg: OperationLeg = {
    async waitFor(marker: string): Promise<void> {
      await runtime.waitFor(
        () => {
          if (lines.some((line) => line.startsWith(marker))) return true;
          const error = lines.find(
            (line) =>
              line.startsWith("OPERATION_PROVIDER_ERROR ") ||
              line.startsWith("RAW_TICKS_ERROR "),
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
    jobExecutions(): number | undefined {
      let latest: number | undefined;
      for (const line of lines) {
        if (line.startsWith("JOB_EXECUTIONS ")) {
          const value = Number(line.slice("JOB_EXECUTIONS ".length));
          if (Number.isFinite(value)) latest = value;
        }
      }
      return latest;
    },
    tickDeliveries(value: string): number {
      let count = 0;
      for (const line of lines) {
        const parts = line.split(" ");
        if (parts[0] === "TICK" && parts.slice(2).join(" ") === value) count++;
      }
      return count;
    },
    error(): string | undefined {
      const line = lines.find((line) =>
        line.startsWith("OPERATION_PROVIDER_ERROR "),
      );
      return line?.slice("OPERATION_PROVIDER_ERROR ".length);
    },
  };

  try {
    await body(leg);
    const terminalError = lines.some((line) =>
      line.startsWith("OPERATION_PROVIDER_ERROR "),
    );
    if (!lines.includes("OPERATION_PROVIDER_DONE") && !exited) {
      if (options.allowTerminalError && terminalError) {
        // The leg is shutting down after a terminal error. Release its command
        // reader so the fixture's blocking stdin read can finish and the process
        // can exit; without this the harness holds the pipe open forever.
        await closeStdin();
      } else {
        await leg.send("EXIT");
        await leg.waitFor("OPERATION_PROVIDER_DONE");
      }
    }
    await runtime.waitFor(() => (exited ? true : undefined), {
      timeoutMs: remainingMs(),
      intervalMs: 50,
    });
    const finalStatus = await status;
    if (!options.allowTerminalError) {
      assert.equal(
        finalStatus.code,
        0,
        `Rust operation provider leg failed: ${output}`,
      );
    }
  } catch (cause) {
    // Snapshot only what was observed before cleanup; awaiting a pending status
    // here would hang on a provider still waiting for a harness command.
    throw new Error(
      `${cause instanceof Error ? cause.message : String(cause)}\n` +
        `Rust operation provider pid=${child.pid}; pre-cleanup exit status: ${
          observedStatus === undefined
            ? "not yet observed"
            : JSON.stringify(observedStatus)
        }\nstdout tail:\n${output || "<empty>"}\nstderr tail:\n${
          stderrOutput || "<empty>"
        }`,
      { cause },
    );
  } finally {
    await closeStdin();
    if (!exited && child.pid !== undefined) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        // the process may have exited between the check and the signal.
      }
    }
    await Promise.allSettled([status, drainSettled, stderrDrainSettled]);
  }
}

test("Rust operation execution survives transport growth and accepts work after cutover", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = apps.TransportGrowthCaller.request;

    // The provider starts approved with the optional capability declined, so
    // it serves `Hold` on a narrower transport generation.
    const providerInstance = await prepareOperationProvider(runtime);
    const providerId = providerContract.identity;

    /** The provider's admitted physical attachments. */
    const attachments = () =>
      readRuntimeServiceAttachments(runtime, providerId);

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
        await leg.waitFor("JOB_STARTED 1 initial");
        const before = await attachments();
        assert.deepEqual(
          before.length,
          1,
          "a fresh operation provider has exactly one admitted attachment",
        );
        const baselineSockets = admittedConnections(
          await readRuntimeBrokerInventory(runtime),
          new Set([before[0].attachmentId]),
        );
        assert.deepEqual(baselineSockets.length, 1);
        const baselineSocket = baselineSockets[0];
        const baselineKey = brokerConnectionKey(baselineSocket);

        // Start one execution on the initial generation; its handler parks.
        const held = await caller.hold({}).start().orThrow();
        await runtime.waitFor(
          async () =>
            (await held.get().orThrow()).state === "running" ? true : undefined,
          { timeoutMs: 30_000, intervalMs: 50 },
        );

        // Grow the provider's optional capability through real consent while
        // the execution is in flight. The provider's transport must adopt a
        // wider generation with candidate intake ready before it is default.
        await waitForServiceRenewalWindow(
          runtime,
          (await attachments())[0].runtimeId,
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: providerInstance.grantee,
          capabilityId: CAPABILITY_EXTEND,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });

        // The provider's physical transport actually grows: a new attachment
        // with a newer context is admitted while the execution stays held on
        // the original one (make-before-break).
        await runtime.waitFor(
          async () => ((await attachments()).length >= 2 ? true : undefined),
          { timeoutMs: 45_000, intervalMs: 100 },
        );
        const grown = await attachments();
        assert(
          grown.some((item) => item.attachmentId !== before[0].attachmentId),
          "growth must admit a new attachment with a newer authorization context",
        );
        assert(
          grown.some((item) => item.attachmentId === before[0].attachmentId),
          "the original attachment must survive the growth",
        );

        // The held execution finishes exactly once on the generation that
        // accepted it, and its control still reaches it across the cutover.
        await held.signal("Continue", { value: "continue" }).orThrow();
        const firstTerminal = await held.wait().orThrow();
        assert.deepEqual(firstTerminal.state, "completed");
        assert.deepEqual(firstTerminal.output?.value, "completed-1");

        // This shared fixture also parks an initial job on G1. Complete that
        // independent accepted work so its legitimate lease cannot mask whether
        // operation execution releases the baseline generation.
        await leg.send("JOB_RELEASE initial");
        await leg.waitFor("JOB_DONE 1 initial");

        // Reap G1 before starting fresh work. Require its exact broker server
        // in every complete inventory, so missing discovery cannot prove absence.
        try {
          await runtime.waitFor(
            async () => {
              const inventory = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [baselineSocket.server],
              });
              return inventory.every(
                (item) => brokerConnectionKey(item) !== baselineKey,
              )
                ? true
                : undefined;
            },
            { timeoutMs: 45_000, intervalMs: 200 },
          );
        } catch (cause) {
          const inventory = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [baselineSocket.server],
          });
          throw new Error(
            `operation baseline ${baselineKey} not reclaimed; broker connections: ${JSON.stringify(
              inventory,
            )}`,
            { cause },
          );
        }
        console.log(
          `operation baseline ${baselineKey} absent before second Hold`,
        );

        // With G1 physically absent, a start on the new generation must persist,
        // accept control, and complete exactly once through ordinary operation APIs.
        const after = await caller
          .hold({})
          .start()
          .orThrow()
          .catch((cause) => {
            console.error("post-reap Hold start failed", cause);
            throw cause;
          });
        await runtime.waitFor(
          async () =>
            (await after.get().orThrow()).state === "running"
              ? true
              : undefined,
          { timeoutMs: 30_000, intervalMs: 50 },
        );
        await after.signal("Continue", { value: "continue" }).orThrow();
        const secondTerminal = await after.wait().orThrow();
        assert.deepEqual(secondTerminal.state, "completed");
        assert.deepEqual(secondTerminal.output?.value, "completed-2");
        const afterSecond = await readRuntimeBrokerInventory(runtime, {
          requiredServerIds: [baselineSocket.server],
        });
        assert(
          afterSecond.every(
            (item) => brokerConnectionKey(item) !== baselineKey,
          ),
          `operation baseline ${baselineKey} reappeared after second Hold`,
        );

        // Exactly two handler invocations for two accepted starts: no restart
        // and no duplicate delivery across the transport cutover.
        await leg.send("COUNT");
        await runtime.waitFor(
          () => (leg.executionCount() === 2 ? true : undefined),
          { timeoutMs: 15_000, intervalMs: 50 },
        );
        assert.deepEqual(
          leg.executionCount(),
          2,
          "each accepted operation must execute exactly once",
        );
      },
    );
  }, runtimeOptions);
});

test("Rust concurrent held RPC callbacks survive transport cutover", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = apps.TransportGrowthCaller.request;

    const providerInstance = await prepareOperationProvider(runtime);
    const providerId = providerContract.identity;
    const attachments = () =>
      readRuntimeServiceAttachments(runtime, providerId);
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
        assert.deepEqual((await attachments()).length, 1);

        // Fire many RPC callbacks concurrently. Each parks inside its
        // handler, so several are accepted and in flight on the broker when
        // the transport cuts over.
        const total = 12;
        const held = Array.from({ length: total }, () => {
          const pending = caller.park({}, { timeout: 90_000 }).orThrow();
          void pending.catch(() => {});
          return pending;
        });
        const parkDeadline = Date.now() + 30_000;
        while ((leg.parkCalls() ?? 0) < total) {
          if (Date.now() > parkDeadline) {
            throw new Error("not all Park callbacks were accepted");
          }
          await leg.send("PARKS");
          await new Promise((resolve) => setTimeout(resolve, 100));
        }

        // Grow only after every callback is accepted and held.
        await waitForServiceRenewalWindow(
          runtime,
          (await attachments())[0].runtimeId,
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: providerInstance.grantee,
          capabilityId: CAPABILITY_EXTEND,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });

        await runtime.waitFor(
          async () => ((await attachments()).length >= 2 ? true : undefined),
          { timeoutMs: 45_000, intervalMs: 100 },
        );

        // Release them: every accepted callback completes exactly once.
        await leg.send("RELEASE");
        const results = await Promise.all(held);
        assert.deepEqual(results.length, total);
        assert.deepEqual(
          results.map((value) => value.value).sort(),
          Array.from(
            { length: total },
            (_, index) => `parked-${index + 1}`,
          ).sort(),
        );
      },
    );
  }, runtimeOptions);
});

test("Rust baseline generation is reclaimed after growth while held and fresh callbacks complete", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = apps.TransportGrowthCaller.request;

    const providerInstance = await prepareOperationProvider(runtime);
    const providerId = providerContract.identity;
    const attachments = () =>
      readRuntimeServiceAttachments(runtime, providerId);
    const caller = await runtime.connectClient({
      name: "tg-op-caller-reclaim",
      contract: callerContract,
    });

    await withOperationProviderLeg(
      runtime,
      providerInstance.seed,
      150_000,
      async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        const original = await attachments();
        assert.deepEqual(original.length, 1);
        const baselineDigest = original[0].attachmentId;
        // Capture the exact broker server from the first complete inventory
        // read and keep it required for every later absence assertion, so a
        // dropped STATSZ discovery reply can never read as absence.
        const before = await readRuntimeBrokerInventory(runtime);
        const brokerServerId = before[0].server;
        assert(
          admittedConnections(before, new Set([baselineDigest])).length > 0,
          "the baseline generation must be physically admitted before growth",
        );

        // Hold one accepted callback on the baseline generation.
        const held = caller.park({}, { timeout: 90_000 }).orThrow();
        // Keep the rejection handled until it is awaited so a failure surfaces
        // through the runtime's control-plane diagnostics rather than as an
        // uncaught module-level promise.
        held.catch(() => {});
        const parkDeadline = Date.now() + 30_000;
        while ((leg.parkCalls() ?? 0) < 1) {
          if (Date.now() > parkDeadline) {
            throw new Error("no Park callback was accepted");
          }
          await leg.send("PARKS");
          await new Promise((resolve) => setTimeout(resolve, 100));
        }

        // Grow the provider. The baseline generation is superseded and, with no
        // baseline pin, is reclaimed once its accepted work completes.
        await waitForServiceRenewalWindow(
          runtime,
          (await attachments())[0].runtimeId,
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: providerInstance.grantee,
          capabilityId: CAPABILITY_EXTEND,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });

        await runtime.waitFor(
          async () => ((await attachments()).length >= 2 ? true : undefined),
          { timeoutMs: 45_000, intervalMs: 100 },
        );

        // New work remains accepted during handover while the baseline callback
        // is still held. Concurrent openings exercise promotion overlapping
        // request authentication, not just accepted callback completion.
        const fresh = Array.from({ length: 12 }, () =>
          caller.park({}, { timeout: 90_000 }).orThrow(),
        );
        let freshError: unknown;
        for (const opening of fresh)
          opening.catch((error: unknown) => {
            freshError = error;
          });
        const freshDeadline = Date.now() + 30_000;
        while ((leg.parkCalls() ?? 0) < fresh.length + 1) {
          if (freshError !== undefined) throw freshError;
          if (Date.now() > freshDeadline) {
            throw new Error("no post-cutover Park callback was accepted");
          }
          await leg.send("PARKS");
          await new Promise((resolve) => setTimeout(resolve, 100));
        }

        // Release: the held callback completes exactly once on the baseline.
        await leg.send("RELEASE");
        const heldResult = await held;
        assert(
          heldResult.value.startsWith("parked-"),
          `held callback completed on the baseline: ${heldResult.value}`,
        );
        const freshResults = await Promise.all(fresh);
        assert.deepEqual(
          freshResults.map((result) => result.value).sort(),
          Array.from(
            { length: fresh.length },
            (_, index) => `parked-${index + 2}`,
          ).sort(),
        );

        // The exact baseline physical attachment is gone from the broker while
        // the logical connection and its current generation keep serving.
        try {
          await runtime.waitFor(
            async () => {
              const now = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [brokerServerId],
              });
              return admittedConnections(now, new Set([baselineDigest]))
                .length === 0
                ? true
                : undefined;
            },
            { timeoutMs: 45_000, intervalMs: 200 },
          );
        } catch (cause) {
          const now = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [brokerServerId],
          });
          throw new Error(
            `baseline ${baselineDigest} not reclaimed; broker users: ${JSON.stringify(
              now.map((item) => item.authorizedUser).sort(),
            )}`,
            { cause },
          );
        }

        // Post-reap authorization: a further real generated Park RPC must be
        // accepted and executed by the current generation through its covered
        // cached context, which must no longer be vetoed by the reclaimed
        // baseline's health. RELEASE is already latched, so it completes
        // without any wait.
        const postReap = caller.park({}, { timeout: 90_000 }).orThrow();
        postReap.catch(() => {});
        const postReapResult = await postReap;
        assert.deepEqual(
          postReapResult.value,
          `parked-${fresh.length + 2}`,
          `post-reap callback must execute on the current generation: ${postReapResult.value}`,
        );
        await leg.send("PARKS");
        await runtime.waitFor(
          () =>
            (leg.parkCalls() ?? 0) === fresh.length + 2 ? true : undefined,
          { timeoutMs: 30_000, intervalMs: 50 },
        );
        // The reclaimed baseline stays absent after post-reap authorization.
        const afterPostReap = await readRuntimeBrokerInventory(runtime, {
          requiredServerIds: [brokerServerId],
        });
        assert(
          admittedConnections(afterPostReap, new Set([baselineDigest]))
            .length === 0,
          `baseline ${baselineDigest} reappeared after post-reap authorization`,
        );
      },
    );
  }, runtimeOptions);
});

/** Withdraw the optional capability using its current accepted revision. */
async function revokeProviderExtension(
  runtime: TrellisTestRuntime,
  provider: Awaited<ReturnType<typeof prepareOperationProvider>>,
) {
  const page = await runtime.callAdminRpc("authCapabilityGrantsList", {
    principalId: provider.grantee,
    capabilityId: CAPABILITY_EXTEND,
  });
  const grant = page.items[0];
  assert(grant);
  await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
    grantId: grant.grantId,
    expectedRevision: grant.revision,
    idempotencyKey: ulid(),
  });
}

/**
 * Withdrawing a capability retires the affected authorization session and every
 * attachment it admitted. A fresh connection using the same provisioned identity
 * can serve RPC under the remaining authority without reviving that session.
 */
test("Rust capability reduction retires the old session and a fresh provider serves requests", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = apps.TransportGrowthCaller.request;

    const providerInstance = await prepareOperationProvider(runtime);
    const providerId = providerContract.identity;
    const attachments = () =>
      readRuntimeServiceAttachments(runtime, providerId);
    const caller = await runtime.connectClient({
      name: "tg-op-caller-reduce",
      contract: callerContract,
    });

    await runtime.callAdminRpc("authCapabilityGrantsGrant", {
      principalId: providerInstance.grantee,
      capabilityId: CAPABILITY_EXTEND,
      expectedRevision: 0n,
      idempotencyKey: ulid(),
    });

    await withOperationProviderLeg(
      runtime,
      providerInstance.seed,
      150_000,
      async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        const original = await attachments();
        assert.deepEqual(original.length, 1);

        await runtime.waitFor(async () => {
          const ready = await caller.advance({});
          if (
            ready.isErr() &&
            !(
              "code" in ready.error &&
              ready.error.code === "trellis.request.unavailable"
            )
          )
            ready.orThrow();
          return ready.isOk() ? true : undefined;
        });
        await revokeProviderExtension(runtime, providerInstance);
        await leg.waitFor("OPERATION_PROVIDER_ERROR");
        await runtime.waitFor(
          async () => ((await attachments()).length === 0 ? true : undefined),
          { timeoutMs: 60_000, intervalMs: 200 },
        );
      },
      { allowTerminalError: true },
    );
    await withOperationProviderLeg(
      runtime,
      providerInstance.seed,
      150_000,
      async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        // The process announces configuration before run() attaches intake.
        // Advance is this fixture's stateless readiness RPC. Only wait through
        // unavailable intake; authorization failures and deadlines remain fatal.
        await runtime.waitFor(async () => {
          const ready = await caller.advance({});
          if (
            ready.isErr() &&
            !(
              "code" in ready.error &&
              ready.error.code === "trellis.request.unavailable"
            )
          )
            ready.orThrow();
          return ready.isOk() ? true : undefined;
        });
      },
    );
  }, runtimeOptions);
});

/**
 * The service hosts a durable `held` job worker. A job accepted before a real
 * transport cutover must complete exactly once (no restart or duplication) on
 * the generation that received it, and a fresh job submitted after cutover must
 * be accepted by the same logical worker host on the new generation.
 */
test("Rust held job completes once across transport growth and a fresh job is accepted after cutover", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = apps.TransportGrowthCaller.request;

    const providerInstance = await prepareOperationProvider(runtime);
    const providerId = providerContract.identity;
    const attachments = () =>
      readRuntimeServiceAttachments(runtime, providerId);
    const caller = await runtime.connectClient({
      name: "tg-op-caller-job",
      contract: callerContract,
    });

    await withOperationProviderLeg(
      runtime,
      providerInstance.seed,
      150_000,
      async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        await leg.waitFor("JOB_SUBMITTED initial");
        // The held job must be accepted and running before the cutover.
        await leg.waitFor("JOB_STARTED 1 initial");
        const original = await attachments();
        assert.deepEqual(original.length, 1);

        // Grow the optional capability while the job is held.
        await waitForServiceRenewalWindow(
          runtime,
          (await attachments())[0].runtimeId,
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: providerInstance.grantee,
          capabilityId: CAPABILITY_EXTEND,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });

        await runtime.waitFor(
          async () => ((await attachments()).length >= 2 ? true : undefined),
          { timeoutMs: 45_000, intervalMs: 100 },
        );

        // Release the held job: it completes exactly once.
        await leg.send("JOB_RELEASE");
        await leg.waitFor("JOB_DONE 1 initial");

        // A fresh job submitted after cutover is accepted on the new
        // generation by the same logical worker host.
        await caller.nudge({}).orThrow();
        await leg.waitFor("JOB_DONE 2 extended");

        await leg.send("JOB_COUNT");
        await runtime.waitFor(
          () => ((leg.jobExecutions() ?? 0) >= 2 ? true : undefined),
          { timeoutMs: 30_000, intervalMs: 100 },
        );
        assert.deepEqual(
          leg.jobExecutions(),
          2,
          "each accepted job must run exactly once",
        );
      },
    );
  }, runtimeOptions);
});

/**
 * The service hosts a durable `ticks` event listener. A delivery received
 * before a real transport cutover must stay held (and pinned to its receiving
 * generation) across the cutover, then be acknowledged; a fresh delivery after
 * cutover must be handled on the new generation.
 */
for (const rawConsumer of [false, true]) {
  test(
    rawConsumer
      ? "Rust raw durable consumer delivery survives transport growth and is acked after cutover"
      : "Rust durable event delivery survives transport growth and is acked after cutover",
    async () => {
      await withTrellisRuntime(
        async (runtime) => {
          const providerContract =
            participants.TransportGrowthOperationProvider.participant;
          const callerContract = apps.TransportGrowthCaller.request;

          const providerInstance = await prepareOperationProvider(runtime);
          const providerId = providerContract.identity;
          const attachments = () =>
            readRuntimeServiceAttachments(runtime, providerId);
          const caller = await runtime.connectClient({
            name: "tg-op-caller-tick",
            contract: callerContract,
          });

          await withOperationProviderLeg(
            runtime,
            providerInstance.seed,
            150_000,
            async (leg) => {
              await leg.waitFor("OPERATION_PROVIDER_READY");
              const original = await attachments();
              assert.deepEqual(original.length, 1);

              // The first delivery is accepted and held before cutover.
              await caller.publishTick({ value: "first" }).orThrow();
              if (rawConsumer) {
                // Both deliveries are already queued when this public stream is
                // first polled. Automatic buffering would reserve both of them.
                await caller.publishTick({ value: "second" }).orThrow();
                await leg.send("START_TICKS");
              }
              await leg.waitFor("TICK 1 first");

              const nats = await connect({
                servers: runtime.natsUrl,
                authenticator: credsAuthenticator(
                  await readFile(
                    `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
                  ),
                ),
              });
              try {
                const manager = await jetstreamManager(nats);
                const consumers = [];
                for await (const stream of manager.streams.list()) {
                  for await (const consumer of manager.consumers.list(
                    stream.config.name,
                  )) {
                    const filters =
                      consumer.config.filter_subjects ??
                      (consumer.config.filter_subject
                        ? [consumer.config.filter_subject]
                        : []);
                    if (filters.some((subject) => subject.endsWith(".Tick"))) {
                      consumers.push({ stream: stream.config.name, consumer });
                    }
                  }
                }
                assert.deepEqual(
                  consumers.length,
                  1,
                  "one provisioned Tick consumer",
                );
                const { stream, consumer } = consumers[0];
                const info = () =>
                  manager.consumers.info(stream, consumer.name);
                const gate = runtime.nativeTransportGate();
                const tickSubject =
                  consumer.config.filter_subject ??
                  consumer.config.filter_subjects?.find((subject) =>
                    subject.endsWith(".Tick"),
                  );
                assert(tickSubject);
                const receivers = () =>
                  gate
                    .connections()
                    .filter((connection) =>
                      connection.deliveries.some(
                        (delivery) => delivery.subject === tickSubject,
                      ),
                    );
                const firstReceivers = receivers();
                assert.deepEqual(firstReceivers.length, 1);
                const g1Proxy = firstReceivers[0].id;
                const originalSockets = admittedConnections(
                  await readRuntimeBrokerInventory(runtime),
                  new Set([original[0].attachmentId]),
                );
                assert.deepEqual(originalSockets.length, 1);
                const g1Socket = originalSockets[0];

                const advanceSubjects = firstReceivers[0].subs
                  .filter((sub) => sub.subject.endsWith(".Advance"))
                  .map((sub) => sub.subject);
                assert.deepEqual(advanceSubjects.length, 1);
                const advanceSubject = advanceSubjects[0];
                const readPinnedIngress = async () => {
                  const system = await connect({
                    servers: runtime.natsUrl,
                    authenticator: credsAuthenticator(
                      await readFile(
                        `${runtime.workdir}/config/trellis/nats/creds/system.creds`,
                      ),
                    ),
                  });
                  try {
                    const reply = await system.request(
                      `$SYS.REQ.SERVER.${g1Socket.server}.CONNZ`,
                      new TextEncoder().encode(
                        JSON.stringify({
                          auth: true,
                          cid: g1Socket.cid,
                          subscriptions: true,
                        }),
                      ),
                    );
                    const envelope: unknown = JSON.parse(
                      new TextDecoder().decode(reply.data),
                    );
                    assert(envelope !== null && typeof envelope === "object");
                    assert(!("error" in envelope));
                    assert(
                      "server" in envelope &&
                        envelope.server !== null &&
                        typeof envelope.server === "object" &&
                        "id" in envelope.server,
                    );
                    assert.deepEqual(envelope.server.id, g1Socket.server);
                    assert(
                      "data" in envelope &&
                        envelope.data !== null &&
                        typeof envelope.data === "object",
                    );
                    const data = envelope.data;
                    assert("server_id" in data);
                    assert.deepEqual(data.server_id, g1Socket.server);
                    assert(
                      "connections" in data && Array.isArray(data.connections),
                    );
                    assert.deepEqual(
                      data.connections.length,
                      1,
                      "Tick1 must still pin the exact G1 broker socket",
                    );
                    const connection: unknown = data.connections[0];
                    assert(
                      connection !== null && typeof connection === "object",
                    );
                    assert("cid" in connection);
                    assert.deepEqual(connection.cid, g1Socket.cid);
                    assert("authorized_user" in connection);
                    assert.deepEqual(
                      connection.authorized_user,
                      g1Socket.authorizedUser,
                    );
                    assert(
                      "subscriptions_list" in connection &&
                        Array.isArray(connection.subscriptions_list) &&
                        connection.subscriptions_list.every(
                          (subject: unknown) => typeof subject === "string",
                        ),
                      `G1 CONNZ must carry subscription detail: ${JSON.stringify(
                        connection,
                      )}`,
                    );
                    return connection.subscriptions_list.includes(
                      advanceSubject,
                    );
                  } finally {
                    await system.close();
                  }
                };
                assert(
                  await readPinnedIngress(),
                  "G1 initially owns Advance ingress",
                );

                // A concurrency-one handler holds its receive slot through its ACK.
                // This delivery must remain queued, not disappear into an old
                // attachment's automatically replenished pull buffer.
                if (!rawConsumer) {
                  await caller.publishTick({ value: "second" }).orThrow();
                }

                // Grow while the delivery is held.
                await waitForServiceRenewalWindow(
                  runtime,
                  (await attachments())[0].runtimeId,
                );
                await runtime.callAdminRpc("authCapabilityGrantsGrant", {
                  principalId: providerInstance.grantee,
                  capabilityId: CAPABILITY_EXTEND,
                  expectedRevision: 0n,
                  idempotencyKey: ulid(),
                });

                await runtime.waitFor(
                  async () =>
                    (await attachments()).length >= 2 ? true : undefined,
                  { timeoutMs: 45_000, intervalMs: 100 },
                );
                await caller.advance({}).orThrow();

                // Confirm retirement at the broker without closing the socket
                // that still owns the accepted delivery.
                await runtime.waitFor(async () =>
                  (await readPinnedIngress()) ? undefined : true,
                );

                const held = await info();
                assert.deepEqual(
                  held.num_ack_pending,
                  1,
                  "only the accepted handler owns a delivery",
                );
                assert.deepEqual(
                  held.num_pending,
                  1,
                  "the next event stays durably queued across growth",
                );
                assert.deepEqual(
                  held.num_waiting,
                  0,
                  "the occupied receive slot issues no second pull",
                );

                // Release: the held delivery is acknowledged after the cutover.
                await leg.send("RELEASE");
                await leg.waitFor("TICK_DONE 1 first");

                // The queued delivery is handled after cutover, without redelivery or
                // replacing the durable consumer's identity.
                await leg.waitFor("TICK_DONE 2 second");
                await runtime.waitFor(async () => {
                  const settled = await info();
                  assert.deepEqual(settled.name, consumer.name);
                  return settled.ack_floor.consumer_seq === 2 &&
                    settled.num_ack_pending === 0 &&
                    settled.num_pending === 0
                    ? true
                    : undefined;
                });
                const nextReceivers = receivers().filter(
                  (connection) => connection.id !== g1Proxy,
                );
                assert.deepEqual(
                  nextReceivers.length,
                  1,
                  "the next event is received on the successor attachment",
                );
                const g2Proxy = nextReceivers[0].id;
                for (const [sequence, expected] of [
                  [1, g1Proxy],
                  [2, g2Proxy],
                ]) {
                  const dispositions = gate
                    .connections()
                    .flatMap((connection) =>
                      connection.outboundContexts
                        .filter(
                          (out) =>
                            out.subject.startsWith(
                              `$JS.ACK.${stream}.${consumer.name}.`,
                            ) && out.subject.split(".")[6] === String(sequence),
                        )
                        .map(() => connection.id),
                    );
                  assert(
                    dispositions.length > 0 &&
                      dispositions.every((receiver) => receiver === expected),
                    "each acknowledged event uses its actual receiving attachment",
                  );
                }
                await runtime.waitFor(async () => {
                  const inventory = await readRuntimeBrokerInventory(runtime, {
                    requiredServerIds: [g1Socket.server],
                  });
                  return inventory.every(
                    (connection) =>
                      connection.server !== g1Socket.server ||
                      connection.cid !== g1Socket.cid,
                  )
                    ? true
                    : undefined;
                });
              } finally {
                await nats.close();
              }
            },
            { rawConsumer },
          );
        },
        { ...runtimeOptions, interruptibleNativeProxy: true },
      );
    },
  );
}

/**
 * A held durable delivery is pinned to the generation that received it. When an
 * its physical attachment is kicked, the broker force-closes it even though
 * the delivery still holds a lease, so the pending
 * acknowledgement cannot reach the broker. The durable intake must not tear the
 * service runtime down: it enters bounded recovery, the unacknowledged event is
 * redelivered, and a subsequent event is processed on the surviving generation —
 * without retiring its authorization session.
 */
test("Rust durable event intake recovers when its held delivery's attachment is kicked", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerContract =
      participants.TransportGrowthOperationProvider.participant;
    const callerContract = apps.TransportGrowthCaller.request;

    const providerInstance = await prepareOperationProvider(runtime);
    const providerId = providerContract.identity;
    const attachments = () =>
      readRuntimeServiceAttachments(runtime, providerId);
    const caller = await runtime.connectClient({
      name: "tg-op-caller-failed-ack",
      contract: callerContract,
    });

    await withOperationProviderLeg(
      runtime,
      providerInstance.seed,
      150_000,
      async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        const original = await attachments();
        assert.deepEqual(original.length, 1);

        const baselineDigest = original[0].attachmentId;

        // Grow: a wider generation becomes current and the durable intake
        // follows it. Only actual accepted work and coverage pins retain the
        // superseded generation; there is no lifetime baseline lease.
        await waitForServiceRenewalWindow(
          runtime,
          (await attachments())[0].runtimeId,
        );
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: providerInstance.grantee,
          capabilityId: CAPABILITY_EXTEND,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });

        await runtime.waitFor(
          async () => ((await attachments()).length >= 2 ? true : undefined),
          { timeoutMs: 45_000, intervalMs: 100 },
        );
        // The wider generation's exact physical identity: the generation that
        // admitted the grown authority, joined to the broker inventory by the
        // context digest embedded in the callout-issued user.
        const grown = await attachments();
        const wide = grown.find((item) => item.attachmentId !== baselineDigest);
        assert(wide, "a wider generation must be admitted after growth");
        const wideDigest = wide.attachmentId;

        // Arm a value-scoped hold, then complete a warmup delivery so the durable
        // intake is positively on the current (wider) generation before the real
        // delivery is held. No timed readiness is used.
        await leg.send("HOLD_ONLY held");
        await caller.publishTick({ value: "warmup" }).orThrow();
        await leg.waitFor("TICK 1 warmup");
        await leg.waitFor("TICK_DONE 1 warmup");

        // Exact physical inventory before the cutover: the broker's own
        // callout-owned sockets, not the auth-side presence records. The wider
        // generation's socket must be present now. The exact broker server is
        // captured and kept required for the critical absence gate below.
        const beforeCutover = await readRuntimeBrokerInventory(runtime);
        const brokerServerId = beforeCutover[0].server;
        assert(
          admittedConnections(beforeCutover, new Set([wideDigest])).length > 0,
          "the wider generation's physical socket must be admitted before the cutover",
        );

        // Hold the real delivery on the current generation before losing it.
        await caller.publishTick({ value: "held" }).orThrow();
        await leg.waitFor("TICK 2 held");

        await runtime.callAdminRpc("authConnectionsKick", {
          attachmentId: wideDigest,
          idempotencyKey: ulid(),
        });
        // Prove the receiving socket is gone while the handler is still held.
        await runtime.waitFor(
          async () => {
            const now = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [brokerServerId],
            });
            const wideGone =
              admittedConnections(now, new Set([wideDigest])).length === 0;
            return wideGone ? true : undefined;
          },
          { timeoutMs: 30_000, intervalMs: 400 },
        );

        // Release: the handler completes, but its acknowledgement cannot reach
        // the broker on the force-closed generation.
        await leg.send("RELEASE");
        await leg.waitFor("TICK_DONE 2 held");

        // Corroboration only: the broker redelivers the unacknowledged event,
        // which proves the ack did not land. The physical closure itself is
        // proven by the exact CONNZ inventory above, not by the redelivery.
        try {
          await runtime.waitFor(
            () => (leg.tickDeliveries("held") >= 2 ? true : undefined),
            { timeoutMs: 90_000, intervalMs: 100 },
          );
        } catch {
          throw new Error(
            "the held delivery stayed acknowledged: the forced-close acknowledgement path was not exercised",
          );
        }
        await leg.waitFor("TICK_DONE 3 held");

        // A subsequent event is processed on the surviving generation: the
        // durable intake recovered instead of tearing down the service runtime.
        await caller.publishTick({ value: "after" }).orThrow();
        await leg.waitFor("TICK_DONE 4 after");
      },
    );
  }, runtimeOptions);
});

/**
 * A supported terminal deployment action (native deployment disable) must make
 * the durable event intake surface its terminal transport error instead of
 * keeping the service runtime alive on a revoked transport. The fixture reports
 * that error on exit, which is the observable asserted here. Presence records
 * are ephemeral admission bookkeeping, so they are not asserted.
 */
test("Rust durable event intake surfaces terminal transport loss after deployment disable", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerInstance = await prepareOperationProvider(runtime);

    await withOperationProviderLeg(
      runtime,
      providerInstance.seed,
      120_000,
      async (leg) => {
        await leg.waitFor("OPERATION_PROVIDER_READY");
        // A supported terminal native action: disabling the deployment
        // terminates its connections and denies new native bootstrap.
        const current = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: codecs.ulid.decode(providerInstance.deploymentId),
        });
        await runtime.callAdminRpc("authDeploymentsDisable", {
          deploymentId: codecs.ulid.decode(providerInstance.deploymentId),
          expectedRevision: current.deployment.revision,
          idempotencyKey: ulid(),
          reason: null,
        });
        // Primary acceptance: the service runtime surfaces the terminal
        // transport error rather than retrying a dead authority forever.
        await runtime.waitFor(
          () => (leg.error() !== undefined ? true : undefined),
          { timeoutMs: 90_000, intervalMs: 250 },
        );
        assert(
          leg.error() !== undefined,
          "the service runtime must surface a terminal transport error",
        );
      },
      { allowTerminalError: true },
    );
  });
});
