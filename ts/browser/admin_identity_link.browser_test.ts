import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { chromium, type Browser, type Page } from "playwright";
import { ulid } from "ulid";

import { apis } from "../packages/trellis-testkit/trellis/index.js";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  approveConsentIfRequired,
  browserRuntimeOptions,
  loginCliAsAdministrator,
  waitForConsoleConnected,
} from "./browser_test_support.ts";
import { openConsoleAsRuntimeAdmin } from "./console_test_support.ts";
import { startDex } from "./dex_support.ts";

/** Whether a rejected Auth call failed with `code`. */
function failedWith(code: string) {
  return (error: unknown) => {
    assert.equal((error as { data?: { code?: string } }).data?.code, code);
    return true;
  };
}

/** Whether a rejected CLI command printed `text`, ignoring its line wrapping. */
function printed(text: string) {
  return (error: { stderr?: string }) => {
    assert.ok(
      (error.stderr ?? "")
        .replace(/[\s│]/g, "")
        .includes(text.replace(/\s/g, "")),
      error.stderr,
    );
    return true;
  };
}

// An administrator migrating people from another installation creates their
// accounts ahead of time and links each person's OIDC sign-in, so that
// person's first sign-in through the provider lands on the account made for
// them. This catches a pre-linked sign-in creating a second account when
// sign-up is on, being refused when sign-up and local users are off, a link
// that takes another account's sign-in or half-applies, and a link that
// pretends to be verified before the provider has said anything.
test(
  "an administrator links OIDC sign-ins to accounts before the people sign in",
  { timeout: 300_000 },
  async (t) => {
    await withTrellisRuntime(async (runtime) => {
      const emails = {
        first: "first@example.com",
        second: "second@example.com",
        third: "third@example.com",
      };
      const dex = await startDex({
        directory: runtime.workdir,
        redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
        emails: Object.values(emails),
      });
      const subject = (email: string) => {
        const sub = dex.subjects.get(email);
        assert.ok(sub);
        return sub;
      };
      const browser: Browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      let page: Page | undefined;
      try {
        page = await browser.newPage();
        page.setDefaultTimeout(30_000);
        const { run } = await loginCliAsAdministrator(runtime, page);
        const secretFile = join(runtime.workdir, `dex-secret-${ulid()}`);
        await writeFile(secretFile, dex.clientSecret);
        // The provider gives a role to the person whose email it vouches for.
        const { provider } = await run(
          "sign-in",
          "providers",
          "add",
          "--name",
          "Company",
          "--issuer",
          dex.issuer,
          "--client-id",
          dex.clientId,
          "--client-secret-file",
          secretFile,
          "--certificate-fingerprint",
          dex.fingerprint,
          "--groups-claim",
          "/email",
        );
        const providerId: string = provider.providerId;
        await run(
          "roles",
          "create",
          "--title",
          "Newcomers",
          "--capability",
          "trellis.auth.v1:users.read",
          "--new-users",
        );
        // A mapped role applies to the sign-ins the provider vouched for, so
        // it shows in what the person's Console may do, not in their account.
        const staff = (
          await run(
            "roles",
            "create",
            "--title",
            "Account managers",
            "--capability",
            "trellis.auth.v1:accounts.manage",
          )
        ).role;
        await runtime.callAdminRpc(
          "authOIDCRoleMappingsPut",
          apis.auth.API.actions["rpc:OIDCRoleMappings.Put"].input.decode({
            providerId,
            externalRole: emails.first,
            roleIds: [staff.roleId],
            expectedRevision: "0",
            idempotencyKey: ulid(),
          }),
        );
        // With sign-up on, a sign-in that missed its link would make a
        // second account rather than fail.
        await run("clients", "sign-up", "trellis.console", "--providers");

        const find = async (userId: string) => {
          const found = (
            await runtime.callAdminRpc(
              "authUsersFind",
              apis.auth.API.actions["rpc:Users.Find"].input.decode({ userId }),
            )
          ).found;
          assert.ok(found?.user.account, userId);
          return found;
        };
        const subjects = async (userId: string) =>
          (await find(userId)).identities.map((identity) => identity.subject);
        const revision = async (userId: string) =>
          (await find(userId)).user.account?.principal.revision;
        const owner = async (sub: string) =>
          (
            await runtime.callAdminRpc(
              "authUsersFind",
              apis.auth.API.actions["rpc:Users.Find"].input.decode({
                identity: { providerId, subject: sub },
              }),
            )
          ).found?.user.principalId;
        const capabilities = async (principalId: string) =>
          new Map(
            (
              await runtime.callAdminRpc(
                "authPrincipalsEffectiveAccess",
                apis.auth.API.actions[
                  "rpc:Principals.EffectiveAccess"
                ].input.decode({ principalId }),
              )
            ).capabilities.map((capability) => [
              capability.definition.identity.capabilityId,
              capability.sources.map((source) => source.kind),
            ]),
          );
        const userCount = async () =>
          (
            await runtime.callAdminRpc("authUsersList", {
              page: { limit: 200 },
            })
          ).items.length;
        const activeSessions = async (principalId: string) =>
          (
            await runtime.callAdminRpc(
              "authAuthorizationSessionsList",
              apis.auth.API.actions[
                "rpc:AuthorizationSessions.List"
              ].input.decode({ principalId }),
            )
          ).items.filter((session) => session.state === "active");

        // Creating with a sign-in links it at once. The account needs no
        // password, so the CLI prints no setup code for it.
        const created = await run(
          "users",
          "create",
          "--username",
          "first",
          "--provider",
          providerId,
          "--subject",
          subject(emails.first),
        );
        assert.equal(created.setupFlow, null);
        const first: string = created.user.principalId;
        assert.deepEqual(await subjects(first), [subject(emails.first)]);
        assert.equal(await owner(subject(emails.first)), first);
        // Until the provider has spoken, the link adds nothing to the
        // profile, but the account has the role new users get.
        const unseen = await find(first);
        assert.equal(unseen.user.email, undefined);
        assert.equal(unseen.user.emailVerified, false);
        assert.deepEqual(
          (await capabilities(first)).get("trellis.auth.v1:users.read"),
          ["role"],
        );

        // The default administrator's sign-ins are only theirs to change.
        const administrator = (
          await runtime.callAdminRpc("authUsersResolve", {})
        ).user.principalId;
        await assert.rejects(
          runtime.callAdminRpc(
            "authUsersIdentitiesLink",
            apis.auth.API.actions["rpc:Users.Identities.Link"].input.decode({
              userId: administrator,
              identity: { providerId, subject: subject(emails.third) },
              expectedRevision: String(await revision(administrator)),
              idempotencyKey: ulid(),
            }),
          ),
          failedWith("access_denied"),
        );

        // Linking after creation, and again, which changes nothing.
        const second: string = (
          await run("users", "create", "--username", "second")
        ).user.principalId;
        const link = (userId: string, sub: string, ...flags: string[]) =>
          run(
            "users",
            "link",
            userId,
            "--provider",
            providerId,
            "--subject",
            sub,
            ...flags,
          );
        const linked = await link(second, subject(emails.second));
        assert.deepEqual(
          linked.identities.map(
            (identity: { subject: string }) => identity.subject,
          ),
          [subject(emails.second)],
        );
        const settled = await revision(second);
        await link(second, subject(emails.second));
        assert.equal(await revision(second), settled);
        assert.deepEqual(await subjects(second), [subject(emails.second)]);

        // Refusals change nothing: another account's sign-in, an unknown
        // provider, a stale revision, and an account that is turned off.
        await assert.rejects(
          link(second, subject(emails.first)),
          printed("This OIDC sign-in is linked to another user"),
        );
        await assert.rejects(
          run(
            "users",
            "link",
            second,
            "--provider",
            ulid(),
            "--subject",
            "anyone",
          ),
          printed("no OIDC provider has that ID"),
        );
        await assert.rejects(
          runtime.callAdminRpc(
            "authUsersIdentitiesLink",
            apis.auth.API.actions["rpc:Users.Identities.Link"].input.decode({
              userId: second,
              identity: { providerId, subject: "stale-revision" },
              expectedRevision: String(Number(settled) - 1),
              idempotencyKey: ulid(),
            }),
          ),
          failedWith("revision_conflict"),
        );
        const disabled: string = (
          await run("users", "create", "--username", "disabled")
        ).user.principalId;
        await run("users", "edit", disabled, "--inactive");
        await assert.rejects(
          link(disabled, subject(emails.third)),
          printed("the account is turned off"),
        );
        assert.deepEqual(await subjects(second), [subject(emails.second)]);
        assert.equal(await revision(second), settled);
        assert.deepEqual(await subjects(disabled), []);
        assert.equal(await owner(subject(emails.first)), first);
        assert.equal(await owner("stale-revision"), undefined);
        assert.equal(await owner(subject(emails.third)), undefined);

        // Creation with sign-ins is all or nothing: one taken sign-in leaves
        // neither the account nor its other sign-in behind.
        const createThird = (subs: string[]) =>
          runtime.callAdminRpc(
            "authUsersCreate",
            apis.auth.API.actions["rpc:Users.Create"].input.decode({
              username: "third",
              displayName: "Third",
              identities: subs.map((sub) => ({ providerId, subject: sub })),
              idempotencyKey: ulid(),
            }),
          );
        await assert.rejects(
          createThird([subject(emails.third), subject(emails.first)]),
          failedWith("identity_linked_to_another_user"),
        );
        assert.equal(await owner(subject(emails.third)), undefined);
        const third = (await createThird([subject(emails.third)])).user
          .principalId;
        assert.equal(await owner(subject(emails.third)), third);

        const signInThroughDex = async (email: string) => {
          const context = await browser.newContext();
          const person = await context.newPage();
          person.setDefaultTimeout(30_000);
          await person.goto(`${runtime.consoleUrl}/console`, {
            waitUntil: "domcontentloaded",
          });
          await person
            .getByRole("link", { name: "Continue with Company", exact: true })
            .click();
          await person.waitForURL(
            (url) => url.origin === new URL(dex.issuer).origin,
          );
          await person.locator('input[name="login"]').fill(email);
          await person.locator('input[name="password"]').fill("password");
          await person.locator('button[type="submit"]').click();
          return person;
        };

        // The first sign-in lands on the account made for the person, with
        // the provider's claims and the role its email maps to.
        const users = await userCount();
        page = await signInThroughDex(emails.first);
        await approveConsentIfRequired(page);
        await waitForConsoleConnected(page);
        assert.equal((await activeSessions(first)).length, 1);
        assert.equal(await userCount(), users);
        const seen = await find(first);
        assert.equal(seen.user.email, emails.first);
        assert.equal(seen.user.emailVerified, true);
        // Only accounts.manage, from the mapped role, opens another person's
        // account details.
        await page.goto(
          `${runtime.consoleUrl}/console/admin/users/edit?userId=${second}`,
        );
        await waitForConsoleConnected(page);
        await page
          .getByRole("heading", { name: "OIDC sign-ins", exact: true })
          .waitFor();
        await page.getByText(subject(emails.second), { exact: true }).waitFor();

        // The Console shows a person's sign-ins and links and unlinks them,
        // never the last way an account has to sign in.
        page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(
          `${runtime.consoleUrl}/console/admin/users/edit?userId=${second}`,
        );
        await waitForConsoleConnected(page);
        const row = (sub: string) =>
          page!.getByRole("listitem").filter({ hasText: sub });
        const unlinkInConsole = async (sub: string) => {
          await row(sub).getByRole("button", { name: "Unlink" }).click();
          await page!
            .getByRole("dialog")
            .getByRole("button", { name: "Unlink" })
            .click();
        };
        await row(subject(emails.second)).waitFor();
        await unlinkInConsole(subject(emails.second));
        await page
          .getByText("this is the account's only way to sign in", {
            exact: false,
          })
          .waitFor();
        await page
          .getByLabel("Subject", { exact: true })
          .fill("console-linked");
        await page.getByRole("button", { name: "Link sign-in" }).click();
        await row("console-linked").waitFor();

        // Another administrator changes the account while it is open here,
        // so linking conflicts. The panel's Reload takes the newer account
        // and keeps the unsaved name; linking and unlinking then work.
        const name = page.getByLabel("Display name", { exact: true });
        await name.fill("Unsaved name");
        await run("users", "edit", second, "--name", "Renamed elsewhere");
        await page.getByLabel("Subject", { exact: true }).fill("after-reload");
        await page.getByRole("button", { name: "Link sign-in" }).click();
        await page.getByRole("button", { name: "Reload", exact: true }).click();
        await page.getByRole("button", { name: "Link sign-in" }).click();
        await row("after-reload").waitFor();
        await unlinkInConsole("after-reload");
        await row("after-reload").waitFor({ state: "detached" });
        assert.equal(await name.inputValue(), "Unsaved name");

        await unlinkInConsole(subject(emails.second));
        await row(subject(emails.second)).waitFor({ state: "detached" });
        assert.deepEqual(await subjects(second), ["console-linked"]);
        await assert.rejects(
          run(
            "users",
            "unlink",
            second,
            "--provider",
            providerId,
            "--subject",
            "console-linked",
          ),
          printed("this is the account's only way to sign in"),
        );

        // Unlinking a sign-in someone is using signs them out of it.
        await link(first, "spare");
        await run(
          "users",
          "unlink",
          first,
          "--provider",
          providerId,
          "--subject",
          subject(emails.first),
        );
        assert.deepEqual(await subjects(first), ["spare"]);
        await runtime.waitFor(
          async () => (await activeSessions(first)).length === 0,
        );

        // With sign-up and local users off, a linked sign-in still lands on
        // its account, and one whose link was removed is refused.
        await run("clients", "sign-up", "trellis.console", "--no-providers");
        await run("sign-in", "set", "--no-local-users");
        page = await signInThroughDex(emails.third);
        await approveConsentIfRequired(page);
        await waitForConsoleConnected(page);
        assert.equal((await activeSessions(third)).length, 1);
        page = await signInThroughDex(emails.second);
        await page.waitForURL(
          (url) => url.searchParams.get("error") === "upstream_login_failed",
        );
        assert.equal(await userCount(), users);
      } catch (error) {
        if (page)
          t.diagnostic(
            `${page.url()}\n${await page.locator("body").innerText()}`,
          );
        t.diagnostic(await dex.log());
        throw error;
      } finally {
        await browser.close();
        await dex.stop();
      }
    }, browserRuntimeOptions());
  },
);
