import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

test("Console creates, reloads, disables and deletes an OAuth client and creates a capability role", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, BROWSER_ADMIN);
      const clientId = `console-development-${ulid()}`;
      await page.goto(`${runtime.consoleUrl}/console/admin/clients`);
      await page.getByRole("button", { name: "New app", exact: true }).click();
      await page.getByLabel("App ID").fill(clientId);
      await page.getByLabel("Name", { exact: true }).fill(clientId);
      await page
        .getByRole("textbox", { name: "Return address 1", exact: true })
        .fill("http://localhost:49371/callback");
      await page.getByLabel(/Local development/).check();
      await page.getByRole("button", { name: "Save app", exact: true }).click();
      await page.getByText("App saved.", { exact: true }).waitFor();
      await page.reload();
      const row = page.getByRole("row").filter({ hasText: clientId });
      await row.getByRole("button", { name: "Edit", exact: true }).click();
      assert.equal(
        await page
          .getByRole("textbox", { name: "Return address 1", exact: true })
          .inputValue(),
        "http://localhost:49371/callback",
      );
      page.once("dialog", (dialog) => dialog.accept());
      await page.getByRole("button", { name: "Turn off", exact: true }).click();
      await row.getByText("Disabled", { exact: true }).waitFor();
      await row.getByRole("button", { name: "Edit", exact: true }).click();
      page.once("dialog", (dialog) => dialog.accept());
      await page
        .getByRole("button", { name: "Delete app", exact: true })
        .click();
      await page.getByText("App deleted.", { exact: true }).waitFor();
      assert.equal(await row.count(), 0);

      const title = `Console role ${ulid()}`;
      await page.goto(`${runtime.consoleUrl}/console/admin/roles/new`);
      await page.getByLabel("Title", { exact: true }).fill(title);
      const auth = page.getByRole("group", { name: "Auth", exact: true });
      const account = auth.getByRole("checkbox", { name: /^Your account/ });
      const profile = auth.getByRole("checkbox", { name: /See your profile/ });
      await account.check();
      await auth.getByRole("button", { name: /^More/ }).click();
      assert.equal(await profile.isChecked(), true);
      assert.equal(await profile.isDisabled(), true);
      await account.uncheck();
      await profile.check();
      assert.equal(await account.isChecked(), false);
      await auth.getByText(/1 of \d+ already included/).waitFor();
      await page
        .getByRole("button", { name: "Save role", exact: true })
        .click();
      await page.getByText(title, { exact: true }).waitFor();
      await page.reload();
      await page.getByText(title, { exact: true }).waitFor();
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
