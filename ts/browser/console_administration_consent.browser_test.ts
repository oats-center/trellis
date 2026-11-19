import { strictEqual } from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
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
// "select-all" grants administration through the bulk action and its caution
// step; "ordinary" grants every other choice and leaves administration out.
for (const selection of ["select-all", "ordinary"] as const) {
  test(`consent grants administration only when selected (${selection})`, async () => {
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

        const access = page.getByRole("checkbox", { name: /^Allow all: / });
        const administration = page
          .getByRole("group", { name: "Manage this Trellis", exact: true })
          .getByRole("checkbox", { name: /^Allow all: / });
        // Consent is opt-in: optional access, administration included, starts
        // unselected.
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(await choice.isChecked(), false);
        }
        if (selection === "select-all") {
          await page
            .getByRole("button", { name: "Select all", exact: true })
            .click();
        } else {
          for (const choice of await access.all())
            if (await choice.isEnabled()) await choice.check();
          await administration.uncheck();
        }
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        // Administration takes the caution step before it is granted.
        if (selection === "select-all")
          await page
            .getByRole("button", { name: "Allow management", exact: true })
            .click();
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
            idempotencyKey: codecs.ulid.decode(ulid()),
          });
          strictEqual(created.isOk(), selection === "select-all");
        } finally {
          await cli.connection.close();
        }
      } catch (error) {
        const results = fileURLToPath(
          new URL("./test-results/", import.meta.url),
        );
        await mkdir(results, { recursive: true });
        const prefix = `${results}consent-${selection}`;
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
