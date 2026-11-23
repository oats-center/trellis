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
import {
  browserRuntimeOptions,
  enterDeviceCode,
} from "./browser_test_support.ts";

// Consent for an app that asks, here the CLI signing in with a device code.
// Administration starts selected like any other optional access. "kept"
// keeps it and confirms it in the caution step; "declined" unchecks it and
// grants every other choice.
for (const selection of ["kept", "declined"] as const) {
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
        const administration = page
          .getByRole("group", { name: "Manage this Trellis", exact: true })
          .getByRole("checkbox", { name: /^Allow all: / });
        const device = (await client.startDevice(apps.cli.request)).orThrow();
        await enterDeviceCode(page, device.verificationUri, device.userCode, {
          username,
          password,
        });
        await page
          .getByRole("heading", { name: /wants to use your account/ })
          .waitFor();
        // Optional access the person was never asked about starts selected,
        // administration included.
        for (const choice of await page
          .getByRole("checkbox", { name: /^Allow all: / })
          .all())
          if (await choice.isEnabled())
            strictEqual(
              await choice.isChecked(),
              true,
              `${await choice.getAttribute("aria-label")} starts selected`,
            );
        if (selection === "declined") await administration.uncheck();
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        // Administration takes the caution step before it is granted.
        if (selection === "kept")
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
          strictEqual(created.isOk(), selection === "kept");
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
