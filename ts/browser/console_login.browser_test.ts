import { assertEquals } from "@std/assert";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeAdminBootstrapInBrowser,
  completeConsoleEntry,
  launchProfile,
  signInIfPrompted,
  waitForConsoleReady,
} from "./browser_test_support.ts";
import { openConsoleAsRuntimeAdmin } from "./console_test_support.ts";

Deno.test("browser network restoration wakes renewal backoff before the next RPC times out", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
          waitUntil: "domcontentloaded",
        });
        await page.getByLabel("Search users").waitFor({ state: "visible" });
        await page.locator(".users-row").first().waitFor({ state: "visible" });

        let failedRenewals = 0;
        page.on("requestfailed", (request) => {
          if (
            request.method() === "POST" &&
            new URL(request.url()).pathname === "/auth/context/refresh"
          ) failedRenewals++;
        });
        // Let real credentials expire and actual renewal failures enter backoff.
        // No fake clock, authorization response, or SDK hook.
        await context.setOffline(true);
        await runtime.waitFor(() => failedRenewals >= 3, { timeoutMs: 60_000 });

        // This row did not exist when the browser disconnected. Showing it proves
        // a fresh authenticated RPC, not merely redisplaying cached data.
        const username = `resume-${crypto.randomUUID().slice(0, 8)}`;
        await runtime.callAdminRpc("authUsersCreate", {
          email: null,
          idempotencyKey: crypto.randomUUID(),
          image: null,
          name: username,
          username,
        });
        await page.getByLabel("Search users").fill(username);
        await Promise.all([
          page.waitForResponse(
            (response) =>
              new URL(response.url()).pathname === "/auth/context/refresh" &&
              response.ok(),
            { timeout: 3_000 },
          ),
          (async () => {
            await context.setOffline(false);
            await page.getByLabel("Search users").press("Enter");
            await page.locator(".users-row", { hasText: username }).waitFor({
              state: "visible",
              timeout: 3_000,
            });
          })(),
        ]);
        assertEquals(await page.locator(".users-row").count(), 1);
      } finally {
        await context.close();
      }
    },
    browserRuntimeOptions({
      authorization: {
        contextLifetimeSeconds: 76,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    }),
  );
});

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

Deno.test("idle signed-intent portal survives reload and completes the initiating Console login", async () => {
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
      const intent = new URL(page.url()).searchParams.get("intent");
      assertEquals(typeof intent, "string");
      await page.reload({ waitUntil: "domcontentloaded" });
      await page
        .getByLabel("Username", { exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(new URL(page.url()).searchParams.get("intent"), intent);

      await signInIfPrompted(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await waitForConsoleReady(page);
      assertEquals(pageErrors, []);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("portal recovers an accepted transaction start after losing its response and reloading", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const context = await launchProfile(runtime);
    try {
      let acceptedTransactionId: string | undefined;
      await context.route("**/auth/transactions", async (route) => {
        const response = await route.fetch();
        assertEquals(response.status(), 200);
        acceptedTransactionId = (await response.json()).transactionId;
        await route.abort("failed");
      }, { times: 1 });
      const page = await context.newPage();
      await page.goto(`${runtime.trellisUrl}/console`, {
        waitUntil: "domcontentloaded",
      });
      await page.getByLabel("Username", { exact: true }).fill(
        runtime.adminUsername,
      );
      await page.getByLabel("Password", { exact: true }).fill(
        runtime.adminPassword,
      );
      const failedStart = page.waitForEvent(
        "requestfailed",
        (request) => new URL(request.url()).pathname === "/auth/transactions",
      );
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await failedStart;
      await page.reload({ waitUntil: "domcontentloaded" });
      const retriedStart = page.waitForResponse((response) =>
        new URL(response.url()).pathname === "/auth/transactions"
      );
      const resumedLogin = page.waitForRequest((request) =>
        new URL(request.url()).pathname === "/auth/login/local" &&
        request.method() === "POST"
      );
      await signInIfPrompted(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      const response = await retriedStart;
      assertEquals(response.status(), 200);
      assertEquals(
        (await resumedLogin).postDataJSON().transactionId,
        acceptedTransactionId,
      );
      await waitForConsoleReady(page);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
