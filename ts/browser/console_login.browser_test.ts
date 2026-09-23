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
      let browserExports = 0;
      let traceExports = 0;
      const captureEndpoint = Deno.env.get(
        "TRELLIS_OBS_BROWSER_CAPTURE_ENDPOINT",
      );
      if (captureEndpoint) {
        await context.route(
          "**/assets/web/runtime-config.js",
          (route) =>
            route.fulfill({
              contentType: "application/javascript",
              body:
                "globalThis.__TRELLIS_RUNTIME_CONFIG__ = { authUrl: location.origin, browserTelemetry: { enabled: true, path: '/otel', traceRatio: 1 } };",
            }),
        );
        await context.route("**/otel/v1/*", async (route) => {
          const response = await route.fetch({
            url: `${captureEndpoint}${
              new URL(route.request().url()).pathname.replace(/^\/otel/, "")
            }`,
          });
          await route.fulfill({ response });
          browserExports++;
          if (route.request().url().endsWith("/v1/traces")) traceExports++;
        });
      }
      const page = await context.newPage();
      await completeAdminBootstrapInBrowser(page, runtime, BROWSER_ADMIN);
      await completeConsoleEntry(page, BROWSER_ADMIN);
      await waitForConsoleReady(page);

      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleReady(page);
      if (captureEndpoint) {
        const traceExportsBeforeEvents = traceExports;
        await page.goto(`${runtime.trellisUrl}/console/admin/events`, {
          waitUntil: "domcontentloaded",
        });
        await page.getByText(/^Updated /).waitFor({
          state: "visible",
          timeout: 30_000,
        });
        // The page must remain alive through its configured trace batch delay.
        await page.waitForTimeout(6_000);
        await runtime.waitFor(() => browserExports > 0, { timeoutMs: 30_000 });
        await runtime.waitFor(() => traceExports > traceExportsBeforeEvents, {
          timeoutMs: 30_000,
        });
      }
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
