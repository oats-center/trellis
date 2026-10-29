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

test("an administrator sets sign-in methods, providers and their group roles in the Console, and a newcomer creates an account on the Portal", async () => {
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
        .getByRole("group", { name: "Auth", exact: true })
        .getByRole("button", { name: /^More/ })
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
      // The same page keeps company sign-in providers and the roles their
      // groups give.
      const edit = async (name: string) => {
        await admin
          .getByRole("row")
          .filter({ hasText: name })
          .getByRole("button", { name: "Edit", exact: true })
          .click();
        await admin.getByRole("heading", { name, exact: true }).waitFor();
      };
      for (const name of ["Acme login", "Beta login"]) {
        await admin
          .getByRole("button", { name: "Add provider", exact: true })
          .click();
        await admin.getByLabel(/^Name/).fill(name);
        await admin
          .getByLabel("Address", { exact: true })
          .fill(`https://${name.split(" ")[0].toLowerCase()}.example`);
        await admin.getByLabel("Client ID", { exact: true }).fill("trellis");
        await admin.getByLabel("Client secret", { exact: true }).fill("secret");
        await admin
          .getByRole("button", { name: "Save provider", exact: true })
          .click();
        await admin.getByText("Provider saved.", { exact: true }).waitFor();
      }
      await edit("Acme login");
      await admin.getByLabel("Group", { exact: true }).fill("engineering");
      await admin.getByRole("checkbox", { name: role, exact: true }).check();
      await admin
        .getByRole("button", { name: "Add group", exact: true })
        .click();
      await admin.getByText(`→ ${role}`).waitFor();
      // Switching to another provider never shows, or removes, Acme's groups.
      await edit("Beta login");
      await admin.getByLabel("Group", { exact: true }).waitFor();
      assert.equal(await admin.getByText(`→ ${role}`).count(), 0);
      await edit("Acme login");
      await admin.getByText(`→ ${role}`).waitFor();
      await admin.reload();
      await edit("Acme login");
      await admin.getByText(`→ ${role}`).waitFor();
      await admin
        .getByRole("button", { name: "Remove engineering", exact: true })
        .click();
      await admin.getByText(`→ ${role}`).waitFor({ state: "detached" });

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
