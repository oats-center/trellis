import { assertEquals, assertThrows } from "@std/assert";
import {
  decodeSubject,
  encodeEventSubjectParameterToken,
  escapeNats,
  template,
} from "../helpers.ts";

Deno.test("Helpers", async (t) => {
  await t.step("Subject Token Escaping", async (t) => {
    await t.step("Ensure basic characters are escaped", () => {
      assertEquals(escapeNats("."), "~2E~");
      assertEquals(escapeNats("*"), "~2A~");
      assertEquals(escapeNats(">"), "~3E~");
      assertEquals(escapeNats("~"), "~7E~");
      assertEquals(escapeNats(" "), "~20~");
      assertEquals(escapeNats("\t"), "~9~");
      assertEquals(escapeNats("\n"), "~A~");
      assertEquals(escapeNats("\r"), "~D~");
      assertEquals(escapeNats("\0"), "~0~");
    });

    await t.step("NATS subject escaping", async () => {
      assertEquals(escapeNats("abc"), "abc");
      assertEquals(escapeNats("ABC_xyz-123"), "ABC_xyz-123");
      assertEquals(escapeNats(""), "_");
      assertEquals(escapeNats("$SYS"), "_$SYS");
    });

    await t.step("Unicode should still work", () => {
      assertEquals(escapeNats("hi😀there"), "hi😀there");
    });

    await t.step("Tokens with periods should stay one token", () => {
      assertEquals(escapeNats("my.subject*test"), "my~2E~subject~2A~test");
    });

    await t.step("Decode valid escape sequences only", () => {
      assertEquals(
        decodeSubject("keep ~zz~ and ~1f~ and lone ~ABC but decode ~41~"),
        "keep ~zz~ and ~1f~ and lone ~ABC but decode A",
      );
    });

    await t.step("Round-trip escape works", () => {
      let input = "a";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "_";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = " ";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "-";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "A_B-C";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "with space";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "dots.are.separators";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "wild*card>chars.";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "mix £ € © ® ™";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "emoji 😀😇🤖";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "\u0000 start and end \u0000";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "~leading and ~tricky~";
      assertEquals(decodeSubject(escapeNats(input)), input);
      input = "$startsWithDollar";
      assertEquals(decodeSubject(escapeNats(input)), input);
    });
  });

  await t.step("Subject templates preserve canonical scalar values", () => {
    for (
      const [input, expected] of [
        ["", ""],
        ["$SYS", "JFNZUw"],
        ["a.b", "YS5i"],
        ["a b", "YSBi"],
        ["*", "Kg"],
        [">", "Pg"],
        ["~", "fg"],
        ["😀", "8J-YgA"],
      ]
    ) {
      assertEquals(encodeEventSubjectParameterToken(input), expected);
    }
    assertEquals(
      template("events.v1.Test.{/id}.{/attempt}", { id: "a.b", attempt: 0 }),
      "events.v1.Test.YS5i.MA",
    );
    assertEquals(
      template("events.v1.Test.{/id}", { id: "😀" }),
      "events.v1.Test.8J-YgA",
    );
    assertEquals(
      template(
        "events.v1.Test.{/tenant}.{/id}",
        {},
        { allowWildcards: true },
      ),
      "events.v1.Test.*.*",
    );
    assertThrows(
      () =>
        template("events.v1.Test.{/value}", { value: 9_007_199_254_740_992 }),
      Error,
      "safe integers",
    );
  });
});
