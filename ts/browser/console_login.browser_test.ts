import { assertEquals } from "@std/assert";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeAdminBootstrapInBrowser,
  completeConsoleEntry,
  launchProfile,
  openConsole,
  waitForConsoleReady,
} from "./browser_test_support.ts";

Deno.test("browser admin bootstrap reaches the authorized console and survives reload", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await completeAdminBootstrapInBrowser(page, runtime, BROWSER_ADMIN);
      await completeConsoleEntry(page, BROWSER_ADMIN);
      await waitForConsoleReady(page);

      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleReady(page);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("expired browser sign-in flow recovers without request errors", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const pageErrors: string[] = [];
      page.on("pageerror", (error) => pageErrors.push(String(error)));

      await page.goto(`${runtime.trellisUrl}/console`, {
        waitUntil: "domcontentloaded",
      });
      await page
        .getByLabel("Username", { exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });
      await new Promise((resolve) => setTimeout(resolve, 10_000));
      await page.reload({ waitUntil: "domcontentloaded" });
      await page
        .getByText("Session expired")
        .waitFor({ state: "visible", timeout: 30_000 });

      await openConsole(page, runtime, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      assertEquals(pageErrors, []);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions({ ttlMs: { pendingAuth: 8_000 } }));
});
