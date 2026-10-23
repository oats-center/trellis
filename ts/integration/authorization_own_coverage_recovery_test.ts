/** Coverage repair must not need application traffic or a replacement socket. */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { ulid } from "ulid";
import { deadline } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { jetstreamManager, type ConsumerInfo } from "@nats-io/jetstream";
import { TrellisService } from "@oatscenter/trellis/service";
import { Result, TransportError } from "@oatscenter/trellis";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

for (const scenario of ["outage", "expiry", "cancelled-renewal"] as const) {
  const spansExpiry = scenario !== "outage";
  const cancelledRenewal = scenario === "cancelled-renewal";
  test(`own coverage restores on the original socket while fresh KV waits${spansExpiry ? " across authority expiry" : ""}${cancelledRenewal ? " with a cancelled renewal outstanding" : ""}`, async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const contract = participants.Provider.participant;
        const identity = await runtime.registerService({
          name: "own-watch-provider",
          contract,
        });
        const service = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: contract,
          seed: identity.seed,
          // Keep the cancelled broker request outstanding across the supported
          // short authority lifetime; this creates the race, not a wider RPC deadline.
          ...(cancelledRenewal ? { timeout: 60_000 } : {}),
        }).orThrow();
        let echoes = 0;
        await service.handleEcho(({ input }) => {
          echoes++;
          return Result.ok(input);
        });
        const serviceExit = service.wait().catch((error: unknown) => error);
        const broker = await runtime.connectNats();
        const manager = await jetstreamManager(broker);
        const system = await connect({
          servers: runtime.natsUrl,
          authenticator: credsAuthenticator(
            await readFile(
              join(runtime.workdir, "config/trellis/nats/creds/system.creds"),
            ),
          ),
        });
        const gate = runtime.nativeTransportGate();
        let hold: ReturnType<typeof gate.armResponseHold> | undefined;
        let write: Promise<unknown> | undefined;
        try {
          const records = service.kv.records;
          assert(records);
          await records.put("own-recovery", { value: "baseline" }).orThrow();
          const proxy = gate
            .connections()
            .find((connection) =>
              connection.outboundContexts.some(
                (publication) =>
                  publication.subject.startsWith("$KV.") &&
                  publication.subject.endsWith(".own-recovery"),
              ),
            );
          assert(proxy);
          const publication = proxy.outboundContexts.find(
            (item) =>
              item.subject.startsWith("$KV.") &&
              item.subject.endsWith(".own-recovery"),
          );
          assert(publication);
          const kvSubject = publication.subject;
          const stream = await manager.streams.find(kvSubject);
          const baseline = await manager.streams.getMessage(stream, {
            last_by_subj: kvSubject,
          });
          assert(baseline);
          const sessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            { state: "active" },
          );
          const session = sessions.items.find(
            (item) => item.binding.instanceId === identity.instanceId,
          );
          assert(session);
          assert(session.maximumContextExpiresAt);
          const originalExpiresAt = Date.parse(session.maximumContextExpiresAt);
          const attachments = await runtime.callAdminRpc(
            "authConnectionsList",
            {
              authorizationSessionId: session.authorizationSessionId,
              state: "admitted",
            },
          );
          assert.equal(attachments.items.length, 1);
          const attachmentIds = new Set(
            attachments.items.map((item) => item.attachmentId),
          );
          const [original] = admittedConnections(
            await completeBrokerInventory(system),
            attachmentIds,
          );
          assert(original);
          const originalKey = brokerConnectionKey(original);
          const mirrorStream = "KV_TRELLIS_AUTH_REVOCATIONS";
          const watches: ConsumerInfo[] = [];
          for await (const info of manager.consumers.list(mirrorStream)) {
            if (
              info.push_bound &&
              proxy.subs.some(
                (subscription) =>
                  subscription.subject === info.config.deliver_subject,
              )
            )
              watches.push(info);
          }
          assert.equal(watches.length, 1);
          const old = watches[0];
          const failedConsumers = new Set([old.name]);
          let replacement: ConsumerInfo | undefined;
          const infoPrefix = `$JS.API.CONSUMER.INFO.${mirrorStream}.`;
          let blockMirror = true;
          const mirrorHeld = Promise.withResolvers<void>();
          const mirrorForwarded = Promise.withResolvers<void>();
          if (cancelledRenewal) {
            // Hold the real scheduled renewal's broker-info reply, independently
            // of mirror INFO replies on the same physical carrier. Later retry
            // replies can restore coverage while the cancelled renewal stays held.
            hold = gate.armResponseHold(
              "$",
              proxy.id,
              (body) => {
                if (body.length === 0) return false;
                const message: ConsumerInfo & { data?: { user?: unknown } } =
                  JSON.parse(new TextDecoder().decode(body));
                if (message === null || typeof message !== "object")
                  return false;
                if (typeof message.data?.user === "string") return true;
                if (
                  message.stream_name !== mirrorStream ||
                  typeof message.name !== "string" ||
                  failedConsumers.has(message.name)
                )
                  return false;
                replacement = message;
                if (blockMirror) mirrorHeld.resolve();
                else mirrorForwarded.resolve();
                return blockMirror;
              },
              { continuous: true },
            );
            const heldRenewal = await deadline(hold.held, 45_000);
            assert.equal(heldRenewal.requestSubject, "$SYS.REQ.USER.INFO");
            assert(await manager.consumers.delete(mirrorStream, old.name));
            await deadline(mirrorHeld.promise, 30_000);
          } else {
            hold = gate.armResponseHold(infoPrefix, proxy.id, (body) => {
              const info: ConsumerInfo = JSON.parse(
                new TextDecoder().decode(body),
              );
              if (
                typeof info.name === "string" &&
                !failedConsumers.has(info.name) &&
                service.connection.availability().resources.extras === false
              ) {
                replacement = info;
                return true;
              }
              return false;
            });
            // This consumer is the ephemeral session mirror, not committed service
            // storage. Withhold its real replacement INFO response until IO is fenced.
            assert(await manager.consumers.delete(mirrorStream, old.name));
            await deadline(hold.held, 30_000);
            assert(replacement);
            // A failed recovery snapshot must stay fenced and be retried without
            // fresh application acquisition or another physical connection.
            assert(
              await manager.consumers.delete(mirrorStream, replacement.name),
            );
            failedConsumers.add(replacement.name);
            await hold.release();
            hold = gate.armResponseHold(
              infoPrefix,
              proxy.id,
              (body) => {
                const info: ConsumerInfo = JSON.parse(
                  new TextDecoder().decode(body),
                );
                if (
                  typeof info.name !== "string" ||
                  failedConsumers.has(info.name)
                )
                  return false;
                replacement = info;
                return (
                  service.connection.availability().resources.extras === false
                );
              },
              { continuous: true },
            );
            await deadline(hold.held, 30_000);
          }
          assert.equal(
            service.connection.availability().resources.extras,
            false,
          );
          if (spansExpiry) {
            // Cross the real signed lifetime and verifier skew, without losing
            // the admitted physical connection or changing request deadlines.
            // Withhold every retry response, not just the first request. The
            // real supervisor keeps retrying with its unchanged timeouts.
            await new Promise((resolve) =>
              setTimeout(
                resolve,
                Math.max(0, originalExpiresAt + 6_000 - Date.now()),
              ),
            );
            if (cancelledRenewal) {
              blockMirror = false;
              await deadline(mirrorForwarded.promise, 10_000);
              await runtime.waitFor(async () => {
                assert(replacement);
                const replay = await manager.consumers.info(
                  mirrorStream,
                  replacement.name,
                );
                return replay.num_pending === 0;
              });
            }
            assert.equal(
              service.connection.availability().resources.extras,
              false,
            );
            assert.equal(gate.connection(proxy.id)?.closed, false);
            const beforeCalls = gate
              .connection(proxy.id)
              ?.outboundContexts.filter((item) =>
                item.subject.endsWith(".Echo"),
              ).length;
            const expiredCall = await service.echo(
              { value: "expired authority" },
              { timeout: 250 },
            );
            assert(expiredCall.isErr());
            assert.equal(echoes, 0);
            assert.equal(
              gate
                .connection(proxy.id)
                ?.outboundContexts.filter((item) =>
                  item.subject.endsWith(".Echo"),
                ).length,
              beforeCalls,
              "expired authority cannot publish an application RPC",
            );
          }
          const countBefore = gate
            .connection(proxy.id)
            ?.outboundContexts.filter(
              (item) => item.subject === kvSubject,
            ).length;
          if (!spansExpiry) {
            // Observe a complete acquisition budget while every replacement
            // reply stays held, rather than racing a single broker read against
            // the asynchronous lease checks before the write reaches the wire.
            const unavailable = await records.put("own-recovery", {
              value: "must not publish without coverage",
            });
            assert(unavailable.isErr());
            assert(unavailable.error instanceof TransportError);
            assert.equal(unavailable.error.code, "trellis.transport.timeout");
            assert.equal(
              gate
                .connection(proxy.id)
                ?.outboundContexts.filter((item) => item.subject === kvSubject)
                .length,
              countBefore,
            );
            const retained = await manager.streams.getMessage(stream, {
              last_by_subj: kvSubject,
            });
            assert(retained);
            assert.equal(retained.seq, baseline.seq);
            assert.deepEqual(retained.data, baseline.data);
          }
          let outcome: "success" | "error" | undefined;
          write = records
            .put("own-recovery", { value: "recovered" })
            .orThrow()
            .then(
              (result) => {
                outcome = "success";
                return result;
              },
              (error: unknown) => {
                outcome = "error";
                throw error;
              },
            );
          void write.catch(() => undefined);
          const fenced = await manager.streams.getMessage(stream, {
            last_by_subj: kvSubject,
          });
          assert(fenced);
          assert.equal(outcome, undefined);
          assert.equal(fenced.seq, baseline.seq);
          assert.deepEqual(fenced.data, baseline.data);
          assert.equal(
            gate
              .connection(proxy.id)
              ?.outboundContexts.filter((item) => item.subject === kvSubject)
              .length,
            countBefore,
          );
          await hold.release();
          hold = undefined;
          await deadline(write, spansExpiry ? 30_000 : 5_000);
          assert.equal(outcome, "success");
          assert.deepEqual(await records.get("own-recovery").orThrow(), {
            value: "recovered",
          });
          const persisted = await manager.streams.getMessage(stream, {
            last_by_subj: kvSubject,
          });
          assert(persisted);
          assert.equal(persisted.seq, baseline.seq + 1);
          assert.equal(
            gate
              .connection(proxy.id)
              ?.outboundContexts.filter((item) => item.subject === kvSubject)
              .length,
            (countBefore ?? 0) + 1,
          );
          const restored = admittedConnections(
            await completeBrokerInventory(system, {
              requiredServerIds: [original.server],
              onlyRequiredServers: true,
            }),
            attachmentIds,
          );
          assert.deepEqual(restored.map(brokerConnectionKey), [originalKey]);
          assert.equal(gate.connection(proxy.id)?.closed, false);
          if (spansExpiry) {
            // Coverage restoration wakes maintenance first; application
            // usability follows verification and promotion of fresh authority.
            await runtime.waitFor(
              () => service.connection.availability().resources.extras === true,
            );
          }
          assert.equal(
            service.connection.availability().resources.extras,
            true,
          );
          if (spansExpiry) {
            const renewed = await runtime.callAdminRpc(
              "authAuthorizationSessionsList",
              {
                state: "active",
              },
            );
            const current = renewed.items.find(
              (item) =>
                item.authorizationSessionId === session.authorizationSessionId,
            );
            assert(current?.maximumContextExpiresAt);
            assert(
              Date.parse(current.maximumContextExpiresAt) > originalExpiresAt,
            );
            assert(Date.parse(current.maximumContextExpiresAt) > Date.now());
            assert.deepEqual(
              await service.echo({ value: "fresh authority" }).orThrow(),
              { value: "fresh authority" },
            );
            assert.equal(echoes, 1);
          }
          if (cancelledRenewal) {
            const publications =
              gate.connection(proxy.id)?.outboundContexts ?? [];
            const digest = publications.findLast(
              (item) => item.context,
            )?.context;
            assert(digest);
            // A subsequent scheduled renewal must promote authority for the
            // production health publisher without application requests waking it.
            // One renewal interval plus one 30s heartbeat bounds observation.
            await runtime.waitFor(
              () =>
                gate
                  .connection(proxy.id)
                  ?.outboundContexts.slice(publications.length)
                  .some((item) => item.context && item.context !== digest),
              { timeoutMs: 70_000 },
            );
            assert.deepEqual(
              await service.echo({ value: "scheduled renewal" }).orThrow(),
              { value: "scheduled renewal" },
            );
            assert.equal(echoes, 2);
          }
          // Close the logical connection while private recovery awaits a real
          // broker response. Graceful service drain intentionally precedes close.
          const activeWatches: ConsumerInfo[] = [];
          for await (const info of manager.consumers.list(mirrorStream)) {
            if (
              info.push_bound &&
              gate
                .connection(proxy.id)
                ?.subs.some(
                  (subscription) =>
                    subscription.subject === info.config.deliver_subject,
                )
            )
              activeWatches.push(info);
          }
          assert.equal(activeWatches.length, 1);
          const closingMirror = Promise.withResolvers<void>();
          hold = gate.armResponseHold(
            cancelledRenewal ? "$" : infoPrefix,
            proxy.id,
            (body) => {
              if (!body.length) return false;
              const info: ConsumerInfo & { data?: { user?: unknown } } =
                JSON.parse(new TextDecoder().decode(body));
              if (cancelledRenewal && typeof info.data?.user === "string")
                return true;
              const matches =
                info.stream_name === mirrorStream &&
                typeof info.name === "string" &&
                service.connection.availability().resources.extras === false;
              if (matches) closingMirror.resolve();
              return matches;
            },
            { continuous: cancelledRenewal },
          );
          if (cancelledRenewal) await deadline(hold.held, 45_000);
          assert(
            await manager.consumers.delete(mirrorStream, activeWatches[0].name),
          );
          await deadline(hold.held, 30_000);
          if (cancelledRenewal) await deadline(closingMirror.promise, 30_000);
          const stoppedWrite = records
            .put("own-recovery", { value: "must-not-write" })
            .orThrow();
          const rejectedWrite = assert.rejects(stoppedWrite);
          await deadline(service.connection.close(), 5_000);
          await deadline(serviceExit, 5_000);
          await deadline(rejectedWrite, 5_000);
          await deadline(gate.waitForClose(proxy.id), 5_000);
          await hold.release();
          hold = undefined;
          const stopped = await manager.streams.getMessage(stream, {
            last_by_subj: kvSubject,
          });
          assert(stopped);
          assert.equal(stopped.seq, persisted.seq);
          assert.deepEqual(stopped.data, persisted.data);
        } finally {
          await hold?.release();
          await write?.catch(() => undefined);
          await service.stop();
          await serviceExit;
          await broker.close();
          await system.close();
        }
      },
      {
        interruptibleNativeProxy: true,
        ...(spansExpiry
          ? {
              authorization: {
                contextLifetimeSeconds: 42,
                allowedClockSkewSeconds: 2,
              },
            }
          : {}),
      },
    );
  });
}

