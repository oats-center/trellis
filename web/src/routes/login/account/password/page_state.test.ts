import { assertEquals, assertRejects } from "@std/assert";

import {
  completeAccountFlowLocalPassword,
  defaultProfileValue,
  formatAccountFlowError,
  hasLocalProvider,
  isExpectedPasswordFlow,
  parseAccountFlowState,
  passwordFlowAction,
  passwordFlowTitle,
  passwordPolicyError,
} from "./page_state.ts";

function jsonResponse(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    ...init,
  });
}

Deno.test("password adapts reset copy", () => {
  assertEquals(
    passwordFlowTitle("password_reset"),
    "Reset your password",
  );
  assertEquals(passwordFlowAction("password_reset"), "Reset password");
  assertEquals(passwordFlowTitle("other"), "Set local credentials");
  assertEquals(passwordFlowAction("other"), "Save credentials");
});

Deno.test("password recognizes reset flows", () => {
  const state = parseAccountFlowState({
    status: "active",
    flowId: "flow-1",
    kind: "password_reset",
    targetUserId: "usr_1",
    allowedProviders: ["local"],
    profileHint: { name: "Hint Name", email: "hint@example.com" },
    expiresAt: "2099-01-01T00:00:00.000Z",
    passwordPolicy: { minLength: 12 },
    providers: [{ id: "local", displayName: "Username and password" }],
    target: { userId: "usr_1", email: "target@example.com", active: true },
  });

  assertEquals(state.status, "active");
  if (state.status === "active") {
    assertEquals(isExpectedPasswordFlow(state), true);
    assertEquals(hasLocalProvider(state), true);
    assertEquals(defaultProfileValue(state, "name"), "Hint Name");
    assertEquals(defaultProfileValue(state, "email"), "target@example.com");
    assertEquals(
      passwordPolicyError(state, "short"),
      "Password must be at least 12 characters.",
    );
    assertEquals(passwordPolicyError(state, "long-enough!"), null);
  }
});

Deno.test("password completion omits username for reset flows", async () => {
  let submitted: Record<string, unknown> | undefined;
  const result = await completeAccountFlowLocalPassword(
    "http://trellis.example",
    "flow-1",
    { password: "secret", name: "", email: "" },
    async (_input, init) => {
      submitted = JSON.parse(String(init?.body));
      return jsonResponse({ status: "created", userId: "usr_1" });
    },
  );

  assertEquals(result, { status: "created", userId: "usr_1" });
  assertEquals(submitted, { password: "secret" });
});

Deno.test("password maps account-flow password policy errors", async () => {
  assertEquals(
    formatAccountFlowError(400, "local_password_too_short"),
    "Choose a longer password.",
  );

  await assertRejects(
    () =>
      completeAccountFlowLocalPassword(
        "http://trellis.example",
        "flow-1",
        { password: "short", name: "", email: "" },
        () =>
          Promise.resolve(
            jsonResponse(
              { error: { code: "local_password_too_short" } },
              { status: 400 },
            ),
          ),
      ),
    Error,
    "Choose a longer password.",
  );
});
