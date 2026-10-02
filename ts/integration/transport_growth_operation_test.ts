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
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
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
  const child = new Deno.Command("setsid", {
    args: rustFixtureArgv("transport_growth_operation"),
    env: {
      TRELLIS_URL: runtime.trellisUrl,
      TRELLIS_IDENTITY_SEED: seed,
      TRELLIS_RAW_CONSUMER: options.rawConsumer ? "1" : "0",
    },
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const lines: string[] = [];
  let output = "";
  let stderrOutput = "";
  let observedStatus: Deno.CommandStatus | undefined;
  let exited = false;
  const status = child.status.then((result) => {
    observedStatus = result;
    exited = true;
    return result;
  });
  const stdin = child.stdin.getWriter();
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
    const reader = child.stdout.pipeThrough(new TextDecoderStream())
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
    const reader = child.stderr.pipeThrough(new TextDecoderStream())
      .getReader();
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      stderrOutput = (stderrOutput + chunk.value).slice(-16_384);
      console.error(
        chunk.value.trimEnd().split("\n").map((line) =>
          `[Rust operation provider pid=${child.pid} stderr] ${line}`
        ).join("\n"),
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
          const error = lines.find((line) =>
            line.startsWith("OPERATION_PROVIDER_ERROR ") ||
            line.startsWith("RAW_TICKS_ERROR ")
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
        line.startsWith("OPERATION_PROVIDER_ERROR ")
      );
      return line?.slice("OPERATION_PROVIDER_ERROR ".length);
    },
  };

  try {
    await body(leg);
    const terminalError = lines.some((line) =>
      line.startsWith("OPERATION_PROVIDER_ERROR ")
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
      assert(
        finalStatus.success,
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
    if (!exited) {
      try {
        Deno.kill(-child.pid, "SIGKILL");
      } catch {
        // the process may have exited between the check and the signal.
      }
    }
    await Promise.allSettled([status, drainSettled, stderrDrainSettled]);
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
          await leg.waitFor("JOB_STARTED 1 initial");
          const before = await attachments();
          assertEquals(
            before.length,
            1,
            "a fresh operation provider has exactly one admitted attachment",
          );
          const baselineSockets = admittedConnections(
            await readRuntimeBrokerInventory(runtime),
            new Set([before[0].contextDigest]),
          );
          assertEquals(baselineSockets.length, 1);
          const baselineSocket = baselineSockets[0];
          const baselineKey = brokerConnectionKey(baselineSocket);

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

          // This shared fixture also parks an initial job on G1. Complete that
          // independent accepted work so its legitimate lease cannot mask whether
          // operation execution releases the baseline generation.
          await leg.send("JOB_RELEASE initial");
          await leg.waitFor("JOB_DONE 1 initial");

          // Reap G1 before starting fresh work. Require its exact broker server
          // in every complete inventory, so missing discovery cannot prove absence.
          try {
            await runtime.waitFor(async () => {
              const inventory = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [baselineSocket.server],
              });
              return inventory.every((item) =>
                  brokerConnectionKey(item) !== baselineKey
                )
                ? true
                : undefined;
            }, { timeoutMs: 45_000, intervalMs: 200 });
          } catch (cause) {
            const inventory = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [baselineSocket.server],
            });
            throw new Error(
              `operation baseline ${baselineKey} not reclaimed; broker connections: ${
                JSON.stringify(inventory)
              }`,
              { cause },
            );
          }
          console.log(
            `operation baseline ${baselineKey} absent before second Hold`,
          );

          // With G1 physically absent, a start on the new generation must persist,
          // accept control, and complete exactly once through ordinary operation APIs.
          const after = await caller.hold({}).start().orThrow().catch(
            (cause) => {
              console.error("post-reap Hold start failed", cause);
              throw cause;
            },
          );
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
          const afterSecond = await readRuntimeBrokerInventory(runtime, {
            requiredServerIds: [baselineSocket.server],
          });
          assert(
            afterSecond.every((item) =>
              brokerConnectionKey(item) !== baselineKey
            ),
            `operation baseline ${baselineKey} reappeared after second Hold`,
          );

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

