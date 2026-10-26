import assert from "node:assert/strict";
import { test } from "node:test";

import { ValidationError } from "../errors/index.ts";
import { codecs } from "../generated.ts";
import { encodeRuntimeSchema, parseRuntimeSchema } from "./wire.ts";

// The codec a generated package emits for `type Name = string(min_length=1)`.
const input = codecs.model({
  name: codecs.bounded(codecs.string, { minCount: 1 }),
});

test("a payload outside its declared limits is a validation error on both sides", () => {
  // A provider decodes the request before its handler runs.
  const received = parseRuntimeSchema(input, { name: "" });
  assert(received.isErr(), "the handler is not given an empty name");
  assert(received.error instanceof ValidationError, String(received.error));
  assert.deepEqual(parseRuntimeSchema(input, { name: "ada" }).take(), {
    name: "ada",
  });

  // A caller refuses to send it.
  const sent = encodeRuntimeSchema(input, { name: "" });
  assert(sent.isErr(), "nothing is sent");
  assert(sent.error instanceof ValidationError, String(sent.error));
  assert.equal(
    encodeRuntimeSchema(input, { name: "ada" }).take(),
    '{"name":"ada"}',
  );
});
