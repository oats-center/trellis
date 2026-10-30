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

test("Console sign-in recovery returns to the protected page it left", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      // Auth recovery sends the Console here; with no session left, sign-in
      // leaves for the authorization server and comes back to bare /login.
      await page.goto(
        `${runtime.consoleUrl}/login?redirectTo=${encodeURIComponent("/console/admin/users?search=admin")}`,
        { waitUntil: "domcontentloaded" },
      );
      await completeConsoleEntry(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await page.waitForURL(
        (url) =>
          url.pathname === "/console/admin/users" &&
          url.searchParams.get("search") === "admin",
        { timeout: 30_000 },
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("Console reaches NATS on the Trellis origin when the runtime relays WebSockets", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const sockets: URL[] = [];
        page.on("websocket", (socket) => sockets.push(new URL(socket.url())));
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.consoleUrl}/console/admin/users`, {
          waitUntil: "domcontentloaded",
        });
        // Listed users arrive over an authenticated NATS RPC.
        await page.locator(".users-row").first().waitFor({ state: "visible" });
        assert(sockets.length > 0, "the Console opened a broker WebSocket");
        const trellis = new URL(runtime.trellisUrl);
        for (const socket of sockets) {
          assert.equal(socket.protocol, "wss:");
          assert.equal(socket.host, trellis.host);
        }
      } finally {
        await context.close();
      }
    },
    browserRuntimeOptions({ sameOriginWebsocket: true }),
  );
});

test("a development installation signs the Console in and reaches NATS on the Trellis port", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const trellis = new URL(runtime.trellisUrl);
        const signIn: URL[] = [];
        page.on("framenavigated", (frame) => {
          const url = new URL(frame.url());
          if (url.host === trellis.host) signIn.push(url);
        });
        const sockets: URL[] = [];
        page.on("websocket", (socket) => sockets.push(new URL(socket.url())));
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.consoleUrl}/console/admin/users`, {
          waitUntil: "domcontentloaded",
        });
        // Listed users arrive over an authenticated NATS RPC.
        await page.locator(".users-row").first().waitFor({ state: "visible" });
        // Its certificates are self-generated, and Console says so.
        await page
          .getByRole("status")
          .filter({ hasText: "using self-generated certificates" })
          .first()
          .waitFor({ state: "visible", timeout: 15_000 });
        assert(signIn.length > 0, "sign-in ran on the Trellis origin");
        for (const url of sockets)
          assert.equal(url.host, trellis.host, url.href);
        assert(sockets.length > 0, "the Console opened a broker WebSocket");
      } finally {
        await context.close();
      }
    },
    browserRuntimeOptions({ development: true }),
  );
});

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
      await page.goto(`${runtime.consoleUrl}/console/admin/users`, {
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
        await page.goto(await runtime.bootstrapUrl(), {
          waitUntil: "domcontentloaded",
        });
        await completeAdminBootstrapInBrowser(page, runtime, BROWSER_ADMIN);
        await page.waitForURL((url) => url.origin === runtime.trellisUrl);
        await completeConsoleEntry(page, BROWSER_ADMIN);
        await waitForConsoleReady(page);

        await page.reload({ waitUntil: "domcontentloaded" });
        await waitForConsoleReady(page);
        if (captureEndpoint) {
          const traceExportsBeforeEvents = traceExports;
          await page.goto(`${runtime.consoleUrl}/console/admin/events`, {
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

      await page.goto(`${runtime.consoleUrl}/console`, {
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