Deno.test(
  "Rust baseline generation is reclaimed after growth while held and fresh callbacks complete",
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
          assertEquals(original.length, 1);
          const baselineDigest = original[0].contextDigest;
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
          await runtime.contracts.apply({
            deployment: PROVIDER_DEPLOYMENT,
            contract: providerContract,
          });
          await runtime.waitFor(
            async () => (await attachments()).length >= 2 ? true : undefined,
            { timeoutMs: 45_000, intervalMs: 100 },
          );

          // New work is accepted on the current generation while the baseline
          // callback is still held.
          const fresh = caller.park({}, { timeout: 90_000 }).orThrow();
          fresh.catch(() => {});
          const freshDeadline = Date.now() + 30_000;
          while ((leg.parkCalls() ?? 0) < 2) {
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
          const freshResult = await fresh;
          assert(
            freshResult.value.startsWith("parked-"),
            `fresh callback completed on the current generation: ${freshResult.value}`,
          );

          // The exact baseline physical attachment is gone from the broker while
          // the logical connection and its current generation keep serving.
          try {
            await runtime.waitFor(async () => {
              const now = await readRuntimeBrokerInventory(runtime, {
                requiredServerIds: [brokerServerId],
              });
              return admittedConnections(now, new Set([baselineDigest]))
                  .length === 0
                ? true
                : undefined;
            }, { timeoutMs: 45_000, intervalMs: 200 });
          } catch (cause) {
            const now = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [brokerServerId],
            });
            throw new Error(
              `baseline ${baselineDigest} not reclaimed; broker users: ${
                JSON.stringify(now.map((item) => item.authorizedUser).sort())
              }`,
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
          assertEquals(
            postReapResult.value,
            "parked-3",
            `post-reap callback must execute on the current generation: ${postReapResult.value}`,
          );
          await leg.send("PARKS");
          await runtime.waitFor(
            () => (leg.parkCalls() ?? 0) === 3 ? true : undefined,
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

/**
 * The service hosts a durable `held` job worker. A job accepted before a real
 * transport cutover must complete exactly once (no restart or duplication) on
 * the generation that received it, and a fresh job submitted after cutover must
 * be accepted by the same logical worker host on the new generation.
 */
Deno.test(
  "Rust held job completes once across transport growth and a fresh job is accepted after cutover",
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
          assertEquals(original.length, 1);
          const originalRuntimeId = original[0].runtimeConnectionId;
          // Captured before growth so the reduction restores exactly the
          // authority the original attachment adopted.
          const baselineKeys = new Set(
            (await readGrantBinding(runtime, providerId)).grants.permissions
              .map(
                atomKey,
              ),
          );

          // Grow the optional capability while the job is held.
          await runtime.contracts.apply({
            deployment: PROVIDER_DEPLOYMENT,
            contract: providerContract,
          });
          await runtime.waitFor(
            async () => (await attachments()).length >= 2 ? true : undefined,
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
            () => (leg.jobExecutions() ?? 0) >= 2 ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 100 },
          );
          assertEquals(
            leg.jobExecutions(),
            2,
            "each accepted job must run exactly once",
          );

          // Reduce back to the original authority: the wider generation closes
          // and the surviving generation's worker intake must be re-adopted, or
          // fresh work would be accepted by a dead intake.
          const binding = await readGrantBinding(runtime, providerId);
          const kept = binding.grants.permissions.filter((atom) =>
            baselineKeys.has(atomKey(atom))
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
          await runtime.waitFor(
            async () => (await attachments()).length === 1 ? true : undefined,
            { timeoutMs: 60_000, intervalMs: 200 },
          );
          await caller.nudge({}).orThrow();
          await leg.waitFor("JOB_DONE 3 extended");
          await leg.send("JOB_COUNT");
          await runtime.waitFor(
            () => (leg.jobExecutions() ?? 0) >= 3 ? true : undefined,
            { timeoutMs: 30_000, intervalMs: 100 },
          );
          assertEquals(
            leg.jobExecutions(),
            3,
            "the survivor's re-adopted intake must accept fresh work exactly once",
          );

          // One logical connection spans the cutover; growth adds a physical
          // attachment without changing the logical identity.
          const grown = await attachments();
          assert(
            grown.length === 1 &&
              grown.every((item) =>
                item.runtimeConnectionId === originalRuntimeId
              ),
            "the reduction must keep one logical connection identity",
          );
        },
      );
    }, runtimeOptions);
  },
);

/**
 * The service hosts a durable `ticks` event listener. A delivery received
 * before a real transport cutover must stay held (and pinned to its receiving
 * generation) across the cutover, then be acknowledged; a fresh delivery after
 * cutover must be handled on the new generation.
 */
