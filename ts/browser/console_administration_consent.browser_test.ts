import { strictEqual } from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { ulid } from "ulid";
import {
  connectNativeClient,
  discoverNativeOAuth,
  NativeOAuthClient,
} from "../packages/trellis/native.ts";
import { apps } from "../packages/trellis-testkit/trellis/index.js";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

// Consent for an app that asks, here the CLI signing in with a device code.
for (const selected of [
  undefined,
  "trellis.auth.v1:accounts.manage",
] as const) {
  test(`consent promotes only selected administration (${selected ?? "none"})`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      const page = await browser.newPage();
      const diagnostics: string[] = [];
      page.on("console", (message) =>
        diagnostics.push(`console ${message.type()}: ${message.text()}`),
      );
      page.on("pageerror", (error) =>
        diagnostics.push(`pageerror: ${error.message}`),
      );
      page.on("request", (request) =>
        diagnostics.push(
          `${request.method()} ${new URL(request.url()).origin}${new URL(request.url()).pathname}`,
        ),
      );
      page.on("response", (response) =>
        diagnostics.push(
          `${response.status()} ${new URL(response.url()).origin}${new URL(response.url()).pathname}`,
        ),
      );
      page.on("requestfailed", (request) =>
        diagnostics.push(
          `failed ${new URL(request.url()).pathname}: ${request.failure()?.errorText}`,
        ),
      );
      page.on("websocket", (socket) => {
        diagnostics.push(
          `websocket ${new URL(socket.url()).origin}${new URL(socket.url()).pathname}`,
        );
        socket.on("socketerror", (error) =>
          diagnostics.push(`websocket error: ${error}`),
        );
        socket.on("close", () => diagnostics.push("websocket closed"));
      });
      try {
        page.setDefaultTimeout(10000);
        page.setDefaultNavigationTimeout(10000);
        const server = (
          await discoverNativeOAuth(runtime.trellisUrl)
        ).orThrow();
        const client = (
          await NativeOAuthClient.open({
            scope: {
              issuer: server.issuer,
              instanceId: server.trust.instanceId,
              account: server.trust.account,
              clientId: "trellis.cli",
              credentialHandle: ulid(),
            },
            trust: server.trust,
          })
        ).orThrow();
        const device = (await client.startDevice(apps.cli.request)).orThrow();
        await page.goto(device.verificationUri);
        await page.getByLabel("Code", { exact: true }).fill(device.userCode);
        await page
          .getByRole("button", { name: "Continue", exact: true })
          .click();
        await page
          .getByRole("heading", { name: "Sign in", exact: true })
          .waitFor();
        await page.getByLabel("Username", { exact: true }).fill(username);
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /wants to use your account/ })
          .waitFor();

        // Each group's checkbox selects all of its optional access.
        const access = page.getByRole("checkbox", { name: /^Allow all: / });
        const administration = page
          .getByRole("group", { name: "Manage this Trellis", exact: true })
          .getByRole("checkbox", { name: /^Allow all: / });
        // Consent is opt-in: optional access starts unselected.
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(await choice.isChecked(), false);
        }
        await page
          .getByRole("button", { name: "Select all", exact: true })
          .click();
        // Bulk selection never includes administration.
        const administrativeLabel =
          await administration.getAttribute("aria-label");
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(
              await choice.isChecked(),
              (await choice.getAttribute("aria-label")) !== administrativeLabel,
            );
        }
        await page
          .getByRole("button", { name: "Clear all", exact: true })
          .click();
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(await choice.isChecked(), false);
        }
        // Accept ordinary capabilities but decline the separate administrative choices.
        for (const choice of await page.getByRole("checkbox").all())
          if (await choice.isEnabled()) await choice.check();
        await administration.uncheck();
        if (selected) {
          await page
            .getByRole("group", { name: "Manage this Trellis", exact: true })
            .getByRole("button")
            .click();
        }
        if (selected)
          await page.getByRole("checkbox", { name: /Manage accounts/ }).check();
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        if (selected) {
          // Administration takes a separate confirmation step.
          await page
            .getByRole("heading", { name: /Allow .* to manage this Trellis\?/ })
            .waitFor();
          await page
            .getByRole("button", { name: "Allow management", exact: true })
            .click();
        }
        await page.waitForURL((url) =>
          url.pathname.endsWith("/device/complete"),
        );
        const session = (await device.complete()).orThrow();
        const cli = await connectNativeClient({
          session,
          servers: server.natsServers,
          request: apps.cli.request,
        }).orThrow();
        try {
          // The app holds administration exactly when it was selected.
          const created = await cli.usersCreate({
            username: `consented-${ulid().toLowerCase()}`,
            displayName: "Consented",
            idempotencyKey: ulid(),
          });
          strictEqual(created.isOk(), selected !== undefined);
        } finally {
          await cli.connection.close();
        }
      } catch (error) {
        const results = fileURLToPath(
          new URL("./test-results/", import.meta.url),
        );
        await mkdir(results, { recursive: true });
        const prefix = `${results}consent-${selected ?? "none"}`;
        // Callback URLs contain one-time credentials; capture the rendered page and
        // request paths, never query strings or transport/token bodies.
        await page.screenshot({ path: `${prefix}.png`, fullPage: true });
        await writeFile(`${prefix}.html`, await page.content());
        await writeFile(`${prefix}.log`, diagnostics.join("\n"));
        console.error(await page.locator("body").innerText());
        throw error;
      } finally {
        await browser.close();
      }
    }, browserRuntimeOptions());
  });
}
