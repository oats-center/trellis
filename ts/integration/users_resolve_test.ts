import assert from "node:assert/strict";
import { test } from "node:test";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("generated Users.Resolve retrieves the authenticated OAuth account", async () => {
  await withTrellisRuntime(async (runtime) => {
    const resolved = await runtime.callAdminRpc("authUsersResolve", {});
    assert.equal(resolved.user.account?.username, runtime.adminUsername);
    const current = await runtime.callAdminRpc("authSessionsList", {});
    assert(
      current.items.some(
        (login) => login.principalId === resolved.user.principalId,
      ),
    );
  });
});
