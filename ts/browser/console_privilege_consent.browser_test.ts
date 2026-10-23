import { strictEqual } from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

// Uses the initialized production host and registered Console, never a fake issuer.
for (const selected of [undefined, "principals.manage"] as const) {
  test(`Console consent promotes only selected optional privileges (${selected ?? "none"})`, async () => {
    await withTrellisRuntime(async (runtime) => {
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;
      const origin = new URL(runtime.trellisUrl).origin;
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
        await page.goto(new URL("/console/profile", origin).href);
        await page
          .getByRole("heading", { name: "Sign in to Trellis" })
          .waitFor();
        await page.getByLabel("Username", { exact: true }).fill(username);
        await page.getByLabel("Password", { exact: true }).fill(password);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
          .waitFor();

        const access = page.locator(
          'input[name="approved"][type="checkbox"], input[name="privilege"][type="checkbox"]',
        );
        // Consent is opt-in: optional access starts unselected.
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(await choice.isChecked(), false);
        }
        await page
          .getByRole("button", { name: "Select all", exact: true })
          .click();
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(await choice.isChecked(), true);
        }
        await page
          .getByRole("button", { name: "Unselect all", exact: true })
          .click();
        for (const choice of await access.all()) {
          if (await choice.isEnabled())
            strictEqual(await choice.isChecked(), false);
        }
        // Accept ordinary capabilities but decline the separate administrative choices.
        for (const choice of await page.getByRole("checkbox").all())
          if (await choice.isEnabled()) await choice.check();
        const privileges = page
          .getByRole("group", {
            name: "Administrative privileges",
            exact: true,
          })
          .getByRole("checkbox");
        for (const privilege of await privileges.all())
          await privilege.uncheck();
        if (selected)
          await page
            .getByRole("checkbox", { name: /Manage identities/ })
            .check();
        await page
          .getByRole("button", { name: "Authorize application" })
          .click();
        await page.waitForURL(
          (url) => url.origin === origin && url.pathname === "/console/profile",
        );
        await page.getByText("browser", { exact: true }).waitFor();
        await page
          .getByText(
            `This connection has ${selected ? 1 : 0} delegated ${selected ? "privilege" : "privileges"}.`,
            { exact: true },
          )
          .waitFor();
        // Reload must resume the same consented grant, not silently acquire the other privileges.
        await page.reload();
        await page.getByText("browser", { exact: true }).waitFor();
        await page
          .getByText(
            `This connection has ${selected ? 1 : 0} delegated ${selected ? "privilege" : "privileges"}.`,
            { exact: true },
          )
          .waitFor();
        if (selected) {
          await page.goto(new URL("/console/admin/users", origin).href);
          await page
            .getByRole("cell", { name: username, exact: true })
            .first()
            .waitFor();
        }
        strictEqual(new URL(page.url()).origin, origin);
      } catch (error) {
        const results = fileURLToPath(
          new URL("./test-results/", import.meta.url),
        );
        await mkdir(results, { recursive: true });
        const prefix = `${results}console-consent-${selected ?? "none"}`;
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
