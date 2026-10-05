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
          const competingBindings = await Promise.all([
            createPortalBinding(),
            createPortalBinding(),
          ]);
          const starts = await Promise.allSettled(
            competingBindings.map((binding) =>
              startPortalTransaction(config, intent, binding)
            ),
          );
          const acceptedIndex = starts.findIndex((result) =>
            result.status === "fulfilled"
          );
          const accepted = starts[acceptedIndex];
          assert(accepted?.status === "fulfilled");
          assertEquals(
            starts.filter((result) => result.status === "fulfilled").length,
            1,
            "concurrent starts must not replace the accepted intent rendezvous",
          );
          const conflict = starts.find((result) =>
            result.status === "rejected"
          );
          assert(conflict?.status === "rejected");
          const cause: unknown = conflict.reason;
          assert(
            cause instanceof Error && "code" in cause && "status" in cause,
          );
          assertEquals(cause.code, "intent_transaction_active");
          assertEquals(cause.status, 409);
          const bindingA = competingBindings[acceptedIndex];
          let bindingB = await createPortalBinding();
          const a = accepted.value;
          const unbound = await fetch(
            `${runtime.trellisUrl}/auth/transactions/${a}`,
          );
          assertEquals(unbound.status, 403, await unbound.text());
          const publicView = await fetch(
            `${runtime.trellisUrl}/auth/intents/view`,
            {
              method: "POST",
              headers,
              body: JSON.stringify({ intent }),
            },
          );
          assertEquals(publicView.status, 200, await publicView.clone().text());
          assertEquals(
            (await publicView.json()).transactionId,
            undefined,
            "public portal choices must not disclose detached correlation",
          );
          const progress = async (
            signer: typeof initiator,
            issuedAt = Date.now(),
            purpose: "userAuthProgress" | "userAuthRequest" =
              "userAuthProgress",
          ) => {
            const unsignedRequest = {
              intent,
              requestId: ulid(),
              issuedAt,
              sessionPublicKey: signer.sessionKey,
            };
            return await fetch(`${runtime.trellisUrl}/auth/intents/progress`, {
              method: "POST",
              headers,
              body: JSON.stringify({
                ...unsignedRequest,
                proof: await signer.signSessionProof({
                  purpose,
                  origin: runtime.trellisUrl,
                  unsignedRequest,
                }),
              }),
            });
          };
          for (
            const response of [
              await progress(attacker),
              await progress(initiator, Date.now() - 600_000),
              await progress(initiator, Date.now(), "userAuthRequest"),
            ]
          ) {
            assertEquals(response.status, 401, await response.text());
          }
          const pending = await progress(initiator);
          assertEquals(pending.status, 200, await pending.clone().text());
          assertEquals(await pending.json(), { status: "pending" });
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
          await assertRejects(() =>
            startPortalTransaction(config, intent, bindingB)
          );
          const consent = await fetchPortalFlowState(config, a, bindingA);
          assert(consent.status === "approval_required");
          assertEquals(consent.user.username, ADMIN_USERNAME);
          assert(consent.user.id !== consent.user.username);
          assertEquals(
            (await submitPortalApproval(config, a, bindingA, "denied")).status,
            "approval_denied",
          );
          const denied = await progress(initiator);
          assertEquals(await denied.json(), { status: "denied" });
          const restartBindings = [bindingB, await createPortalBinding()];
          const restarts = await Promise.allSettled(
            restartBindings.map((binding) =>
              startPortalTransaction(config, intent, binding)
            ),
          );
          const restartedIndex = restarts.findIndex((result) =>
            result.status === "fulfilled"
          );
          const restarted = restarts[restartedIndex];
          assert(restarted?.status === "fulfilled");
          assertEquals(
            restarts.filter((result) => result.status === "fulfilled").length,
            1,
          );
          bindingB = restartBindings[restartedIndex];
          const b = restarted.value;
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
          await assertRejects(() =>
            startPortalTransaction(config, intent, bindingA)
          );
          const ready = await progress(initiator);
          assertEquals(ready.status, 200, await ready.clone().text());
          assertEquals(await ready.json(), {
            status: "ready",
            transactionId: b,
          });
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
          const bindingC = await createPortalBinding();
          const [c, recoveredC] = await Promise.all([
            startPortalTransaction(config, intent, bindingC),
            startPortalTransaction(config, intent, bindingC),
          ]);
          assert(c !== b);
          assertEquals(recoveredC, c);
          await assertRejects(() =>
            startPortalTransaction(config, intent, bindingA)
          );
          const restartedProgress = await progress(initiator);
          assertEquals(
            restartedProgress.status,
            200,
            await restartedProgress.clone().text(),
          );
          assertEquals(await restartedProgress.json(), { status: "pending" });
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
