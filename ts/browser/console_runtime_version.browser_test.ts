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

test("an ordinary user sees the connected server's version in the Console header", async () => {
  await withTrellisRuntime(async (runtime) => {
    const credentials = {
      username: "version-reader",
      password: "version-reader-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await page.goto(`${runtime.trellisUrl}/healthz`);
      const { version } = JSON.parse(
        await page.locator("body").innerText(),
      ) as { version: string };
      assert.match(version, /^\d+\.\d+\.\d+/);

      await openConsole(page, runtime, credentials);
      await waitForConsoleShell(page);
      const header = page.getByTestId("runtime-version");
      await header.getByText(`Trellis ${version}`, { exact: true }).waitFor();
      // Only administrators are told about newer releases.
      assert.equal(await header.getByRole("link").count(), 0);
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
