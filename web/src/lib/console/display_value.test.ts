import { deepEqual, equal, throws } from "node:assert/strict";

import {
  CIRCULAR_MARKER,
  decodeKnownJsonBytes,
  displayJson,
  formatTimestamp,
  normalizeTimestamp,
  projectConsoleError,
} from "./display_value.ts";

Deno.test("U08 displayJson renders nested bigint exactly", () => {
  const value = { revision: 9007199254740993n, nested: { count: 42n } };
  deepEqual(JSON.parse(displayJson(value)), {
    revision: "9007199254740993",
    nested: { count: "42" },
  });
});

Deno.test("U08 displayJson renders typed bytes and handles cycles", () => {
  const bytes = new Uint8Array([1, 2, 3]);
  deepEqual(JSON.parse(displayJson({ payload: bytes })), {
    payload: { "$bytes": "AQID", length: 3 },
  });

  const cyclic: Record<string, unknown> = { name: "root" };
  cyclic.self = cyclic;
  const rendered = JSON.parse(displayJson(cyclic));
  equal(rendered.self, CIRCULAR_MARKER);
});

Deno.test("U08 displayJson tolerates repeated noncyclic references", () => {
  const shared = { value: 1 };
  const rendered = JSON.parse(displayJson({ left: shared, right: shared }));
  deepEqual(rendered.left, { value: 1 });
  deepEqual(rendered.right, { value: 1 });
});

Deno.test("U08 displayJson renders null, undefined, and non-finite numbers", () => {
  deepEqual(JSON.parse(displayJson({ a: null, b: undefined, c: Infinity })), {
    a: null,
    b: null,
    c: "[Infinity]",
  });
  throws(() => JSON.stringify({ value: 1n }));
});

Deno.test("U09 timestamp normalization handles zero, decimals, ISO, and limits", () => {
  deepEqual(normalizeTimestamp(0), {
    kind: "valid",
    date: new Date(0),
    milliseconds: 0n,
  });
  deepEqual(normalizeTimestamp(0n), {
    kind: "valid",
    date: new Date(0),
    milliseconds: 0n,
  });
  deepEqual(normalizeTimestamp("1700000000000"), {
    kind: "valid",
    date: new Date(1_700_000_000_000),
    milliseconds: 1_700_000_000_000n,
  });
  equal(normalizeTimestamp("2024-01-01T00:00:00.000Z").kind, "valid");
  deepEqual(normalizeTimestamp(null), { kind: "absent" });
  deepEqual(normalizeTimestamp(undefined), { kind: "absent" });
  deepEqual(normalizeTimestamp(""), { kind: "absent" });
  equal(normalizeTimestamp("not-a-date").kind, "invalid");
  equal(normalizeTimestamp(Number.NaN).kind, "invalid");
  equal(normalizeTimestamp(Number.POSITIVE_INFINITY).kind, "invalid");
  equal(normalizeTimestamp(9_000_000_000_000_000n).kind, "invalid");
  equal(
    formatTimestamp(0, (date) => date.toISOString()),
    "1970-01-01T00:00:00.000Z",
  );
  equal(formatTimestamp(null), "-");
  equal(formatTimestamp("garbage").startsWith("Invalid timestamp"), true);
});

Deno.test("U10 known JSON-byte decoder distinguishes malformed inputs", () => {
  const valid = new TextEncoder().encode(JSON.stringify({ kind: "kv" }));
  deepEqual(decodeKnownJsonBytes(valid), { ok: true, value: { kind: "kv" } });

  const invalidUtf8 = decodeKnownJsonBytes(new Uint8Array([0xff, 0xfe]));
  equal(invalidUtf8.ok, false);
  if (!invalidUtf8.ok) equal(invalidUtf8.error.includes("UTF-8"), true);

  const invalidJson = decodeKnownJsonBytes(new TextEncoder().encode("{"));
  equal(invalidJson.ok, false);
  if (!invalidJson.ok) equal(invalidJson.error.includes("JSON"), true);

  const unknownEnum = new TextEncoder().encode(
    JSON.stringify({ kind: "futureProvider" }),
  );
  deepEqual(decodeKnownJsonBytes(unknownEnum), {
    ok: true,
    value: { kind: "futureProvider" },
  });

  equal(decodeKnownJsonBytes("not-bytes").ok, false);
});

Deno.test("U12 error projection keeps code and id without private details", () => {
  const generated = projectConsoleError({
    id: "err_123",
    message: "The request is invalid.",
    data: { code: "invalid_request", field: null, retryable: false },
  });
  deepEqual(generated, {
    message: "The request is invalid.",
    code: "invalid_request",
    field: null,
    retryable: false,
    id: "err_123",
  });

  const remote = projectConsoleError({
    remoteError: {
      type: "trellis.auth@v1::AuthError",
      message: "Approval required.",
      code: "approval_required",
      id: "err_456",
      consentRequest: { participantId: "p" },
    },
  });
  equal(remote.code, "approval_required");
  equal(remote.id, "err_456");
  deepEqual(remote.consentRequest, { participantId: "p" });

  deepEqual(projectConsoleError(new Error("boom")), { message: "boom" });
  deepEqual(projectConsoleError(undefined), { message: "Unexpected error" });
  deepEqual(projectConsoleError({}), { message: "Unexpected error" });
});

Deno.test("U12 error projection unwraps Result-shaped errors", () => {
  const projected = projectConsoleError({
    error: {
      name: "AuthError",
      message: "The requested authentication record was not found.",
      id: "err_1",
      data: { code: "not_found", field: null, retryable: false, id: "err_1" },
    },
  });
  deepEqual(projected, {
    message: "The requested authentication record was not found.",
    code: "not_found",
    field: null,
    retryable: false,
    id: "err_1",
  });
});

Deno.test("U12 error projection reads serialized and context shapes", () => {
  const serialized = projectConsoleError({
    toSerializable: () => ({
      id: "err_789",
      message: "Conflict.",
      context: { reason: "revision_conflict" },
    }),
  });
  equal(serialized.code, "revision_conflict");
  equal(serialized.id, "err_789");

  const context = projectConsoleError({
    message: "fallback",
    getContext: () => ({ message: "from context", reason: "not_found" }),
  });
  equal(context.message, "from context");
  equal(context.code, "not_found");
});
