import { strictEqual } from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

test("company sign-in navigates through Dex and returns to Trellis consent under CSP", async () => {
  const authorization = process.env.TRELLIS_TEST_OAUTH_AUTHORIZATION_URI;
  const issuer = process.env.TRELLIS_TEST_OIDC_ISSUER;
  const capability = process.env.TRELLIS_TEST_OIDC_CAPABILITY;
  if (!authorization || !issuer || !capability)
    throw new Error(
      "the upstream runner must supply its live OAuth and Dex endpoints",
    );

  const browser = await chromium.launch({
    headless: true,
    env: browserTrustEnvironment(),
  });
  try {
    // The fixture uses isolated loopback TLS listeners with a disposable CA.
    const page = await browser.newPage({ ignoreHTTPSErrors: true });
    page.setDefaultTimeout(10000);
    page.setDefaultNavigationTimeout(10000);
    const failures: string[] = [];
    page.on("pageerror", (error) => failures.push(error.message));
    page.on("console", (message) => {
      if (message.text().includes("form-action")) failures.push(message.text());
    });
    await page.goto(authorization);
    await page.getByRole("heading", { name: "Sign in to Trellis" }).waitFor();
    await page
      .getByRole("link", { name: "Continue with Company identity" })
      .click();
    await page.waitForURL((url) => url.origin === new URL(issuer).origin);
    await page.locator('input[name="login"]').fill("admin@example.com");
    await page.locator('input[name="password"]').fill("password");
    await page.locator('button[type="submit"]').click();
    const approve = page.getByRole("button", {
      name: "Grant Access",
      exact: true,
    });
    await approve.waitFor();
    await approve.click();
    await page
      .getByRole("heading", { name: "Allow Native client to access Trellis?" })
      .waitFor();
    strictEqual(new URL(page.url()).origin, new URL(authorization).origin);
    await page.getByText(capability, { exact: true }).waitFor();
    strictEqual(failures.length, 0, failures.join("\n"));
  } finally {
    await browser.close();
  }
});
