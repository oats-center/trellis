import assert from "node:assert/strict";
import { test } from "node:test";
import { definitionsOf, descriptionOf, fieldsOf, typeLabel } from "./schema.ts";

// The shape `Apis.Get` returns for `rpc Users.Get`: a root reference into
// named definitions, with descriptions on the model and its fields.
const root = {
  $ref: "#/$defs/trellis.AuthUserGetResponse",
  $defs: {
    "trellis.AuthUserGetResponse": {
      type: "object",
      description: "One account.",
      properties: {
        user: { $ref: "#/$defs/trellis.AuthUser", description: "The person." },
        identities: {
          type: "array",
          items: { $ref: "#/$defs/trellis.AuthIdentity" },
          maxItems: 10,
        },
        note: {
          anyOf: [{ $ref: "#/$defs/trellis.AuthNote" }, { type: "null" }],
        },
      },
      required: ["user", "identities"],
    },
    "trellis.AuthUser": {
      type: "object",
      properties: {
        createdAt: { type: "string", format: "date-time" },
        state: { $ref: "#/$defs/trellis.AuthUserState" },
      },
      required: ["createdAt"],
    },
    "trellis.AuthUserState": {
      type: "string",
      "x-trellis-symbols": ["active", "disabled"],
    },
    "trellis.AuthIdentity": {
      type: "object",
      properties: {
        revision: { type: "string", "x-trellis-integer": "uint64" },
      },
      required: ["revision"],
    },
    "trellis.AuthNote": {
      type: "string",
      minLength: 1,
      maxLength: 200,
    },
  },
};

test("a response schema reads as named fields with readable types", () => {
  const defs = definitionsOf(root);
  assert.equal(descriptionOf(root, defs), "One account.");
  const fields = fieldsOf(root, defs);
  assert.deepEqual(
    fields?.map((field) => [
      field.name,
      field.required,
      typeLabel(field.schema, defs),
      field.description,
    ]),
    [
      ["user", true, "AuthUser", "The person."],
      ["identities", true, "list of AuthIdentity (up to 10 items)", undefined],
      ["note", false, "AuthNote or null", undefined],
    ],
  );
});

test("nested models open through references, lists and null", () => {
  const defs = definitionsOf(root);
  const [user, identities, note] = fieldsOf(root, defs) ?? [];
  assert.deepEqual(
    fieldsOf(user!.schema, defs)?.map((field) => [
      field.name,
      typeLabel(field.schema, defs),
    ]),
    [
      ["createdAt", "timestamp"],
      ["state", "AuthUserState"],
    ],
  );
  assert.deepEqual(
    fieldsOf(identities!.schema, defs)?.map((field) =>
      typeLabel(field.schema, defs),
    ),
    ["uint64"],
  );
  // A constrained string is a value, not a model.
  assert.equal(fieldsOf(note!.schema, defs), null);
  assert.equal(
    typeLabel(defs["trellis.AuthNote"]!, defs),
    "string (1–200 characters)",
  );
  assert.equal(
    typeLabel(defs["trellis.AuthUserState"]!, defs),
    "one of active, disabled",
  );
});
