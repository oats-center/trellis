import { assert, assertEquals, assertRejects } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { participants as webParticipants } from "trellis-web-generated";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

async function collect<T>(
  values: AsyncIterable<{ orThrow(): T }>,
): Promise<T[]> {
  const items = [];
  for await (const value of values) items.push(value.orThrow());
  return items;
}

Deno.test("generated keyset clients cross the live runtime boundary", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.registerService({
      name: "store-pagination-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: "store-pagination-provider",
      seed: key.seed,
    }).orThrow();
    try {
      const files = await service.store.files.open().orThrow();
      const names = Array.from(
        { length: 502 },
        (_, index) => `p/${index.toString().padStart(3, "0")}`,
      );
      for (const name of names) {
        await files.put(name, new Uint8Array([1])).orThrow();
      }

      const maximum = await files.list({
        prefix: "p/",
        limit: 500,
      }).orThrow();
      assertEquals(maximum.entries.length, 500);
      assert(maximum.nextCursor);

      const first = await files.list({ prefix: "p/" })
        .orThrow();
      assertEquals(first.entries.length, 100);
      assertEquals(
        first.entries.map((entry) => entry.key),
        names.slice(0, 100),
      );
      assert(first.nextCursor);

      await files.put("p/050a", new Uint8Array([1])).orThrow();
      await files.delete("p/000").orThrow();
      const second = await files.list({
        prefix: "p/",
        cursor: first.nextCursor,
        limit: 500,
      }).orThrow();
      assertEquals(second.nextCursor, undefined);
      const seen = [...first.entries, ...second.entries].map((entry) =>
        entry.key
      );
      assertEquals(seen, names);
      assertEquals(new Set(seen).size, seen.length);

      assert((await files.list({ limit: 0 })).isErr());
      assert((await files.list({ limit: 501 })).isErr());
      assert((await files.list({ cursor: "malformed" })).isErr());
      assert((await files.list({
        prefix: "other/",
        cursor: first.nextCursor,
      })).isErr());

      service.jobs.work.handle(({ job }) =>
        Promise.resolve(Result.ok(job.payload))
      );
      const consoleClient = await runtime.connectClient({
        name: "pagination-console",
        contract: webParticipants.Console.participant,
      });
      for (let index = 0; index < 51; index++) {
        await consoleClient.usersCreate({
          email: null,
          idempotencyKey: crypto.randomUUID(),
          image: null,
          name: `pagination-user-${index.toString().padStart(2, "0")}`,
          username: null,
        }).orThrow();
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
      assertEquals(new Set(users.map((row) => row.userId)).size, users.length);
      assertEquals(new Set(jobs.map((row) => row.id)).size, jobs.length);
      assertEquals(
        new Set(events.map((row) => row.eventId)).size,
        events.length,
      );

      const consumers = (await runtime.events.consumersQuery({})).items;
      const health = await collect(consoleClient.healthQuery.items({}));
      const deadLetters = (await runtime.events.deadLettersQuery({})).items;
      assert(consumers.length >= 1);
      assert(health.length >= 1);
      assertEquals(
        new Set(consumers.map((row) => row.resourceId)).size,
        consumers.length,
      );
      assertEquals(
        new Set(
          health.map((row) => `${row.participantKind}:${row.participantId}`),
        ).size,
        health.length,
      );
      assertEquals(
        new Set(deadLetters.map((row) => row.deadLetterId)).size,
        deadLetters.length,
      );

      const userPage = await consoleClient.usersList({ page: { limit: 1 } })
        .orThrow();
      assert(userPage.page.nextCursor);
      assert((await consoleClient.usersList({
        page: { cursor: userPage.page.nextCursor },
        search: "different-query",
      })).isErr());
      for (
        const malformed of [
          consoleClient.usersList({ page: { cursor: "malformed" } }),
          consoleClient.eventsQuery({ page: { cursor: "malformed" } }),
          consoleClient.jobsQuery({ page: { cursor: "malformed" } }),
          consoleClient.healthQuery({ page: { cursor: "malformed" } }),
        ]
      ) assert((await malformed).isErr());
      await assertRejects(() =>
        runtime.events.consumersQuery({ page: { cursor: "malformed" } })
      );
      await assertRejects(() =>
        runtime.events.deadLettersQuery({ page: { cursor: "malformed" } })
      );
    } finally {
      await service.stop();
    }
  });
});
