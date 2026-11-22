/** A service whose revocation mirror is down keeps working on its own session. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { jetstreamManager, type ConsumerInfo } from "@nats-io/jetstream";
import { TrellisService } from "@oatscenter/trellis/service";
import { Result } from "@oatscenter/trellis";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const MIRROR_STREAM = "KV_TRELLIS_AUTH_REVOCATIONS";

test("KV writes and RPCs proceed on the original socket while the revocation mirror is down", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: "own-watch-provider",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      let echoes = 0;
      await service.handleEcho(({ input }) => {
        echoes++;
        return Result.ok(input);
      });
      const serviceExit = service.wait().catch((error: unknown) => error);
      const broker = await runtime.connectNats();
      const manager = await jetstreamManager(broker);
      const gate = runtime.nativeTransportGate();
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
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
        const mirrors = async () => {
          const bound: ConsumerInfo[] = [];
          for await (const info of manager.consumers.list(MIRROR_STREAM)) {
            if (
              info.push_bound &&
              gate
                .connection(proxy.id)
                ?.subs.some(
                  (subscription) =>
                    subscription.subject === info.config.deliver_subject,
                )
            )
              bound.push(info);
          }
          return bound;
        };
        const [old] = await mirrors();
        assert(old);
        // Delete this socket's mirror consumer and withhold every replacement
        // snapshot, so the service has no current mirror at all.
        hold = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${MIRROR_STREAM}.`,
          proxy.id,
          (body) => {
            const info: ConsumerInfo = JSON.parse(
              new TextDecoder().decode(body),
            );
            return typeof info.name === "string" && info.name !== old.name;
          },
          { continuous: true },
        );
        assert(await manager.consumers.delete(MIRROR_STREAM, old.name));
        await deadline(hold.held, 30_000);

        // Own work does not wait for the mirror. A client that did would
        // time out on each of these.
        for (let i = 0; i < 3; i++) {
          await deadline(
            records.put("own-recovery", { value: `down ${i}` }).orThrow(),
            3_000,
          );
          assert.deepEqual(
            await service
              .echo({ value: `down ${i}` }, { timeout: 3_000 })
              .orThrow(),
            { value: `down ${i}` },
          );
        }
        assert.equal(echoes, 3);
        assert.deepEqual(await records.get("own-recovery").orThrow(), {
          value: "down 2",
        });

        // Released, the mirror catches up on the same socket.
        await hold.release();
        hold = undefined;
        await runtime.waitFor(async () => (await mirrors()).length === 1);
        assert.equal(gate.connection(proxy.id)?.closed, false);
        assert.deepEqual(await service.echo({ value: "caught up" }).orThrow(), {
          value: "caught up",
        });
      } finally {
        await hold?.release();
        await service.stop();
        await serviceExit;
        await broker.close();
      }
    },
    { interruptibleNativeProxy: true },
  );
});
