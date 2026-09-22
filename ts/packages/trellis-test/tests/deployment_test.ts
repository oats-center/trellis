import { assertEquals } from "@std/assert";

import { applyWithServerConsent } from "../src/admin/deployment.ts";
import type { AdminRpcInput } from "../src/admin/methods.ts";
import { apis } from "../trellis/index.js";

Deno.test("deployment apply retries with server-computed approval", async () => {
  const consentRequest = {
    capabilities: [{
      alreadyApproved: false,
      consentDigest: "capability-digest",
      consequence: "",
      description: "",
      eligible: true,
      id: "api::write",
      required: true,
      title: "Write",
    }],
    decisionDigest: "decision-digest",
    expectedGrantRevision: 0n,
    installedRevision: 1n,
    packageDigest: "package-digest",
    participantId: "demo.Service",
    resources: [],
  };
  const calls: AdminRpcInput<"authDeploymentsApply">[] = [];

  await applyWithServerConsent(
    (input) => {
      calls.push(input);
      if (calls.length === 1) {
        throw new apis.auth.AuthError({
          id: "error-id",
          type: "trellis.auth@v1::AuthError",
          code: "approval_required",
          consentRequest,
          field: null,
          message: "approval required",
          retryable: false,
        });
      }
      return Promise.resolve({
        binding: {
          deploymentId: "deployment-id",
          installedRevision: 1n,
          kind: "service",
          participantId: "demo.Service",
          revision: 1n,
          state: "resolved",
        },
      });
    },
    {
      approval: undefined,
      deploymentId: "deployment-id",
      expectedRevision: 0n,
      idempotencyKey: "idempotency-key",
      packageDigest: "package-digest",
      packageEvidence: {
        packages: [],
        rootDigest: "package-digest",
        rootPackage: "demo",
      },
      participantPath: "demo.Service",
    },
  );

  assertEquals(calls.length, 2);
  assertEquals(calls[1]?.approval, {
    approvedCapabilities: [{
      id: "api::write",
      consentDigest: "capability-digest",
    }],
    approvedResources: [],
    companionApproved: false,
    decisionDigest: "decision-digest",
    expectedGrantRevision: 0n,
    installedRevision: 1n,
    mode: "capabilities",
  });
  assertEquals(calls[1]?.idempotencyKey === calls[0]?.idempotencyKey, false);
});
