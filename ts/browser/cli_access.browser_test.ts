import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { ulid } from "ulid";
import { apis } from "../packages/trellis-testkit/trellis/index.js";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  loginCliAsAdministrator,
} from "./browser_test_support.ts";

test(
  "CLI manages roles for new users, sign-in methods, and an app's sign-up",
  { timeout: 90_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      const page = await browser.newPage();
      page.setDefaultTimeout(10_000);
      try {
        const { run } = await loginCliAsAdministrator(runtime, page);
        // A role marked for new users is given to each user created while the
        // mark stands, and to nobody created after it is cleared.
        const { role } = await run(
          "roles",
          "create",
          "--title",
          "Readers",
          "--capability",
          "trellis.auth.v1:users.read",
          "--new-users",
        );
        const createUser = async () =>
          (await run("users", "create", "--username", `cli-${ulid()}`)).user
            .principalId;
        const before = await createUser();
        const renamed = await run(
          "roles",
          "edit",
          role.roleId,
          "--title",
          "Directory readers",
          "--no-new-users",
        );
        assert.deepEqual(
          renamed.role.capabilities.map(
            (capability: { capabilityId: string }) => capability.capabilityId,
          ),
          ["trellis.auth.v1:users.read"],
        );
        const after = await createUser();
        const readers = await run("roles", "show", role.roleId);
        assert.equal(readers.role.title, "Directory readers");
        assert.equal(readers.role.assignToNewUsers, false);
        assert.deepEqual(readers.members, [before]);
        // An administrator can still give the role to anyone, or take it away.
        await run("roles", "assign", role.roleId, after);
        await run("roles", "revoke", role.roleId, before);
        assert.deepEqual((await run("roles", "show", role.roleId)).members, [
          after,
        ]);
        await assert.rejects(run("roles", "revoke", role.roleId, before));
        await run("roles", "delete", role.roleId);
        await assert.rejects(run("roles", "show", role.roleId));
        // Recreated under the same ID, the role keeps the earlier assignment,
        // which no longer gives it. Show and revoke act on the current one.
        const put = (input: Record<string, unknown>) =>
          runtime.callAdminRpc(
            "authRolesPut",
            apis.auth.API.actions["rpc:Roles.Put"].input.decode({
              expectedRevision: "0",
              description: "",
              capabilityIds: [],
              assignToNewUsers: false,
              idempotencyKey: ulid(),
              ...input,
            }),
          );
        await put({
          roleId: role.roleId,
          title: "Directory readers",
          capabilityIds: ["trellis.auth.v1:users.read"],
        });
        await runtime.callAdminRpc(
          "authRoleAssignmentsAssign",
          apis.auth.API.actions["rpc:RoleAssignments.Assign"].input.decode({
            principalId: after,
            roleId: role.roleId,
            expectedRevision: "0",
            idempotencyKey: ulid(),
          }),
        );
        assert.deepEqual((await run("roles", "show", role.roleId)).members, [
          after,
        ]);
        await run("roles", "revoke", role.roleId, after);
        assert.deepEqual((await run("roles", "show", role.roleId)).members, []);
        // Listing follows every page rather than stopping at the first 100.
        const extra = Array.from(
          { length: 101 },
          (_, index) => `Bulk ${index}`,
        );
        for (let start = 0; start < extra.length; start += 20)
          await Promise.all(
            extra.slice(start, start + 20).map((title) => put({ title })),
          );
        const listed = (await run("roles", "list")).items.map(
          (item: { title: string }) => item.title,
        );
        for (const title of extra) assert.ok(listed.includes(title), title);
        // Sign-in methods change live, and an app chooses its own sign-up.
        assert.equal(
          (await run("sign-in", "show")).settings.localSignUp,
          false,
        );
        const opened = await run("sign-in", "set", "--local-sign-up");
        assert.equal(opened.settings.localSignUp, true);
        assert.equal(opened.settings.localUsers, true);
        await assert.rejects(
          run("sign-in", "set", "--no-local-users", "--no-providers"),
        );
        const joined = await run(
          "clients",
          "sign-up",
          "trellis.console",
          "--password",
        );
        assert.deepEqual(joined.client.signUp, {
          password: true,
          providers: false,
        });
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);
