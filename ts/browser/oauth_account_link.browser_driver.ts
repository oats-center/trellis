import { ok, strictEqual } from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

test("account linking crosses Dex through a reviewed navigation under CSP", async () => {
  const link = process.env.TRELLIS_TEST_ACCOUNT_LINK_URI;
  const issuer = process.env.TRELLIS_TEST_OIDC_ISSUER;
  const adminLink = process.env.TRELLIS_TEST_ADMIN_LINK_URI;
  const wrongProvider = process.env.TRELLIS_TEST_WRONG_PROVIDER;
  if (!link || !issuer || !adminLink || !wrongProvider)
    throw new Error(
      "the account runner must supply real portal and Dex endpoints",
    );
  const browser = await chromium.launch({
    headless: true,
    env: browserTrustEnvironment(),
  });
  try {
    const context = await browser.newContext({ ignoreHTTPSErrors: true });
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    page.setDefaultNavigationTimeout(10000);
    const failures: string[] = [];
    page.on("pageerror", (error) => failures.push(error.message));
    page.on("console", (message) => {
      if (message.text().includes("form-action")) failures.push(message.text());
    });
    await page.goto(link);
    await page.getByLabel("Username", { exact: true }).fill("alice");
    await page
      .getByLabel("Password", { exact: true })
      .fill("initial alice password");
    const loginResponse = page.waitForResponse((response) =>
      response.url().endsWith("/portal/account/login"),
    );
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    const loggedIn = await loginResponse;
    strictEqual(loggedIn.status(), 303, failures.join("\n"));
    await page
      .getByRole("heading", { name: "Link sign-in identity" })
      .waitFor();
    const csrf = await page.locator('input[name="csrf"]').inputValue();
    const token = await page.locator('input[name="token"]').inputValue();
    const origin = new URL(link).origin;
    for (const [requestOrigin, requestCsrf] of [
      ["https://attacker.example", csrf],
      [origin, "forged-csrf"],
    ]) {
      const denied = await context.request.post(
        `${origin}/portal/account/link`,
        {
          headers: { Origin: requestOrigin },
          form: { token, csrf: requestCsrf },
        },
      );
      strictEqual(denied.status(), 400);
    }
    const submitted = page.waitForResponse(
      (response) =>
        response.url() === `${origin}/portal/account/link` &&
        response.request().method() === "POST",
    );
    await page
      .getByRole("button", { name: "Continue to identity provider" })
      .click();
    const response = await submitted;
    strictEqual(response.status(), 200);
    strictEqual(new URL(page.url()).origin, origin);
    strictEqual(response.headers()["cache-control"], "no-store");
    strictEqual(response.headers()["referrer-policy"], "no-referrer");
    ok(
      response
        .headers()
        ["content-security-policy"].includes("form-action 'self'"),
    );
    const continuation = page.getByRole("link", {
      name: "Continue to identity provider",
    });
    const href = await continuation.getAttribute("href");
    ok(href);
    const handoff = new URL(href, origin).href;
    const other = await browser.newContext({ ignoreHTTPSErrors: true });
    const otherPage = await other.newPage();
    await otherPage.goto(adminLink);
    await otherPage.getByLabel("Username", { exact: true }).fill("admin");
    await otherPage
      .getByLabel("Password", { exact: true })
      .fill("a strong bootstrap password");
    await otherPage
      .getByRole("button", { name: "Sign in", exact: true })
      .click();
    await otherPage
      .getByRole("heading", { name: "Link sign-in identity" })
      .waitFor();
    strictEqual((await other.request.get(handoff)).status(), 400);
    await other.close();
    const replay = await context.request.post(`${origin}/portal/account/link`, {
      headers: { Origin: origin },
      form: { token, csrf },
    });
    strictEqual(replay.status(), 400);
    strictEqual(
      (
        await context.request.get(handoff, {
          headers: { Origin: "https://attacker.example" },
        })
      ).status(),
      400,
    );
    strictEqual(
      (
        await context.request.get(
          `${origin}/portal/oidc/${wrongProvider}/begin?link=${new URL(handoff).searchParams.get("token")}`,
          { headers: { Origin: origin } },
        )
      ).status(),
      400,
    );
    let referrer: string | undefined;
    let reachedProvider = false;
    page.on("request", (request) => {
      if (
        !reachedProvider &&
        request.isNavigationRequest() &&
        new URL(request.url()).origin === new URL(issuer).origin
      ) {
        reachedProvider = true;
        referrer = request.headers()["referer"];
      }
    });
    await continuation.click();
    await page.waitForURL((url) => url.origin === new URL(issuer).origin);
    ok(reachedProvider);
    strictEqual(referrer, undefined);
    strictEqual((await context.request.get(handoff)).status(), 400);
    await page.locator('input[name="login"]').fill("admin@example.com");
    await page.locator('input[name="password"]').fill("password");
    await page.locator('button[type="submit"]').click();
    await page
      .getByRole("button", { name: "Grant Access", exact: true })
      .click();
    await page.getByRole("heading", { name: "Identity linked" }).waitFor();
    strictEqual(new URL(page.url()).origin, origin);
    strictEqual(failures.length, 0, failures.join("\n"));
  } finally {
    await browser.close();
  }
});
