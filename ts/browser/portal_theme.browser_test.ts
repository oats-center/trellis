import assert from "node:assert/strict";
import { appendFile, mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
} from "./browser_test_support.ts";

// An SVG that would run script if the OAuth origin ever executed it.
const svg = (fill: string) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">` +
  `<script>document.title = "pwned"</script>` +
  `<rect width="10" height="10" fill="${fill}"/></svg>`;

test("the Portal shows installation and client branding through a real sign-in", async () => {
  await withTrellisRuntime(async (runtime) => {
    const branding = join(runtime.workdir, "branding");
    await mkdir(branding);
    await writeFile(join(branding, "logo.svg"), svg("#0050b3"));
    await writeFile(join(branding, "console.svg"), svg("#b30050"));
    await writeFile(
      join(branding, "portal.css"),
      ".brand span { letter-spacing: 3px; }\n",
    );
    await appendFile(
      join(runtime.workdir, "config", "trellis", "config.toml"),
      `
[auth.portal.theme]
name = "Acme Cloud"
logo = ${JSON.stringify(join(branding, "logo.svg"))}
stylesheet = ${JSON.stringify(join(branding, "portal.css"))}
colors = { primary = "#0050b3" }
copy = { sign_in_title = "Sign in to Acme", footer = "Acme support: help@acme.test" }

[auth.portal.clients."trellis.console".theme]
name = "Acme Console"
logo = ${JSON.stringify(join(branding, "console.svg"))}
`,
    );
    await runtime.restart();

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await page.goto(`${runtime.trellisUrl}/login`, {
        waitUntil: "domcontentloaded",
      });
      // The Console's sign-in reaches the Portal with its own transaction.
      await page.waitForURL((url) => url.origin === runtime.oauthIssuer, {
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
        `${runtime.oauthIssuer}/portal/theme?transaction=${encodeURIComponent(transaction)}&client=someone-else`,
      );
      assert.equal((await crafted.json()).name, "Acme Console");
      const unknown = await page.request.get(
        `${runtime.oauthIssuer}/portal/theme?transaction=unknown`,
      );
      assert.equal((await unknown.json()).name, "Acme Cloud");

      // A logo opened directly is sandboxed and cannot run script.
      const logo = await context.newPage();
      const response = await logo.goto(
        `${runtime.oauthIssuer}/portal-theme/logo`,
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
          url.origin === runtime.trellisUrl &&
          url.pathname.startsWith("/console"),
        { timeout: 30_000 },
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
