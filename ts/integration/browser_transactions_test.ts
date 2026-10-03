import { assert, assertEquals, assertRejects } from "@std/assert";
import { ulid } from "ulid";
import { Result, TrellisClient } from "@oatscenter/trellis";
import {
  createPortalBinding,
  fetchPortalFlowState,
  fetchPortalIntentState,
  startPortalTransaction,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";
import { TrellisService } from "@oatscenter/trellis/service";
import { createAuth } from "../packages/trellis/auth/session_auth.ts";
import { base64urlEncode } from "../packages/trellis/auth/utils.ts";
import { participants } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import { ADMIN_USERNAME } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("signed intent survives a denied attempt; transaction binding and initiating key prevent substitution; completion replays one login", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "transaction-provider",
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
      name: "transaction-caller",
      contract: participants.Caller.participant,
    });
    const auth = runtime.clientAuth(key).auth;
    assert(auth.mode === "session_key");
    const initiator = await createAuth({ sessionKeySeed: auth.sessionKeySeed });
    const attacker = await createAuth({
      sessionKeySeed: base64urlEncode(
        crypto.getRandomValues(new Uint8Array(32)),
      ),
    });
    const config = {
      authUrl: runtime.trellisUrl,
      portalOrigin: runtime.trellisUrl,
    };
    const headers = {
      "content-type": "application/json",
      origin: runtime.trellisUrl,
    };
    let completedSessionId = "";
    try {
      const client = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Caller.participant,
        auth,
        onAuthRequired: async ({ loginUrl }) => {
          const intent = new URL(loginUrl).searchParams.get("intent");
          assert(intent);
          // Rendering choices must leave the request usable for a later attempt.
          const choices = await fetchPortalIntentState(config, intent);
          assert(choices.providers.some((provider) => provider.id === "local"));
          const malformed = `${intent.slice(0, -2)}${
            intent.endsWith("AA") ? "BB" : "AA"
          }`;
          await assertRejects(() => fetchPortalIntentState(config, malformed));
          const bindingA = await createPortalBinding();
          const bindingB = await createPortalBinding();
          const a = await startPortalTransaction(config, intent, bindingA);
          const authenticate = async (
            transactionId: string,
            binding: typeof bindingA,
          ) => {
            const response = await fetch(
              `${runtime.trellisUrl}/auth/login/local`,
              {
                method: "POST",
                headers: {
                  ...headers,
                  "trellis-portal-binding": binding.secret,
                },
                body: JSON.stringify({
                  transactionId,
                  username: ADMIN_USERNAME,
                  password: runtime.adminPassword,
                  portalBindingDigest: binding.digest,
                }),
              },
            );
            assertEquals(response.status, 200, await response.text());
          };
          await authenticate(a, bindingA);
          assertEquals(
            (await fetchPortalFlowState(config, a, bindingA)).status,
            "approval_required",
          );
          assertEquals(
            (await submitPortalApproval(config, a, bindingA, "denied")).status,
            "approval_denied",
          );
          const b = await startPortalTransaction(config, intent, bindingB);
          assert(a !== b);
          assertEquals(
            (await fetchPortalFlowState(config, b, bindingB)).status,
            "choose_provider",
          );
          const foreignBinding = await fetch(
            `${runtime.trellisUrl}/auth/login/local`,
            {
              method: "POST",
              headers: {
                ...headers,
                "trellis-portal-binding": bindingA.secret,
              },
              body: JSON.stringify({
                transactionId: b,
                username: ADMIN_USERNAME,
                password: runtime.adminPassword,
                portalBindingDigest: bindingB.digest,
              }),
            },
          );
          assertEquals(foreignBinding.status, 403, await foreignBinding.text());
          await authenticate(b, bindingB);
          await assertRejects(() => fetchPortalFlowState(config, b, bindingA));
          assertEquals(
            (await fetchPortalFlowState(config, b, bindingB)).status,
            "approval_required",
          );
          assertEquals(
            (await submitPortalApproval(config, b, bindingB, "approved"))
              .status,
            "redirect",
          );
          const unsigned = { requestId: ulid(), issuedAt: Date.now() };
          const bindRequest = async (signer: typeof initiator) => ({
            ...unsigned,
            proof: await signer.signSessionProof({
              purpose: "userAuthBind",
              origin: runtime.trellisUrl,
              transactionId: b,
              sessionPublicKey: signer.sessionKey,
              unsignedRequest: unsigned,
            }),
          });
          const bindUrl = `${runtime.trellisUrl}/auth/transactions/${b}/bind`;
          const wrong = await fetch(bindUrl, {
            method: "POST",
            headers,
            body: JSON.stringify(await bindRequest(attacker)),
          });
          assertEquals(wrong.status, 401, await wrong.text());
          const body = JSON.stringify(await bindRequest(initiator));
          for (let retry = 0; retry < 2; retry++) {
            const response = await fetch(bindUrl, {
              method: "POST",
              headers,
              body,
            });
            assertEquals(response.status, 200, await response.clone().text());
            const result: { session: { sessionId: string } } = await response
              .json();
            if (retry === 0) completedSessionId = result.session.sessionId;
            else assertEquals(result.session.sessionId, completedSessionId);
          }
          return { status: "bound", transactionId: b };
        },
      }).orThrow();
      try {
        assertEquals(
          (await client.echo({ value: "independent transaction" }).orThrow())
            .value,
          "independent transaction",
        );
        assertEquals(
          (await client.sessionsMe({}).orThrow()).session?.sessionId,
          completedSessionId,
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
