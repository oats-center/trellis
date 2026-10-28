import assert from "node:assert/strict";
import { test } from "node:test";
import { TransferIngress } from "./queue.ts";

test("transfer ingress hands whole frames to storage and releases bounded capacity", async () => {
  const credited: Array<[bigint, number]> = [];
  const ingress = new TransferIngress(4, 2, 6, (seq, bytes) =>
    credited.push([seq, bytes]),
  );
  ingress.push(1n, new Uint8Array([1, 2, 3, 4]));
  ingress.push(2n, new Uint8Array([5, 6]));
  assert.throws(() => ingress.push(3n, new Uint8Array([7])), Error);
  const reader = ingress[Symbol.asyncIterator]();
  assert.deepEqual(await reader.next(), {
    value: new Uint8Array([1, 2, 3, 4]),
    done: false,
  });
  assert.deepEqual(credited, [[1n, 4]]);
  ingress.push(3n, new Uint8Array([7, 8, 9, 10]));
  assert.throws(() => ingress.close(4n), Error);
  ingress.close(3n);
  const drained: number[] = [];
  while (true) {
    const next = await reader.next();
    if (next.done) break;
    drained.push(...next.value);
  }
  assert.deepEqual(drained, [5, 6, 7, 8, 9, 10]);
  assert.deepEqual(credited, [
    [1n, 4],
    [2n, 2],
    [3n, 4],
  ]);
});

test("transfer cancellation errors blocked storage reads instead of committing truncated EOF", async () => {
  const ingress = new TransferIngress(4, 2, 8, () => {});
  const reader = ingress[Symbol.asyncIterator]();
  const pending = reader.next();
  ingress.fail(new Error("cancelled"));
  await assert.rejects(
    () => pending,
    (error: unknown) =>
      error instanceof Error && error.message.includes("cancelled"),
  );
});

test("transfer ingress rejects sequence gaps and oversized frames before storage sees them", async () => {
  const ingress = new TransferIngress(4, 2, 8, () => {});
  assert.throws(() => ingress.push(2n, new Uint8Array([2])), Error);
  assert.throws(() => ingress.push(1n, new Uint8Array(5)), Error);
  ingress.push(1n, new Uint8Array([1]));
  ingress.close(1n);
  const received: number[] = [];
  for await (const frame of ingress) received.push(...frame);
  assert.deepEqual(received, [1]);
});
