import { assertEquals, assertRejects } from "@std/assert";

import {
  fetchPortalFlowState,
  portalFlowIdFromUrl,
  portalProviderLoginUrl,
  portalRedirectLocation,
  submitPortalApproval,
} from "./portal.ts";

const binding = { secret: "portal-secret", digest: "portal-digest" };
const consentView = {
  participantId: "trellis-app.console@v1",
  packageDigest: "digest",
  installedRevision: 1,
  expectedGrantRevision: 0,
  capabilities: [{
    id: "trellis.auth@v1::public",
    title: "Public",
    description: "Public access",
    consequence: "The caller can use public access.",
    consentDigest: "capability-digest",
    required: true,
    eligible: true,
    alreadyApproved: false,
  }],
  resources: [],
  decisionDigest: "consent-digest",
};

Deno.test("portalFlowIdFromUrl reads flowId from URL", () => {
  assertEquals(
    portalFlowIdFromUrl(
      new URL("https://portal.example.com/login?flowId=flow-1"),
    ),
    "flow-1",
  );
  assertEquals(
    portalFlowIdFromUrl(
      new URL("https://portal.example.com/login?redirectTo=%2F"),
    ),
    null,
  );
});

Deno.test("portalProviderLoginUrl keeps flowId on provider links", () => {
  assertEquals(
    portalProviderLoginUrl(
      { authUrl: "https://auth.example.com/" },
      "google",
      "flow-1",
      binding,
    ),
    "https://auth.example.com/auth/login/google?flowId=flow-1&portalBindingDigest=portal-digest",
  );
});

Deno.test("portalRedirectLocation returns auth-owned redirect locations", () => {
  assertEquals(
    portalRedirectLocation({
      status: "redirect",
      location: "https://app.example.com/callback?flowId=flow-1",
    }),
    "https://app.example.com/callback?flowId=flow-1",
  );
  assertEquals(
    portalRedirectLocation({
      status: "approval_denied",
      flowId: "flow-1",
      approval: {
        contractId: "trellis-app.console@v1",
        contractDigest: "digest",
        displayName: "Trellis Console",
        description: "Admin console",
        capabilities: {},
      },
      returnLocation:
        "https://app.example.com/callback?authError=approval_denied",
    }),
    "https://app.example.com/callback?authError=approval_denied",
  );
  assertEquals(portalRedirectLocation({ status: "expired" }), null);
  assertEquals(
    portalRedirectLocation({
      status: "expired",
      returnLocation: "https://app.example.com/callback",
    }),
    "https://app.example.com/callback",
  );
});

Deno.test("fetchPortalFlowState returns auth-owned portal state directly", async () => {
  const originalFetch = globalThis.fetch;
  try {
    globalThis.fetch = (async (input) => {
      assertEquals(String(input), "https://auth.example.com/auth/flow/flow-1");
      return new Response(JSON.stringify({
        flowId: "flow-1",
        expiresAt: 2_000_000_000_000,
        state: "choose_provider",
        providers: ["github", "auth0"],
        registrationEnabled: false,
        federatedRegistrationEnabled: false,
        futureTopLevelHint: true,
        consentView,
      }));
    }) as typeof fetch;

    const flow = await fetchPortalFlowState(
      {
        authUrl: "https://auth.example.com",
      },
      "flow-1",
      binding,
    );
    assertEquals(flow.status, "choose_provider");
    if (flow.status === "choose_provider") {
      assertEquals(flow.providers.length, 2);
      assertEquals(flow.app.displayName, "trellis-app.console@v1");
    }
  } finally {
    globalThis.fetch = originalFetch;
  }
});

Deno.test("fetchPortalFlowState throws on non-success responses", async () => {
  const originalFetch = globalThis.fetch;
  try {
    globalThis.fetch = (async () =>
      Response.json({ error: { code: "flow_not_found" } }, {
        status: 404,
      })) as typeof fetch;

    await assertRejects(
      () =>
        fetchPortalFlowState(
          { authUrl: "https://auth.example.com" },
          "missing",
          binding,
        ),
      Error,
      "Trellis HTTP 404: flow_not_found",
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

Deno.test("submitPortalApproval posts decision and parses next state", async () => {
  const originalFetch = globalThis.fetch;
  try {
    let call = 0;
    globalThis.fetch = (async (input, init) => {
      call += 1;
      if (call <= 2) {
        return new Response(JSON.stringify({
          flowId: "flow-1",
          expiresAt: 2_000_000_000_000,
          state: "approval_required",
          providers: ["local"],
          registrationEnabled: false,
          federatedRegistrationEnabled: false,
          ...(call === 2 ? { decisionDigest: "consent-digest" } : {}),
          consentView,
          ...(call === 2
            ? {
              user: {
                origin: "trellis",
                id: "usr-1",
                name: "Admin",
              },
            }
            : {}),
          redirectTarget:
            "https://app.example.com/callback?portalCallback=token",
        }));
      }
      assertEquals(
        String(input),
        "https://auth.example.com/auth/flow/flow-1/approval",
      );
      assertEquals(init?.method, "POST");
      assertEquals(init?.headers, {
        "content-type": "application/json",
        "trellis-portal-binding": binding.secret,
      });
      const body = JSON.parse(String(init?.body));
      assertEquals(body, {
        decision: "approve",
        approval: {
          mode: "capabilities",
          installedRevision: 1,
          expectedGrantRevision: 0,
          decisionDigest: "consent-digest",
          approvedCapabilities: [{
            id: "trellis.auth@v1::public",
            consentDigest: "capability-digest",
          }],
          approvedResources: [],
          companionApproved: false,
        },
      });

      return new Response(JSON.stringify({
        flowId: "flow-1",
        expiresAt: 2_000_000_000_000,
        state: "approved",
        providers: [],
        registrationEnabled: false,
        federatedRegistrationEnabled: false,
        decisionDigest: "consent-digest",
        consentView,
        user: { origin: "trellis", id: "usr-1", name: "Admin" },
        redirectTarget: "https://app.example.com/callback?portalCallback=token",
      }));
    }) as typeof fetch;

    const state = await submitPortalApproval(
      { authUrl: "https://auth.example.com/" },
      "flow-1",
      binding,
      "approved",
    );
    assertEquals(state, {
      status: "redirect",
      location:
        "https://app.example.com/callback?portalCallback=token&flowId=flow-1",
    });
  } finally {
    globalThis.fetch = originalFetch;
  }
});
