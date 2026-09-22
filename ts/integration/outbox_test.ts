import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { assert, assertEquals } from "@std/assert";
import { Value } from "typebox/value";
import { TypedKV } from "../packages/trellis/kv.ts";
import {
  type KvOutboxRecord,
  KvOutboxRecordSchema,
  NatsKvOutboxRepository,
} from "../packages/trellis/service/outbox_inbox.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("NATS KV outbox claims recover with CAS-fenced completion", async () => {
  await withTrellisRuntime(async (runtime) => {
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(`${runtime.workdir}/nats/creds/trellis-auth.creds`),
      ),
    });
    try {
      const bucket = "outbox_claims";
      const representation = {
        codec: {
          decode: (value: unknown) => Value.Decode(KvOutboxRecordSchema, value),
          encode: (value: KvOutboxRecord) =>
            Value.Encode(KvOutboxRecordSchema, value),
        },
        version: 1,
        migrations: {},
      };
      const kv = await TypedKV.open(nats, bucket, representation, {})
        .orThrow();
      const other = await TypedKV.open(nats, bucket, representation, {
        bindOnly: true,
      }).orThrow();
      const repositories = [kv, other].map((store) =>
        new NatsKvOutboxRepository(store)
      );
      await repositories[0].enqueue({
        id: "event",
        kind: "event.publish",
        name: "Created",
        subject: "events.v1.outbox.Created",
        payload: "{}",
        headers: {},
      });
      const now = new Date();
      const claims = (await Promise.all(repositories.map((repository) =>
        repository.claimDue(1, now)
      ))).flat();
      assertEquals(claims.length, 1);
      assertEquals(await repositories[1].claimDue(1, now), []);
      const reopened = await TypedKV.open(nats, bucket, representation, {
        bindOnly: true,
      }).orThrow();
      const recovered = new NatsKvOutboxRepository(reopened);
      assert(claims[0].nextAttemptAt);
      const expired = new Date(claims[0].nextAttemptAt);
      const [current] = await recovered.claimDue(1, expired);
      assert(current);
      assertEquals(
        await repositories[0].markDispatched(claims[0], expired),
        false,
      );
      assertEquals(
        await repositories[0].markFailed(claims[0], {
          error: "stale",
          now: expired,
          nextAttemptAt: expired,
        }),
        false,
      );
      assertEquals(await recovered.claimDue(1, expired), []);
      const completions = await Promise.all([
        recovered.markDispatched(current, expired),
        repositories[1].markDispatched(current, expired),
      ]);
      assertEquals(completions.sort(), [false, true]);
      assertEquals((await recovered.get("event"))?.state, "dispatched");
    } finally {
      await nats.close();
    }
  });
});
