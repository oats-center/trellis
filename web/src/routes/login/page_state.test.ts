import { assertEquals, assertRejects } from "@std/assert";

import {
  GENERIC_MISSING_CAPABILITY_LABEL,
  isFederatedRegistrationAvailable,
  isFederatedRegistrationProvider,
  isLocalRegistrationAvailable,
  localLoginErrorMessage,
  localRegistrationErrorMessage,
  MISSING_PORTAL_FLOW_ID_ERROR,
  portalCapabilityDisplayName,
  portalReturnLocation,
  shouldOfferPortalReturnLink,
  shouldShowPortalExpiredState,
  shouldStayOnPortalCompletionPage,
  submitLocalLogin,
  submitLocalRegistration,
  visiblePortalFlowError,
} from "./page_state.ts";

Deno.test("shouldStayOnPortalCompletionPage keeps same-page detached completion in portal", () => {
  assertEquals(
    shouldStayOnPortalCompletionPage(
      new URL(
        "https://auth.example.com/login?flowId=flow-1",
      ),
      "https://auth.example.com/login?flowId=flow-1",
    ),
    true,
  );
});

Deno.test("shouldStayOnPortalCompletionPage still follows app callbacks", () => {
  assertEquals(
    shouldStayOnPortalCompletionPage(
      new URL(
        "https://auth.example.com/login?flowId=flow-1",
      ),
      "http://localhost:4173/callback?flowId=flow-1",
    ),
    false,
  );
});

Deno.test("shouldOfferPortalReturnLink hides self-links back to the same portal page", () => {
  assertEquals(
    shouldOfferPortalReturnLink(
      new URL(
        "https://auth.example.com/login?flowId=flow-1",
      ),
      "https://auth.example.com/login?flowId=flow-1",
    ),
    false,
  );
});

Deno.test("shouldOfferPortalReturnLink still allows returning to app callbacks", () => {
  assertEquals(
    shouldOfferPortalReturnLink(
      new URL(
        "https://auth.example.com/login?flowId=flow-1",
      ),
      "http://localhost:4173/callback?flowId=flow-1",
    ),
    true,
  );
});

Deno.test("portalReturnLocation uses expired flow return location when present", () => {
  assertEquals(
    portalReturnLocation(
      { status: "expired", returnLocation: "https://app.example.com/login" },
      "https://auth.example.com",
    ),
    "https://app.example.com/login",
  );
});

Deno.test("portalReturnLocation keeps detached expired flow on portal fallback", () => {
  assertEquals(
    portalReturnLocation({ status: "expired" }, "https://auth.example.com"),
    "https://auth.example.com",
  );
});

Deno.test("missing portal flow id maps to expired state without raw error", () => {
  assertEquals(
    shouldShowPortalExpiredState(MISSING_PORTAL_FLOW_ID_ERROR),
    true,
  );
  assertEquals(visiblePortalFlowError(MISSING_PORTAL_FLOW_ID_ERROR), null);
});

Deno.test("recoverable shared expired-flow classification maps to expired portal state", () => {
  assertEquals(
    shouldShowPortalExpiredState(
      "Trellis sign-in did not complete.",
      "flow_expired",
    ),
    true,
  );
  assertEquals(
    visiblePortalFlowError("Trellis sign-in did not complete.", "flow_expired"),
    null,
  );
});

Deno.test("insufficient capabilities stay user-safe in primary display", () => {
  const rawCapability = "workspace::workspace.read_sensitive";
  assertEquals(
    portalCapabilityDisplayName({}, rawCapability),
    GENERIC_MISSING_CAPABILITY_LABEL,
  );
  assertEquals(
    portalCapabilityDisplayName({
      [rawCapability]: {
        displayName: "Workspace profile access",
        description: "Read workspace profile details.",
      },
    }, rawCapability),
    "Workspace profile access",
  );
});

Deno.test("localLoginErrorMessage formats expected local-login failures", () => {
  assertEquals(
    localLoginErrorMessage(403, "invalid_credentials"),
    "Invalid username or password.",
  );
  assertEquals(
    localLoginErrorMessage(403, "user_inactive"),
    "This account is inactive. Contact an administrator for access.",
  );
  assertEquals(
    localLoginErrorMessage(404, null),
    "This sign-in request has expired. Return to the app and start sign-in again.",
  );
});