for (const rawConsumer of [false, true]) {
  Deno.test(
    rawConsumer
      ? "Rust raw durable consumer delivery survives transport growth and is acked after cutover"
      : "Rust durable event delivery survives transport growth and is acked after cutover",
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
          const page = await runtime.callAdminRpc(
            "authConnectionsList",
            {},
          ) as {
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
            assertEquals(original.length, 1);
            const originalRuntimeId = original[0].runtimeConnectionId;
            const baselineKeys = new Set(
              (await readGrantBinding(runtime, providerId)).grants.permissions
                .map(
                  atomKey,
                ),
            );

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
                await Deno.readFile(
                  `${runtime.workdir}/nats/creds/trellis-auth.creds`,
                ),
              ),
            });
            try {
              const manager = await jetstreamManager(nats);
              const consumers = [];
              for await (const stream of manager.streams.list()) {
                for await (
                  const consumer of manager.consumers.list(stream.config.name)
                ) {
                  const filters = consumer.config.filter_subjects ??
                    (consumer.config.filter_subject
                      ? [consumer.config.filter_subject]
                      : []);
                  if (filters.some((subject) => subject.endsWith(".Tick"))) {
                    consumers.push({ stream: stream.config.name, consumer });
                  }
                }
              }
              assertEquals(
                consumers.length,
                1,
                "one provisioned Tick consumer",
              );
              const { stream, consumer } = consumers[0];
              const info = () => manager.consumers.info(stream, consumer.name);
              const gate = runtime.nativeTransportGate();
              const tickSubject = consumer.config.filter_subject ??
                consumer.config.filter_subjects?.find((subject) =>
                  subject.endsWith(".Tick")
                );
              assert(tickSubject);
              const receivers = () =>
                gate.connections().filter((connection) =>
                  connection.deliveries.some((delivery) =>
                    delivery.subject === tickSubject
                  )
                );
              const firstReceivers = receivers();
              assertEquals(firstReceivers.length, 1);
              const g1Proxy = firstReceivers[0].id;
              const originalSockets = admittedConnections(
                await readRuntimeBrokerInventory(runtime),
                new Set([original[0].contextDigest]),
              );
              assertEquals(originalSockets.length, 1);
              const g1Socket = originalSockets[0];

              const advanceSubjects = firstReceivers[0].subs.filter((sub) =>
                sub.subject.endsWith(".Advance")
              ).map((sub) => sub.subject);
              assertEquals(advanceSubjects.length, 1);
              const advanceSubject = advanceSubjects[0];
              const readPinnedIngress = async () => {
                const system = await connect({
                  servers: runtime.natsUrl,
                  authenticator: credsAuthenticator(
                    await Deno.readFile(
                      `${runtime.workdir}/nats/creds/system.creds`,
                    ),
                  ),
                });
                try {
                  const reply = await system.request(
                    `$SYS.REQ.SERVER.${g1Socket.server}.CONNZ`,
                    new TextEncoder().encode(JSON.stringify({
                      auth: true,
                      cid: g1Socket.cid,
                      subscriptions: true,
                    })),
                  );
                  const envelope: unknown = JSON.parse(
                    new TextDecoder().decode(reply.data),
                  );
                  assert(envelope !== null && typeof envelope === "object");
                  assert(!("error" in envelope));
                  assert(
                    "server" in envelope && envelope.server !== null &&
                      typeof envelope.server === "object" &&
                      "id" in envelope.server,
                  );
                  assertEquals(envelope.server.id, g1Socket.server);
                  assert(
                    "data" in envelope && envelope.data !== null &&
                      typeof envelope.data === "object",
                  );
                  const data = envelope.data;
                  assert("server_id" in data);
                  assertEquals(data.server_id, g1Socket.server);
                  assert(
                    "connections" in data && Array.isArray(data.connections),
                  );
                  assertEquals(
                    data.connections.length,
                    1,
                    "Tick1 must still pin the exact G1 broker socket",
                  );
                  const connection: unknown = data.connections[0];
                  assert(connection !== null && typeof connection === "object");
                  assert("cid" in connection);
                  assertEquals(connection.cid, g1Socket.cid);
                  assert("authorized_user" in connection);
                  assertEquals(
                    connection.authorized_user,
                    g1Socket.authorizedUser,
                  );
                  assert(
                    "subscriptions_list" in connection &&
                      Array.isArray(connection.subscriptions_list) &&
                      connection.subscriptions_list.every((subject: unknown) =>
                        typeof subject === "string"
                      ),
                    `G1 CONNZ must carry subscription detail: ${
                      JSON.stringify(connection)
                    }`,
                  );
                  return connection.subscriptions_list.includes(advanceSubject);
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
              await runtime.contracts.apply({
                deployment: PROVIDER_DEPLOYMENT,
                contract: providerContract,
              });
              await runtime.waitFor(
                async () =>
                  (await attachments()).length >= 2 ? true : undefined,
                { timeoutMs: 45_000, intervalMs: 100 },
              );
              await caller.advance({}).orThrow();

              // Admission and a successful Advance can occur during overlapping
              // candidate intake, before publication. Retirement is issued inside
              // the locked publication transition; the next durable receive takes
              // that same lock. Broker removal of G1's exact ingress therefore
              // establishes the cutover without closing its held-delivery socket.
              const ingressAtAdmission = await readPinnedIngress();
              console.log("TICK_CUTOVER", {
                rawConsumer,
                server: g1Socket.server,
                cid: g1Socket.cid,
                advanceSubject,
                g1IngressAtAdmission: ingressAtAdmission,
              });
              await runtime.waitFor(async () =>
                await readPinnedIngress() ? undefined : true
              );
              console.log(
                "TICK_CUTOVER G1 ingress retired; Tick1 still pins G1",
              );

              const held = await info();
              assertEquals(
                held.num_ack_pending,
                1,
                "only the accepted handler owns a delivery",
              );
              assertEquals(
                held.num_pending,
                1,
                "the next event stays durably queued across growth",
              );
              assertEquals(
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
                assertEquals(settled.name, consumer.name);
                return settled.ack_floor.consumer_seq === 2 &&
                    settled.num_ack_pending === 0 && settled.num_pending === 0
                  ? true
                  : undefined;
              });
              const nextReceivers = receivers().filter((connection) =>
                connection.id !== g1Proxy
              );
              assertEquals(
                nextReceivers.length,
                1,
                "the next event is received on the successor attachment",
              );
              const g2Proxy = nextReceivers[0].id;
              for (const [sequence, expected] of [[1, g1Proxy], [2, g2Proxy]]) {
                const dispositions = gate.connections().flatMap((connection) =>
                  connection.outboundContexts.filter((out) =>
                    out.subject.startsWith(
                      `$JS.ACK.${stream}.${consumer.name}.`,
                    ) &&
                    out.subject.split(".")[6] === String(sequence)
                  ).map(() => connection.id)
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
                return inventory.every((connection) =>
                    connection.server !== g1Socket.server ||
                    connection.cid !== g1Socket.cid
                  )
                  ? true
                  : undefined;
              });

              // Reduce back to baseline: the unsafe wider generation must close
              // immediately regardless of any lingering lease or subscription, and a
              // fresh delivery must still be handled under the original authority.
              const widerSockets = admittedConnections(
                await readRuntimeBrokerInventory(runtime, {
                  requiredServerIds: [g1Socket.server],
                }),
                new Set(
                  (await attachments()).map((item) => item.contextDigest),
                ),
              );
              assertEquals(widerSockets.length, 1);
              const widerSocket = widerSockets[0];
              const binding = await readGrantBinding(runtime, providerId);
              const kept = binding.grants.permissions.filter((atom) =>
                baselineKeys.has(atomKey(atom))
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
              await runtime.waitFor(
                async () => {
                  const current = await attachments();
                  assert(
                    current.every((item) =>
                      item.runtimeConnectionId === originalRuntimeId
                    ),
                    "reduction must never change the logical connection identity",
                  );
                  const inventory = await readRuntimeBrokerInventory(runtime, {
                    requiredServerIds: [g1Socket.server, widerSocket.server],
                  });
                  return current.length === 1 &&
                      inventory.every((item) =>
                        item.server !== widerSocket.server ||
                        item.cid !== widerSocket.cid
                      ) &&
                      admittedConnections(
                          inventory,
                          new Set(current.map((item) => item.contextDigest)),
                        ).length === 1
                    ? true
                    : undefined;
                },
                { timeoutMs: 60_000, intervalMs: 200 },
              );
              await caller.publishTick({ value: "third" }).orThrow();
              await leg.waitFor("TICK_DONE 3 third");

              const grown = await attachments();
              assert(
                grown.length === 1 &&
                  grown.every((item) =>
                    item.runtimeConnectionId === originalRuntimeId
                  ),
                "the reduction must keep one logical connection identity",
              );
            } finally {
              await nats.close();
            }
          },
          { rawConsumer },
        );
      }, { ...runtimeOptions, interruptibleNativeProxy: true });
    },
  );
}

