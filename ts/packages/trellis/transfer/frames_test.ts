import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { test } from "node:test";
import { frames } from "./frames.ts";

test("ready frames coalesce available chunks, preserve slow delivery, and bound paused prefetch", async () => {
  const bulk = Promise.withResolvers<void>();
  const ready = Promise.withResolvers<void>();
  const data = Uint8Array.from({ length: 41 }, (_, index) => index);
  const source = (async function* () {
    yield data.subarray(0, 4);
    await bulk.promise;
    for (let offset = 4; offset < data.length; offset += 4) {
      yield data.subarray(offset, offset + 4);
      if (offset === 8) ready.resolve();
    }
  })();
  const iterator = frames(source, 16, new AbortController().signal, "upload");
  const first = await iterator.next();
  assert.equal(first.done, false);
  assert.deepEqual(
    first.value,
    data.subarray(0, 4),
    "send the available prefix before the producer releases its next chunk",
  );
  bulk.resolve();
  await ready.promise;
  const second = await iterator.next();
  assert.equal(second.done, false);
  assert(
    second.value.length > 4 && second.value.length <= 16,
    "combine already queued chunks within the negotiated limit",
  );
  const actual = [...first.value, ...second.value];
  for await (const frame of iterator) {
    assert(frame.length > 0 && frame.length <= 16);
    actual.push(...frame);
  }
  assert.deepEqual(Uint8Array.from(actual), data);

  const advance = Promise.withResolvers<void>();
  const advanced = Promise.withResolvers<void>();
  const scratch = Buffer.alloc(4);
  const reusing = frames(
    (async function* () {
      yield scratch.fill(7);
      await advance.promise;
      yield scratch.fill(9);
      advanced.resolve();
    })(),
    16,
    new AbortController().signal,
    "upload",
  );
  const borrowed = await reusing.next();
  advance.resolve();
  await advanced.promise;
  assert.deepEqual(
    borrowed.value,
    new Uint8Array(4).fill(7),
    "prefetch must not let an iterator overwrite an in-flight frame",
  );
  assert.deepEqual((await reusing.next()).value, new Uint8Array(4).fill(9));
  assert.equal((await reusing.next()).done, true);

  let producedBytes = 0;
  const paused = frames(
    (async function* () {
      for (let index = 0; index < 200; index++) {
        producedBytes += 1024;
        yield new Uint8Array(1024).fill(index);
      }
    })(),
    4096,
    new AbortController().signal,
    "download",
  );
  const held = await paused.next();
  assert.equal(held.done, false);
  let independentBytes = 0;
  for await (const frame of frames(
    new Uint8Array(16387),
    4096,
    new AbortController().signal,
    "upload",
  )) {
    independentBytes += frame.length;
  }
  assert.equal(independentBytes, 16387);
  assert(
    producedBytes <= held.value.length + 4096 + 1024,
    "a paused consumer must not drain its source while another transfer runs",
  );
  await paused.return(undefined);
});

test("aborting a pending frame cancels its real Web Stream source", async () => {
  const pending = Promise.withResolvers<void>();
  let cancelled = false;
  const source = new ReadableStream<Uint8Array>({
    async pull() {
      await pending.promise;
    },
    cancel() {
      cancelled = true;
    },
  });
  const abort = new AbortController();
  const iterator = frames(source, 4096, abort.signal, "upload");
  const reading = iterator.next();
  const reason = new Error("stop pending source");
  abort.abort(reason);
  await assert.rejects(reading, (error) => error === reason);
  assert(cancelled);
  pending.resolve();
});

test("a prefetched source failure cannot become successful EOF", async () => {
  const release = Promise.withResolvers<void>();
  const failure = new Error("source interrupted after its prefix");
  const source = (async function* () {
    yield new Uint8Array([1, 2, 3]);
    await release.promise;
    throw failure;
  })();
  const iterator = frames(source, 4096, new AbortController().signal, "upload");
  assert.deepEqual((await iterator.next()).value, new Uint8Array([1, 2, 3]));
  release.resolve();
  await assert.rejects(iterator.next(), (error) => error === failure);
});
