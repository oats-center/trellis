import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium, type Page } from "playwright";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  browserRuntimeOptions,
  completeConsoleEntry,
} from "./browser_test_support.ts";

// The page backgrounds of the light and dark palettes (#f3f0e9, #11151b).
const LIGHT = "rgb(243, 240, 233)";
const DARK = "rgb(17, 21, 27)";

const background = (page: Page) =>
  page.evaluate(
    () => getComputedStyle(document.documentElement).backgroundColor,
  );

test("light or dark follows the device until the Console's choice overrides it, in the Portal too", async () => {
  await withTrellisRuntime(async (runtime) => {
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    try {
      const context = await browser.newContext({ colorScheme: "dark" });
      const page = await context.newPage();

      // No saved choice: the Portal's sign-in follows the dark device.
      await page.goto(`${runtime.consoleUrl}/console/login`, {
        waitUntil: "domcontentloaded",
      });
      await page.getByLabel("Username", { exact: true }).first().waitFor();
      assert.equal(await background(page), DARK);

      await completeConsoleEntry(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      assert.equal(await background(page), DARK);
      const toggle = page.getByRole("button", { name: /^Theme: / });
      assert.equal(await toggle.getAttribute("aria-label"), "Theme: System");

      // While on System, the Console follows the device as it changes.
      await page.emulateMedia({ colorScheme: "light" });
      await page.waitForFunction(
        (light) =>
          getComputedStyle(document.documentElement).backgroundColor === light,
        LIGHT,
      );
      await page.emulateMedia({ colorScheme: "dark" });
      await page.waitForFunction(
        (dark) =>
          getComputedStyle(document.documentElement).backgroundColor === dark,
        DARK,
      );

      // Choosing Light wins over the dark device, here and in the Portal.
      await toggle.click();
      assert.equal(await toggle.getAttribute("aria-label"), "Theme: Light");
      assert.equal(await background(page), LIGHT);

      const portal = await context.newPage();
      await portal.goto(`${runtime.trellisUrl}/device`, {
        waitUntil: "domcontentloaded",
      });
      await portal.locator("h1").first().waitFor();
      assert.equal(await background(portal), LIGHT);

      // The choice survives a reload of the Console.
      await page.reload({ waitUntil: "domcontentloaded" });
      assert.equal(await background(page), LIGHT);
    } finally {
      await browser.close();
    }
  }, browserRuntimeOptions());
});
