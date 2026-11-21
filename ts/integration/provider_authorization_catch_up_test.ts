/** A provider whose revocation mirror is briefly behind waits for it instead of dropping requests. */
import assert from "node:assert/strict";
import { setTimeout as sleep } from "node:timers/promises";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { jetstreamManager, type ConsumerInfo } from "@nats-io/jetstream";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const ECHO = apis.runtime.API.actions["rpc:Echo"].subject;
const MIRROR_STREAM = "KV_TRELLIS_AUTH_REVOCATIONS";

test("a Node provider answers a request that arrives while its revocation mirror catches up", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "catch-up-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      let echoes = 0;
      await service.handleEcho(({ input }) => {
        echoes++;
        return Result.ok(input);
      });
      const caller = await runtime.connectClient({
        name: "catch-up-caller",
        contract: apps.Caller.request,
      });
      const broker = await runtime.connectNats();
      const gate = runtime.nativeTransportGate();
      let hold: ReturnType<typeof gate.armResponseHold> | undefined;
      try {
        assert.deepEqual(await caller.echo({ value: "ready" }).orThrow(), {
          value: "ready",
        });
        const provider = gate
          .connections()
          .find((connection) =>
            connection.subs.some((sub) => sub.subject === ECHO),
          );
        assert(provider);
        const manager = await jetstreamManager(broker);
        const watches: ConsumerInfo[] = [];
        for await (const info of manager.consumers.list(MIRROR_STREAM)) {
          if (
            info.push_bound &&
            provider.subs.some(
              (sub) => sub.subject === info.config.deliver_subject,
            )
          )
            watches.push(info);
        }
        assert.equal(watches.length, 1);
        const [old] = watches;
        // Losing the provider's mirror consumer leaves its route served on the
        // same socket while the replacement snapshot is withheld here.
        hold = gate.armResponseHold(
          `$JS.API.CONSUMER.INFO.${MIRROR_STREAM}.`,
          provider.id,
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
        assert.equal(
          service.connection.availability().resources.extras,
          false,
          "the provider's revocation coverage is withheld",
        );

        const delivered = gate.waitForDelivery(ECHO, provider.id);
        const call = caller.echo(
          { value: "during catch-up" },
          { timeout: 15_000 },
        );
        await deadline(delivered, 10_000);
        // The provider receives the request while its mirror is behind. One
        // that rejects it then gives it no reply, so the call could only time
        // out; leave that attempt time to happen before coverage returns.
        await sleep(1_000);
        assert.equal(echoes, 1);
        await hold.release();
        hold = undefined;

        assert.deepEqual((await call).orThrow(), { value: "during catch-up" });
        assert.equal(echoes, 2);
      } finally {
        await hold?.release();
        await broker.close();
        await caller.connection.close();
        await service.stop();
      }
    },
    { interruptibleNativeProxy: true },
  );
});
