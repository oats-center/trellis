import assert from "node:assert/strict";
import { test } from "node:test";
import {
  decodeSubject,
  encodeEventSubjectParameterToken,
  escapeNats,
  template,
} from "../helpers.ts";

test("Helpers", async (t) => {
  await t.test("Subject Token Escaping", async (t) => {
    await t.test("Ensure basic characters are escaped", () => {
      assert.deepEqual(escapeNats("."), "~2E~");
      assert.deepEqual(escapeNats("*"), "~2A~");
      assert.deepEqual(escapeNats(">"), "~3E~");
      assert.deepEqual(escapeNats("~"), "~7E~");
      assert.deepEqual(escapeNats(" "), "~20~");
      assert.deepEqual(escapeNats("\t"), "~9~");
      assert.deepEqual(escapeNats("\n"), "~A~");
      assert.deepEqual(escapeNats("\r"), "~D~");
      assert.deepEqual(escapeNats("\0"), "~0~");
    });

    await t.test("NATS subject escaping", async () => {
      assert.deepEqual(escapeNats("abc"), "abc");
      assert.deepEqual(escapeNats("ABC_xyz-123"), "ABC_xyz-123");
      assert.deepEqual(escapeNats(""), "_");
      assert.deepEqual(escapeNats("$SYS"), "_$SYS");
    });

    await t.test("Unicode should still work", () => {
      assert.deepEqual(escapeNats("hi😀there"), "hi😀there");
    });

    await t.test("Tokens with periods should stay one token", () => {
      assert.deepEqual(escapeNats("my.subject*test"), "my~2E~subject~2A~test");
    });

    await t.test("Decode valid escape sequences only", () => {
      assert.deepEqual(
        decodeSubject("keep ~zz~ and ~1f~ and lone ~ABC but decode ~41~"),
        "keep ~zz~ and ~1f~ and lone ~ABC but decode A",
      );
    });

    await t.test("Round-trip escape works", () => {
      let input = "a";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "_";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = " ";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "-";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "A_B-C";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "with space";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "dots.are.separators";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "wild*card>chars.";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "mix £ € © ® ™";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "emoji 😀😇🤖";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "\u0000 start and end \u0000";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "~leading and ~tricky~";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
      input = "$startsWithDollar";
      assert.deepEqual(decodeSubject(escapeNats(input)), input);
    });
  });

  await t.test("Subject templates preserve canonical scalar values", () => {
    for (const [input, expected] of [
      ["", ""],
      ["$SYS", "JFNZUw"],
      ["a.b", "YS5i"],
      ["a b", "YSBi"],
      ["*", "Kg"],
      [">", "Pg"],
      ["~", "fg"],
      ["😀", "8J-YgA"],
    ]) {
      assert.deepEqual(encodeEventSubjectParameterToken(input), expected);
    }
    assert.deepEqual(
      template("events.v1.Test.{/id}.{/attempt}", { id: "a.b", attempt: 0 }),
      "events.v1.Test.YS5i.MA",
    );
    assert.deepEqual(
      template("events.v1.Test.{/id}", { id: "😀" }),
      "events.v1.Test.8J-YgA",
    );
    assert.deepEqual(
      template("events.v1.Test.{/tenant}.{/id}", {}, { allowWildcards: true }),
      "events.v1.Test.*.*",
    );
    assert.throws(
      () =>
        template("events.v1.Test.{/value}", { value: 9_007_199_254_740_992 }),
      Error,
      "safe integers",
    );
  });
});
