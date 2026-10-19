import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import type { WebSocketRoute } from "playwright";

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

test("browser carrier interruption resumes a fresh authenticated RPC without reloading", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      let interrupted = false;
      let recovered = false;
      const sockets = new Map<WebSocketRoute, WebSocketRoute>();
      await context.routeWebSocket(/.*/, (socket) => {
        const upstream = socket.connectToServer();
        upstream.onMessage((message) => {
          socket.send(message);
          if (interrupted && message.toString().split("\r\n").includes("PONG"))
            recovered = true;
        });
        sockets.set(socket, upstream);
        socket.onClose((code, reason) => {
          upstream.close({ code, reason });
          sockets.delete(socket);
        });
      });
      const page = await context.newPage();
      let tokenExchanges = 0;
      page.on("request", (request) => {
        if (
          request.method() === "POST" &&
          new URL(request.url()).pathname === "/token"
        )
          tokenExchanges++;
      });
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await page.getByLabel("Search users").waitFor({ state: "visible" });
      await page.locator(".users-row").first().waitFor({ state: "visible" });

      assert(
        sockets.size > 0,
        "the browser has a real broker carrier to interrupt",
      );
      const exchangesBeforeInterruption = tokenExchanges;
      assert(
        exchangesBeforeInterruption > 0,
        "the browser completed a real OAuth token exchange",
      );
      interrupted = true;
      // Close real broker carriers without replacing protocol replies or
      // constructing a mock pre-handshake WebSocket on reconnect.
      for (const [socket, upstream] of sockets) {
        socket.close({ code: 1012, reason: "network unavailable" });
        upstream.close({ code: 1012, reason: "network unavailable" });
      }

      // This row did not exist when the browser disconnected. Showing it proves
      // a fresh authenticated RPC, not merely redisplaying cached data.
      const username = `resume-${ulid()}`.toLowerCase();
      await runtime.callAdminRpc("authUsersCreate", {
        idempotencyKey: ulid(),
        displayName: username,
        username,
      });
      await page.getByLabel("Search users").fill(username);
      await runtime.waitFor(() => recovered, { timeoutMs: 10_000 });
      await page.getByLabel("Search users").press("Enter");
      try {
        await page.locator(".users-row", { hasText: username }).waitFor({
          state: "visible",
          timeout: 3_000,
        });
      } catch (cause) {
        throw new Error(
          `Fresh browser user read did not resume: ${await page.locator("main").innerText()}`,
          { cause },
        );
      }
      assert.deepEqual(await page.locator(".users-row").count(), 1);
      assert.equal(
        tokenExchanges,
        exchangesBeforeInterruption,
        "recovering a carrier with valid admission must not spend refresh custody",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("browser admin bootstrap reaches the authorized console and survives reload", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const context = await launchProfile(runtime);
      try {
        let browserExports = 0;
        let traceExports = 0;
        const captureEndpoint =
          process.env["TRELLIS_OBS_BROWSER_CAPTURE_ENDPOINT"];
        if (captureEndpoint) {
          await context.route("**/assets/web/runtime-config.js", (route) =>
            route.fulfill({
              contentType: "application/javascript",
              body: "globalThis.__TRELLIS_RUNTIME_CONFIG__ = { authUrl: location.origin, browserTelemetry: { enabled: true, path: '/otel', traceRatio: 1 } };",
            }),
          );
          await context.route("**/otel/v1/*", async (route) => {
            const response = await route.fetch({
              url: `${captureEndpoint}${new URL(
                route.request().url(),
              ).pathname.replace(/^\/otel/, "")}`,
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
          await runtime.waitFor(() => browserExports > 0, {
            timeoutMs: 30_000,
          });
          await runtime.waitFor(() => traceExports > traceExportsBeforeEvents, {
            timeoutMs: 30_000,
          });
        }
      } finally {
        await context.close();
      }
    },
    browserRuntimeOptions({ firstAdmin: "browser-flow" }),
  );
});

test("an OAuth authorization page survives reload and completes the initiating Console login", async () => {
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
      const authorizationUrl = page.url();
      await page.reload({ waitUntil: "domcontentloaded" });
      await page
        .getByLabel("Username", { exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });
      assert.equal(page.url(), authorizationUrl);

      await signInIfPrompted(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await waitForConsoleReady(page);
      assert.deepEqual(pageErrors, []);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
