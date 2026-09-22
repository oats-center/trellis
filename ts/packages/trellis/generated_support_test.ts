import { assertEquals, assertThrows } from "@std/assert";

import type { Codec, SerializableErrorData } from "./generated.ts";
import * as generated from "./generated.ts";

Deno.test("generated support exports only its runtime ABI values", () => {
  assertEquals(Object.keys(generated).sort(), [
    "TrellisError",
    "apiDescriptor",
    "codecs",
    "participantDescriptor",
  ]);
  const codec: Codec<string> = generated.codecs.string;
  const error: SerializableErrorData = {
    id: "error-id",
    type: "example.Failed",
    message: "failed",
  };
  assertEquals(codec.decode(error.message), "failed");
  assertEquals("number" in generated.codecs, false);
});

Deno.test("generated codecs preserve wire representations and composition", () => {
  type OrderId = string & { readonly __trellisType: "example.OrderId" };
  const orderId: Codec<OrderId> = generated.codecs.named(
    "example.OrderId",
    generated.codecs.string,
  );
  const id: OrderId = orderId.decode("order-1");
  assertEquals(orderId.encode(id), "order-1");
  assertThrows(() => orderId.decode(1));

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

  assertEquals(decoded.id as string, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
  assertEquals(decoded.count, 9223372036854775807n);
  assertEquals(model.encode(decoded), {
    id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    count: "9223372036854775807",
  });
  assertEquals(
    generated.codecs.bytes.encode(new Uint8Array([0, 1, 254, 255])),
    "AAH+/w==",
  );
  assertEquals(
    generated.codecs.bytes.decode("AAH+/w=="),
    new Uint8Array([0, 1, 254, 255]),
  );
  assertEquals(
    new TextDecoder().decode(
      generated.codecs.bytes.decode("InJlcXVpcmVkIg=="),
    ),
    '"required"',
  );
  assertEquals(
    generated.codecs.timestamp.decode(
      "2026-09-10T12:34:56.123456789Z",
    ) as string,
    "2026-09-10T12:34:56.123456789Z",
  );
  for (
    const [input, canonical] of [
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
    ]
  ) {
    assertEquals(generated.codecs.timestamp.decode(input), canonical);
    assertEquals(
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
      assertEquals(
        generated.codecs.timestamp.decode(
          `${base}.${String(millis).padStart(3, "0")}Z`,
        ),
        `${base}${fraction}Z`,
      );
    }
  }
  for (
    const invalid of [
      "2026-02-30T12:34:56Z",
      "2026-09-10T12:34:56.1234567890Z",
      "2026-09-10.12:34:56.1234567891Z",
      "2016-12-31T23:59:60Z",
      "2026-09-10T12:34:56+24:00",
      "2026-09-10T12:34:56+00:60",
    ]
  ) assertThrows(() => generated.codecs.timestamp.decode(invalid));
  assertThrows(() => generated.codecs.i64.decode(Number.MAX_SAFE_INTEGER));
  assertThrows(() => generated.codecs.bytes.decode("AAE"));
  assertThrows(() => generated.codecs.bytes.decode("AB=="));
  assertThrows(() => generated.codecs.i32.decode(-0));
  assertThrows(() => generated.codecs.f64.decode(Number.POSITIVE_INFINITY));
});

Deno.test("generated descriptors check evidence structure without deriving it", () => {
  const api = generated.apiDescriptor({
    identity: "example/Orders@v1",
    actions: {
      "rpc:Orders.Get": {
        kind: "rpc",
        descriptorName: "rpc:Orders.Get",
        input: generated.codecs.string,
        output: generated.codecs.string,
        errors: [],
        download: false,
        pagination: undefined,
      },
    },
  });
  assertEquals(api.identity, "example/Orders@v1");
  assertThrows(() =>
    generated.apiDescriptor({
      identity: "",
      actions: {},
    })
  );
  assertEquals(
    generated.participantDescriptor({
      kind: "service",
      id: "example.Processor",
      identity: "example.Processor",
      path: "Processor",
      actionNames: {},
      implements: [api],
      uses: [{
        api,
        actions: [{ descriptorName: "rpc:Orders.Get", direction: "call" }],
        optionalCapabilities: [],
      }],
      resources: {
        cache: {
          kind: "kv",
          availability: "optional",
          codec: generated.codecs.string,
          version: 2,
          migrations: { 1: generated.codecs.i32 },
        },
      },
      packageEvidence: {
        rootPackage: "example",
        rootDigest: "digest",
        packages: [{
          name: "example",
          version: "1.0.0",
          digest: "digest",
          source: 'package "example";\n',
        }],
      },
    }).kind,
    "service",
  );
});

Deno.test("device companion descriptors require one lexical App or Agent child", () => {
  const evidence = () => ({
    rootPackage: "example",
    rootDigest: "digest",
    packages: [{
      name: "example",
      version: "1.0.0",
      digest: "digest",
      source: 'package "example";\n',
    }],
  });
  const participant = (
    kind: "app" | "agent",
    identity: string,
    path: string,
  ) => ({
    kind,
    id: identity,
    identity,
    path,
    implements: [],
    uses: [],
    actionNames: {},
    resources: {},
    packageEvidence: evidence(),
  });
  const child = participant(
    "app",
    "example.Sensor.Companion",
    "Sensor.Companion",
  );
  assertEquals(
    generated.participantDescriptor({
      ...participant("agent", "example.Sensor", "Sensor"),
      kind: "device",
      companion: { participant: child, availability: "required" },
    }).companion?.participant.identity,
    child.identity,
  );
  assertThrows(() =>
    generated.participantDescriptor({
      ...participant("agent", "example.Sensor", "Sensor"),
      kind: "device",
      companion: {
        participant: participant("agent", "example.Sibling", "Sibling"),
        availability: "optional",
      },
    })
  );
  assertThrows(() =>
    generated.participantDescriptor({
      ...participant("agent", "example.Service", "Service"),
      kind: "service",
      companion: { participant: child, availability: "required" },
    })
  );
});
