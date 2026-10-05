import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { TransferIngress } from "./queue.ts";

Deno.test("transfer ingress hands whole frames to storage and releases bounded capacity", async () => {
  const credited: Array<[bigint, number]> = [];
  const ingress = new TransferIngress(
    4,
    2,
    6,
    (seq, bytes) => credited.push([seq, bytes]),
  );
  ingress.push(1n, new Uint8Array([1, 2, 3, 4]));
  ingress.push(2n, new Uint8Array([5, 6]));
  assertThrows(() => ingress.push(3n, new Uint8Array([7])));
  const reader = ingress[Symbol.asyncIterator]();
  assertEquals(await reader.next(), {
    value: new Uint8Array([1, 2, 3, 4]),
    done: false,
  });
  assertEquals(credited, [[1n, 4]]);
  ingress.push(3n, new Uint8Array([7, 8, 9, 10]));
  assertThrows(() => ingress.close(4n));
  ingress.close(3n);
  const drained: number[] = [];
  while (true) {
    const next = await reader.next();
    if (next.done) break;
    drained.push(...next.value);
  }
  assertEquals(drained, [5, 6, 7, 8, 9, 10]);
  assertEquals(credited, [[1n, 4], [2n, 2], [3n, 4]]);
});

Deno.test("transfer cancellation errors blocked storage reads instead of committing truncated EOF", async () => {
  const ingress = new TransferIngress(4, 2, 8, () => {});
  const reader = ingress[Symbol.asyncIterator]();
  const pending = reader.next();
  ingress.fail(new Error("cancelled"));
  await assertRejects(() => pending, Error, "cancelled");
});

Deno.test("transfer ingress rejects sequence gaps and oversized frames before storage sees them", async () => {
  const ingress = new TransferIngress(4, 2, 8, () => {});
  assertThrows(() => ingress.push(2n, new Uint8Array([2])));
  assertThrows(() => ingress.push(1n, new Uint8Array(5)));
  ingress.push(1n, new Uint8Array([1]));
  ingress.close(1n);
  const received: number[] = [];
  for await (const frame of ingress) received.push(...frame);
  assertEquals(received, [1]);
});
