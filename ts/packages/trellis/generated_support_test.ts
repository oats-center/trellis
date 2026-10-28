import assert from "node:assert/strict";
import { test } from "node:test";

import type { Codec } from "./generated.ts";
import * as generated from "./generated.ts";

test("generated codecs preserve wire representations and composition", () => {
  type OrderId = string & { readonly __trellisType: "example.OrderId" };
  const orderId: Codec<OrderId> = generated.codecs.named(
    "example.OrderId",
    generated.codecs.string,
  );
  const id: OrderId = orderId.decode("order-1");
  assert.deepEqual(orderId.encode(id), "order-1");
  assert.throws(() => orderId.decode(1), Error);

  const model = generated.codecs.model({
    id: generated.codecs.ulid,
    count: generated.codecs.i64,
    note: generated.codecs.optional(
      generated.codecs.nullable(generated.codecs.string),
    ),
  });
  const decoded = model.decode({
    id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    count: "9223372036854775807",
    future: true,
  });

  assert.deepEqual(decoded.id as string, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
  assert.deepEqual(decoded.count, 9223372036854775807n);
  assert.deepEqual((decoded as Record<string, unknown>).future, true);
  assert.deepEqual(model.encode(decoded), {
    id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    count: "9223372036854775807",
    future: true,
  });
  assert.deepEqual(
    generated.codecs.bytes.encode(new Uint8Array([0, 1, 254, 255])),
    "AAH+/w==",
  );
  assert.deepEqual(
    generated.codecs.bytes.decode("AAH+/w=="),
    new Uint8Array([0, 1, 254, 255]),
  );
  assert.deepEqual(
    new TextDecoder().decode(generated.codecs.bytes.decode("InJlcXVpcmVkIg==")),
    '"required"',
  );
  assert.deepEqual(
    generated.codecs.timestamp.decode(
      "2026-09-10T12:34:56.123456789Z",
    ) as string,
    "2026-09-10T12:34:56.123456789Z",
  );
  for (const [input, canonical] of [
    ["2026-09-10T12:34:56.000Z", "2026-09-10T12:34:56Z"],
    ["2026-09-10T12:34:56.010Z", "2026-09-10T12:34:56.01Z"],
    ["2026-09-10T12:34:56.100Z", "2026-09-10T12:34:56.1Z"],
    ["2026-09-10T12:34:56.120Z", "2026-09-10T12:34:56.12Z"],
    ["2026-09-10T12:34:56.123Z", "2026-09-10T12:34:56.123Z"],
    ["2026-09-10T12:34:56.500Z", "2026-09-10T12:34:56.5Z"],
    ["2026-09-10T12:34:56.999Z", "2026-09-10T12:34:56.999Z"],
    ["2026-09-10T14:34:56.120+02:00", "2026-09-10T12:34:56.12Z"],
    ["2026-09-10T12:34:56.120+00:00", "2026-09-10T12:34:56.12Z"],
    ["2026-09-10t12:34:56.120z", "2026-09-10T12:34:56.12Z"],
    ["2026-09-10T12:34:56.123456789Z", "2026-09-10T12:34:56.123456789Z"],
  ]) {
    assert.deepEqual(generated.codecs.timestamp.decode(input), canonical);
    assert.deepEqual(
      generated.codecs.timestamp.encode(
        generated.codecs.timestamp.decode(input),
      ),
      canonical,
    );
  }
  for (let second = 0; second < 60; second++) {
    for (const millis of [0, 10, 100, 120, 123, 500, 999]) {
      const base = `2026-09-10T12:34:${String(second).padStart(2, "0")}`;
      const fraction = millis
        ? `.${String(millis).padStart(3, "0")}`.replace(/0+$/, "")
        : "";
      assert.deepEqual(
        generated.codecs.timestamp.decode(
          `${base}.${String(millis).padStart(3, "0")}Z`,
        ),
        `${base}${fraction}Z`,
      );
    }
  }
  for (const invalid of [
    "2026-02-30T12:34:56Z",
    "2026-09-10T12:34:56.1234567890Z",
    "2026-09-10.12:34:56.1234567891Z",
    "2016-12-31T23:59:60Z",
    "2026-09-10T12:34:56+24:00",
    "2026-09-10T12:34:56+00:60",
  ])
    assert.throws(() => generated.codecs.timestamp.decode(invalid), Error);
  assert.throws(
    () => generated.codecs.i64.decode(Number.MAX_SAFE_INTEGER),
    Error,
  );
  assert.throws(() => generated.codecs.bytes.decode("AAE"), Error);
  assert.throws(() => generated.codecs.bytes.decode("AB=="), Error);
  assert.throws(() => generated.codecs.i32.decode(-0), Error);
  assert.throws(
    () => generated.codecs.f64.decode(Number.POSITIVE_INFINITY),
    Error,
  );
});

test("the json codec keeps __proto__ as an ordinary key", () => {
  const wire = JSON.parse('{"__proto__":{"admin":true},"kept":1}');
  const decoded = generated.codecs.json.decode(wire);
  assert.equal(
    JSON.stringify(generated.codecs.json.encode(decoded)),
    JSON.stringify(wire),
  );
  assert.equal(Object.getPrototypeOf(decoded), Object.prototype);
});
