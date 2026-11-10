import { equal } from "node:assert/strict";
import { test } from "node:test";

import { compactDuration, errorMessage } from "./format.ts";

test("compactDuration preserves millisecond resolution", () => {
  equal(compactDuration(0), "0ms");
  equal(compactDuration(243.4), "243ms");
  equal(compactDuration(2_184), "2.184s");
  equal(compactDuration(580_132), "9m 40.132s");
  equal(compactDuration(3_723_006), "1h 2m 3.006s");
});

test("errorMessage gives a service's unexpected failure its error ID", () => {
  const wire = {
    id: "rust-server-error-1-2",
    type: "UnexpectedError",
    message: "An unexpected error has occurred",
    context: { requestId: "request-1" },
  };
  const expected =
    "Something went wrong on the server. Look for error ID rust-server-error-1-2 in the service's log.";
  equal(errorMessage({ error: { remoteError: wire } }), expected);
  equal(errorMessage({ remoteError: wire }), expected);
});

test("errorMessage keeps the cause of an error raised in this browser", () => {
  equal(
    errorMessage({
      id: "local-1",
      type: "UnexpectedError",
      message: "An unexpected error has occurred",
      context: { causeMessage: "Could not reach Trellis" },
      getContext: () => ({ causeMessage: "Could not reach Trellis" }),
    }),
    "Could not reach Trellis",
  );
});

test("errorMessage prefers explicit server messages", () => {
  equal(
    errorMessage({
      getContext: () => ({ message: "Current password is incorrect." }),
      message: "Auth failed: invalid_request",
      reason: "invalid_request",
    }),
    "Current password is incorrect.",
  );
});

test("errorMessage prefers validation issues over generic invalid_request copy", () => {
  equal(
    errorMessage({
      getContext: () => ({ reason: "invalid_request" }),
      error: {
        remoteError: {
          issues: [{ path: "#/properties/limit", message: "must be <= 100" }],
        },
      },
    }),
    "#/properties/limit: must be <= 100",
  );
});

test("errorMessage renders auth reasons as actionable copy", () => {
  equal(
    errorMessage({ reason: "insufficient_permissions" }),
    "This Console session is missing permission for that action. Sign out and connect the Console again to accept the updated access.",
  );
  equal(
    errorMessage({ error: { remoteError: { reason: "session_not_found" } } }),
    "Your session has expired. Sign in again.",
  );
  equal(
    errorMessage({ error: { remoteError: { reason: "username_taken" } } }),
    "That username is already in use.",
  );
});
