import { assert, assertEquals } from "@std/assert";
import { Result, TrellisClient } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  participants,
  type types,
} from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { participants as narrow } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import {
  issueRawClientConnection,
  observeClientConnection,
} from "./_support/client_session.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("browser login revision changes preserve compatible sessions and recover incompatible ones without widening denied authority", async () => {
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
    try {
      const narrowKey = await runtime.registerClient({
        name: "compatible-login",
        contract: narrow.Caller.participant,
      });
      const narrowClient = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: narrow.Caller.participant,
        ...runtime.clientAuth(narrowKey),
      }).orThrow();
      assertEquals(
        (await narrowClient.echo({ value: "before upgrade" }).orThrow()).value,
        "before upgrade",
      );
      const narrowLogin = await observeClientConnection(runtime, narrowKey);
      const narrowDefinition =
        (await runtime.callAdminRpc("authParticipantsGet", {
          participantId: narrow.Caller.participant.id,
        })).participant;
      await narrowClient.connection.close();

      // Install real authored definitions and replace current user authority via
      // the supported admin API, never by mutating auth/session database rows.
      const broadKey = await runtime.registerClient({
        name: "incompatible-login",
        contract: participants.Caller.participant,
      });
      const savedAuth = runtime.clientAuth(broadKey).auth;
      assert(savedAuth.mode === "session_key");
      const approveRevision = async (
        revision: bigint,
        permissions: types.AuthPermissionAtom[],
      ) => {
        const listed = await runtime.callAdminRpc("authGrantsList", {
          participantId: participants.Caller.participant.id,
        });
        assertEquals(listed.items.length, 1);
        const binding = listed.items[0];
        await runtime.callAdminRpc("authGrantsSet", {
          expectedRevision: binding.revision,
          expiresAt: binding.expiresAt,
          grants: { format: binding.grants.format, permissions },
          idempotencyKey: crypto.randomUUID(),
          installedRevision: revision,
          ownerId: binding.ownerId,
          ownerKind: binding.ownerKind,
          participantId: binding.participantId,
          platformPrivileges: binding.platformPrivileges,
        });
      };
      const broadDefinition =
        (await runtime.callAdminRpc("authParticipantsGet", {
          participantId: participants.Caller.participant.id,
        })).participant;
      await approveRevision(
        broadDefinition.revision,
        broadDefinition.requiredGrants.permissions,
      );
      // An additive revision retains the old login and does not require a new
      // sign-in merely because its immutable definition is no longer current.
      const compatible = await issueRawClientConnection(
        runtime,
        narrowKey,
        narrowLogin.loginSessionId,
      );
      assertEquals(compatible.loginSessionId, narrowLogin.loginSessionId);

      const broadClient = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Caller.participant,
        ...runtime.clientAuth(broadKey),
      }).orThrow();
      const broadLogin = await observeClientConnection(runtime, broadKey);
      const broadSession = (await broadClient.sessionsMe({}).orThrow()).session;
      assert(broadSession !== null);
      await broadClient.connection.close();
      await approveRevision(
        narrowDefinition.revision,
        narrowDefinition.requiredGrants.permissions,
      );

      // The updated client starts from the genuinely saved old login. Its
      // ordinary authentication callback completes a real portal flow and bind.
      let signIns = 0;
      const recovered = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: narrow.Caller.participant,
        auth: {
          ...savedAuth,
          sessionId: broadLogin.loginSessionId,
        },
        onAuthRequired: async (ctx) => {
          signIns += 1;
          return await runtime.completeClientAuth(ctx);
        },
      }).orThrow();
      assertEquals(signIns, 1);
      assertEquals(
        (await recovered.echo({ value: "after recovery" }).orThrow()).value,
        "after recovery",
      );
      const recoveredSession =
        (await recovered.sessionsMe({}).orThrow()).session;
      assert(recoveredSession !== null);
      assert(
        recoveredSession.sessionId !== broadSession.sessionId ||
          recoveredSession.version > broadSession.version,
        "sign-in must replace or version-renew the incompatible login",
      );
      await recovered.connection.close();

      // A denial at the current revision is not an obsolete-login signal and
      // must never be converted into automatic consent or wider permissions.
      await approveRevision(narrowDefinition.revision, []);
      signIns = 0;
      const currentAuth = runtime.clientAuth(narrowKey).auth;
      assert(currentAuth.mode === "session_key");
      const denied = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: narrow.Caller.participant,
        auth: {
          ...currentAuth,
          sessionId: narrowLogin.loginSessionId,
        },
        onAuthRequired: () => {
          signIns += 1;
          return Promise.reject(
            new Error("same-revision denial must not request sign-in"),
          );
        },
      });
      assert(denied.isErr());
      assertEquals(signIns, 0);
    } finally {
      await service.stop();
      assertEquals(await serviceExit, undefined);
    }
  });
});