/**
 * A held durable delivery is pinned to the generation that received it. When an
 * ordinary grant reduction makes that generation unsafe, the new architecture
 * force-closes it even though the delivery still holds a lease, so the pending
 * acknowledgement cannot reach the broker. The durable intake must not tear the
 * service runtime down: it enters bounded recovery, the unacknowledged event is
 * redelivered, and a subsequent event is processed on the surviving generation —
 * under one stable logical connection identity.
 */
Deno.test(
  "Rust durable event intake recovers when a reduction force-closes the generation holding a delivery",
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
          assertEquals(original.length, 1);
          const originalRuntimeId = original[0].runtimeConnectionId;
          const baselineDigest = original[0].contextDigest;
          const baselineKeys = new Set(
            (await readGrantBinding(runtime, providerId)).grants.permissions
              .map(atomKey),
          );

          // Grow: a wider generation becomes current and the durable intake
          // follows it. Only actual accepted work and coverage pins retain the
          // superseded generation; there is no lifetime baseline lease.
          await runtime.contracts.apply({
            deployment: PROVIDER_DEPLOYMENT,
            contract: providerContract,
          });
          await runtime.waitFor(
            async () => (await attachments()).length >= 2 ? true : undefined,
            { timeoutMs: 45_000, intervalMs: 100 },
          );
          // The wider generation's exact physical identity: the generation that
          // admitted the grown authority, joined to the broker inventory by the
          // context digest embedded in the callout-issued user.
          const grown = await attachments();
          const wide = grown.find((item) =>
            item.contextDigest !== baselineDigest
          );
          assert(wide, "a wider generation must be admitted after growth");
          const wideDigest = wide.contextDigest;

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
            admittedConnections(beforeCutover, new Set([wideDigest])).length >
              0,
            "the wider generation's physical socket must be admitted before the cutover",
          );

          // Hold the real delivery on the current (wider) generation before the
          // reduce.
          await caller.publishTick({ value: "held" }).orThrow();
          await leg.waitFor("TICK 2 held");

          // Reduce to the baseline authority. The wider generation is unsafe and
          // is force-closed even though the held delivery still pins its lease.
          const binding = await readGrantBinding(runtime, providerId);
          const kept = binding.grants.permissions.filter((atom) =>
            baselineKeys.has(atomKey(atom))
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
          // Exact broker proof, while the delivery is still held: the unsafe wider
          // socket is physically gone from the callout-owned CONNZ inventory and a
          // fresh survivor socket has opened. This attributes the close to the real
          // socket, not to a lagging presence record.
          await runtime.waitFor(async () => {
            // One exact physical snapshot: the wider generation's socket is gone
            // while the baseline generation's socket is still present, so the
            // logical connection is alive and only the unsafe generation closed.
            const now = await readRuntimeBrokerInventory(runtime, {
              requiredServerIds: [brokerServerId],
            });
            const wideGone =
              admittedConnections(now, new Set([wideDigest])).length === 0;
            const baselinePresent =
              admittedConnections(now, new Set([baselineDigest])).length > 0;
            return wideGone && baselinePresent ? true : undefined;
          }, { timeoutMs: 30_000, intervalMs: 400 });

          // Release: the handler completes, but its acknowledgement cannot reach
          // the broker on the force-closed generation.
          await leg.send("RELEASE");
          await leg.waitFor("TICK_DONE 2 held");

          // Corroboration only: the broker redelivers the unacknowledged event,
          // which proves the ack did not land. The physical closure itself is
          // proven by the exact CONNZ inventory above, not by the redelivery.
          try {
            await runtime.waitFor(
              () => leg.tickDeliveries("held") >= 2 ? true : undefined,
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

          const remaining = await attachments();
          assert(
            remaining.every((item) =>
              item.runtimeConnectionId === originalRuntimeId
            ),
            "recovery must keep one logical connection identity",
          );
        },
      );
    }, runtimeOptions);
  },
);

