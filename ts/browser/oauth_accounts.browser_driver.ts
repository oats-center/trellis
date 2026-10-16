import { strictEqual, ok } from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";
import { chromium } from "playwright";
import { ulid } from "ulid";

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
      .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
      .waitFor();
    for (const choice of await page.getByRole("checkbox").all())
      if (await choice.isEnabled()) await choice.check();
    await page.getByRole("button", { name: "Authorize application" }).click();
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
    });
    page = await context.newPage();
    page.setDefaultTimeout(15000);
    await page.goto(new URL("/console/admin/users", application).href);
    await page
      .getByRole("cell", { name: username, exact: true })
      .first()
      .waitFor();

    // First withdraw the app's delegation through its normal sign-out. The
    // independent portal session survives, then is explicitly logged out.
    await page.getByLabel("Open user menu", { exact: true }).click();
    await page.getByRole("button", { name: "Sign out", exact: true }).click();
    await page
      .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
      .waitFor();
    strictEqual(new URL(page.url()).origin, portal);
    const logout = await page.evaluate(async (requestId) => {
      const csrf = document.cookie
        .split("; ")
        .find((value) => value.startsWith("__Host-trellis-portal-csrf="))
        ?.split("=")[1];
      if (!csrf) throw new Error("portal CSRF cookie missing");
      return (
        await fetch("/portal/logout", {
          method: "POST",
          body: new URLSearchParams({
            csrf: decodeURIComponent(csrf),
            request_id: requestId,
          }),
        })
      ).status;
    }, ulid());
    strictEqual(logout, 200);
    await page.goto(new URL("/console/profile", application).href);
    await page
      .getByLabel("Username", { exact: true })
      .fill("browser-second-user");
    await page
      .getByLabel("Password", { exact: true })
      .fill("a separate browser account password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await page
      .getByRole("heading", { name: /Allow .* to access Trellis\?/ })
      .waitFor();
    await page
      .getByRole("checkbox", { name: /Read your account and sign out/ })
      .check();
    await page.getByRole("button", { name: "Authorize application" }).click();
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
      .getByText("Trellis could not reach the requested capability.", {
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
