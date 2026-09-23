import { assertEquals, assertInstanceOf } from "@std/assert";
import { Result } from "@oatscenter/result";

import { codecs } from "./generated_support.ts";
import { Conflict } from "./internal_sdk/generated/apis/state/mod.js";
import type { KvRepresentation } from "./kv.ts";
import {
  ResourceUnavailableError,
  StateConflictError,
  StateHandle,
} from "./session.ts";

const definition: KvRepresentation<{ count: number }> = {
  version: 2,
  codec: {
    encode: (value) => value,
    decode: (value) => ({
      count: Reflect.get(value as object, "count") as number,
    }),
  },
  migrations: {
    1: { encode: (value) => value, decode: (value) => value },
  },
};

function countOf(value: unknown): number {
  if (!value || typeof value !== "object") throw new Error("invalid value");
  const count = Reflect.get(value, "count");
  if (typeof count !== "number") throw new Error("invalid count");
  return count;
}

Deno.test("State uses one-value CAS modes and direct read migration", async () => {
  const requests: Array<Record<string, unknown>> = [];
  const historical = new TextEncoder().encode(JSON.stringify({ count: 1 }));
  const handle = new StateHandle({
    resourceName: "counter",
    definition,
    migrations: { 1: (value) => ({ count: countOf(value) + 1 }) },
    calls: {
      get: () =>
        Promise.resolve(Result.ok({
          entry: {
            value: historical,
            representationVersion: 1,
            revision: "4",
            createdAt: codecs.timestamp.decode("2026-01-01T00:00:00Z"),
            updatedAt: codecs.timestamp.decode("2026-01-01T00:00:00Z"),
          },
        })),
      set: (input) => {
        requests.push(input);
        return Promise.resolve(Result.ok({
          entry: {
            value: input.value,
            representationVersion: Number(input.representationVersion),
            revision: "5",
            createdAt: codecs.timestamp.decode("2026-01-01T00:00:00Z"),
            updatedAt: codecs.timestamp.decode("2026-01-01T00:00:01Z"),
          },
        }));
      },
      delete: (input) => {
        requests.push(input);
        return Promise.resolve(Result.ok({ deleted: true }));
      },
    },
  });

  assertEquals((await handle.get()).unwrapOrElse(() => undefined)?.value, {
    count: 2,
  });
  await handle.create({ count: 3 });
  await handle.set({ count: 4 });
  await handle.replace("5", { count: 5 });
  await handle.delete("6");
  assertEquals(
    new TextDecoder().decode(requests[0].value as Uint8Array),
    '{"count":3}',
  );
  assertEquals(requests.map((request) => request.mode), [
    "create",
    "set",
    "replace",
    undefined,
  ]);
  assertEquals(requests.map((request) => request.revision), [
    undefined,
    undefined,
    "5",
    "6",
  ]);
  assertEquals(
    requests.some((request) => "expectedRevision" in request),
    false,
  );
});

Deno.test("State decodes generated create, replace, and delete conflicts", async () => {
  const conflict = Conflict.fromSerializable({
    id: "conflict-id",
    type: Conflict.type,
    message: "State revision conflict",
    current: {
      value: "eyJjb3VudCI6N30=",
      representationVersion: 2,
      revision: "9",
      createdAt: "2026-01-01T00:00:00Z",
      updatedAt: "2026-01-01T00:00:01Z",
    },
  });
  const handle = new StateHandle({
    resourceName: "counter",
    definition,
    calls: {
      get: () => Promise.resolve(Result.ok({})),
      set: () => Promise.resolve(Result.err(conflict)),
      delete: () => Promise.resolve(Result.err(conflict)),
    },
  });

  for (
    const result of [
      await handle.create({ count: 1 }),
      await handle.replace("8", { count: 1 }),
      await handle.delete("8"),
    ]
  ) {
    assertInstanceOf(result.error, StateConflictError);
    assertEquals(result.error.current, {
      value: { count: 7 },
      revision: "9",
      createdAt: codecs.timestamp.decode("2026-01-01T00:00:00Z"),
      updatedAt: codecs.timestamp.decode("2026-01-01T00:00:01Z"),
    });
  }
});

Deno.test("State denies retained handles after a generation change", async () => {
  let current = true;
  const handle = new StateHandle({
    resourceName: "counter",
    definition,
    isCurrent: () => current,
    calls: {
      get: () => Promise.resolve(Result.ok({})),
      set: () =>
        Promise.resolve(Result.ok({
          entry: {
            value: new Uint8Array(),
            representationVersion: 2,
            revision: "1",
            createdAt: codecs.timestamp.decode("2026-01-01T00:00:00Z"),
            updatedAt: codecs.timestamp.decode("2026-01-01T00:00:00Z"),
          },
        })),
      delete: () => Promise.resolve(Result.ok({ deleted: false })),
    },
  });
  current = false;
  const result = await handle.get();
  assertInstanceOf(result.error.cause, Error);
  assertEquals(
    result.error.cause.message,
    "State resource 'counter' has a stale generation",
  );
});

Deno.test("optional State reports a dedicated unavailable error", async () => {
  const handle = new StateHandle({
    resourceName: "counter",
    definition,
    isAvailable: () => false,
    calls: {
      get: () => Promise.resolve(Result.ok({})),
      set: () => Promise.reject(new Error("unreachable")),
      delete: () => Promise.reject(new Error("unreachable")),
    },
  });
  const result = await handle.get();
  assertInstanceOf(result.error, ResourceUnavailableError);
});