/**
 * A supported terminal deployment action (native deployment disable) must make
 * the durable event intake surface its terminal transport error instead of
 * keeping the service runtime alive on a revoked transport. The fixture reports
 * that error on exit, which is the observable asserted here. Presence records
 * are ephemeral admission bookkeeping, so they are not asserted.
 */
Deno.test(
  "Rust durable event intake surfaces terminal transport loss after deployment disable",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract =
        participants.TransportGrowthOperationProvider.participant;

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

      await withOperationProviderLeg(
        runtime,
        providerInstance.seed,
        120_000,
        async (leg) => {
          await leg.waitFor("OPERATION_PROVIDER_READY");
          // A supported terminal native action: disabling the deployment
          // terminates its connections and denies new native bootstrap.
          const current = await runtime.callAdminRpc("authDeploymentsGet", {
            deploymentId: providerInstance.deploymentId,
          });
          await runtime.callAdminRpc("authDeploymentsDisable", {
            deploymentId: providerInstance.deploymentId,
            expectedVersion: current.deployment.version,
            idempotencyKey: crypto.randomUUID(),
            reason: null,
          });
          // Primary acceptance: the service runtime surfaces the terminal
          // transport error rather than retrying a dead authority forever.
          await runtime.waitFor(
            () => leg.error() !== undefined ? true : undefined,
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
  },
);
