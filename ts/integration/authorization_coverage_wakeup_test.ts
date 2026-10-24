import assert from "node:assert/strict";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("fresh KV acquisitions resume after unrelated signed revocation updates", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.Provider.participant;
      const identity = await runtime.registerService({
        name: "coverage-wakeup-provider",
        contract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      const stopped = service.wait().catch(() => undefined);
      const others = [];
      let writes: Promise<void> | undefined;
      let completed = 0;
      try {
        for (let index = 0; index < 8; index++) {
          const client = await runtime.connectClient({
            name: `unrelated-wakeup-${index}`,
            contract: adminParticipant,
          });
          others.push(client);
        }
        const sessions = await Promise.all(
          others.map((client) => client.sessionsMe({}).orThrow()),
        );
        const records = service.kv.records;
        assert(records);
        await records.put("coverage-wakeup", { value: "baseline" }).orThrow();
        const gate = runtime.nativeTransportGate();
        const socket = gate
          .connections()
          .find((connection) =>
            connection.outboundContexts.some((message) =>
              message.subject.endsWith(".coverage-wakeup"),
            ),
          );
        assert(socket);
        const before = (
          await runtime.callAdminRpc("authAuthorizationSessionsList", {
            state: "active",
          })
        ).items.find((item) => item.binding.instanceId === identity.instanceId);
        assert(before);
        assert(before.maximumContextExpiresAt);
        writes = (async () => {
          for (let index = 0; index < 512; index++) {
            await records
              .put("coverage-wakeup", { value: String(index) })
              .orThrow();
            completed++;
          }
        })();
        void writes.catch(() => undefined);
        const revocations = (async () => {
          for (const session of sessions) {
            await runtime.callAdminRpc("authAuthorizationSessionsRevoke", {
              authorizationSessionId: session.authorizationSessionId,
              idempotencyKey: ulid(),
            });
          }
        })();
        await deadline(Promise.all([writes, revocations]), 10_000).catch(
          (cause: unknown) => {
            throw new Error(
              `KV progress stalled at ${completed}; usable=${service.connection.availability().resources.extras}`,
              { cause },
            );
          },
        );
        assert.deepEqual(await records.get("coverage-wakeup").orThrow(), {
          value: "511",
        });
        assert.equal(gate.connection(socket.id)?.closed, false);
        const after = (
          await runtime.callAdminRpc("authAuthorizationSessionsList", {
            state: "active",
          })
        ).items.find(
          (item) =>
            item.authorizationSessionId === before.authorizationSessionId,
        );
        assert(after);
        assert.equal(
          after.maximumContextExpiresAt,
          before.maximumContextExpiresAt,
          "ordinary coverage restoration must not require authority renewal",
        );
        assert.equal(
          gate
            .connections()
            .filter((connection) =>
              connection.outboundContexts.some((message) =>
                message.subject.endsWith(".coverage-wakeup"),
              ),
            ).length,
          1,
        );
      } finally {
        await service.stop();
        await stopped;
        await writes?.catch(() => undefined);
        await Promise.all(others.map((client) => client.connection.close()));
      }
    },
    { interruptibleNativeProxy: true },
  );
});
