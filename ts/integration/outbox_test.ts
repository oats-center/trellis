import { connect, credsAuthenticator } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { Value } from "typebox/value";
import { TypedKV } from "../packages/trellis/kv.ts";
import { fixedTransportProvider } from "../packages/trellis/transport/generations.ts";
import {
  type KvOutboxRecord,
  KvOutboxRecordSchema,
  NatsKvOutboxRepository,
} from "../packages/trellis/service/outbox_inbox.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("NATS KV outbox claims recover with CAS-fenced completion", async () => {
  await withTrellisRuntime(async (runtime) => {
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await readFile(
          `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
        ),
      ),
    });
    try {
      const transport = fixedTransportProvider(nats);
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
      const kv = await TypedKV.open(
        transport,
        bucket,
        representation,
        {},
      ).orThrow();
      const other = await TypedKV.open(transport, bucket, representation, {
        bindOnly: true,
      }).orThrow();
      const repositories = [kv, other].map(
        (store) => new NatsKvOutboxRepository(store),
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
      const claims = (
        await Promise.all(
          repositories.map((repository) => repository.claimDue(1, now)),
        )
      ).flat();
      assert.deepEqual(claims.length, 1);
      assert.deepEqual(await repositories[1].claimDue(1, now), []);
      const reopened = await TypedKV.open(transport, bucket, representation, {
        bindOnly: true,
      }).orThrow();
      const recovered = new NatsKvOutboxRepository(reopened);
      assert(claims[0].nextAttemptAt);
      const expired = new Date(claims[0].nextAttemptAt);
      const [current] = await recovered.claimDue(1, expired);
      assert(current);
      assert.deepEqual(
        await repositories[0].markDispatched(claims[0], expired),
        false,
      );
      assert.deepEqual(
        await repositories[0].markFailed(claims[0], {
          error: "stale",
          now: expired,
          nextAttemptAt: expired,
        }),
        false,
      );
      assert.deepEqual(await recovered.claimDue(1, expired), []);
      const completions = await Promise.all([
        recovered.markDispatched(current, expired),
        repositories[1].markDispatched(current, expired),
      ]);
      assert.deepEqual(completions.sort(), [false, true]);
      assert.deepEqual((await recovered.get("event"))?.state, "dispatched");
    } finally {
      await nats.close();
    }
  });
});
