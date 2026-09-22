import { assertEquals } from "@std/assert";

import { parseAccountFlowState } from "./account_flow_state.ts";

Deno.test("administrator edit state preserves mode and username", () => {
  const state = parseAccountFlowState({
    status: "pending",
    flowId: "flow_admin",
    kind: "admin_account",
    mode: "edit",
    username: "bootstrap-admin",
    expiresAt: 4_070_908_800_000,
    allowedProviders: ["local"],
    target: {
      userId: "usr_admin",
      name: "Bootstrap Admin",
      email: "admin@example.com",
      active: true,
    },
    providers: [],
  });

  assertEquals(state.status, "active");
  if (state.status !== "active") return;
  assertEquals(state.mode, "edit");
  assertEquals(state.username, "bootstrap-admin");
  assertEquals(state.target?.name, "Bootstrap Admin");
  assertEquals(state.target?.email, "admin@example.com");
});
