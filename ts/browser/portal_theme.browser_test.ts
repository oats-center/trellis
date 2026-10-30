import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

// An SVG that would run script if the OAuth origin ever executed it.
const svg = (fill: string) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">` +
  `<script>document.title = "pwned"</script>` +
  `<rect width="10" height="10" fill="${fill}"/></svg>`;

test("themes saved in the Console brand the Portal at once, per installation and client", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await launchProfile(runtime);
    try {
      const page = await admin.newPage();
      await openConsole(page, runtime, BROWSER_ADMIN);
      await page.goto(`${runtime.consoleUrl}/console/admin/branding`);
      const form = page.locator("form");
      await form.getByLabel("Name", { exact: true }).fill("Acme Cloud");
      await form.locator("input[type=file]").setInputFiles({
        name: "logo.svg",
        mimeType: "image/svg+xml",
        buffer: Buffer.from(svg("#0050b3")),
      });
      await form.getByAltText("Current logo").waitFor();
      await form.getByLabel("Buttons and links").first().fill("#0050b3");
      await form.getByLabel("Sign-in heading").fill("Sign in to Acme");
      await form.getByLabel("Footer").fill("Acme support: help@acme.test");
      await form.getByText("Custom stylesheet").click();
      await form
        .locator("textarea")
        .fill(".brand span { letter-spacing: 3px; }\n");
      await form.getByRole("button", { name: "Save", exact: true }).click();
      await page.getByText(/^Saved\./).waitFor();

      // The Console's own client gets a name and logo of its own.
      await page.getByLabel("Applies to").selectOption("trellis.console");
      await form.getByLabel("Name", { exact: true }).fill("Acme Console");
      await form.locator("input[type=file]").setInputFiles({
        name: "console.svg",
        mimeType: "image/svg+xml",
        buffer: Buffer.from(svg("#b30050")),
      });
      await form.getByAltText("Current logo").waitFor();
      await form.getByRole("button", { name: "Save", exact: true }).click();
      await page.getByText(/^Saved\./).waitFor();
    } finally {
      await admin.close();
    }

    // A first-time visitor, not the administrator's signed-in profile.
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      await page.goto(`${runtime.consoleUrl}/login`, {
        waitUntil: "domcontentloaded",
      });
      // The Console's sign-in reaches the Portal with its own transaction.
      await page.waitForURL((url) => url.origin === runtime.trellisUrl, {
        timeout: 30_000,
      });
      const transaction = new URL(page.url()).searchParams.get("transaction");
      assert(transaction, "the Portal carries the authorization transaction");
      const brand = page.locator("header.brand span");
      await brand.filter({ hasText: "Acme Console" }).waitFor();
      // The client overrides name and logo; it inherits the rest.
      assert.equal(
        await page.locator("h1").first().textContent(),
        "Sign in to Acme",
      );
      assert.equal(
        (await page.locator(".portal-footer").textContent())?.trim(),
        "Acme support: help@acme.test",
      );
      assert.match(
        (await page.locator("img.brand-logo").getAttribute("src")) ?? "",
        /\/portal-theme\/logo\?client=trellis\.console$/,
      );
      assert.equal(
        await page.evaluate(() =>
          getComputedStyle(document.documentElement)
            .getPropertyValue("--primary")
            .trim(),
        ),
        "#0050b3",
      );
      // The installation stylesheet loads after the Portal's own.
      assert.equal(
        await brand.evaluate((node) => getComputedStyle(node).letterSpacing),
        "3px",
      );

      // The server resolves the client from its own record of the
      // transaction; a client named in the request changes nothing.
      const crafted = await page.request.get(
        `${runtime.trellisUrl}/portal/theme?transaction=${encodeURIComponent(transaction)}&client=someone-else`,
      );
      assert.equal((await crafted.json()).name, "Acme Console");
      const unknown = await page.request.get(
        `${runtime.trellisUrl}/portal/theme?transaction=unknown`,
      );
      assert.equal((await unknown.json()).name, "Acme Cloud");

      // A logo opened directly is sandboxed and cannot run script.
      const logo = await context.newPage();
      const response = await logo.goto(
        `${runtime.trellisUrl}/portal-theme/logo`,
      );
      assert.match(
        response?.headers()["content-security-policy"] ?? "",
        /sandbox/,
      );
      assert.notEqual(await logo.title(), "pwned");
      await logo.close();

      // Theming leaves sign-in and consent working.
      await completeConsoleEntry(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await page.waitForURL(
        (url) =>
          url.origin === runtime.consoleUrl &&
          url.pathname.startsWith("/console"),
        { timeout: 30_000 },
      );
    } finally {
      await browser.close();
    }
  }, browserRuntimeOptions());
});
