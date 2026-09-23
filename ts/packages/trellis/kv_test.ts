import { assertEquals, assertRejects } from "@std/assert";
import { Result, UnexpectedError } from "@oats-center/result";

import {
  decodeResourceValue,
  encodeResourceValue,
  ensureExistingBucketOptions,
  type KvRepresentation,
} from "./kv.ts";

const current: KvRepresentation<{ count: number }> = {
  version: 2,
  codec: {
    encode: (value) => value,
    decode: (value) => {
      if (
        !value || typeof value !== "object" ||
        typeof Reflect.get(value, "count") !== "number"
      ) throw new Error("invalid current value");
      return { count: Reflect.get(value, "count") as number };
    },
  },
  migrations: {
    1: {
      encode: (value) => value,
      decode: (value) => value,
    },
  },
};

function countOf(value: unknown): number {
  if (!value || typeof value !== "object") throw new Error("invalid value");
  const count = Reflect.get(value, "count");
  if (typeof count !== "number") throw new Error("invalid count");
  return count;
}

Deno.test("resource envelope is strict and current values round trip", async () => {
  const encoded = encodeResourceValue(current, { count: 3 });
  assertEquals(new TextDecoder().decode(encoded.subarray(0, 4)), "TRKV");
  assertEquals(await decodeResourceValue(current, {}, encoded), { count: 3 });
  await assertRejects(() =>
    decodeResourceValue(current, {}, new Uint8Array([1, 2, 3]))
  );
});

Deno.test("resource migration is direct, fallible, and read-only", async () => {
  const historical = encodeResourceValue({ ...current, version: 1 }, {
    count: 2,
  });
  let calls = 0;
  assertEquals(
    await decodeResourceValue(current, {
      1: (value) => {
        calls += 1;
        return { count: countOf(value) + 1 };
      },
    }, historical),
    { count: 3 },
  );
  assertEquals(calls, 1);

  await assertRejects(() =>
    decodeResourceValue(current, {
      1: () => Result.err(new UnexpectedError({ cause: new Error("failed") })),
    }, historical)
  );
});

Deno.test("existing KV bucket options reject data-loss drift", async () => {
  const cases = [
    ["exact", 2, 1_000, 100, 0, true],
    ["over-history", 3, 1_000, 100, 0, true],
    ["short history", 1, 1_000, 100, 0, false],
    ["shorter TTL", 2, 999, 100, 0, false],
    ["longer TTL", 2, 1_001, 100, 0, false],
    ["unlimited TTL", 2, 0, 100, 0, false],
    ["unlimited value", 2, 1_000, 0, 0, true],
    ["unlimited total", 2, 1_000, 100, 0, true],
    ["small value", 2, 1_000, 99, 0, false],
    ["finite total", 2, 1_000, 100, 1_000, false],
    ["small total", 2, 1_000, 100, 99, false],
  ] as const;

  for (
    const [label, history, ttl, maxValueSize, maxBytes, compatible] of cases
  ) {
    const check = () =>
      ensureExistingBucketOptions(
        {
          status: () =>
            Promise.resolve({
              bucket: "orders",
              history,
              ttl,
              maxValueSize,
              max_bytes: maxBytes,
            }),
        },
        "orders",
        { history: 2, ttl: 1_000, maxValueBytes: 100 },
      );
    if (compatible) await check();
    else await assertRejects(check, Error, undefined, label);
  }

  await ensureExistingBucketOptions(
    {
      status: () =>
        Promise.resolve({
          bucket: "orders",
          history: 1,
          ttl: 0,
          maxValueSize: 0,
          max_bytes: 0,
        }),
    },
    "orders",
    {},
  );
  await assertRejects(() =>
    ensureExistingBucketOptions(
      {
        status: () =>
          Promise.resolve({
            bucket: "other",
            history: 2,
            ttl: 1_000,
            maxValueSize: 100,
            max_bytes: 0,
          }),
      },
      "orders",
      { history: 2, ttl: 1_000, maxValueBytes: 100 },
    )
  );
});
