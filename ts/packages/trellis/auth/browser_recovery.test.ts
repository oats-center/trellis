import assert from "node:assert/strict";
import { test } from "node:test";

import {
  type BrowserAuthRecoveryClassification,
  classifyBrowserAuthError,
  isRecoverableBrowserAuthError,
} from "./browser.ts";
import { TransportError } from "../errors/index.ts";
import { decodeTrellisHttpError } from "./http_error.ts";

test("OAuth consent requirements remain explicit rather than silently restarting sign-in", async () => {
  const error = await decodeTrellisHttpError(
    Response.json({ error: { code: "consent_required" } }, { status: 403 }),
  );
  assertClassification(error, { kind: "consent_required", recoverable: false });
});

function assertClassification(
  error: unknown,
  expected: Pick<BrowserAuthRecoveryClassification, "kind" | "recoverable">,
): void {
  const classification = classifyBrowserAuthError(error);
  assert.deepEqual(classification.kind, expected.kind);
  assert.deepEqual(classification.recoverable, expected.recoverable);
  assert.deepEqual(isRecoverableBrowserAuthError(error), expected.recoverable);
}

test("classifyBrowserAuthError treats missing session reason as stale session recovery", () => {
  assertClassification(
    new TransportError({
      code: "session_not_found",
      message: "Trellis could not prepare the client session.",
      hint: "Retry the connection.",
      context: { reason: "session_not_found" },
    }),
    { kind: "recoverable_stale_session", recoverable: true },
  );
});

test("classifyBrowserAuthError treats expired session reason as stale session recovery", () => {
  assertClassification(
    { code: "session_expired" },
    { kind: "recoverable_stale_session", recoverable: true },
  );
});

test("classifyBrowserAuthError does not recover explicit approval denial", () => {
  assertClassification(
    new TransportError({
      code: "approval_denied",
      message: "Trellis sign-in did not complete.",
      hint: "Start sign-in again if you want to approve access.",
      context: { reason: "approval_denied" },
    }),
    { kind: "policy_denied", recoverable: false },
  );
});

test("classifyBrowserAuthError does not silently recover insufficient capabilities", () => {
  assertClassification(
    new TransportError({
      code: "insufficient_capabilities",
      message: "The signed-in Trellis account lacks required capabilities.",
      hint: "Ask an administrator to grant the missing capabilities.",
      context: { missingCapabilities: ["workspace.read"] },
    }),
    { kind: "insufficient_capabilities", recoverable: false },
  );
});

test("classifyBrowserAuthError treats bootstrap not_ready as runtime unavailable", () => {
  assertClassification(
    new TransportError({
      code: "not_ready",
      message: "Trellis is not ready to connect this client.",
      hint: "Wait for app access to become available.",
      context: { reason: "insufficient_permissions" },
    }),
    { kind: "runtime_unavailable", recoverable: false },
  );
});

test("classifyBrowserAuthError does not infer machine state from messages", () => {
  assertClassification(
    new Error("wrapper", {
      cause: {
        code: "trellis.bootstrap.failed",
        context: { causeMessage: "Session expired while reconnecting." },
      },
    }),
    { kind: "unknown", recoverable: false },
  );
});
