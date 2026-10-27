import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { ulid } from "ulid";
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