Deno.test("submitLocalLogin posts expected URL and payload", async () => {
  let capturedUrl = "";
  let capturedInit: RequestInit | undefined;

  await submitLocalLogin(
    "https://auth.example.com/base",
    {
      flowId: "flow-1",
      username: "ada",
      password: "secret",
      portalBindingDigest: "digest",
    },
    (input, init) => {
      capturedUrl = input.toString();
      capturedInit = init;
      return Promise.resolve(
        Response.json({ state: "authenticated", flowId: "flow-1" }),
      );
    },
  );

  assertEquals(capturedUrl, "https://auth.example.com/auth/login/local");
  assertEquals(capturedInit?.method, "POST");
  assertEquals(capturedInit?.headers, { "content-type": "application/json" });
  assertEquals(
    capturedInit?.body,
    JSON.stringify({
      flowId: "flow-1",
      username: "ada",
      password: "secret",
      portalBindingDigest: "digest",
    }),
  );
});

Deno.test("submitLocalLogin accepts trusted portal approval", async () => {
  const result = await submitLocalLogin(
    "https://auth.example.com",
    {
      flowId: "flow-1",
      username: "ada",
      password: "secret",
      portalBindingDigest: "digest",
    },
    () =>
      Promise.resolve(Response.json({ state: "approved", flowId: "flow-1" })),
  );

  assertEquals(result, { state: "approved", flowId: "flow-1" });
});

Deno.test("submitLocalLogin accepts manual approval requirement", async () => {
  const result = await submitLocalLogin(
    "https://auth.example.com",
    {
      flowId: "flow-1",
      username: "ada",
      password: "secret",
      portalBindingDigest: "digest",
    },
    () =>
      Promise.resolve(
        Response.json({ state: "approval_required", flowId: "flow-1" }),
      ),
  );

  assertEquals(result, { state: "approval_required", flowId: "flow-1" });
});

Deno.test("registration gating requires explicit local availability", () => {
  assertEquals(
    isLocalRegistrationAvailable({
      status: "choose_provider",
      providers: [{ id: "local", displayName: "Local" }],
      registration: { localIdentity: { available: true } },
    }),
    true,
  );
  assertEquals(
    isLocalRegistrationAvailable({
      status: "choose_provider",
      providers: [{ id: "local", displayName: "Local" }],
    }),
    false,
  );
});

Deno.test("federated registration gating does not infer from provider list", () => {
  const state = {
    status: "choose_provider",
    providers: [{ id: "github", displayName: "GitHub" }],
  };

  assertEquals(isFederatedRegistrationAvailable(state), false);
  assertEquals(isFederatedRegistrationProvider(state, "github"), false);
  assertEquals(
    isFederatedRegistrationAvailable({
      ...state,
      registration: { federatedIdentity: { available: true, providers: [] } },
    }),
    true,
  );
  assertEquals(
    isFederatedRegistrationProvider({
      ...state,
      registration: {
        federatedIdentity: {
          available: true,
          providers: [{ id: "github", displayName: "GitHub" }],
        },
      },
    }, "github"),
    true,
  );
});

Deno.test("localRegistrationErrorMessage formats expected failures", () => {
  assertEquals(
    localRegistrationErrorMessage(409, "username_taken"),
    "That username is already in use.",
  );
  assertEquals(
    localRegistrationErrorMessage(403, "registration_unavailable"),
    "Account creation is not available for this sign-in request.",
  );
  assertEquals(
    localRegistrationErrorMessage(404, null),
    "This sign-in request has expired. Return to the app and start sign-in again.",
  );
});

Deno.test("submitLocalRegistration posts expected URL and payload", async () => {
  let capturedUrl = "";
  let capturedInit: RequestInit | undefined;

  await submitLocalRegistration(
    "https://auth.example.com/base",
    "flow-1",
    {
      username: "ada",
      password: "secret",
      name: "Ada Lovelace",
      email: "ada@example.com",
      portalBindingDigest: "digest",
    },
    (input, init) => {
      capturedUrl = input.toString();
      capturedInit = init;
      return Promise.resolve(Response.json({ status: "authenticated" }));
    },
  );

  assertEquals(
    capturedUrl,
    "https://auth.example.com/auth/flow/flow-1/register/local",
  );
  assertEquals(capturedInit?.method, "POST");
  assertEquals(capturedInit?.headers, { "content-type": "application/json" });
  assertEquals(
    capturedInit?.body,
    JSON.stringify({
      username: "ada",
      password: "secret",
      name: "Ada Lovelace",
      email: "ada@example.com",
      portalBindingDigest: "digest",
    }),
  );
});

Deno.test("submitLocalRegistration throws formatted response errors", async () => {
  await assertRejects(
    () =>
      submitLocalRegistration(
        "https://auth.example.com",
        "flow-1",
        {
          username: "ada",
          password: "secret",
          name: "Ada Lovelace",
          email: "ada@example.com",
          portalBindingDigest: "digest",
        },
        () =>
          Promise.resolve(
            Response.json({ error: { code: "username_taken" } }, {
              status: 409,
            }),
          ),
      ),
    Error,
    "That username is already in use.",
  );
});
