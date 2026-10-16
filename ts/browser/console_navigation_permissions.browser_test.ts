import assert from "node:assert/strict";
import { test } from "node:test";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  waitForConsoleShell,
} from "./console_test_support.ts";

test("a denied administrative read preserves ordinary account access and navigation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const credentials = {
      username: "restricted-console-user",
      password: "restricted-console-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsole(page, runtime, credentials);
      await page.goto(`${runtime.trellisUrl}/console/profile`);
      await page.getByText("How you sign in", { exact: true }).waitFor();
      try {
        await page
          .locator("main")
          .getByText(credentials.username, { exact: true })
          .first()
          .waitFor();
      } catch (cause) {
        throw new Error(
          `Account read did not resolve: ${await page.locator("main").innerText()}`,
          { cause },
        );
      }
      await page.goto(`${runtime.trellisUrl}/console/admin/services`);
      await waitForConsoleShell(page);
      await page
        .getByText("Trellis could not reach the requested capability.", {
          exact: false,
        })
        .waitFor();
      assert.equal(new URL(page.url()).pathname, "/console/admin/services");
      await page.getByRole("link", { name: "Account", exact: true }).click();
      await page.getByText("How you sign in", { exact: true }).waitFor();
      await page
        .locator("main")
        .getByText(credentials.username, { exact: true })
        .first()
        .waitFor();
      assert.equal(new URL(page.url()).pathname, "/console/profile");
      await page.reload();
      await page
        .locator("main")
        .getByText(credentials.username, { exact: true })
        .first()
        .waitFor();
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
