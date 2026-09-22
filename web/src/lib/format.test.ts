import { equal } from "node:assert/strict";

import { compactDuration, errorMessage } from "./format.ts";

declare const Deno: {
  test(name: string, fn: () => void): void;
};

Deno.test("compactDuration preserves millisecond resolution", () => {
  equal(compactDuration(0), "0ms");
  equal(compactDuration(243.4), "243ms");
  equal(compactDuration(2_184), "2.184s");
  equal(compactDuration(580_132), "9m 40.132s");
  equal(compactDuration(3_723_006), "1h 2m 3.006s");
});

Deno.test("errorMessage prefers explicit server messages", () => {
  equal(
    errorMessage({
      getContext: () => ({ message: "Current password is incorrect." }),
      message: "Auth failed: invalid_request",
      reason: "invalid_request",
    }),
    "Current password is incorrect.",
  );
});

Deno.test("errorMessage prefers validation issues over generic invalid_request copy", () => {
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

Deno.test("errorMessage renders auth reasons as actionable copy", () => {
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
