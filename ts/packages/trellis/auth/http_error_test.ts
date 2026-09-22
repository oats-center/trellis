import { assertEquals } from "@std/assert";
import {
  decodeTrellisHttpError,
  INVALID_HTTP_ERROR_ENVELOPE,
} from "./http_error.ts";

Deno.test("Trellis HTTP errors preserve exact status and machine code", async () => {
  const error = await decodeTrellisHttpError(
    new Response('{"error":{"code":"flow_expired"}}', { status: 410 }),
  );
  assertEquals(error.status, 410);
  assertEquals(error.code, "flow_expired");
});

Deno.test("Trellis HTTP error decoder tolerates unknown envelope members", async () => {
  const error = await decodeTrellisHttpError(
    new Response(
      '{"error":{"code":"session_revoked","futureDetail":{"retryable":false}},"futureTopLevel":true}',
      { status: 401 },
    ),
  );
  assertEquals(error.status, 401);
  assertEquals(error.code, "session_revoked");
  assertEquals(
    (await decodeTrellisHttpError(
      new Response('{"error":{"code":""}}', { status: 401 }),
    )).code,
    INVALID_HTTP_ERROR_ENVELOPE,
  );
});
