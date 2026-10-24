import assert from "node:assert/strict";
import { test } from "node:test";
import {
  decodeTrellisHttpError,
  INVALID_HTTP_ERROR_ENVELOPE,
} from "./http_error.ts";

test("Trellis HTTP errors preserve exact status and machine code", async () => {
  const error = await decodeTrellisHttpError(
    new Response('{"error":{"code":"flow_expired"}}', { status: 410 }),
  );
  assert.deepEqual(error.status, 410);
  assert.deepEqual(error.code, "flow_expired");
});

test("Trellis HTTP error decoder tolerates unknown envelope members", async () => {
  const error = await decodeTrellisHttpError(
    new Response(
      '{"error":{"code":"session_revoked","futureDetail":{"retryable":false}},"futureTopLevel":true}',
      { status: 401 },
    ),
  );
  assert.deepEqual(error.status, 401);
  assert.deepEqual(error.code, "session_revoked");
  assert.deepEqual(
    (
      await decodeTrellisHttpError(
        new Response('{"error":{"code":""}}', { status: 401 }),
      )
    ).code,
    INVALID_HTTP_ERROR_ENVELOPE,
  );
});
