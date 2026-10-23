import assert from "node:assert/strict";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { ulid } from "ulid";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { PermissionViolationError } from "@nats-io/nats-core";
import {
  connectNativeClient,
  connectNativeSession,
} from "@oatscenter/trellis/native";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

async function collect<T>(
  values: AsyncIterable<{ orThrow(): T }>,
): Promise<T[]> {
  const items = [];
  for await (const value of values) items.push(value.orThrow());
  return items;
}

test("Store deletion preserves neighboring objects and rejects a foreign purge", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "store-delete",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      const exit = service.wait().catch((error: unknown) => error);
      try {
        const files = await service.store.files.open().orThrow();
        await files.put("deleted", new Uint8Array([1, 2, 3])).orThrow();
        await files.put("retained", new Uint8Array([4, 5])).orThrow();
        await files.put("retained", new Uint8Array([4, 5, 6])).orThrow();
        await files.delete("deleted").orThrow();
        assert.deepEqual(
          (await files.list().orThrow()).entries.map((entry) => entry.key),
          ["retained"],
        );
        const retained = await files.get("retained").orThrow();
        assert(retained);
        assert.deepEqual(
          await retained.bytes().orThrow(),
          new Uint8Array([4, 5, 6]),
        );
        const subject = runtime
          .nativeTransportGate()
          .connections()
          .flatMap((connection) => connection.outboundContexts)
          .find((item) =>
            item.subject.startsWith("$JS.API.STREAM.PURGE.OBJ_"),
          )?.subject;
        assert(subject, "the real object deletion must exercise chunk cleanup");
        const login = await runtime.registerClient({
          name: "foreign-purge",
          contract: apps.EchoCaller.request,
        });
        const unrelated = await connectNativeSession(login).orThrow();
        const socket = unrelated.manager.currentNats();
        const statuses = socket.status();
        try {
          const denied = (async () => {
            for await (const status of statuses) {
              if (
                status.type === "error" &&
                status.error instanceof PermissionViolationError
              ) {
                assert.equal(status.error.operation, "publish");
                assert.equal(status.error.subject, subject);
                return true;
              }
            }
            return false;
          })();
          socket.publish(subject, new TextEncoder().encode("{}"));
          let result: boolean | undefined;
          void denied.then((value) => {
            result = value;
          });
          await runtime.waitFor(() => result !== undefined);
          assert.equal(result, true);
        } finally {
          await unrelated.manager.close();
        }
        const stillRetained = await files.get("retained").orThrow();
        assert(stillRetained);
        assert.deepEqual(
          await stillRetained.bytes().orThrow(),
          new Uint8Array([4, 5, 6]),
        );
      } finally {
        await service.stop();
        await exit;
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("generated keyset clients cross the live runtime boundary", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.registerService({
      name: "store-pagination-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: "store-pagination-provider",
      seed: key.seed,
    }).orThrow();
    let closeAdmin: (() => Promise<void>) | undefined;
    try {
      const files = await service.store.files.open().orThrow();
      const names = Array.from(
        { length: 502 },
        (_, index) => `p/${index.toString().padStart(3, "0")}`,
      );
      for (const name of names) {
        await files.put(name, new Uint8Array([1])).orThrow();
      }

      const maximum = await files
        .list({
          prefix: "p/",
          limit: 500,
        })
        .orThrow();
      assert.deepEqual(maximum.entries.length, 500);
      assert(maximum.nextCursor);

      const first = await files.list({ prefix: "p/" }).orThrow();
      assert.deepEqual(first.entries.length, 100);
      assert.deepEqual(
        first.entries.map((entry) => entry.key),
        names.slice(0, 100),
      );
      assert(first.nextCursor);

      await files.put("p/050a", new Uint8Array([1])).orThrow();
      await files.delete("p/000").orThrow();
      const second = await files
        .list({
          prefix: "p/",
          cursor: first.nextCursor,
          limit: 500,
        })
        .orThrow();
      assert.deepEqual(second.nextCursor, undefined);
      const seen = [...first.entries, ...second.entries].map(
        (entry) => entry.key,
      );
      assert.deepEqual(seen, names);
      assert.deepEqual(new Set(seen).size, seen.length);

      assert((await files.list({ limit: 0 })).isErr());
      assert((await files.list({ limit: 501 })).isErr());
      assert((await files.list({ cursor: "malformed" })).isErr());
      assert(
        (
          await files.list({
            prefix: "other/",
            cursor: first.nextCursor,
          })
        ).isErr(),
      );

      service.jobs.work.handle(({ job }) =>
        Promise.resolve(Result.ok(job.payload)),
      );
      const login = await loginNativeTestClient({
        issuer: runtime.oauthIssuer,
        clientId: "trellis.cli",
        request: adminParticipant,
        username: "admin",
        password: runtime.adminPassword,
        administrativeCapabilities: ["trellis.managePrincipals"],
      });
      const consoleClient = await connectNativeClient({
        ...login,
        request: adminParticipant,
        name: "pagination-admin",
      }).orThrow();
      closeAdmin = () => consoleClient.connection.close();
      for (let index = 0; index < 51; index++) {
        await consoleClient
          .usersCreate({
            idempotencyKey: ulid(),
            displayName: `Pagination user ${index}`,
            username: `pagination-user-${index.toString().padStart(2, "0")}`,
          })
          .orThrow();
      }
      for (let index = 0; index < 101; index++) {
        await service.jobs.work.create({ value: `page-${index}` }).orThrow();
        await service.publishChanged({ value: `page-${index}` }).orThrow();
      }
      await runtime.waitFor(
        async () =>
          (await consoleClient.usersList({}).orThrow()).items.length === 50 &&
          (await consoleClient.jobsQuery({}).orThrow()).items.length === 100 &&
          (await consoleClient.eventsQuery({}).orThrow()).items.length === 100,
        { timeoutMs: 60_000 },
      );

      const users = await collect(consoleClient.usersList.items({}));
      const jobs = await collect(consoleClient.jobsQuery.items({}));
      const events = await collect(consoleClient.eventsQuery.items({}));
      assert(users.length > 50);
      assert(jobs.length > 100);
      assert(events.length > 100);
      assert.deepEqual(
        new Set(users.map((row) => row.principalId)).size,
        users.length,
      );
      assert.deepEqual(new Set(jobs.map((row) => row.id)).size, jobs.length);
      assert.deepEqual(
        new Set(events.map((row) => row.eventId)).size,
        events.length,
      );

      const consumers = (await runtime.events.consumersQuery({})).items;
      const health = await collect(consoleClient.healthQuery.items({}));
      const deadLetters = (await runtime.events.deadLettersQuery({})).items;
      assert(consumers.length >= 1);
      assert(health.length >= 1);
      assert.deepEqual(
        new Set(consumers.map((row) => row.resourceId)).size,
        consumers.length,
      );
      assert.deepEqual(
        new Set(health.map((row) => `${row.participantKind}:${row.contractId}`))
          .size,
        health.length,
      );
      assert.deepEqual(
        new Set(deadLetters.map((row) => row.deadLetterId)).size,
        deadLetters.length,
      );

      const eventPage = await consoleClient
        .eventsQuery({ page: { limit: 1 } })
        .orThrow();
      assert(eventPage.page.nextCursor);
      assert(
        (
          await consoleClient.eventsQuery({
            page: { cursor: eventPage.page.nextCursor },
            ownerEventName: "different-query",
          })
        ).isErr(),
      );
      for (const [endpoint, malformed] of [
        [
          "Events.Query",
          consoleClient.eventsQuery({ page: { cursor: "malformed" } }),
        ],
        [
          "Jobs.Query",
          consoleClient.jobsQuery({ page: { cursor: "malformed" } }),
        ],
        [
          "Health.Query",
          consoleClient.healthQuery({ page: { cursor: "malformed" } }),
        ],
      ] as const)
        assert(
          (await malformed).isErr(),
          `${endpoint} must reject an invalid opaque cursor`,
        );
      await assert.rejects(() =>
        runtime.events.consumersQuery({ page: { cursor: "malformed" } }),
      );
      await assert.rejects(() =>
        runtime.events.deadLettersQuery({ page: { cursor: "malformed" } }),
      );
    } finally {
      await closeAdmin?.();
      await service.stop();
    }
  });
});
