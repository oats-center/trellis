import { type Client, createClient } from "@libsql/client";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import {
  createSqliteOutboxSchema,
  MemoryOutboxRepository,
  OutboxDuplicateIdentityError,
  outboxMessageToPreparedEvent,
  preparedTrellisEventToOutboxRecord,
  type SqlExecutor,
  SqlInboxRepository,
  SqlOutboxRepository,
} from "./outbox_inbox.ts";

test("event outbox preserves exact descriptor identity", () => {
  const descriptorIdentity = "v1.dGVzdEB2MQ.Q29ubmVjdGlvbnMuT3BlbmVk.0";
  const record = preparedTrellisEventToOutboxRecord({
    event: "Connections.Opened",
    descriptorIdentity,
    subject: "events.v1.dGVzdEB2MQ.Connections.Opened",
    header: { id: "event-1", time: "2026-01-01T00:00:00Z" },
    payload: {},
    encodedPayload: "{}",
    headers: {},
  });
  const prepared = outboxMessageToPreparedEvent({
    ...record,
    headers: { ...record.headers },
    state: "pending",
    attempts: 0,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
  });

  assert.deepEqual(prepared.descriptorIdentity, descriptorIdentity);
});

function sqlExecutor(database: Pick<Client, "execute">): SqlExecutor {
  const execute = (sql: string, params: readonly unknown[]) =>
    database.execute({
      sql,
      args: params.map((value) => {
        assert(
          value === null ||
            typeof value === "string" ||
            typeof value === "number" ||
            typeof value === "bigint" ||
            typeof value === "boolean" ||
            value instanceof Uint8Array ||
            value instanceof ArrayBuffer,
        );
        return value;
      }),
    });
  return {
    async query(sql, params) {
      return (await execute(sql, params)).rows;
    },
    async execute(sql, params) {
      await execute(sql, params);
    },
  };
}

test("SQLite commits and rolls back business writes with outbox records", async () => {
  const dir = await mkdtemp(join(tmpdir(), "trellis-outbox-"));
  const client = createClient({ url: pathToFileURL(`${dir}/outbox.db`).href });
  try {
    await client.execute("CREATE TABLE orders (id TEXT PRIMARY KEY)");
    for (const statement of createSqliteOutboxSchema()) {
      await client.execute(statement);
    }
    for (const commit of [true, false]) {
      const id = commit ? "committed" : "rolled-back";
      const transaction = (async () => {
        const tx = await client.transaction("write");
        try {
          await tx.execute({
            sql: "INSERT INTO orders (id) VALUES (?)",
            args: [id],
          });
          const outbox = new SqlOutboxRepository(sqlExecutor(tx), "sqlite");
          await outbox.enqueue({
            id,
            kind: "event.publish",
            name: "Orders.Created",
            subject: "orders.created",
            payload: JSON.stringify({ id }),
            headers: {},
          });
          if (!commit) {
            // A real constraint failure after both writes must roll back both rows.
            await tx.execute({
              sql: "INSERT INTO orders (id) VALUES (?)",
              args: [id],
            });
          }
          await tx.commit();
        } catch (error) {
          await tx.rollback();
          throw error;
        } finally {
          tx.close();
        }
      })();
      if (commit) await transaction;
      else await assert.rejects(() => transaction);
      assert.deepEqual(
        (
          await client.execute({
            sql: "SELECT id FROM orders WHERE id = ?",
            args: [id],
          })
        ).rows.length,
        commit ? 1 : 0,
      );
      const outbox = new SqlOutboxRepository(sqlExecutor(client), "sqlite");
      assert.deepEqual((await outbox.get(id))?.id, commit ? id : undefined);
    }
  } finally {
    client.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("SQLite inbox accepts a concurrent message ID exactly once", async () => {
  const dir = await mkdtemp(join(tmpdir(), "trellis-outbox-"));
  const clients = [0, 1].map(() =>
    createClient({ url: pathToFileURL(`${dir}/inbox.db`).href }),
  );
  try {
    for (const statement of createSqliteOutboxSchema()) {
      await clients[0].execute(statement);
    }
    const inboxes = clients.map(
      (client) => new SqlInboxRepository(sqlExecutor(client), "sqlite"),
    );
    assert.deepEqual(
      (
        await Promise.all(inboxes.map((inbox) => inbox.record("same-message")))
      ).sort(),
      [false, true],
    );
  } finally {
    for (const client of clients) client.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("outbox duplicate IDs preserve stored identity and state", async () => {
  const repository = new MemoryOutboxRepository();
  const record = {
    id: "same-id",
    kind: "event.publish" as const,
    name: "Orders.Created",
    subject: "orders.created",
    payload: "{}",
    headers: { proof: "original" },
  };
  await repository.enqueue(record);
  const [claim] = await repository.claimDue(1, new Date());
  assert(claim);
  assert.deepEqual((await repository.enqueue(record)).state, "claimed");
  await assert.rejects(
    () => repository.enqueue({ ...record, payload: '{"different":true}' }),
    OutboxDuplicateIdentityError,
  );
});

test("SQLite outbox duplicate IDs return the winning stored row", async () => {
  const dir = await mkdtemp(join(tmpdir(), "trellis-outbox-"));
  const client = createClient({
    url: pathToFileURL(`${dir}/duplicates.db`).href,
  });
  try {
    for (const statement of createSqliteOutboxSchema()) {
      await client.execute(statement);
    }
    const repository = new SqlOutboxRepository(sqlExecutor(client), "sqlite");
    const record = {
      id: "same-id",
      kind: "event.publish" as const,
      name: "Orders.Created",
      subject: "orders.created",
      payload: "{}",
      headers: { proof: "original" },
    };
    await repository.enqueue(record);
    const [claim] = await repository.claimDue(1, new Date());
    assert(claim);
    assert.deepEqual((await repository.enqueue(record)).state, "claimed");
    await assert.rejects(
      () => repository.enqueue({ ...record, subject: "orders.changed" }),
      OutboxDuplicateIdentityError,
    );
  } finally {
    client.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test("SQLite outbox recovers abandoned claims without accepting stale completion", async () => {
  const dir = await mkdtemp(join(tmpdir(), "trellis-outbox-"));
  const url = pathToFileURL(`${dir}/outbox.db`).href;
  const clients = [createClient({ url }), createClient({ url })];
  try {
    for (const statement of createSqliteOutboxSchema()) {
      await clients[0].execute(statement);
    }
    const repositories = clients.map(
      (client) => new SqlOutboxRepository(sqlExecutor(client), "sqlite"),
    );
    await repositories[0].enqueue({
      id: "event",
      kind: "event.publish",
      name: "Created",
      subject: "events.v1.Created",
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
    for (const client of clients) {
      client.close();
    }
    const reopened = createClient({ url });
    clients.push(reopened);
    const repository = new SqlOutboxRepository(sqlExecutor(reopened), "sqlite");
    assert(claims[0].nextAttemptAt);
    const expired = new Date(claims[0].nextAttemptAt);
    const [current] = await repository.claimDue(1, expired);
    assert(current);
    assert.deepEqual(
      await repository.markDispatched(claims[0], expired),
      false,
    );
    assert.deepEqual(
      await repository.markFailed(claims[0], {
        error: "stale",
        now: expired,
        nextAttemptAt: expired,
      }),
      false,
    );
    assert.deepEqual(await repository.claimDue(1, expired), []);
    assert.deepEqual(await repository.markDispatched(current, expired), true);
    assert.deepEqual((await repository.get("event"))?.state, "dispatched");
  } finally {
    for (const client of clients) client.close();
    await rm(dir, { recursive: true, force: true });
  }
});
