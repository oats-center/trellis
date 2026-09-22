import { assertEquals } from "@std/assert";

import {
  accountFlowIdFromUrl,
  formatAccountFlowError,
  isExpectedInviteFlow,
  parseAccountFlowState,
} from "./page_state.ts";

Deno.test("invite accountFlowIdFromUrl reads non-empty flow id", () => {
  assertEquals(
    accountFlowIdFromUrl(
      new URL(
        "https://auth.example.com/login/admin/invite?flowId=flow-1",
      ),
    ),
    "flow-1",
  );
});

Deno.test("invite accountFlowIdFromUrl rejects missing and blank flow id", () => {
  assertEquals(
    accountFlowIdFromUrl(
      new URL("https://auth.example.com/login/admin/invite"),
    ),
    null,
  );
  assertEquals(
    accountFlowIdFromUrl(
      new URL(
        "https://auth.example.com/login/admin/invite?flowId=%20",
      ),
    ),
    null,
  );
});

Deno.test("invite route no longer accepts active account-flow state", () => {
  for (const kind of ["identity_link", "local_password_reset"] as const) {
    const state = parseAccountFlowState({
      status: "active",
      flowId: "flow-1",
      kind,
      targetUserId: "usr_1",
      allowedProviders: ["local", "github"],
      profileHint: { name: "Ada" },
      expiresAt: "2099-01-01T00:00:00.000Z",
      providers: [{ id: "local", displayName: "Username and password" }],
      target: { userId: "usr_1", name: "Ada", active: true },
    });

    assertEquals(state.status, "active");
    if (state.status === "active") {
      assertEquals(isExpectedInviteFlow(state), false);
      assertEquals(state.target?.name, "Ada");
    }
  }
});

Deno.test("invite maps known account-flow errors", () => {
  assertEquals(
    formatAccountFlowError(409, "local_identity_exists"),
    "That username is already in use. Choose a different username.",
  );
  assertEquals(
    formatAccountFlowError(403, "flow_missing_target_user"),
    "This account request is missing its target account.",
  );
});
