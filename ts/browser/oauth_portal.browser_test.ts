import { strictEqual } from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";

test("real OAuth portal signs in and approves the requesting device", async () => {
  const verification = process.env.TRELLIS_TEST_OAUTH_VERIFICATION_URI;
  const userCode = process.env.TRELLIS_TEST_OAUTH_USER_CODE;
  if (!verification || !userCode)
    throw new Error(
      "the OAuth component runner must supply its live verification URI",
    );
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(5000);
    page.setDefaultNavigationTimeout(10000);
    const failures: string[] = [];
    page.on("pageerror", (error) => failures.push(error.message));
    await page.goto(verification);
    await page.getByRole("heading", { name: "Verify your device" }).waitFor();
    await page.getByLabel("Device code", { exact: true }).fill(userCode);
    await page.getByRole("button", { name: "Review device request" }).click();
    await page
      .getByRole("heading", { name: "Sign in to Trellis" })
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
      .getByRole("heading", { name: "Allow Native client to access Trellis?" })
      .waitFor();
    await page
      .getByRole("checkbox", { name: /This code matches the device/ })
      .check();
    await page.getByRole("button", { name: "Authorize application" }).click();
    await page
      .getByRole("heading", { name: "Device access approved" })
      .waitFor();
    strictEqual(failures.length, 0, failures.join("\n"));
  } finally {
    await browser.close();
  }
});
