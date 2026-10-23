import { strictEqual } from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

test("real OAuth portal signs in and approves the requesting device", async () => {
  const verification = process.env.TRELLIS_TEST_OAUTH_VERIFICATION_URI;
  const userCode = process.env.TRELLIS_TEST_OAUTH_USER_CODE;
  if (!verification || !userCode)
    throw new Error(
      "the OAuth component runner must supply its live verification URI",
    );
  const browser = await chromium.launch({
    headless: true,
    env: browserTrustEnvironment(),
  });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(5000);
    page.setDefaultNavigationTimeout(10000);
    const failures: string[] = [];
    page.on("pageerror", (error) => failures.push(error.message));
    await page.goto(verification);
    await page.getByRole("heading", { name: "Connect a device" }).waitFor();
    await page.getByLabel("Code", { exact: true }).fill(userCode);
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page
      .getByRole("heading", { name: "Sign in", exact: true })
      .waitFor()
      .catch(async (error) => {
        console.error(
          "Device verification result:",
          page.url(),
          await page.locator("body").innerText(),
        );
        throw error;
      });
    await page.getByLabel("Username", { exact: true }).fill("admin");
    await page
      .getByLabel("Password", { exact: true })
      .fill("a strong bootstrap password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page
      .getByRole("heading", { name: "Native client wants to use your account" })
      .waitFor();
    await page
      .getByRole("checkbox", { name: /This matches the code on my device/ })
      .check();
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await page
      .getByRole("heading", { name: "Your device is connected" })
      .waitFor();
    strictEqual(failures.length, 0, failures.join("\n"));
  } finally {
    await browser.close();
  }
});
