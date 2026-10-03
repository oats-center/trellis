import { assert, assertEquals } from "@std/assert";
import { Result, TrellisClient } from "@oatscenter/trellis";
import {
  createPortalBinding,
  fetchPortalFlowState,
  startPortalTransaction,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { participants as narrow } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
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
    const login = async (loginUrl: string, expectConsent: boolean) => {
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
      );
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
      const state = await fetchPortalFlowState(config, transactionId, binding);
      assertEquals(
        state.status,
        expectConsent ? "approval_required" : "redirect",
      );
      if (expectConsent) {
        consents += 1;
        const approved = await submitPortalApproval(
          config,
          transactionId,
          binding,
          "approved",
        );
        assertEquals(approved.status, "redirect");
      }
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
        onAuthRequired: (ctx) => login(ctx.loginUrl, true),
      }).orThrow();
      assertEquals((await old.echo({ value: "old" }).orThrow()).value, "old");
      const oldSession = (await old.sessionsMe({}).orThrow()).session;
      assert(oldSession !== null);
      await old.connection.close();

      const newKey = await runtime.registerClient({
        name: "expanded-revision",
        contract: participants.Caller.participant,
      });
      const expanded = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Caller.participant,
        auth: runtime.clientAuth(newKey).auth,
        onAuthRequired: (ctx) => login(ctx.loginUrl, true),
      }).orThrow();
      assertEquals(
        (await expanded.extra({ value: "new permission" }).orThrow()).value,
        "new permission",
      );
      const session = (await expanded.sessionsMe({}).orThrow()).session;
      assert(session !== null);
      await expanded.connection.close();
      assertEquals(passwords, 2);
      assertEquals(consents, 2);

      const oldAuth = runtime.clientAuth(oldKey).auth;
      assert(oldAuth.mode === "session_key");
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

      const compatibleKey = await runtime.registerClient({
        name: "compatible-narrowing",
        contract: narrow.Caller.participant,
      });
      const compatible = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: narrow.Caller.participant,
        auth: runtime.clientAuth(compatibleKey).auth,
        onAuthRequired: (ctx) => login(ctx.loginUrl, false),
      }).orThrow();
      assertEquals(
        (await compatible.echo({ value: "reused" }).orThrow()).value,
        "reused",
      );
      await compatible.connection.close();
      assertEquals(consents, 2);

      const grants = await runtime.callAdminRpc("authGrantsList", {
        participantId: participants.Caller.participant.id,
      });
      const binding = grants.items[0];
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
      const saved = runtime.clientAuth(newKey).auth;
      assert(saved.mode === "session_key");
      const denied = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Caller.participant,
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
        participant: participants.Caller.participant,
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
