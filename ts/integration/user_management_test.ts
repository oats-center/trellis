import { assert, assertEquals } from "@std/assert";
import { TrellisClient } from "@oatscenter/trellis";
import { base64urlEncode } from "@oatscenter/trellis/auth";
import {
  createPortalBinding,
  fetchPortalFlowState,
  startPortalTransaction,
  submitPortalApproval,
} from "@oatscenter/trellis/auth/browser";
import { participants } from "trellis-web-generated";
import { AuthError as RpcAuthError } from "../packages/trellis/internal_sdk/generated/apis/auth/mod.js";
import { participants as runtimeParticipants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participant as adminParticipant } from "../packages/trellis/internal_sdk/generated/participants/console/mod.js";
import { ulid } from "ulid";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("bound portal reads the canonical username before and after a local account rename", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.contracts.install({
      contract: participants.Console.participant,
    });
    const admin = await TrellisClient.connect({
      trellisUrl: runtime.trellisUrl,
      participant: adminParticipant,
      ...runtime.clientAuth({
        seed: base64urlEncode(crypto.getRandomValues(new Uint8Array(32))),
        participantId: "trellis.console",
      }),
    }).orThrow();
    try {
      const created = (await admin.usersCreate({
        username: "portal-before",
        name: "Display name is not the username",
        email: "display@example.com",
        image: null,
        idempotencyKey: ulid(),
      }).orThrow()).user;
      const reset = await admin.usersPasswordResetCreate({
        userId: created.userId,
        returnTarget: null,
        idempotencyKey: ulid(),
      }).orThrow();
      const password = "portal-username-test-password!";
      const response = await fetch(
        `${reset.flow.completionUrl}/local-password`,
        {
          method: "POST",
          headers: {
            "content-type": "application/json",
            origin: runtime.publicOrigin,
          },
          body: JSON.stringify({ username: "portal-before", password }),
        },
      );
      assertEquals(response.status, 200, await response.clone().text());
      await response.arrayBuffer();
      const participant = (await admin.participantsGet({
        participantId: participants.Console.participant.id,
      }).orThrow()).participant;
      await admin.grantsSet({
        ownerKind: "user",
        ownerId: created.userId,
        participantId: participant.participantId,
        installedRevision: participant.revision,
        expectedRevision: 0n,
        grants: participant.requiredGrants,
        platformPrivileges: [],
        expiresAt: null,
        idempotencyKey: ulid(),
      }).orThrow();
      const location = {
        authUrl: runtime.trellisUrl,
        portalOrigin: runtime.publicOrigin,
      };
      let checked = false;
      const client = await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Console.participant,
        auth: {
          mode: "session_key",
          sessionKeySeed: base64urlEncode(
            crypto.getRandomValues(new Uint8Array(32)),
          ),
          redirectTo: `${runtime.trellisUrl}/_trellis/test/admin-auth`,
        },
        onAuthRequired: async ({ loginUrl }) => {
          const intent = new URL(loginUrl).searchParams.get("intent");
          assert(intent);
          const binding = await createPortalBinding();
          const flowId = await startPortalTransaction(
            location,
            intent,
            binding,
          );
          const login = await fetch(`${runtime.trellisUrl}/auth/login/local`, {
            method: "POST",
            headers: {
              "content-type": "application/json",
              origin: runtime.publicOrigin,
              "trellis-portal-binding": binding.secret,
            },
            body: JSON.stringify({
              transactionId: flowId,
              username: "portal-before",
              password,
              portalBindingDigest: binding.digest,
            }),
          });
          assertEquals(login.status, 200, await login.clone().text());
          await login.arrayBuffer();
          const readIdentity = async () => {
            const portal = await fetch(
              `${runtime.trellisUrl}/auth/transactions/${flowId}/portal`,
              {
                method: "POST",
                headers: {
                  origin: runtime.publicOrigin,
                  "trellis-portal-binding": binding.secret,
                },
              },
            );
            assertEquals(portal.status, 200, await portal.clone().text());
            return (await portal.json()).user;
          };
          const before = await readIdentity();
          assertEquals(before.username, "portal-before");
          assertEquals(before.id, created.userId);
          assertEquals(before.name, "Display name is not the username");
          const current =
            (await admin.usersGet({ userId: created.userId }).orThrow()).user;
          await admin.usersUpdate({
            userId: current.userId,
            username: "  Portal-After  ",
            name: current.name,
            email: current.email,
            image: current.image,
            state: current.state,
            expectedVersion: current.version,
            idempotencyKey: ulid(),
          }).orThrow();
          const after = await readIdentity();
          assertEquals(after.username, "portal-after");
          assertEquals(after.id, created.userId);
          checked = true;
          const state = await fetchPortalFlowState(location, flowId, binding);
          if (state.status === "approval_required") {
            assertEquals(
              (await submitPortalApproval(
                location,
                flowId,
                binding,
                "approved",
              )).status,
              "redirect",
            );
          } else assertEquals(state.status, "redirect");
          return { status: "bound" as const, transactionId: flowId };
        },
      }).orThrow();
      await client.connection.close();
      assert(checked);
    } finally {
      await admin.connection.close();
    }
  });
});

