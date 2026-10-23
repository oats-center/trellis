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
      await page.goto(`${runtime.trellisUrl}/console/admin/clients`);
      await page
        .getByRole("button", { name: "New client", exact: true })
        .click();
      await page.getByLabel("Client ID", { exact: true }).fill(clientId);
      await page.getByLabel("Title", { exact: true }).fill(clientId);
      await page
        .getByLabel("Redirect URIs (one per line)")
        .fill("http://localhost:49371/callback");
      await page.getByLabel("Loopback development client").check();
      await page
        .getByRole("button", { name: "Save client", exact: true })
        .click();
      await page.getByText("Client saved.", { exact: true }).waitFor();
      await page.reload();
      const row = page.getByRole("row").filter({ hasText: clientId });
      await row.getByRole("button", { name: "Edit", exact: true }).click();
      assert.equal(
        await page.getByLabel("Redirect URIs (one per line)").inputValue(),
        "http://localhost:49371/callback",
      );
      page.once("dialog", (dialog) => dialog.accept());
      await page
        .getByRole("button", { name: "Disable client", exact: true })
        .click();
      await row.getByText("Disabled", { exact: true }).waitFor();
      await row.getByRole("button", { name: "Edit", exact: true }).click();
      page.once("dialog", (dialog) => dialog.accept());
      await page
        .getByRole("button", { name: "Delete client", exact: true })
        .click();
      await page.getByText("Client deleted.", { exact: true }).waitFor();
      assert.equal(await row.count(), 0);

      const title = `Console role ${ulid()}`;
      await page.goto(`${runtime.trellisUrl}/console/admin/roles/new`);
      await page.getByLabel("Title", { exact: true }).fill(title);
      await page
        .getByRole("checkbox", { name: /trellis.auth@v1::readAccount/ })
        .check();
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
