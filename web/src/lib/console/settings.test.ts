import assert from "node:assert/strict";
import { test } from "node:test";
import {
  fromLocalInput,
  hasProblem,
  problemOf,
  toJson,
  toLocalInput,
} from "./settings.ts";

// A 64-bit integer travels as a decimal string with its bounds as strings,
// so the form compares it exactly instead of through a lossy number.
test("a 64-bit setting is checked against its bounds without rounding", () => {
  const schema = {
    type: "string",
    pattern: "^(0|-?[1-9][0-9]*)$",
    "x-trellis-integer": "int64",
    "x-trellis-minimum": "1",
    "x-trellis-maximum": "9007199254740992",
  };
  assert.equal(problemOf(schema, {}, "9007199254740992"), null);
  assert.equal(
    problemOf(schema, {}, "9007199254740993"),
    "Must be at most 9007199254740992.",
  );
  assert.equal(problemOf(schema, {}, "0"), "Must be at least 1.");
  assert.equal(problemOf(schema, {}, "1.5"), "Enter a whole number.");
});

// The picker shows local time; what is saved is the same instant in UTC.
test("a timestamp entered in local time is saved as the same instant in UTC", () => {
  const zone = process.env.TZ;
  process.env.TZ = "America/Chicago";
  try {
    assert.equal(toLocalInput("2026-01-15T08:30:15Z"), "2026-01-15T02:30:15");
    assert.equal(fromLocalInput("2026-01-15T02:30:15"), "2026-01-15T08:30:15Z");
    assert.equal(
      fromLocalInput(toLocalInput("2026-11-21T23:59:59Z")),
      "2026-11-21T23:59:59Z",
    );
    assert.equal(fromLocalInput(""), undefined);
  } finally {
    if (zone === undefined) delete process.env.TZ;
    else process.env.TZ = zone;
  }
});

// A model in a list: a missing required field blocks the save, and an
// optional field left out is absent from what is stored, not null.
test("a list of models is saved only once each model's required fields are set", () => {
  const defs = {
    "fixture.Upstream": {
      type: "object",
      properties: {
        url: { type: "string", minLength: 1 },
        team: { type: "string" },
      },
      required: ["url"],
      additionalProperties: true,
    },
  };
  const schema = {
    type: "array",
    items: { $ref: "#/$defs/fixture.Upstream" },
    maxItems: 2,
  };
  assert.equal(hasProblem(schema, defs, [{ team: "core" }]), true);
  assert.equal(
    hasProblem(schema, defs, [{ url: "a" }, { url: "b" }, { url: "c" }]),
    true,
  );
  const draft = [{ url: "https://up.example", team: undefined }];
  assert.equal(hasProblem(schema, defs, draft), false);
  assert.deepEqual(toJson(draft), [{ url: "https://up.example" }]);
});
