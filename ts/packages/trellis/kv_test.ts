import { assertEquals, assertInstanceOf, assertRejects } from "@std/assert";
import { jetstreamManager } from "@nats-io/jetstream";
import { startTrellisRuntime } from "../../integration/_support/runtime.ts";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { KVError } from "./errors/KVError.ts";
import { Result, UnexpectedError } from "@oatscenter/result";

import {
  decodeResourceValue,
  encodeResourceValue,
  type KvRepresentation,
  TypedKV,
} from "./kv.ts";
import { fixedTransportProvider } from "./transport/generations.ts";

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

Deno.test("bucket watch initializes last values, delivers changes and stops on abort", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "trellis-kv-watch-" });
  let nats: NatsTestContainer | undefined;
  const abort = new AbortController();
  try {
    nats = await NatsTestContainer.start(workdir);
    const kv = await TypedKV.open(
      fixedTransportProvider(nats.nc),
      "watch_bucket",
      current,
      { history: 5 },
    ).orThrow();
    await kv.put("first", { count: 1 }).orThrow();
    await kv.put("first", { count: 2 }).orThrow();
    await kv.put("second", { count: 3 }).orThrow();
    const watcher = (await kv.watch(undefined, {
      signal: AbortSignal.any([abort.signal, AbortSignal.timeout(15_000)]),
    }).orThrow())
      [Symbol.asyncIterator]();
    const initial = new Map<string, number | undefined>();
    for (let i = 0; i < 2; i++) {
      const next = await watcher.next();
      const entry = next.value!.orThrow();
      initial.set(entry.key, entry.value?.count);
    }
    assertEquals(initial, new Map([["first", 2], ["second", 3]]));
    await kv.put("first", { count: 4 }).orThrow();
    const update = (await watcher.next()).value!.orThrow();
    assertEquals([update.key, update.value?.count], ["first", 4]);
    await kv.delete("second").orThrow();
    const deleted = (await watcher.next()).value!.orThrow();
    assertEquals([deleted.key, deleted.operation], ["second", "delete"]);
    const pending = watcher.next();
    abort.abort();
    assertEquals((await pending).done, true);
  } finally {
    abort.abort();
    await nats?.stop();
    await Deno.remove(workdir, { recursive: true });
  }
});

Deno.test("KV keys exposes the broker error if its bucket disappears", async () => {
  const workdir = await Deno.makeTempDir({ prefix: "trellis-kv-keys-" });
  let nats: TrellisTestRuntime | undefined;
  try {
    nats = await startTrellisRuntime();
    const nc = await nats.connectNats();
    const kv = (await TypedKV.open(
      fixedTransportProvider(nc),
      "missing_after_open",
      current,
    ))
      .orThrow();
    await kv.put("present", { count: 1 }).orThrow();
    assertEquals(await Array.fromAsync((await kv.keys()).orThrow()), [
      "present",
    ]);

    await (await jetstreamManager(nc)).streams.delete(
      "KV_missing_after_open",
    );
    let failure: unknown;
    try {
      await Array.fromAsync((await kv.keys()).orThrow());
    } catch (error) {
      failure = error;
    }
    assertInstanceOf(failure, KVError);
    assertEquals(failure.operation, "keys");
    assertEquals(failure.message.includes("not found"), true);
  } finally {
    await nats?.stop();
    await Deno.remove(workdir, { recursive: true });
  }
});