Deno.test("user administration renames local login, restores access, and scopes identity actions", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.contracts.install({
      contract: participants.Console.participant,
    });
    await runtime.contracts.install({
      contract: runtimeParticipants.StateCaller.participant,
    });
    const adminAuth = runtime.clientAuth({
      seed: base64urlEncode(crypto.getRandomValues(new Uint8Array(32))),
      participantId: "trellis.console",
    });
    const admin = await TrellisClient.connect({
      trellisUrl: runtime.trellisUrl,
      name: "user-management-admin",
      participant: adminParticipant,
      ...adminAuth,
    }).orThrow();
    const created = await admin.usersCreate({
      username: "managed-before",
      name: "Managed account",
      email: null,
      image: null,
      idempotencyKey: ulid(),
    }).orThrow();
    const other = await admin.usersCreate({
      username: "other-login",
      name: "Other account",
      email: null,
      image: null,
      idempotencyKey: ulid(),
    }).orThrow();
    const targetId = created.user.userId;
    const password = "a-real-user-password-123!";

    const reset = await admin.usersPasswordResetCreate({
      userId: targetId,
      returnTarget: null,
      idempotencyKey: ulid(),
    }).orThrow();
    assertEquals(reset.flow.targetPrincipalId, targetId);
    const resetEndpoint = `${reset.flow.completionUrl}/local-password`;
    const body = JSON.stringify({ username: "managed-before", password });
    const resetResponse = await fetch(resetEndpoint, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        origin: runtime.publicOrigin,
      },
      body,
    });
    assertEquals(resetResponse.status, 200, await resetResponse.clone().text());
    assertEquals((await resetResponse.json()).userId, targetId);
    const consumed = await fetch(resetEndpoint, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        origin: runtime.publicOrigin,
      },
      body,
    });
    assertEquals(consumed.status, 409);
    await consumed.arrayBuffer();

    const discovered = (await admin.participantsList({ kind: "app" }).orThrow())
      .items.find((candidate) =>
        candidate.participantId === participants.Console.participant.id
      );
    assert(
      discovered,
      "The installed application must be discoverable for granting access",
    );
    const participant = (await admin.participantsGet({
      participantId: discovered.participantId,
    }).orThrow()).participant;
    const grant = await admin.grantsSet({
      ownerKind: "user",
      ownerId: targetId,
      participantId: participant.participantId,
      installedRevision: participant.revision,
      expectedRevision: 0n,
      grants: participant.requiredGrants,
      platformPrivileges: [],
      expiresAt: null,
      idempotencyKey: ulid(),
    }).orThrow();
    assertEquals(
      (await admin.grantsGet({
        ownerKind: "user",
        ownerId: targetId,
        participantId: participant.participantId,
      }).orThrow()).binding?.grants,
      grant.binding.grants,
    );

    async function login<
      const TParticipant extends
        | typeof participants.Console.participant
        | typeof runtimeParticipants.StateCaller.participant,
    >(username: string, contract: TParticipant, allowConsent = true) {
      return await TrellisClient.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        name: `managed-login-${username}`,
        auth: {
          mode: "session_key",
          sessionKeySeed: base64urlEncode(
            crypto.getRandomValues(new Uint8Array(32)),
          ),
          redirectTo: `${runtime.trellisUrl}/_trellis/test/admin-auth`,
        },
        onAuthRequired: async ({ loginUrl }) => {
          const intent = new URL(loginUrl).searchParams.get("intent");
          assert(intent);
          const binding = await createPortalBinding();
          const location = {
            authUrl: runtime.trellisUrl,
            portalOrigin: runtime.publicOrigin,
          };
          const flowId = await startPortalTransaction(
            location,
            intent,
            binding,
          );
          const response = await fetch(
            `${runtime.trellisUrl}/auth/login/local`,
            {
              method: "POST",
              headers: {
                "content-type": "application/json",
                origin: runtime.publicOrigin,
                "trellis-portal-binding": binding.secret,
              },
              body: JSON.stringify({
                transactionId: flowId,
                username,
                password,
                portalBindingDigest: binding.digest,
              }),
            },
          );
          assertEquals(response.status, 200, await response.clone().text());
          await response.arrayBuffer();
          const state = await fetchPortalFlowState(location, flowId, binding);
          if (state.status === "approval_required") {
            assert(
              allowConsent,
              "Login must not replace the administrator's read-only grant with a broader consent approval",
            );
            const approved = await submitPortalApproval(
              location,
              flowId,
              binding,
              "approved",
            );
            assertEquals(approved.status, "redirect");
          } else assertEquals(state.status, "redirect");
          return { status: "bound" as const, transactionId: flowId };
        },
      }).orThrow();
    }

    const resourceParticipant = (await admin.participantsGet({
      participantId: runtimeParticipants.StateCaller.participant.id,
    }).orThrow()).participant;
    await admin.grantsSet({
      ownerKind: "user",
      ownerId: targetId,
      participantId: resourceParticipant.participantId,
      installedRevision: resourceParticipant.revision,
      expectedRevision: 0n,
      grants: resourceParticipant.requiredGrants,
      platformPrivileges: [],
      expiresAt: null,
      idempotencyKey: ulid(),
    }).orThrow();

    const user = await login(
      "managed-before",
      participants.Console.participant,
    );
    try {
      const me = await user.sessionsMe({}).orThrow();
      assertEquals(me.user?.userId, targetId);
      assertEquals(me.connection.platformPrivileges, []);
      assertEquals(
        (await user.userIdentitiesList({}).orThrow()).items[0].principalId,
        targetId,
      );
      assert(
        (await user.userIdentitiesList({ userId: other.user.userId })).isErr(),
      );
      assert(
        (await user.userIdentitiesUnlink({
          userId: other.user.userId,
          providerId: "local",
          subject: "other-login",
          idempotencyKey: ulid(),
        })).isErr(),
      );
      assert(
        (await user.usersIdentityLinkCreate({
          userId: other.user.userId,
          allowedProviders: [],
          returnTarget: null,
          idempotencyKey: ulid(),
        })).isErr(),
      );
      assertEquals(
        (await admin.userIdentitiesList({ userId: other.user.userId })
          .orThrow()).items[0].subject,
        "other-login",
      );

      const revoked = await admin.grantsRevoke({
        ownerKind: "user",
        ownerId: targetId,
        participantId: participant.participantId,
        expectedRevision: grant.binding.revision,
        reason: "Verify access restoration",
        idempotencyKey: ulid(),
      }).orThrow();
      assertEquals(revoked.binding.state, "revoked");
      const restored = await admin.grantsSet({
        ownerKind: "user",
        ownerId: targetId,
        participantId: participant.participantId,
        installedRevision: participant.revision,
        expectedRevision: revoked.binding.revision,
        grants: participant.requiredGrants,
        platformPrivileges: ["trellis.auth::admin"],
        expiresAt: null,
        idempotencyKey: ulid(),
      }).orThrow();
      assertEquals(
        (await admin.grantsGet({
          ownerKind: "user",
          ownerId: targetId,
          participantId: participant.participantId,
        }).orThrow()).binding?.grants,
        restored.binding.grants,
      );

      const loaded =
        (await admin.usersGet({ userId: targetId }).orThrow()).user;
      const updateInput = {
        userId: targetId,
        username: "  Managed-After  ",
        name: "Renamed account",
        email: "renamed@example.com",
        image: null,
        state: "active" as const,
        expectedVersion: loaded.version,
        idempotencyKey: ulid(),
      };
      const updated = await admin.usersUpdate(updateInput).orThrow();
      assertEquals(updated.user.username, "managed-after");
      const replayed = await admin.usersUpdate(updateInput).orThrow();
      assertEquals(replayed.user, updated.user);
      assertEquals(
        (await admin.usersGet({ userId: targetId }).orThrow()).user,
        updated.user,
      );
      const staleUpdate = await admin.usersUpdate({
        ...updateInput,
        idempotencyKey: ulid(),
        name: "Stale update must not commit",
      });
      assert(staleUpdate.isErr());
      assert(staleUpdate.error instanceof RpcAuthError);
      assertEquals(staleUpdate.error.data.code, "conflict");
      assertEquals(
        (await admin.usersGet({ userId: targetId }).orThrow()).user,
        updated.user,
      );
      assertEquals(
        (await admin.userIdentitiesList({ userId: targetId }).orThrow())
          .items[0].subject,
        "managed-after",
      );
      assert(
        (await admin.usersResolve({
          selector: new TextEncoder().encode(
            JSON.stringify({
              kind: "provider",
              providerId: "local",
              providerSubject: "managed-before",
            }),
          ),
        })).isErr(),
      );
      const renamedLogin = await login(
        "managed-after",
        participants.Console.participant,
      );
      try {
        assertEquals(
          (await renamedLogin.sessionsMe({}).orThrow()).user?.userId,
          targetId,
        );
        assertEquals(
          (await renamedLogin.userIdentitiesList({ userId: other.user.userId })
            .orThrow()).items[0].subject,
          "other-login",
        );
      } finally {
        await renamedLogin.connection.close();
      }

      assert(
        (await admin.usersUpdate({
          userId: targetId,
          username: "other-login",
          name: "Must not commit",
          email: null,
          image: null,
          state: "active",
          expectedVersion: updated.user.version,
          idempotencyKey: ulid(),
        })).isErr(),
      );
      const unchanged =
        (await admin.usersGet({ userId: targetId }).orThrow()).user;
      assertEquals(unchanged.username, "managed-after");
      assertEquals(unchanged.name, "Renamed account");
      assertEquals(unchanged.version, updated.user.version);
      const link = await admin.usersIdentityLinkCreate({
        userId: targetId,
        allowedProviders: [],
        returnTarget: null,
        idempotencyKey: ulid(),
      }).orThrow();
      assertEquals(link.flow.targetPrincipalId, targetId);
      assert(
        (await admin.userIdentitiesUnlink({
          userId: targetId,
          providerId: "local",
          subject: "managed-after",
          idempotencyKey: ulid(),
        })).isErr(),
      );
      const connections = await admin.connectionsList({ principalId: targetId })
        .orThrow();
      assert(connections.items.length > 0);
      assert(
        connections.items.every((connection) =>
          connection.principalId === targetId
        ),
      );

      const stateClient = await login(
        "managed-after",
        runtimeParticipants.StateCaller.participant,
      );
      try {
        await stateClient.state.savedResource.set({ value: "approved write" })
          .orThrow();
        assertEquals(
          (await stateClient.state.savedResource.get().orThrow())?.value,
          { value: "approved write" },
        );
      } finally {
        await stateClient.connection.close();
      }

      const currentResourceGrant = (await admin.grantsGet({
        ownerKind: "user",
        ownerId: targetId,
        participantId: resourceParticipant.participantId,
      }).orThrow()).binding;
      assert(currentResourceGrant);
      await admin.grantsSet({
        ownerKind: "user",
        ownerId: targetId,
        participantId: resourceParticipant.participantId,
        installedRevision: resourceParticipant.revision,
        expectedRevision: currentResourceGrant.revision,
        grants: {
          ...resourceParticipant.requiredGrants,
          permissions: resourceParticipant.requiredGrants.permissions.filter(
            (permission) => {
              const target: unknown = JSON.parse(
                new TextDecoder().decode(permission.target),
              );
              return !(typeof target === "object" && target !== null &&
                "kind" in target && target.kind === "participantResource") ||
                permission.action === "read";
            },
          ),
        },
        platformPrivileges: [],
        expiresAt: null,
        idempotencyKey: ulid(),
      }).orThrow();
      const readOnlyClient = await login(
        "managed-after",
        runtimeParticipants.StateCaller.participant,
        false,
      );
      try {
        assertEquals(
          (await readOnlyClient.state.savedResource.get().orThrow())?.value,
          { value: "approved write" },
        );
        assert(
          (await readOnlyClient.state.savedResource.set({
            value: "must not write",
          })).isErr(),
        );
        assertEquals(
          (await readOnlyClient.state.savedResource.get().orThrow())?.value,
          { value: "approved write" },
        );
      } finally {
        await readOnlyClient.connection.close();
      }
    } finally {
      await user.connection.close();
      await admin.connection.close();
    }
  });
});
