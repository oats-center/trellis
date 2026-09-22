import { assertEquals } from "@std/assert";

import {
  flowKindLabel,
  isExpectedLinkFlow,
  parseAccountFlowState,
  unavailableProviders,
} from "./page_state.ts";

Deno.test("link recognizes identity_link active state", () => {
  const state = parseAccountFlowState({
    status: "active",
    flowId: "flow-1",
    kind: "identity_link",
    allowedProviders: null,
    profileHint: null,
    expiresAt: "2099-01-01T00:00:00.000Z",
    providers: [
      { id: "local", displayName: "Username and password" },
      { id: "github", displayName: "GitHub" },
    ],
  });

  assertEquals(state.status, "active");
  if (state.status === "active") {
    assertEquals(isExpectedLinkFlow(state), true);
    assertEquals(unavailableProviders(state), [{
      id: "github",
      displayName: "GitHub",
    }]);
  }
});

Deno.test("link accepts the Rust pending account-flow response", () => {
  const state = parseAccountFlowState({
    status: "pending",
    flowId: "flow-token",
    kind: "identity_link",
    allowedProviders: ["local"],
    expiresAt: 4_071_859_200_000,
  });

  assertEquals(state.status, "active");
  if (state.status === "active") {
    assertEquals(state.providers, [{ id: "local", displayName: "local" }]);
    assertEquals(state.expiresAt, "2099-01-12T00:00:00.000Z");
  }
});

Deno.test("link labels flow kinds for user copy", () => {
  assertEquals(flowKindLabel("identity_link"), "account link");
  assertEquals(flowKindLabel("custom_flow"), "custom flow");
});

Deno.test("link parses expired and consumed state", () => {
  assertEquals(parseAccountFlowState({ status: "expired" }), {
    status: "expired",
  });
  assertEquals(
    parseAccountFlowState({ status: "consumed", kind: "identity_link" }),
    {
      status: "consumed",
      kind: "identity_link",
    },
  );
});
