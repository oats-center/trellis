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
import { Result } from "@oatscenter/trellis";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  admittedConnections,
  brokerConnectionKey,
  completeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("own coverage restores on the original socket while fresh KV waits", async () => {
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
      }).orThrow();
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
        const attachments = await runtime.callAdminRpc("authConnectionsList", {
          authorizationSessionId: session.authorizationSessionId,
          state: "admitted",
        });
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
        let replacement: ConsumerInfo | undefined;
        const infoPrefix = `$JS.API.CONSUMER.INFO.${mirrorStream}.`;
        hold = gate.armResponseHold(infoPrefix, proxy.id, (body) => {
          const info: ConsumerInfo = JSON.parse(new TextDecoder().decode(body));
          if (
            info.name !== old.name &&
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
        assert(await manager.consumers.delete(mirrorStream, replacement.name));
        await hold.release();
        hold = gate.armResponseHold(
          infoPrefix,
          proxy.id,
          () => service.connection.availability().resources.extras === false,
        );
        await deadline(hold.held, 30_000);
        assert.equal(service.connection.availability().resources.extras, false);
        const countBefore = gate
          .connection(proxy.id)
          ?.outboundContexts.filter(
            (item) => item.subject === kvSubject,
          ).length;
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
        await deadline(write, 5_000);
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
          }),
          attachmentIds,
        );
        assert.deepEqual(restored.map(brokerConnectionKey), [originalKey]);
        assert.equal(gate.connection(proxy.id)?.closed, false);
        assert.equal(service.connection.availability().resources.extras, true);
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
        hold = gate.armResponseHold(
          infoPrefix,
          proxy.id,
          () => service.connection.availability().resources.extras === false,
        );
        assert(
          await manager.consumers.delete(mirrorStream, activeWatches[0].name),
        );
        await deadline(hold.held, 30_000);
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
    { interruptibleNativeProxy: true },
  );
});

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
              "runtime-trellis.transport_growth@v1::extend"
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
              "runtime-trellis.transport_growth@v1::extend"
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
        const capabilityId = "runtime-trellis.transport_growth@v1::extend";
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
        contextLifetimeSeconds: 125,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 95,
      },
    },
  );
});
