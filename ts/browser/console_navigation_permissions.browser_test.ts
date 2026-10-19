import assert from "node:assert/strict";
import { test } from "node:test";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  waitForConsoleShell,
} from "./console_test_support.ts";

test("a denied administrative read preserves ordinary account access and navigation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const credentials = {
      username: "restricted-console-user",
      password: "restricted-console-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsole(page, runtime, credentials);
      await page.goto(`${runtime.trellisUrl}/console/profile`);
      await page.getByText("How you sign in", { exact: true }).waitFor();
      try {
        await page
          .locator("main")
          .getByText(credentials.username, { exact: true })
          .first()
          .waitFor();
      } catch (cause) {
        throw new Error(
          `Account read did not resolve: ${await page.locator("main").innerText()}`,
          { cause },
        );
      }
      await page
        .getByLabel("Display name", { exact: true })
        .fill("Chosen directory name");
      await page.getByLabel("Given name", { exact: true }).fill("Directory");
      await page.getByLabel("Email", { exact: true }).fill("owner@example.com");
      await page
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await page
        .getByRole("heading", { name: "Chosen directory name", exact: true })
        .waitFor();
      await page.reload();
      await page
        .getByRole("heading", { name: "Chosen directory name", exact: true })
        .waitFor();
      assert.equal(
        await page.getByLabel("Given name", { exact: true }).inputValue(),
        "Directory",
      );
      assert.equal(
        await page.getByLabel("Email", { exact: true }).inputValue(),
        "owner@example.com",
      );
      await page.goto(`${runtime.trellisUrl}/console/admin/services`);
      await waitForConsoleShell(page);
      await page
        .getByText("Trellis could not reach the requested capability.", {
          exact: false,
        })
        .waitFor();
      assert.equal(new URL(page.url()).pathname, "/console/admin/services");
      await page.getByRole("link", { name: "Account", exact: true }).click();
      await page.getByText("How you sign in", { exact: true }).waitFor();
      await page
        .locator("main")
        .getByText(credentials.username, { exact: true })
        .first()
        .waitFor();
      assert.equal(new URL(page.url()).pathname, "/console/profile");
      await page.reload();
      await page
        .locator("main")
        .getByText(credentials.username, { exact: true })
        .first()
        .waitFor();
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("an unavailable profile photo falls back to local initials without browser requests to the source", async () => {
  await withTrellisRuntime(async (runtime) => {
    const credentials = {
      username: "avatar-owner",
      password: "avatar-owner-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const requests: string[] = [];
      page.on("request", (request) => requests.push(request.url()));
      await openConsole(page, runtime, credentials);
      await page.goto(`${runtime.trellisUrl}/console/profile`);
      await page
        .getByLabel("New profile picture URL", { exact: true })
        .fill("https://images.invalid/photo.png");
      const unavailable = page.waitForResponse(
        (response) =>
          new URL(response.url()).pathname.startsWith("/profile/avatars/") &&
          response.status() === 404,
      );
      await page
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await unavailable;
      await page
        .locator('img[referrerpolicy="no-referrer"][hidden]')
        .waitFor({ state: "attached" });
      assert(
        await page.getByLabel("Profile initials", { exact: true }).isVisible(),
      );
      assert(
        requests.every((url) => new URL(url).hostname !== "images.invalid"),
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
