import { strictEqual, ok } from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";

test("Console reload, simultaneous tabs, Chromium restart and account switching preserve account ownership", async () => {
  const application = process.env.TRELLIS_TEST_CONSOLE_URL;
  const portal = process.env.TRELLIS_TEST_OAUTH_ORIGIN;
  const username = process.env.TRELLIS_TEST_ADMIN_USERNAME;
  const password = process.env.TRELLIS_TEST_ADMIN_PASSWORD;
  if (!application || !portal || !username || !password)
    throw new Error("run through the live oauth_browser Rust integration test");
  const directory = await mkdtemp(join(tmpdir(), "trellis-browser-accounts-"));
  let context = await chromium.launchPersistentContext(directory, {
    headless: true,
    env: browserTrustEnvironment(),
  });
  let page = await context.newPage();
  try {
    page.on("pageerror", (error) => console.error(error.message));
    page.setDefaultTimeout(15000);
    await page.goto(new URL("/console/admin/users", application).href);
    await page.getByLabel("Username", { exact: true }).fill(username);
    await page.getByLabel("Password", { exact: true }).fill(password);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page
      .getByRole("heading", { name: /wants to use your account/ })
      .waitFor();
    for (const choice of await page.getByRole("checkbox").all())
      if (await choice.isEnabled()) await choice.check();
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    // Administration takes a separate confirmation step.
    await page.getByRole("checkbox", { name: /I understand this app/ }).check();
    await page
      .getByRole("button", { name: "Allow management", exact: true })
      .click();
    await page
      .getByRole("cell", { name: username, exact: true })
      .first()
      .waitFor();

    const tab = await context.newPage();
    await tab.goto(new URL("/console/admin/users", application).href);
    await tab
      .getByRole("cell", { name: username, exact: true })
      .first()
      .waitFor();
    await Promise.all([page.reload(), tab.reload()]);
    await Promise.all(
      [page, tab].map((current) =>
        current
          .getByRole("cell", { name: username, exact: true })
          .first()
          .waitFor(),
      ),
    );
    await context.close();
    context = await chromium.launchPersistentContext(directory, {
      headless: true,
      env: browserTrustEnvironment(),
    });
    page = await context.newPage();
    page.setDefaultTimeout(15000);
    await page.goto(new URL("/console/admin/users", application).href);
    await page
      .getByRole("cell", { name: username, exact: true })
      .first()
      .waitFor();

    // Signing out of the Console ends the Portal login too, so the next
    // sign-in asks for credentials instead of returning to the same account.
    await page.getByLabel("Open user menu", { exact: true }).click();
    await page.getByRole("button", { name: "Sign out", exact: true }).click();
    await page.getByLabel("Username", { exact: true }).waitFor();
    strictEqual(new URL(page.url()).origin, portal);
    strictEqual(new URL(page.url()).pathname, "/login");
    await page
      .getByLabel("Username", { exact: true })
      .fill("browser-second-user");
    await page
      .getByLabel("Password", { exact: true })
      .fill("a separate browser account password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page
      .getByRole("heading", { name: /wants to use your account/ })
      .waitFor();
    await page
      .getByRole("group", { name: "Account", exact: true })
      .getByRole("button")
      .click();
    await page.getByRole("checkbox", { name: /See your profile/ }).check();
    await page.getByRole("button", { name: "Allow", exact: true }).click();
    await page.waitForURL(
      (url) =>
        url.origin === new URL(application).origin &&
        url.pathname === "/console/profile",
    );
    await page
      .getByText("Browser second user", { exact: true })
      .filter({ visible: true })
      .first()
      .waitFor();
    await page.reload();
    await page
      .getByText("Browser second user", { exact: true })
      .filter({ visible: true })
      .first()
      .waitFor();
    await page.goto(new URL("/console/admin/users", application).href);
    await page
      .getByText("You haven't allowed the Console to do this.", {
        exact: true,
      })
      .waitFor();
    ok(
      !(await page.getByRole("cell", { name: username, exact: true }).count()),
      "new account must not retain the administrator's authority",
    );
  } catch (error) {
    console.error(
      new URL(page.url()).pathname,
      await page.locator("body").innerText(),
    );
    throw error;
  } finally {
    await context.close();
    await rm(directory, { recursive: true, force: true });
  }
});
