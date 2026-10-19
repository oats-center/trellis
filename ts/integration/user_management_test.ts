/** Ordinary user administration through generated APIs and real native OAuth. */
import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("user updates preserve identity while disabling retires authorization and re-enable permits new login", async () => {
  await withTrellisRuntime(async (runtime) => {
    const clientId = "user-management-native";
    await runtime.callAdminRpc("authOAuthClientsPut", {
      clientId,
      kind: "native",
      title: "User management",
      description: "User lifecycle caller",
      redirectUris: ["http://127.0.0.1/callback"],
      development: true,
      impliedConsent: [],
      expectedRevision: 0n,
      idempotencyKey: ulid(),
    });
    const password = "user-management-password-123";
    const create = {
      username: "managed-user",
      displayName: "Before",
      password,
      idempotencyKey: ulid(),
    };
    const created = (await runtime.callAdminRpc("authUsersCreate", create))
      .user;
    assert.equal(
      (await runtime.callAdminRpc("authUsersCreate", create)).user.principalId,
      created.principalId,
    );
    const userId = created.principalId;
    const required = [
      ...new Set([
        ...adminParticipant.requiredCapabilities,
        "trellis.auth@v1::sessionsRead",
      ]),
    ];
    for (const capabilityId of required)
      await runtime.callAdminRpc("authCapabilityGrantsGrant", {
        principalId: userId,
        capabilityId,
        expectedRevision: 0n,
        idempotencyKey: ulid(),
      });
    const login = await loginNativeTestClient({
      issuer: runtime.oauthIssuer,
      clientId,
      request: adminParticipant,
      username: create.username,
      password,
      approvedCapabilities: required,
    });
    const caller = await connectNativeClient({
      ...login,
      request: adminParticipant,
    }).orThrow();
    try {
      const original = await caller.sessionsMe({}).orThrow();
      assert.equal(original.principal.principalId, userId);
      assert.equal(
        (await caller.usersResolve({}).orThrow()).user.account?.username,
        create.username,
      );
      assert(created.account);
      const renamed = (
        await runtime.callAdminRpc("authUsersUpdate", {
          userId,
          displayName: "After",
          enabled: true,
          expectedRevision: created.account.principal.revision,
          idempotencyKey: ulid(),
        })
      ).user;
      assert.equal(
        (await runtime.callAdminRpc("authUsersGet", { userId })).user
          .displayName,
        "After",
      );
      assert.equal(renamed.principalId, original.principal.principalId);
      assert.equal(
        (await caller.sessionsMe({}).orThrow()).principal.principalId,
        userId,
      );
      const admitted = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { principalId: userId },
      );
      assert(admitted.items.length > 0);
      const originalAuthorityIds = new Set(
        admitted.items.map((session) => session.authorizationSessionId),
      );
      assert(renamed.account);
      const disabled = (
        await runtime.callAdminRpc("authUsersUpdate", {
          userId,
          displayName: renamed.displayName,
          enabled: false,
          expectedRevision: renamed.account.principal.revision,
          idempotencyKey: ulid(),
        })
      ).user;
      await runtime.waitFor(async () =>
        (await caller.sessionsMe({}, { timeout: 1000 })).isErr(),
      );
      const rejectedLogin = loginNativeTestClient({
        issuer: runtime.oauthIssuer,
        clientId,
        request: adminParticipant,
        username: create.username,
        password,
        approvedCapabilities: required,
      });
      await assert.rejects(rejectedLogin);
      assert(disabled.account);
      await runtime.callAdminRpc("authUsersUpdate", {
        userId,
        displayName: disabled.displayName,
        enabled: true,
        expectedRevision: disabled.account.principal.revision,
        idempotencyKey: ulid(),
      });
      const restored = await loginNativeTestClient({
        issuer: runtime.oauthIssuer,
        clientId,
        request: adminParticipant,
        username: create.username,
        password,
        approvedCapabilities: required,
      });
      const fresh = await connectNativeClient({
        ...restored,
        request: adminParticipant,
      }).orThrow();
      try {
        const current = await fresh.sessionsMe({}).orThrow();
        assert.equal(current.principal.principalId, userId);
        assert(current.login && original.login);
        assert.notEqual(
          current.login.loginSessionId,
          original.login.loginSessionId,
        );
        const sessions = await runtime.callAdminRpc(
          "authAuthorizationSessionsList",
          { principalId: userId },
        );
        for (const id of originalAuthorityIds)
          assert.equal(
            sessions.items.find(
              (session) => session.authorizationSessionId === id,
            )?.state,
            "revoked",
          );
      } finally {
        await fresh.connection.close();
      }
    } finally {
      await caller.connection.close();
    }
  });
});
