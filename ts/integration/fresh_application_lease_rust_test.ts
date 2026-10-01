/** Fresh application work must not use a live carrier with lost own coverage. */
import { assert, assertEquals } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { encodePermissionTargetWasm } from "@oatscenter/trellis/auth";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("Rust fresh Jobs leases reject suspended own coverage and resume on the same carrier", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthOperationProvider.participant;
    const deployment = "fresh-lease-rust";
    await runtime.contracts.apply({ deployment, contract });
    const instance = await runtime.services.createInstance({
      deployment,
      name: deployment,
      contract,
    });
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    const js = await jetstreamManager(nats);
    const gate = runtime.nativeTransportGate();
    const child = new Deno.Command("setsid", {
      args: rustFixtureArgv("transport_growth_operation"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: instance.seed,
      },
      stdin: "piped",
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const stdin = child.stdin.getWriter();
    let exited = false;
    const status = child.status.then((value) => {
      exited = true;
      return value;
    });
    const lines: string[] = [];
    const drain = (async () => {
      let pending = "";
      for await (
        const chunk of child.stdout.pipeThrough(new TextDecoderStream())
      ) {
        const complete = (pending + chunk).split("\n");
        pending = complete.pop() ?? "";
        lines.push(...complete.map((line) => line.trim()));
      }
    })();
    const send = async (command: string) => {
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    };
    const marker = async (prefix: string, after = 0) =>
      await runtime.waitFor(() => {
        const found = lines.slice(after).find((line) =>
          line.startsWith(prefix)
        );
        if (found) return found;
        if (exited) throw new Error(`fixture exited: ${lines.join("\n")}`);
      }, { timeoutMs: 15_000, intervalMs: 20 }).catch((cause) => {
        throw new Error(
          `missing ${prefix}; fixture output:\n${lines.join("\n")}`,
          {
            cause,
          },
        );
      });
    let hold: ReturnType<typeof gate.armResponseHold> | undefined;
    let creates: ReturnType<typeof nats.subscribe> | undefined;
    const releases: Promise<void>[] = [];
    try {
      await marker("OPERATION_PROVIDER_READY");
      await marker("JOB_STARTED 1 initial");
      const attachments =
        (await runtime.callAdminRpc("authConnectionsList", {}) as {
          items: { participantId: string; contextDigest: string }[];
        }).items.filter((item) => item.participantId === contract.identity);
      assertEquals(attachments.length, 1);
      const digest = attachments[0].contextDigest;
      const sockets = admittedConnections(
        await readRuntimeBrokerInventory(runtime),
        new Set([digest]),
      );
      assertEquals(sockets.length, 1);
      const physical = brokerConnectionKey(sockets[0]);
      const own = [];
      for await (const stream of js.streams.list()) {
        for await (const consumer of js.consumers.list(stream.config.name)) {
          if (
            consumer.config.filter_subject?.endsWith(`.revocation.${digest}`)
          ) {
            const carrier = gate.connections().find((connection) =>
              !connection.closed &&
              connection.subs.some((sub) =>
                sub.subject === consumer.config.deliver_subject
              )
            );
            if (carrier) {
              own.push({ stream: stream.config.name, consumer, carrier });
            }
          }
        }
      }
      assertEquals(
        own.length,
        1,
        "delete only installed own coverage on its real carrier",
      );
      const { stream, consumer, carrier } = own[0];
      const heartbeats = () =>
        gate.connection(carrier.id)!.outboundContexts.filter((out) =>
          out.subject.startsWith("health.v1.heartbeat.")
        );
      assert(
        heartbeats().length > 0,
        "the ordinary service heartbeat really publishes before coverage loss",
      );
      let held = false;
      const arm = () => {
        held = false;
        hold = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${stream}.`,
          carrier.id,
          (body) => {
            const info = JSON.parse(new TextDecoder().decode(body));
            return info.config?.filter_subject ===
              consumer.config.filter_subject;
          },
        );
        hold.held.then(() => {
          held = true;
        });
      };
      let rearming = true;
      let retryFailure: unknown;
      let retries = 0;
      // A timed-out readiness request cannot install coverage. Hold each genuine
      // successor reply, rearming on its broker CREATE before its later INFO.
      creates = nats.subscribe(`$JS.API.CONSUMER.CREATE.${stream}.>`, {
        callback: (_error, message) => {
          const request = JSON.parse(new TextDecoder().decode(message.data));
          if (
            !rearming || !held ||
            request.config?.filter_subject !== consumer.config.filter_subject
          ) return;
          try {
            assert(
              ++retries <= 12,
              "bounded genuine warmup retry chain exhausted",
            );
            const release = hold!.release();
            arm();
            releases.push(release.catch((cause) => {
              retryFailure = cause;
            }));
          } catch (cause) {
            retryFailure = cause;
          }
        },
      });
      await nats.flush();
      arm();
      assert(await js.consumers.delete(stream, consumer.name));
      await runtime.waitFor(async () => {
        const before = lines.length;
        await send("AUTH_STATUS");
        return (await marker("AUTH_USABLE", before)) === "AUTH_USABLE false"
          ? true
          : undefined;
      }, { timeoutMs: 15_000, intervalMs: 50 });
      await runtime.waitFor(() => held ? true : undefined);
      assert(!gate.connection(carrier.id)!.closed);
      const before = lines.length;
      const outbound = gate.connection(carrier.id)!.outboundContexts.length;
      await send("JOB_STATUS initial");
      const query = await marker("JOB_STATUS", before);
      assert(
        query.startsWith("JOB_STATUS_ERROR") && query.includes("suspended"),
        query,
      );
      await send("JOB_SUBMIT suspended");
      const submit = await marker("JOB_SUBMIT_ERROR", before);
      assert(submit.includes("suspended"), submit);
      assertEquals(
        gate.connection(carrier.id)!.outboundContexts.slice(outbound).filter((
          out,
        ) =>
          out.subject.includes("JOBS_KEYS") ||
          out.subject.startsWith("$JS.API.STREAM.MSG.GET.JOBS") ||
          out.subject.startsWith("jobs.v1.")
        ).length,
        0,
        `rejected fresh query and submit must perform no Jobs broker IO (registry maintenance and retained ACKs remain permitted): ${
          JSON.stringify(
            gate.connection(carrier.id)!.outboundContexts.slice(outbound),
          )
        }`,
      );
      const heartbeatCount = heartbeats().length;
      const suspendedAt = Date.now();
      await send("JOB_RELEASE initial");
      await marker("JOB_DONE 1 initial");
      // The real production heartbeat interval is 30s. Keep coverage unavailable
      // through a full due interval, using ordinary public queries to establish
      // that this same live process remains responsive, not a paused runtime.
      await runtime.waitFor(async () => {
        if (retryFailure) throw retryFailure;
        const after = lines.length;
        await send("AUTH_STATUS");
        assertEquals(await marker("AUTH_USABLE", after), "AUTH_USABLE false");
        assertEquals(
          heartbeats().length,
          heartbeatCount,
          "unsigned fresh heartbeat must not publish while own coverage is suspended",
        );
        return Date.now() - suspendedAt >= 35_000 && held ? true : undefined;
      }, { timeoutMs: 40_000, intervalMs: 250 });
      rearming = false;
      await hold!.release();
      hold = undefined;
      await runtime.waitFor(async () => {
        const after = lines.length;
        await send("AUTH_STATUS");
        return (await marker("AUTH_USABLE", after)) === "AUTH_USABLE true"
          ? true
          : undefined;
      });
      const after = lines.length;
      await send("JOB_STATUS initial");
      assert(
        (await marker("JOB_STATUS", after)).startsWith(
          "JOB_STATUS initial Completed 1",
        ),
      );
      await send("JOB_SUBMIT resumed");
      await marker("JOB_SUBMITTED resumed", after);
      await marker("JOB_STARTED 2 resumed", after);
      const resumed = admittedConnections(
        await readRuntimeBrokerInventory(runtime),
        new Set([digest]),
      );
      assertEquals(resumed.map(brokerConnectionKey), [physical]);
      await runtime.waitFor(
        () => heartbeats().length > heartbeatCount ? true : undefined,
        { timeoutMs: 35_000, intervalMs: 50 },
      );
      await send("EXIT");
      await marker("OPERATION_PROVIDER_DONE");
      await runtime.waitFor(() => exited ? true : undefined);
      assert((await status).success);
    } finally {
      creates?.unsubscribe();
      await hold?.release().catch(() => undefined);
      await Promise.all(releases);
      await stdin.close().catch(() => undefined);
      if (!exited) {
        try {
          Deno.kill(-child.pid, "SIGKILL");
        } catch { /* already exited */ }
      }
      await Promise.allSettled([status, drain]);
      await nats.close();
    }
  }, { interruptibleNativeProxy: true });
});

Deno.test("Rust fresh RPC waits for a parked stage's actual intake publication", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.TransportGrowthSubject.participant;
    const targetContract = participants.TransportGrowthTarget.participant;
    await runtime.contracts.apply({
      deployment: "fresh-stage-target",
      contract: targetContract,
    });
    const targetInstance = await runtime.services.createInstance({
      deployment: "fresh-stage-target",
      name: "fresh-stage-target",
      contract: targetContract,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: targetContract,
      name: "fresh-stage-target",
      seed: targetInstance.seed,
    }).orThrow();
    let advances = 0;
    await target.handleAdvance(() => {
      advances++;
      return Result.ok({});
    });
    await target.handleExtend(() => Result.ok({}));
    await target.handleProgress(async ({ emit, signal }) => {
      while (!signal.aborted) {
        await emit({ value: "ready" });
        await new Promise((resolve) => setTimeout(resolve, 150));
      }
    });
    const targetExit = target.wait().catch(() => undefined);
    await runtime.contracts.apply({
      deployment: "fresh-stage-subject",
      contract,
    });
    const instance = await runtime.services.createInstance({
      deployment: "fresh-stage-subject",
      name: "fresh-stage-subject",
      contract,
    });
    const child = new Deno.Command("setsid", {
      args: rustFixtureArgv("transport_growth_subject"),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: instance.seed,
      },
      stdin: "piped",
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const stdin = child.stdin.getWriter();
    const lines: string[] = [];
    let exited = false;
    const status = child.status.then((value) => {
      exited = true;
      return value;
    });
    const drain = (async () => {
      let pending = "";
      for await (
        const chunk of child.stdout.pipeThrough(new TextDecoderStream())
      ) {
        const complete = (pending + chunk).split("\n");
        pending = complete.pop() ?? "";
        lines.push(...complete.map((line) => line.trim()));
      }
    })();
    const send = async (command: string) => {
      await stdin.write(new TextEncoder().encode(`${command}\n`));
    };
    const marker = async (prefix: string, count = 1) =>
      await runtime.waitFor(() => {
        const failure = lines.find((line) =>
          line.startsWith("TRANSPORT_GROWTH_ERROR ") ||
          line.startsWith("TRANSPORT_GROWTH_ADVANCE_ERROR ")
        );
        if (failure) throw new Error(failure);
        if (lines.filter((line) => line.startsWith(prefix)).length >= count) {
          return true;
        }
        if (exited) throw new Error(lines.join("\n"));
      }, { timeoutMs: 30_000, intervalMs: 20 });
    const gate = runtime.nativeTransportGate();
    let warm: ReturnType<typeof gate.armResponseHold> | undefined;
    let readiness: ReturnType<typeof gate.armResponseHold> | undefined;
    try {
      await marker("TRANSPORT_GROWTH_ADVANCE_OK");
      assertEquals(advances, 1);
      const bindings = await runtime.callAdminRpc("authGrantsList", {
        participantId: contract.identity,
      }) as {
        items: {
          revision: bigint;
          installedRevision: bigint;
          expiresAt: bigint | null;
          ownerId: string;
          ownerKind: string;
          participantId: string;
          platformPrivileges: string[];
          grants: {
            format: string;
            permissions: { action: string; target: Uint8Array }[];
          };
        }[];
      };
      const binding = bindings.items.find((item) =>
        item.participantId === contract.identity
      )!;
      assert(binding);
      const extend = await encodePermissionTargetWasm({
        kind: "apiSurface",
        api: "runtime-trellis.transport_growth@v1",
        surface: "rpc",
        name: "Extend",
      });
      const kept = binding.grants.permissions.filter((atom) =>
        atom.action !== "call" || atom.target.length !== extend.length ||
        atom.target.some((byte, index) => byte !== extend[index])
      );
      assertEquals(kept.length, binding.grants.permissions.length - 1);
      warm = gate.armResponseHold("$JS.API.CONSUMER.INFO.");
      await runtime.callAdminRpc("authGrantsSet", {
        expiresAt: binding.expiresAt,
        installedRevision: binding.installedRevision,
        ownerId: binding.ownerId,
        ownerKind: binding.ownerKind,
        participantId: binding.participantId,
        platformPrivileges: binding.platformPrivileges,
        expectedRevision: binding.revision,
        grants: { format: binding.grants.format, permissions: kept },
        idempotencyKey: crypto.randomUUID(),
      });
      let warmed: Awaited<typeof warm.held> | undefined;
      warm.held.then((value) => {
        warmed = value;
      });
      await runtime.waitFor(() => warmed, {
        timeoutMs: 60_000,
        intervalMs: 20,
      });
      assert(warmed);
      const releasing = warm.release();
      readiness = gate.armResponseHold(
        "$SYS.REQ.USER.INFO",
        warmed.connectionId,
      );
      await releasing;
      let ready = false;
      readiness.held.then(() => {
        ready = true;
      });
      await runtime.waitFor(() => ready ? true : undefined);
      const stage = gate.connection(warmed.connectionId)!;
      assert(
        !stage.closed &&
          stage.subs.some((sub) => sub.subject.endsWith(".Watch")),
      );
      const before = stage.outboundContexts.length;
      await send("START_ADVANCE");
      await marker("TRANSPORT_GROWTH_ADVANCE_STARTED");
      await send("PING");
      await marker("TRANSPORT_GROWTH_PONG");
      // Independent real broker round trips while the exact readiness remains
      // withheld. A queued app call must not use the private parked attachment.
      await readRuntimeBrokerInventory(runtime);
      await readRuntimeBrokerInventory(runtime);
      assertEquals(
        advances,
        1,
        "the target must not receive the fresh call before stage publication",
      );
      assertEquals(
        gate.connection(warmed.connectionId)!.outboundContexts.slice(before)
          .filter((out) => out.subject.endsWith(".Advance")).length,
        0,
      );
      await readiness.release();
      await marker("TRANSPORT_GROWTH_ADVANCE_OK", 2);
      assertEquals(advances, 2);
      assertEquals(
        gate.connection(warmed.connectionId)!.outboundContexts.slice(before)
          .filter((out) => out.subject.endsWith(".Advance")).length,
        1,
      );
      await send("EXIT");
      await marker("TRANSPORT_GROWTH_DONE");
      await runtime.waitFor(() => exited ? true : undefined);
      assert((await status).success);
    } finally {
      await readiness?.release().catch(() => undefined);
      await warm?.release().catch(() => undefined);
      await stdin.close().catch(() => undefined);
      if (!exited) {
        try {
          Deno.kill(-child.pid, "SIGKILL");
        } catch { /* already exited */ }
      }
      await Promise.allSettled([status, drain]);
      await target.connection.close();
      await targetExit;
    }
  }, {
    interruptibleNativeProxy: true,
    authorization: {
      contextLifetimeSeconds: 76,
      refreshLeadSeconds: 15,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 46,
    },
  });
});
