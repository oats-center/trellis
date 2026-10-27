import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { ulid } from "ulid";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

test("an administrator opens self sign-up in the Console and a newcomer creates an account on the Portal", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    try {
      const admin = await context.newPage();
      await openConsole(admin, runtime, BROWSER_ADMIN);
      const role = `Newcomers ${ulid()}`;
      await admin.goto(`${runtime.trellisUrl}/console/admin/roles/new`);
      await admin.getByLabel("Title", { exact: true }).fill(role);
      await admin
        .getByRole("group", { name: "Account", exact: true })
        .first()
        .getByRole("button")
        .click();
      await admin.getByRole("checkbox", { name: /See your profile/ }).check();
      await admin.getByRole("checkbox", { name: /Give to new users/ }).check();
      await admin
        .getByRole("button", { name: "Save role", exact: true })
        .click();
      await admin.getByText(role, { exact: true }).waitFor();

      // Signing up needs the installation and the app to allow it.
      const signUp = async () => {
        const page = await browser.newPage();
        await page.goto(`${runtime.trellisUrl}/console/`);
        await page
          .getByRole("heading", { name: "Sign in", exact: true })
          .waitFor();
        return page;
      };
      const closed = await signUp();
      await closed.getByLabel("Username", { exact: true }).waitFor();
      assert.equal(
        await closed.getByRole("button", { name: "Create an account" }).count(),
        0,
      );
      await closed.close();

      await admin.goto(`${runtime.trellisUrl}/console/admin/sign-in`);
      await admin
        .getByRole("checkbox", { name: /People can create their own account/ })
        .check();
      await admin.getByRole("button", { name: "Save", exact: true }).click();
      await admin
        .getByText("Sign-in methods saved.", { exact: true })
        .waitFor();
      await admin.goto(`${runtime.trellisUrl}/console/admin/clients`);
      await admin
        .getByRole("row")
        .filter({ hasText: "trellis.console" })
        .getByRole("button", { name: "Edit", exact: true })
        .click();
      await admin
        .getByRole("checkbox", { name: /With a username and password/ })
        .check();
      await admin
        .getByRole("button", { name: "Save app", exact: true })
        .click();
      await admin.getByText("App saved.", { exact: true }).waitFor();

      const username = `newcomer-${ulid().toLowerCase()}`;
      const newcomer = await signUp();
      await newcomer
        .getByRole("button", { name: "Create an account", exact: true })
        .click();
      await newcomer.getByLabel("Name", { exact: true }).fill("New Person");
      await newcomer.getByLabel("Username", { exact: true }).fill(username);
      await newcomer
        .getByLabel("Password", { exact: true })
        .fill("a long enough password");
      await newcomer
        .getByRole("button", { name: "Create account", exact: true })
        .click();
      await newcomer
        .getByRole("heading", { name: /wants to use your account/ })
        .waitFor();
      await newcomer.getByText(username).waitFor();
      // The role lets the newcomer allow the Console to show their profile.
      for (const checkbox of await newcomer.getByRole("checkbox").all())
        if (await checkbox.isEnabled()) await checkbox.check();
      await newcomer
        .getByRole("button", { name: "Allow", exact: true })
        .click();
      await newcomer
        .getByRole("heading", { name: "Your account", exact: true })
        .waitFor();
      assert.match(await newcomer.locator("main").innerText(), /New Person/);

      // The new account holds the role marked for new users.
      await admin.goto(`${runtime.trellisUrl}/console/admin/users`);
      await admin
        .getByRole("row")
        .filter({ hasText: username })
        .getByText(role)
        .waitFor();
    } finally {
      await browser.close();
      await context.close();
    }
  }, browserRuntimeOptions());
});
