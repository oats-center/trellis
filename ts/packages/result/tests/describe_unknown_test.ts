import assert from "node:assert/strict";
import { test } from "node:test";
import { describeUnknown, UnexpectedError } from "@oatscenter/result";

test("describeUnknown prints values JSON cannot, without throwing", () => {
  const cyclic: Record<string, unknown> = { name: "loop" };
  cyclic.self = cyclic;
  assert.equal(describeUnknown(cyclic), '{"name":"loop","self":"[Circular]"}');
  assert.equal(describeUnknown({ size: 10n }), '{"size":"10n"}');

  const getter = {
    get broken(): never {
      throw new Error("getter failed");
    },
  };
  assert.equal(describeUnknown(getter), "[object Object]");

  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error("trap");
      },
      ownKeys() {
        throw new Error("trap");
      },
    },
  );
  assert.equal(describeUnknown(hostile), "[unprintable cause]");
});

test("describeUnknown bounds depth and length", () => {
  let nested: Record<string, unknown> = {};
  const root = nested;
  for (let level = 0; level < 20; level++) {
    const next: Record<string, unknown> = {};
    nested.next = next;
    nested = next;
  }
  const text = describeUnknown(root);
  assert(text.includes('"[Object]"'), text);
  assert(text.length < 200, text);

  const long = describeUnknown("x".repeat(100_000));
  assert.equal(long.length, 4097);
  assert(long.endsWith("…"));
});

test("an UnexpectedError wraps a cyclic cause chain and an unprintable cause", () => {
  const first = new Error("first");
  const second = new Error("second", { cause: first });
  (first as { cause?: unknown }).cause = second;
  const looped = new UnexpectedError({ cause: first });
  assert.equal(typeof looped.getContext().causeMessage, "string");

  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error("trap");
      },
    },
  );
  const wrapped = new UnexpectedError({
    cause: new Error("outer", { cause: hostile }),
  });
  assert.equal(wrapped.getContext().causeMessage, "[unprintable cause]");
});
