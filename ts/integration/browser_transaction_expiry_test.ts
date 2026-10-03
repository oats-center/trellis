import { assert, assertEquals, assertRejects } from "@std/assert";
import { Result, TrellisClient } from "@oatscenter/trellis";
import {
  createPortalBinding,
  fetchPortalFlowState,
  fetchPortalIntentState,
  startPortalTransaction,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import { ADMIN_USERNAME } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

// Use the ordinary runtime deadline: no shorter test-only lifetime or clock hook.
Deno.test("the signed request remains usable after its authentication attempt expires", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "expiry-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    await service.handleEcho(({ input }) => Result.ok(input));
    const key = await runtime.registerClient({
      name: "expiry-caller",
      contract: participants.Caller.participant,
    });
    try {
      const client = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Caller.participant,
        auth: runtime.clientAuth(key).auth,
        onAuthRequired: async ({ loginUrl }) => {
          const intent = new URL(loginUrl).searchParams.get("intent");
          assert(intent);
          const config = {
            authUrl: runtime.trellisUrl,
            portalOrigin: runtime.trellisUrl,
          };
          const bindingA = await createPortalBinding();
          await fetchPortalIntentState(config, intent);
          const a = await startPortalTransaction(config, intent, bindingA);
          const response = await fetch(
            `${runtime.trellisUrl}/auth/transactions/${a}`,
            {
              headers: {
                origin: runtime.trellisUrl,
                "trellis-portal-binding": bindingA.secret,
              },
            },
          );
          assertEquals(response.status, 200, await response.clone().text());
          const active: { expiresAt: number } = await response.json();
          // This is a wait for the server's actual deadline, not an arbitrary readiness delay.
          await new Promise((resolve) =>
            setTimeout(resolve, Math.max(0, active.expiresAt - Date.now()) + 1)
          );
          await assertRejects(() => fetchPortalFlowState(config, a, bindingA));
          assertEquals(
            (await fetchPortalIntentState(config, intent)).status,
            "choose_provider",
          );
          const bindingB = await createPortalBinding();
          const b = await startPortalTransaction(config, intent, bindingB);
          assert(a !== b);
          assertEquals(
            (await fetchPortalFlowState(config, b, bindingB)).status,
            "choose_provider",
          );
          const authenticated = await fetch(
            `${runtime.trellisUrl}/auth/login/local`,
            {
              method: "POST",
              headers: {
                "content-type": "application/json",
                origin: runtime.trellisUrl,
                "trellis-portal-binding": bindingB.secret,
              },
              body: JSON.stringify({
                transactionId: b,
                username: ADMIN_USERNAME,
                password: runtime.adminPassword,
                portalBindingDigest: bindingB.digest,
              }),
            },
          );
          assertEquals(authenticated.status, 200, await authenticated.text());
          assertEquals(
            (await fetchPortalFlowState(config, b, bindingB)).status,
            "approval_required",
          );
          assertEquals(
            (await submitPortalApproval(config, b, bindingB, "approved"))
              .status,
            "redirect",
          );
          return { status: "bound", transactionId: b };
        },
      }).orThrow();
      try {
        assertEquals(
          (await client.echo({ value: "restarted after expiry" }).orThrow())
            .value,
          "restarted after expiry",
        );
      } finally {
        await client.connection.close();
      }
    } finally {
      await service.stop();
      await serviceExit;
    }
  });
});
