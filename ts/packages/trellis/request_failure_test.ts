/**
 * T16 component proof: an ordinary RPC permission denial is surfaced as a
 * request-level error and never classified as a connection failure, so an
 * active Live session on the same connection is unaffected.
 */

import { assertEquals } from "@std/assert";

import { classifyRequestTransportFailure } from "./session.ts";

function permissionViolation(): Error {
  const error = new Error(
    "permissions violation for publish to rpc.v1.example",
  );
  error.name = "PermissionViolationError";
  return error;
}

Deno.test("T16 an RPC permission denial is a request error", () => {
  const error = classifyRequestTransportFailure({
    subject: "rpc.v1.example",
    cause: permissionViolation(),
  });
  assertEquals(error.code, "trellis.request.denied");
});

Deno.test("T16 a no-responders failure is unavailable, not denied", () => {
  const error = classifyRequestTransportFailure({
    subject: "rpc.v1.example",
    cause: new Error("no responders"),
  });
  assertEquals(error.code, "trellis.request.unavailable");
});

Deno.test("T16 a denial is never a connection-failure classification", () => {
  const error = classifyRequestTransportFailure({
    subject: "rpc.v1.example",
    cause: permissionViolation(),
  });
  assertEquals(
    error.code.startsWith("trellis.connection"),
    false,
    "a denial must not be treated as connection loss",
  );
});