test("a granted optional RPC waits for coverage rather than being classified as denied", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract =
        participants.TransportGrowthOperationProvider.participant;
      const identity = await runtime.registerService({
        name: "optional-coverage",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      let calls = 0;
      await service.handleExtend(() => {
        calls++;
        return Result.ok({});
      });
      const broker = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            join(
              runtime.workdir,
              "config/trellis/nats/creds/trellis-auth.creds",
            ),
          ),
        ),
      });
      const manager = await jetstreamManager(broker);
      const gate = runtime.nativeTransportGate();
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      let pending: Promise<unknown> | undefined;
      try {
        await runtime.waitFor(
          () =>
            service.connection.availability().capabilities[
              "runtime-trellis.transport_growth.v1:growth.extend"
            ] === true,
        );
        assert.deepEqual(await service.extend({}).orThrow(), {});
        const route = "rpc.v1.transport_growth.Extend";
        const sockets = gate
          .connections()
          .filter((connection) =>
            connection.outboundContexts.some(
              (message) => message.subject === route,
            ),
          );
        assert.equal(sockets.length, 1);
        const socket = sockets[0];
        const stream = "KV_TRELLIS_AUTH_REVOCATIONS";
        const mirrors: ConsumerInfo[] = [];
        for await (const info of manager.consumers.list(stream)) {
          if (
            info.push_bound &&
            socket.subs.some(
              (subscription) =>
                subscription.subject === info.config.deliver_subject,
            )
          )
            mirrors.push(info);
        }
        assert.equal(mirrors.length, 1);
        hold = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${stream}.`,
          socket.id,
          () =>
            service.connection.availability().capabilities[
              "runtime-trellis.transport_growth.v1:growth.extend"
            ] === false,
        );
        assert(await manager.consumers.delete(stream, mirrors[0].name));
        await deadline(hold.held, 30_000);
        const cancelled = new AbortController();
        const cancelledRequest = service
          .extend({}, { signal: cancelled.signal })
          .orThrow();
        cancelled.abort();
        await assert.rejects(cancelledRequest, {
          code: "trellis.request.cancelled",
        });
        await assert.rejects(service.extend({}, { timeout: 100 }).orThrow(), {
          code: "trellis.request.timeout",
        });
        assert.equal(calls, 1, "fenced requests must not reach the handler");
        pending = service.extend({}, { timeout: 10_000 }).orThrow();
        void pending.catch(() => undefined);
        await hold.release();
        hold = undefined;
        assert.deepEqual(await deadline(pending, 12_000), {});
        assert.equal(calls, 2, "coverage recovery must not retransmit the RPC");
        assert.equal(gate.connection(socket.id)?.closed, false);
        const capabilityId = "runtime-trellis.transport_growth.v1:growth.extend";
        const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
          capabilityId,
        });
        assert.equal(grants.items.length, 1);
        const grant = grants.items[0];
        await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
          grantId: grant.grantId,
          expectedRevision: grant.revision,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(
          () =>
            service.connection.allowsAction(
              "runtime-trellis.transport_growth@v1",
              "rpc",
              "Extend",
              "call",
            ) === false,
          { timeoutMs: 90_000 },
        );
        await assert.rejects(service.extend({}).orThrow(), {
          code: "trellis.request.unavailable",
        });
        assert.equal(
          calls,
          2,
          "current authority denial must still fence publication",
        );
      } finally {
        await hold?.release();
        await pending?.catch(() => undefined);
        await service.stop();
        await serviceExit;
        await broker.close();
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 60,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
