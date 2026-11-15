import assert from "node:assert/strict";
import { test } from "node:test";
import { signInRows } from "./sign_in_rows.ts";

const identity = (identityId: string, providerId: string, subject: string) => ({
  identityId,
  providerId,
  issuer: `https://${providerId}.example`,
  subject,
});

test("every identity linked through one provider gets its own row to disconnect, and the provider can connect another", () => {
  const first = identity("i1", "acme", "alice");
  const second = identity("i2", "acme", "alice-admin");
  const retired = identity("i3", "retired", "old");
  const rows = signInRows(
    [
      { providerId: "acme", title: "Acme" },
      { providerId: "other", title: "Other" },
    ],
    [first, retired, second],
  );
  assert.deepEqual(
    rows.map((row) => [
      row.title,
      row.providerId,
      row.identity?.subject,
      row.another ?? false,
    ]),
    [
      ["Acme", "acme", "alice", false],
      ["Acme", "acme", "alice-admin", false],
      ["Acme", "acme", undefined, true],
      ["Other", "other", undefined, false],
      ["https://retired.example", null, "old", false],
    ],
  );
  // Disconnecting the second row targets the second identity, not the first.
  assert.equal(rows[1]?.identity, second);
  assert.equal(new Set(rows.map((row) => row.key)).size, rows.length);
});
