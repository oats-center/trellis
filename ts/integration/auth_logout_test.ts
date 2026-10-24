import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("revoking a closed login is idempotent and preserves a separate login", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "already-revoked-logout",
      contract: adminParticipant,
    });
    const session = (await client.sessionsMe({}).orThrow()).login;
    assert(session != null);
    const other = await runtime.connectClient({
      name: "separate-login",
      contract: adminParticipant,
    });
    const separate = (await other.sessionsMe({}).orThrow()).login;
    assert(separate);
    await client.connection.close();
    const revoke = {
      loginSessionId: session.loginSessionId,
      idempotencyKey: ulid(),
    };
    await runtime.callAdminRpc("authSessionsRevoke", revoke);
    await runtime.callAdminRpc("authSessionsRevoke", revoke);
    assert.deepEqual(
      (
        await runtime.callAdminRpc("authSessionsList", {
          principalId: session.principalId,
        })
      ).items.find((entry) => entry.loginSessionId === session.loginSessionId)
        ?.state,
      "revoked",
    );
    assert.equal(
      (await other.sessionsMe({}).orThrow()).login?.loginSessionId,
      separate.loginSessionId,
    );
    await other.connection.close();
  });
});

test("authorization-session revocation preserves the login and permits fresh admission", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "authority-revoked-logout",
      contract: adminParticipant,
    });
    const before = await client.sessionsMe({}).orThrow();
    const session = before.login;
    assert(session != null);
    await runtime.callAdminRpc("authAuthorizationSessionsRevoke", {
      authorizationSessionId: before.authorizationSessionId,
      idempotencyKey: ulid(),
    });
    const after = await runtime.waitFor(async () => {
      const result = await client.sessionsMe({}, { timeout: 1000 });
      if (result.isErr()) return false;
      const value = result.orThrow();
      return value.authorizationSessionId !== before.authorizationSessionId
        ? value
        : false;
    });
    assert.equal(after.login?.loginSessionId, session.loginSessionId);
    assert.deepEqual(
      (
        await runtime.callAdminRpc("authSessionsList", {
          principalId: session.principalId,
        })
      ).items.find((entry) => entry.loginSessionId === session.loginSessionId)
        ?.state,
      "active",
    );
    await client.connection.close();
  });
});
