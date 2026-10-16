import { strictEqual } from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { test } from "node:test";
import { chromium } from "playwright";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserRuntimeOptions } from "./browser_test_support.ts";

test("registered Console delegates administrative authority through consent and uses the real authenticated client", async () => {
  await withTrellisRuntime(async (runtime) => {
    const application = runtime.trellisUrl;
    const username = runtime.adminUsername;
    const password = runtime.adminPassword;
    const origin = new URL(application).origin;
    const browser = await chromium.launch({ headless: true });
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
    try {
      page.setDefaultTimeout(10000);
      page.setDefaultNavigationTimeout(10000);
      let callback: URL | undefined;
      page.on("request", (request) => {
        const url = new URL(request.url());
        if (url.searchParams.has("code") && url.searchParams.has("state"))
          callback = url;
      });
      await page.goto(new URL("/console/admin/users", origin).href);
      await page.getByRole("heading", { name: "Sign in to Trellis" }).waitFor();
      await page.getByLabel("Username", { exact: true }).fill(username);
      await page.getByLabel("Password", { exact: true }).fill(password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await page
        .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
        .waitFor();
      await page.getByRole("checkbox", { name: /Manage identities/ }).check();
      const choices = page.getByRole("checkbox");
      for (const choice of await choices.all())
        if (await choice.isEnabled()) await choice.check();
      await page.getByRole("button", { name: "Authorize application" }).click();
      await page.waitForURL(
        (url) =>
          url.origin === origin && url.pathname === "/console/admin/users",
      );
      await page
        .getByRole("cell", { name: username, exact: true })
        .first()
        .waitFor();
      strictEqual(callback?.origin, origin);
      strictEqual(callback?.pathname, "/login");
      await page.reload();
      await page
        .getByRole("cell", { name: username, exact: true })
        .first()
        .waitFor();
      // Reusing an existing username reaches Users.Create's declared conflict;
      // it must leave the protected account unchanged and the client usable.
      await page.getByRole("link", { name: "New user", exact: true }).click();
      await page.getByLabel(/Username/).fill(username);
      await page
        .getByRole("textbox", { name: /^Name\b/i })
        .fill("Duplicate account must not be created");
      await page
        .getByRole("button", { name: "Create user", exact: true })
        .click();
      await page.getByText("Revision conflict", { exact: false }).waitFor();
      strictEqual(
        await page.getByText("Account created", { exact: true }).count(),
        0,
      );
      await page
        .getByRole("link", { name: "Back to users", exact: true })
        .click();
      await page
        .getByRole("cell", { name: username, exact: true })
        .first()
        .waitFor();
      await page.goto(new URL("/console/admin/apis", origin).href);
      await page
        .getByRole("button", { name: "trellis.auth@v1", exact: true })
        .click();
      await page.locator("pre").filter({ hasText: "OAuthClients" }).waitFor();
      await page.goto(new URL("/console/admin/users", origin).href);
      await page.getByLabel("Open user menu", { exact: true }).click();
      await page
        .getByRole("button", { name: "Sign out", exact: true })
        .waitFor();
      const revoked = page.waitForResponse(
        (response) =>
          new URL(response.url()).pathname === "/revoke" &&
          response.request().method() === "POST",
      );
      const reauthorize = page.waitForRequest((request) => {
        const url = new URL(request.url());
        return (
          url.searchParams.get("response_type") === "code" &&
          url.searchParams.get("client_id") === "trellis.console"
        );
      });
      const [response] = await Promise.all([
        revoked,
        reauthorize,
        (async () => {
          await page
            .getByRole("button", { name: "Sign out", exact: true })
            .click();
        })(),
      ]);
      strictEqual(response.status(), 200);
      // App revocation must preserve the separate Portal login: authorization
      // returns straight to consent, without asking the operator to sign in again.
      await page
        .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
        .waitFor();
    } catch (error) {
      await page.screenshot({
        path: "/tmp/opencode/console-signout-failure.png",
        fullPage: true,
      });
      await writeFile(
        "/tmp/opencode/console-signout-failure.html",
        await page.content(),
      );
      await writeFile(
        "/tmp/opencode/console-signout-network.log",
        diagnostics.join("\n"),
      );
      console.error(await page.locator("body").innerText());
      throw error;
    } finally {
      await browser.close();
    }
  }, browserRuntimeOptions());
});
