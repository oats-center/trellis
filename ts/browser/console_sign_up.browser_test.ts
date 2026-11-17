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

test("an administrator sets sign-in methods, providers, their group roles and the sign-in page in the Console, and a newcomer creates an account on the Portal", async () => {
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
      await admin.goto(`${runtime.consoleUrl}/console/admin/roles/new`);
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
        await page.goto(`${runtime.consoleUrl}/console/`);
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

      // One Sign-in item leads to the methods and, on a tab, the appearance.
      await admin
        .getByRole("link", { name: "Sign-in", exact: true })
        .first()
        .click();
      await admin.waitForURL(/\/console\/admin\/sign-in$/);
      const tab = (name: string) =>
        admin
          .getByRole("navigation", { name: "Sign-in" })
          .getByRole("link", { name, exact: true });
      const selfSignUp = admin.getByRole("switch", {
        name: "Users can create their own account",
      });
      const minimum = admin.getByLabel("Minimum password length");
      await selfSignUp.check();
      await minimum.fill("20");
      await admin.getByText("Unsaved changes", { exact: true }).waitFor();
      await admin
        .getByRole("region", { name: "Local users" })
        .getByRole("button", { name: "Save", exact: true })
        .click();
      await admin.getByText("Local users saved.", { exact: true }).waitFor();
      await admin.reload();
      await minimum.waitFor();
      assert.equal(await selfSignUp.isChecked(), true);
      assert.equal(await minimum.inputValue(), "20");

      await tab("Appearance").click();
      await admin.waitForURL(/\/console\/admin\/sign-in\/appearance$/);
      await admin.getByLabel("Footer").fill("Acme support: help@acme.test");
      await admin.getByRole("button", { name: "Save", exact: true }).click();
      await admin.getByText(/^Saved\./).waitFor();
      await tab("Methods").click();
      await admin.waitForURL(/\/console\/admin\/sign-in$/);

      // Without a provider that is on, OIDC sign-in is off.
      const status = (text: string) =>
        admin.getByText(text, { exact: true }).waitFor();
      await status("No provider is on, so OIDC sign-in is off.");
      // The same page keeps OIDC providers and the roles their
      // groups give.
      const edit = async (name: string) => {
        await admin
          .getByRole("button", { name: `Actions for ${name}`, exact: true })
          .click();
        await admin.getByRole("button", { name: "Edit", exact: true }).click();
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
      await status(
        "OIDC sign-in is on: users can sign in through Acme login or Beta login.",
      );
      // Each provider turns on and off in its row.
      const beta = admin.getByRole("switch", { name: "Beta login" });
      await beta.uncheck();
      await admin
        .getByText("Beta login turned off.", { exact: true })
        .waitFor();
      await status("OIDC sign-in is on: users can sign in through Acme login.");
      await beta.check();
      await status(
        "OIDC sign-in is on: users can sign in through Acme login or Beta login.",
      );

      // A draft keeps the revision it was made from across a provider change,
      // so saving it cannot undo another administrator's change.
      const saveLocal = (page: typeof admin) =>
        page
          .getByRole("region", { name: "Local users" })
          .getByRole("button", { name: "Save", exact: true })
          .click();
      await minimum.fill("8");
      const other = await context.newPage();
      await other.goto(`${runtime.consoleUrl}/console/admin/sign-in`);
      const otherSignUp = other.getByRole("switch", {
        name: "Users can create their own account",
      });
      await otherSignUp.uncheck();
      await saveLocal(other);
      await other.getByText("Local users saved.", { exact: true }).waitFor();
      await beta.uncheck();
      await admin
        .getByText("Beta login turned off.", { exact: true })
        .waitFor();
      await saveLocal(admin);
      await admin
        .getByText(/^Another administrator changed these settings/)
        .waitFor();
      await other.reload();
      const otherMinimum = other.getByLabel("Minimum password length");
      await otherMinimum.waitFor();
      assert.equal(await otherSignUp.isChecked(), false);
      assert.equal(await otherMinimum.inputValue(), "20");
      await other.close();
      await admin
        .getByRole("button", { name: "Reload settings", exact: true })
        .click();
      await admin
        .getByRole("switch", {
          name: "Users can create their own account",
          checked: false,
        })
        .waitFor();
      assert.equal(await minimum.inputValue(), "20");
      assert.equal(
        await admin
          .getByText(/^Another administrator changed these settings/)
          .count(),
        0,
      );
      await selfSignUp.check();
      await minimum.fill("8");
      await saveLocal(admin);
      await admin.getByText("Local users saved.", { exact: true }).waitFor();
      await beta.check();
      await status(
        "OIDC sign-in is on: users can sign in through Acme login or Beta login.",
      );
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

      await admin.goto(`${runtime.consoleUrl}/console/admin/clients`);
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
      // The saved appearance and minimum reach the Portal at once.
      await newcomer.getByText("Acme support: help@acme.test").waitFor();
      await newcomer
        .getByRole("button", { name: "Create an account", exact: true })
        .click();
      await newcomer
        .getByText("At least 8 characters.", { exact: true })
        .waitFor();
      await newcomer.getByLabel("Name", { exact: true }).fill("New Person");
      await newcomer.getByLabel("Username", { exact: true }).fill(username);
      // The minimum counts characters: seven emoji are 14 UTF-16 code units
      // and 28 bytes but too short; eight mixed characters are enough.
      const password = newcomer.getByLabel("Password", { exact: true });
      await password.fill("🔑".repeat(7));
      const create = newcomer.getByRole("button", {
        name: "Create account",
        exact: true,
      });
      await create.click();
      await newcomer
        .getByRole("alert")
        .getByText("Use at least 8 characters.", { exact: true })
        .waitFor();
      assert.equal(await password.getAttribute("aria-invalid"), "true");
      await password.fill("Grüße🔑ok");
      await create.click();
      await newcomer
        .getByRole("heading", { name: "Your account", exact: true })
        .waitFor();
      assert.match(await newcomer.locator("main").innerText(), /New Person/);

      // The new account holds the role marked for new users.
      await admin.goto(`${runtime.consoleUrl}/console/admin/users`);
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
