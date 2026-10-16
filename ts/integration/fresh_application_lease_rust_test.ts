/** Fresh application work must not use a live carrier with lost own coverage. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Readable, Writable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { AckPolicy, jetstreamManager } from "@nats-io/jetstream";
import { deadline } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { ulid } from "ulid";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { readRuntimeBrokerInventory } from "./_support/broker_inventory.ts";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("Rust fresh Jobs leases reject suspended own coverage and resume on the same carrier", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
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
          await readFile(
            `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
          ),
        ),
      });
      const js = await jetstreamManager(nats);
      const gate = runtime.nativeTransportGate();
      const argv = rustFixtureArgv("transport_growth_operation");
      const child = spawnTracked(argv[0], argv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: instance.seed,
        },
      });
      child.stderr.pipe(process.stderr);
      const stdin = Writable.toWeb(child.stdin).getWriter();
      let exited = false;
      const status = child.exited.then((value) => {
        exited = true;
        return value;
      });
      const lines: string[] = [];
      const drain = (async () => {
        let pending = "";
        for await (const chunk of Readable.toWeb(child.stdout).pipeThrough(
          new TextDecoderStream(),
        )) {
          const complete = (pending + chunk).split("\n");
          pending = complete.pop() ?? "";
          lines.push(...complete.map((line) => line.trim()));
        }
      })();
      const send = async (command: string) => {
        await stdin.write(new TextEncoder().encode(`${command}\n`));
      };
      const marker = async (prefix: string, after = 0) =>
        await runtime
          .waitFor(
            () => {
              const found = lines
                .slice(after)
                .find((line) => line.startsWith(prefix));
              if (found) return found;
              if (exited)
                throw new Error(`fixture exited: ${lines.join("\n")}`);
            },
            { timeoutMs: 15_000, intervalMs: 20 },
          )
          .catch((cause) => {
            throw new Error(
              `missing ${prefix}; fixture output:\n${lines.join("\n")}`,
              {
                cause,
              },
            );
          });
      const stream = "KV_TRELLIS_AUTH_REVOCATIONS";
      const settings = (await js.streams.info(stream)).config;
      let blocker: string | undefined;
      try {
        await marker("OPERATION_PROVIDER_READY");
        await marker("JOB_STARTED 1 initial");
        const prefix = `$JS.API.CONSUMER.MSG.NEXT.${stream}.`;
        const carriers = gate
          .connections()
          .filter((connection) =>
            connection.outboundContexts.some(
              (message) =>
                message.subject.startsWith("trellis.jobs.") &&
                message.subject.endsWith(".created"),
            ),
          );
        assert.equal(
          carriers.length,
          1,
          JSON.stringify(
            gate.connections().map((connection) => ({
              id: connection.id,
              closed: connection.closed,
              subjects: connection.outboundContexts.map(
                (message) => message.subject,
              ),
            })),
          ),
        );
        const carrier = carriers[0];
        const pull = carrier.outboundContexts.findLast((message) =>
          message.subject.startsWith(prefix),
        );
        assert(pull, "identify the shared mirror on the actual Jobs carrier");
        const consumer = await js.consumers.info(
          stream,
          pull.subject.slice(prefix.length),
        );
        const heartbeats = () =>
          gate
            .connection(carrier.id)!
            .outboundContexts.filter(
              (out) =>
                out.subject ===
                apis.health.API.actions["event:Heartbeat"].subject,
            );
        assert(
          heartbeats().length > 0,
          "the ordinary service heartbeat really publishes before coverage loss",
        );
        // Exhaust real ephemeral-consumer capacity without changing retained data.
        const constrained = {
          ...settings,
          max_consumers: (await js.streams.info(stream)).state.consumer_count,
        };
        await js.streams.update(stream, constrained);
        assert(await js.consumers.delete(stream, consumer.name));
        blocker = (
          await js.consumers.add(stream, {
            durable_name: ulid(),
            ack_policy: AckPolicy.None,
          })
        ).name;
        await runtime.waitFor(
          async () => {
            const before = lines.length;
            await send("AUTH_STATUS");
            return (await marker("AUTH_USABLE", before)) === "AUTH_USABLE false"
              ? true
              : undefined;
          },
          { timeoutMs: 15_000, intervalMs: 50 },
        );
        assert(!gate.connection(carrier.id)!.closed);
        const before = lines.length;
        const outbound = gate.connection(carrier.id)!.outboundContexts.length;
        await send("JOB_STATUS initial");
        const query = await marker("JOB_STATUS", before);
        assert(query.startsWith("JOB_STATUS_ERROR"), query);
        await send("JOB_SUBMIT suspended");
        const submit = await marker("JOB_SUBMIT_ERROR", before);
        assert(submit.startsWith("JOB_SUBMIT_ERROR"), submit);
        assert.deepEqual(
          gate
            .connection(carrier.id)!
            .outboundContexts.slice(outbound)
            .filter(
              (out) =>
                out.subject.startsWith("rpc.v1.jobs.") ||
                out.subject.startsWith("trellis.work.") ||
                (out.subject.startsWith("trellis.jobs.") &&
                  !out.subject.startsWith("trellis.jobs.workers.")),
            ).length,
          0,
          `rejected fresh query and submit must perform no Jobs broker IO (registry maintenance and retained ACKs remain permitted): ${JSON.stringify(
            gate.connection(carrier.id)!.outboundContexts.slice(outbound),
          )}`,
        );
        const heartbeatCount = heartbeats().length;
        const suspendedAt = Date.now();
        await send("JOB_RELEASE initial");
        await marker("JOB_DONE 1 initial");
        // The real production heartbeat interval is 30s. Keep coverage unavailable
        // through a full due interval, using ordinary public queries to establish
        // that this same live process remains responsive, not a paused runtime.
        await runtime.waitFor(
          async () => {
            const after = lines.length;
            await send("AUTH_STATUS");
            assert.deepEqual(
              await marker("AUTH_USABLE", after),
              "AUTH_USABLE false",
            );
            assert.deepEqual(
              heartbeats().length,
              heartbeatCount,
              "unsigned fresh heartbeat must not publish while own coverage is suspended",
            );
            return Date.now() - suspendedAt >= 35_000 ? true : undefined;
          },
          { timeoutMs: 40_000, intervalMs: 250 },
        );
        await js.consumers.delete(stream, blocker);
        blocker = undefined;
        await js.streams.update(stream, settings);
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
        assert.equal(gate.connection(carrier.id)?.closed, false);
        await runtime.waitFor(
          () => (heartbeats().length > heartbeatCount ? true : undefined),
          { timeoutMs: 35_000, intervalMs: 50 },
        );
        await send("EXIT");
        await marker("OPERATION_PROVIDER_DONE");
        await runtime.waitFor(() => (exited ? true : undefined));
        assert((await status).code === 0);
      } finally {
        if (blocker) await js.consumers.delete(stream, blocker);
        await js.streams.update(stream, settings);
        await stdin.close().catch(() => undefined);
        if (!exited && child.pid !== undefined) {
          try {
            child.kill("SIGKILL");
          } catch {
            /* already exited */
          }
        }
        await Promise.allSettled([status, drain]);
        await nats.close();
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("Rust fresh RPC is fenced during loss and resumes only after broker-ready intake publication", async () => {
  await withTrellisRuntime(
    async (runtime) => {
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
        trellisUrl: runtime.oauthIssuer,
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
      const argv = rustFixtureArgv("transport_growth_subject");
      const child = spawnTracked(argv[0], argv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: instance.seed,
        },
      });
      child.stderr.pipe(process.stderr);
      const stdin = Writable.toWeb(child.stdin).getWriter();
      const lines: string[] = [];
      let exited = false;
      const status = child.exited.then((value) => {
        exited = true;
        return value;
      });
      const drain = (async () => {
        let pending = "";
        for await (const chunk of Readable.toWeb(child.stdout).pipeThrough(
          new TextDecoderStream(),
        )) {
          const complete = (pending + chunk).split("\n");
          pending = complete.pop() ?? "";
          lines.push(...complete.map((line) => line.trim()));
        }
      })();
      const send = async (command: string) => {
        await stdin.write(new TextEncoder().encode(`${command}\n`));
      };
      const marker = async (prefix: string, count = 1) =>
        await runtime.waitFor(
          () => {
            const failure = lines.find((line) =>
              line.startsWith("TRANSPORT_GROWTH_ERROR "),
            );
            if (failure) throw new Error(failure);
            if (
              lines.filter((line) => line.startsWith(prefix)).length >= count
            ) {
              return true;
            }
            if (exited) throw new Error(lines.join("\n"));
          },
          { timeoutMs: 30_000, intervalMs: 20 },
        );
      const gate = runtime.nativeTransportGate();
      let readiness: ReturnType<typeof gate.armResponseHold> | undefined;
      try {
        await marker("TRANSPORT_GROWTH_ADVANCE_OK");
        assert.deepEqual(advances, 1);
        const original = gate
          .connections()
          .find((connection) =>
            connection.outboundContexts.some(
              (message) =>
                message.subject ===
                apis.transport_growth.API.actions["rpc:Advance"].subject,
            ),
          );
        assert(original);
        readiness = gate.armResponseHold(
          "$SYS.REQ.USER.INFO",
          undefined,
          (_body, connection) =>
            connection.id !== original.id &&
            connection.subs.some((sub) => sub.subject.endsWith(".Watch")),
        );
        runtime.interruptNativeTransport();
        runtime.restoreNativeTransport();
        const stageId = (await deadline(readiness.held, 60_000)).connectionId;
        assert.notEqual(stageId, original.id);
        const stage = gate.connection(stageId);
        assert(stage);
        assert(
          !stage.closed &&
            stage.subs.some((sub) => sub.subject.endsWith(".Watch")),
        );
        const before = stage.outboundContexts.length;
        await send("START_ADVANCE");
        await marker("TRANSPORT_GROWTH_ADVANCE_STARTED");
        await marker("TRANSPORT_GROWTH_ADVANCE_ERROR");
        await send("PING");
        await marker("TRANSPORT_GROWTH_PONG");
        // Independent real broker round trips while the exact readiness remains
        // withheld. A fresh app call must not use the private parked attachment.
        await readRuntimeBrokerInventory(runtime);
        await readRuntimeBrokerInventory(runtime);
        assert.deepEqual(
          advances,
          1,
          "the target must not receive the fresh call before stage publication",
        );
        assert.deepEqual(
          gate
            .connection(stageId)!
            .outboundContexts.slice(before)
            .filter((out) => out.subject.endsWith(".Advance")).length,
          0,
        );
        await readiness.release();
        await runtime.waitFor(async () => {
          const before = lines.length;
          await send("AUTH_STATUS");
          await runtime.waitFor(() =>
            lines.slice(before).find((line) => line.startsWith("AUTH_USABLE ")),
          );
          return lines.slice(before).includes("AUTH_USABLE true");
        });
        await send("ADVANCE");
        await marker("TRANSPORT_GROWTH_ADVANCE_OK", 2);
        assert.deepEqual(advances, 2);
        assert.deepEqual(
          gate
            .connection(stageId)!
            .outboundContexts.slice(before)
            .filter((out) => out.subject.endsWith(".Advance")).length,
          1,
        );
        await send("EXIT");
        await marker("TRANSPORT_GROWTH_DONE");
        await runtime.waitFor(() => (exited ? true : undefined));
        assert((await status).code === 0);
      } finally {
        await readiness?.release().catch(() => undefined);
        await stdin.close().catch(() => undefined);
        if (!exited && child.pid !== undefined) {
          try {
            child.kill("SIGKILL");
          } catch {
            /* already exited */
          }
        }
        await Promise.allSettled([status, drain]);
        await target.stop();
        await targetExit;
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 106,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    },
  );
});
