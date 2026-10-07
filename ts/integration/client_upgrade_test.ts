import { assert, assertEquals } from "@std/assert";
import { Result, TrellisClient } from "@oatscenter/trellis";
import {
  base64urlDecode,
  createPortalBinding,
  fetchPortalFlowState,
  startPortalTransaction,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { participants as narrow } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import { participants as revisedConsent } from "../../integration/fixtures/client-upgrade-consent/packages/client-upgrade/index.js";
import { ADMIN_USERNAME } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("expanded browser authority renews consent once; compatible sign-in reuses it; denied authority does not reauthenticate", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "upgrade-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    await service.handleEcho(({ input }) => Result.ok(input));
    await service.handleExtra(({ input }) => Result.ok(input));
    let passwords = 0;
    let consents = 0;
    let rememberedLogin: {
      generation: number;
      seed: Uint8Array;
      sessionKey: string;
      loginSessionId: string;
    } | undefined;
    const login = async (loginUrl: string) => {
      const intent = new URL(loginUrl).searchParams.get("intent");
      assert(intent);
      const binding = await createPortalBinding();
      const config = {
        authUrl: runtime.trellisUrl,
        portalOrigin: runtime.trellisUrl,
      };
      const transactionId = await startPortalTransaction(
        config,
        intent,
        binding,
        rememberedLogin,
      );
      const pending = await fetchPortalFlowState(
        config,
        transactionId,
        binding,
      );
      if (pending.status === "choose_provider") {
        passwords += 1;
        const response = await fetch(`${runtime.trellisUrl}/auth/login/local`, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            origin: runtime.trellisUrl,
            "trellis-portal-binding": binding.secret,
          },
          body: JSON.stringify({
            transactionId,
            username: ADMIN_USERNAME,
            password: runtime.adminPassword,
            portalBindingDigest: binding.digest,
          }),
        });
        assertEquals(response.status, 200, await response.text());
      }
      const state = await fetchPortalFlowState(config, transactionId, binding);
      assertEquals(state.status, "approval_required");
      consents += 1;
      const approved = await submitPortalApproval(
        config,
        transactionId,
        binding,
        "approved",
      );
      assertEquals(approved.status, "redirect");
      return { status: "bound" as const, transactionId };
    };
    try {
      const oldKey = await runtime.registerClient({
        name: "old-revision",
        contract: narrow.Caller.participant,
      });
      const old = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: narrow.Caller.participant,
        auth: runtime.clientAuth(oldKey).auth,
        onAuthRequired: (ctx) => login(ctx.loginUrl),
      }).orThrow();
      assertEquals((await old.echo({ value: "old" }).orThrow()).value, "old");
      const oldSession = (await old.sessionsMe({}).orThrow()).session;
      assert(oldSession !== null);
      await old.connection.close();

      await runtime.registerClient({
        name: "expanded-revision",
        contract: participants.Caller.participant,
      });
      const oldAuth = runtime.clientAuth(oldKey).auth;
      assert(oldAuth.mode === "session_key");
      rememberedLogin = {
        generation: 0,
        seed: base64urlDecode(oldAuth.sessionKeySeed),
        sessionKey: oldSession.sessionPublicKey,
        loginSessionId: oldSession.sessionId,
      };
      const expanded = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Caller.participant,
        auth: {
          ...oldAuth,
          sessionId: oldSession.sessionId,
        },
        onAuthRequired: (ctx) => login(ctx.loginUrl),
      }).orThrow();
      assertEquals(
        (await expanded.extra({ value: "new permission" }).orThrow()).value,
        "new permission",
      );
      const session = (await expanded.sessionsMe({}).orThrow()).session;
      assert(session !== null);
      assertEquals(session.sessionId, oldSession.sessionId);
      assertEquals(
        session.lastAuthenticatedAt,
        oldSession.lastAuthenticatedAt,
        "renewing consent is not a fresh password/provider authentication",
      );
      assertEquals(session.expiresAt, oldSession.expiresAt);
      await expanded.connection.close();
      assertEquals(
        passwords,
        1,
        "a valid login must authenticate renewed consent",
      );
      assertEquals(consents, 2);

      const oldAgain = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: narrow.Caller.participant,
        auth: { ...oldAuth, sessionId: oldSession.sessionId },
        onAuthRequired: () =>
          Promise.reject(new Error("covered old login must remain usable")),
      }).orThrow();
      assertEquals(
        (await oldAgain.echo({ value: "retained" }).orThrow()).value,
        "retained",
      );
      await oldAgain.connection.close();

      await runtime.registerClient({
        name: "revised-consent",
        contract: revisedConsent.Caller.participant,
      });
      await runtime.ensurePortalConsentPolicy(
        participants.Caller.participant.id,
        [
          "capability:client-upgrade-fixture.upgrade@v1::read_documents",
        ],
      );
      const ungrantedUpgrade = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: revisedConsent.Caller.participant,
        auth: { ...oldAuth, sessionId: session.sessionId },
        onAuthRequired: (ctx) => login(ctx.loginUrl),
      });
      assert(
        ungrantedUpgrade.isErr(),
        "consent cannot create missing user authority",
      );
      assertEquals(passwords, 1);
      assertEquals(consents, 2);
      const installed = await runtime.callAdminRpc("authParticipantsGet", {
        participantId: participants.Caller.participant.id,
      });
      const previous = (await runtime.callAdminRpc("authGrantsList", {
        participantId: participants.Caller.participant.id,
      })).items[0];
      const granted = await runtime.callAdminRpc("authGrantsSet", {
        expectedRevision: previous.revision,
        expiresAt: previous.expiresAt,
        grants: installed.participant.requiredGrants,
        idempotencyKey: crypto.randomUUID(),
        installedRevision: installed.participant.revision,
        ownerId: previous.ownerId,
        ownerKind: previous.ownerKind,
        participantId: previous.participantId,
        platformPrivileges: previous.platformPrivileges,
      });
      assertEquals(granted.binding.approvalMode, "exact");
      const consentUpgrade = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: revisedConsent.Caller.participant,
        auth: { ...oldAuth, sessionId: session.sessionId },
        onAuthRequired: (ctx) => login(ctx.loginUrl),
      }).orThrow();
      assertEquals(
        (await consentUpgrade.extra({ value: "revised consent" }).orThrow())
          .value,
        "revised consent",
      );
      await consentUpgrade.connection.close();
      assertEquals(consents, 3);

      const grants = await runtime.callAdminRpc("authGrantsList", {
        participantId: participants.Caller.participant.id,
      });
      let binding = grants.items[0];
      assertEquals(
        binding,
        granted.binding,
        "re-consent must preserve administrator authority exactly",
      );
      binding = (await runtime.callAdminRpc("authGrantsSet", {
        expectedRevision: binding.revision,
        expiresAt: binding.expiresAt,
        grants: binding.grants,
        idempotencyKey: crypto.randomUUID(),
        installedRevision: binding.installedRevision,
        ownerId: binding.ownerId,
        ownerKind: binding.ownerKind,
        participantId: binding.participantId,
        platformPrivileges: binding.platformPrivileges,
      })).binding;
      const exactApproval = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: revisedConsent.Caller.participant,
        auth: { ...oldAuth, sessionId: session.sessionId },
        onAuthRequired: () =>
          Promise.reject(
            new Error("explicit permission approval must remain usable"),
          ),
      }).orThrow();
      assertEquals(
        (await exactApproval.extra({ value: "exact approval" }).orThrow())
          .value,
        "exact approval",
      );
      await exactApproval.connection.close();
      await runtime.callAdminRpc("authGrantsSet", {
        expectedRevision: binding.revision,
        expiresAt: binding.expiresAt,
        grants: { format: binding.grants.format, permissions: [] },
        idempotencyKey: crypto.randomUUID(),
        installedRevision: binding.installedRevision,
        ownerId: binding.ownerId,
        ownerKind: binding.ownerKind,
        participantId: binding.participantId,
        platformPrivileges: binding.platformPrivileges,
      });
      let signIns = 0;
      const saved = runtime.clientAuth(oldKey).auth;
      assert(saved.mode === "session_key");
      const denied = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: revisedConsent.Caller.participant,
        auth: { ...saved, sessionId: session.sessionId },
        onAuthRequired: () => {
          signIns += 1;
          return Promise.reject(
            new Error("valid login must not reauthenticate"),
          );
        },
      });
      assert(denied.isErr());
      assertEquals(signIns, 0);

      const current = await runtime.callAdminRpc("authSessionsList", {
        participantId: participants.Caller.participant.id,
      });
      const retained = current.items.find((entry) =>
        entry.sessionId === session.sessionId
      );
      assert(retained);
      await runtime.callAdminRpc("authSessionsRevoke", {
        expectedVersion: retained.version,
        reason: "verify revoked-login recovery",
        sessionId: session.sessionId,
        idempotencyKey: crypto.randomUUID(),
      });
      const revoked = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: revisedConsent.Caller.participant,
        auth: { ...saved, sessionId: session.sessionId },
        onAuthRequired: () => {
          signIns += 1;
          return Promise.reject(
            new Error("observed legitimate reauthentication"),
          );
        },
      });
      assert(revoked.isErr());
      assertEquals(signIns, 1);
    } finally {
      await service.stop();
      assertEquals(await serviceExit, undefined);
    }
  });
});
