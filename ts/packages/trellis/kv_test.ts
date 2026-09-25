import { assertEquals, assertRejects } from "@std/assert";
import { Result, UnexpectedError } from "@oatscenter/result";

import {
  decodeResourceValue,
  encodeResourceValue,
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
