// Console user and session journeys. Real runtime, real generated client.

import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";

test("B25 users list renders and exact edit reloads on query change", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const username = `ct-b25-${ulid()}`.toLowerCase();
    const otherUsername = `ct-b25-other-${ulid()}`.toLowerCase();
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: "ct-b25@example.com",
      idempotencyKey: ulid(),
      image: null,
      displayName: "Console B25",
      username,
    });
    const userId = created.user.principalId;
    assert.ok(userId);
    const other = await runtime.callAdminRpc("authUsersCreate", {
      email: "ct-b25-other@example.com",
      idempotencyKey: ulid(),
      image: null,
      displayName: "Console B25 Other",
      username: otherUsername,
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByRole("heading", { name: "Users", exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${userId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page
        .getByRole("heading", {
          name: username,
          exact: true,
          level: 1,
        })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/users/edit?userId=${encodeURIComponent(
          id,
        )}`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, other.user.principalId);
      await page
        .getByRole("heading", {
          name: otherUsername,
          exact: true,
          level: 1,
        })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page
        .getByLabel("Display name", { exact: true })
        .fill("Updated B25 Other");
      await page
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await page.getByText("Profile updated.", { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${userId}`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      await page
        .getByRole("heading", {
          name: username,
          exact: true,
          level: 1,
        })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assert.deepEqual(
        await page.getByLabel("Display name", { exact: true }).inputValue(),
        "Console B25",
      );
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${other.user.principalId}`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      await page
        .getByRole("heading", {
          name: otherUsername,
          exact: true,
          level: 1,
        })
        .waitFor({ state: "visible" });
      assert.deepEqual(
        await page.getByLabel("Display name", { exact: true }).inputValue(),
        "Updated B25 Other",
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B27 a disabled user stays disabled after a metadata edit", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const username = `ct-b27-${ulid()}`.toLowerCase();
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: "ct-b27@example.com",
      idempotencyKey: ulid(),
      image: null,
      displayName: "Console B27",
      username,
    });
    const userId = created.user.principalId;

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${userId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      const main = page.locator("main");
      await main.getByRole("checkbox", { name: /^Enabled/ }).uncheck();
      await main
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await page.getByText("Profile updated.", { exact: true }).waitFor({
        state: "visible",
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await main
        .getByRole("heading", { name: username, exact: true, level: 1 })
        .waitFor();
      assert.equal(
        await main.getByRole("checkbox", { name: /^Enabled/ }).isChecked(),
        false,
      );
      await main
        .getByLabel("Display name", { exact: true })
        .fill("Console B27 edited");
      await main
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await page.getByText("Profile updated.", { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await main
        .getByRole("heading", {
          name: username,
          exact: true,
          level: 1,
        })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assert.deepEqual(
        await main.getByLabel("Display name", { exact: true }).inputValue(),
        "Console B27 edited",
      );
      assert.deepEqual(
        await main.getByRole("checkbox", { name: /^Enabled/ }).isChecked(),
        false,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B28 user creation rotates the one-time setup receipt", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/users/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);

      const main = page.locator("main");
      const username = `ct-b28-${ulid()}`.toLowerCase();
      await main.getByRole("textbox").nth(0).fill(username);
      await main.getByRole("textbox").nth(1).fill("Console B28");
      await main.getByRole("button", { name: "Create user" }).click();

      // The account is reported as created, independent of the setup step.
      await page.getByText("Account created").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // This administrator is permitted the setup step, so the terminal state
      // is the ready receipt. Wait for that actual condition rather than
      // counting headings while the separate setup request is still running.
      await page.getByText("Password-reset token").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page
          .getByRole("button", { name: "Create user", exact: true })
          .count(),
        0,
        "the account was created, so the create form must not be re-offered",
      );
      const firstId = await main
        .locator(".trellis-identifier")
        .first()
        .innerText();
      const firstUrl = await page
        .getByLabel("Password-reset token")
        .inputValue();
      await page.getByRole("button", { name: "Create another user" }).click();
      assert.deepEqual(
        await page.getByLabel("Password-reset token").count(),
        0,
      );
      const secondUsername = `ct-b28-next-${ulid()}`.toLowerCase();
      await main.getByRole("textbox").nth(0).fill(secondUsername);
      await main.getByRole("textbox").nth(1).fill("Console B28 Next");
      await main.getByRole("button", { name: "Create user" }).click();
      await page.getByText("Account created").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText("Password-reset token").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const secondId = await main
        .locator(".trellis-identifier")
        .first()
        .innerText();
      assert.deepEqual(secondId !== firstId, true);
      assert.deepEqual(
        (await page.getByLabel("Password-reset token").inputValue()) ===
          firstUrl,
        false,
      );
      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByLabel("Search users").fill(username);
      await page.getByLabel("Search users").press("Enter");
      await page.getByText(username, { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("a password reset receipt clears its token when closed", async () => {
  await withTrellisRuntime(async (runtime) => {
    const created = await runtime.callAdminRpc("authUsersCreate", {
      idempotencyKey: ulid(),
      displayName: "Console N07",
      username: `ct-n07-${ulid()}`,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      assert(created.user.account);
      await page.getByLabel("Search users").fill(created.user.account.username);
      await page.getByLabel("Search users").press("Enter");
      const row = page.locator(".users-row", {
        hasText: created.user.principalId,
      });
      await row.waitFor({ state: "visible", timeout: 30_000 });
      await row.locator("summary").click();
      await row.getByRole("button", { name: "Create reset token" }).click();
      await page.getByLabel("Password-reset token").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const url = await page.getByLabel("Password-reset token").inputValue();
      assert.deepEqual(url.length > 0, true);
      await page
        .getByRole("button", {
          name: "Close password reset link dialog",
        })
        .click();
      await page.getByLabel("Password-reset token").waitFor({
        state: "detached",
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("N09 a stale user edit cannot overwrite a concurrent update and can be resubmitted after reload", async () => {
  await withTrellisRuntime(async (runtime) => {
    const username = `ct-n09-${ulid()}`.toLowerCase();
    const created = await runtime.callAdminRpc("authUsersCreate", {
      idempotencyKey: ulid(),
      displayName: "Original name",
      username,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${created.user.principalId}`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      await page
        .getByLabel("Display name", { exact: true })
        .fill("Operator draft");

      const secondPage = await context.newPage();
      await secondPage.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${created.user.principalId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(secondPage);
      await secondPage
        .getByLabel("Display name", { exact: true })
        .fill("Concurrent edit");
      await secondPage
        .getByRole("button", {
          name: "Save profile",
          exact: true,
        })
        .click();
      await secondPage
        .getByText("Profile updated.", {
          exact: true,
        })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await secondPage.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(secondPage);
      await secondPage
        .getByRole("heading", {
          name: username,
          exact: true,
          level: 1,
        })
        .waitFor({ state: "visible" });
      assert.deepEqual(
        await secondPage
          .getByLabel("Display name", { exact: true })
          .inputValue(),
        "Concurrent edit",
      );
      await page
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await page
        .getByRole("button", { name: "Reload user", exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assert.deepEqual(
        await page.getByLabel("Display name", { exact: true }).inputValue(),
        "Operator draft",
      );
      await page
        .getByRole("button", { name: "Reload user", exact: true })
        .click();
      await page
        .getByRole("button", { name: "Save profile", exact: true })
        .waitFor({
          state: "visible",
        });
      await runtime.waitFor(
        async () =>
          (await page
            .getByLabel("Display name", { exact: true })
            .inputValue()) === "Concurrent edit",
      );
      await page
        .getByLabel("Display name", { exact: true })
        .fill("Operator draft");
      await page
        .getByRole("button", { name: "Save profile", exact: true })
        .click();
      await page.getByText("Profile updated.", { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByLabel("Display name", { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page.getByLabel("Display name", { exact: true }).inputValue(),
        "Operator draft",
      );
      await secondPage.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(secondPage);
      assert.deepEqual(
        await secondPage
          .getByLabel("Display name", { exact: true })
          .inputValue(),
        "Operator draft",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B30 session and connection URLs use canonical IDs with no first-row fallback", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      // A missing session target must render unavailable, not select row one.
      await page.goto(
        `${runtime.trellisUrl}/console/admin/sessions/revoke?authorizationSessionId=${ulid()}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page
        .getByText("The requested target is unavailable", { exact: false })
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });

      // A missing connection target behaves the same way.
      await page.goto(
        `${runtime.trellisUrl}/console/admin/sessions/kick?attachmentId=${ulid()}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page
        .getByText("The requested target is unavailable", { exact: false })
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("N03 query-only client navigation clears session and connection targets", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/sessions/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const sessionId = await page
        .getByLabel("authorizationSessionId")
        .locator("option")
        .nth(1)
        .getAttribute("value");
      assert.ok(sessionId);
      await page.evaluate(() => {
        const marker = document.createElement("div");
        marker.id = "same-document-marker";
        document.body.append(marker);
      });
      const navigate = async (path: string) => {
        await page.evaluate((href) => {
          const anchor = document.createElement("a");
          anchor.href = href;
          anchor.textContent = "Navigate to target";
          document.body.append(anchor);
          anchor.click();
          anchor.remove();
        }, `${runtime.trellisUrl}/console${path}`);
      };
      await navigate(
        `/admin/sessions/revoke?authorizationSessionId=${encodeURIComponent(sessionId)}`,
      );
      await page
        .getByLabel("authorizationSessionId")
        .and(page.locator("input"))
        .waitFor({ state: "visible", timeout: 30_000 });
      assert.deepEqual(
        await page.getByLabel("authorizationSessionId").inputValue(),
        sessionId,
      );
      await page
        .locator("main")
        .getByRole("button", {
          name: /Revoke (session|and sign out)/,
        })
        .click();
      await page.locator("dialog[open]").waitFor({ state: "visible" });
      await navigate(`/admin/sessions/revoke?authorizationSessionId=${ulid()}`);
      await page
        .getByText("The requested target is unavailable", { exact: false })
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page
        .locator("dialog[open]")
        .locator(".modal-action")
        .getByRole("button", { name: "Cancel", exact: true })
        .click();
      await page.locator("dialog[open]").waitFor({ state: "hidden" });
      assert.deepEqual(
        await page.getByRole("button", { name: /Revoke/ }).count(),
        0,
      );
      assert.deepEqual(await page.locator("#same-document-marker").count(), 1);
      await navigate(
        `/admin/sessions/revoke?authorizationSessionId=${encodeURIComponent(sessionId)}`,
      );
      await page.getByLabel("authorizationSessionId").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page.getByLabel("authorizationSessionId").inputValue(),
        sessionId,
      );
      await navigate("/admin/sessions/kick");
      const connectionId = await page
        .getByLabel("attachmentId")
        .locator("option")
        .nth(1)
        .getAttribute("value");
      assert.ok(connectionId);
      await navigate(
        `/admin/sessions/kick?attachmentId=${encodeURIComponent(connectionId)}`,
      );
      await page
        .getByLabel("attachmentId")
        .and(page.locator("input"))
        .waitFor({ state: "visible", timeout: 30_000 });
      assert.deepEqual(
        await page.getByLabel("attachmentId").inputValue(),
        connectionId,
      );
      await navigate(`/admin/sessions/kick?attachmentId=${ulid()}`);
      await page
        .getByText("The requested target is unavailable", { exact: false })
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assert.deepEqual(
        await page.getByRole("button", { name: "Kick attachment" }).count(),
        0,
      );
      assert.deepEqual(await page.locator("#same-document-marker").count(), 1);
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("N03 switching targets during A confirmation revokes only B", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contextA = await launchProfile(runtime);
    const contextB = await chromium.launchPersistentContext(
      join(runtime.workdir, "browser-profile-b"),
      { headless: true, env: browserTrustEnvironment() },
    );
    try {
      const pageA = await contextA.newPage();
      const pageB = await contextB.newPage();
      const errors = await captureBrowserErrors(pageA);
      await openConsoleAsRuntimeAdmin(pageA, runtime);
      const initialSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      const sessionA = initialSessions.items.find(
        (item) => item.binding.clientId === "trellis.console",
      )?.authorizationSessionId;
      assert(sessionA);
      await openConsoleAsRuntimeAdmin(pageB, runtime);
      const laterSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      const sessionB = laterSessions.items.find(
        (item) =>
          item.binding.clientId === "trellis.console" &&
          item.authorizationSessionId !== sessionA,
      )?.authorizationSessionId;
      assert(sessionB);
      assert.deepEqual(sessionA === sessionB, false);

      await pageA.goto(
        `${runtime.trellisUrl}/console/admin/sessions/revoke?authorizationSessionId=${sessionA}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(pageA);
      await pageA
        .getByLabel("authorizationSessionId")
        .and(pageA.locator("input"))
        .waitFor({ state: "visible" });
      await pageA
        .locator("main")
        .getByRole("button", { name: /Revoke/ })
        .click();
      const dialog = pageA.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await pageA.evaluate((href) => {
        const marker = document.createElement("div");
        marker.id = "same-document-marker";
        document.body.append(marker);
        const anchor = document.createElement("a");
        anchor.href = href;
        document.body.append(anchor);
        anchor.click();
        anchor.remove();
      }, `${runtime.trellisUrl}/console/admin/sessions/revoke?authorizationSessionId=${sessionB}`);
      await runtime.waitFor(
        async () =>
          (await pageA.getByLabel("authorizationSessionId").inputValue()) ===
          sessionB,
      );
      await dialog.locator("input").fill(sessionA);
      await dialog.getByRole("button", { name: "Revoke", exact: true }).click();
      await dialog.waitFor({ state: "hidden", timeout: 30_000 });
      assert.deepEqual(
        await pageA.getByLabel("authorizationSessionId").inputValue(),
        sessionB,
      );
      assert.deepEqual(await pageA.locator("#same-document-marker").count(), 1);
      await pageA.getByRole("button", { name: "Revoke session" }).click();
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(sessionB);
      await dialog.getByRole("button", { name: "Revoke", exact: true }).click();
      await pageA
        .getByText("Accepted. Enforcement", { exact: false })
        .waitFor();
      const finalSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      assert.equal(
        finalSessions.items.find(
          (item) => item.authorizationSessionId === sessionB,
        )?.state,
        "revoked",
      );
      assert.equal(
        finalSessions.items.find(
          (item) => item.authorizationSessionId === sessionA,
        )?.state,
        "active",
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await contextB.close();
      await contextA.close();
    }
  }, browserRuntimeOptions());
});

test("B31 cancelling a revoke confirmation keeps its session active", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/sessions/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const firstOption = await page
        .getByLabel("authorizationSessionId")
        .locator("option")
        .nth(1)
        .getAttribute("value");
      assert.deepEqual(typeof firstOption, "string");
      if (!firstOption) throw new Error("Expected an active session to revoke");
      await page.getByLabel("authorizationSessionId").selectOption(firstOption);
      await page
        .getByRole("button", { name: /Revoke/ })
        .first()
        .click();
      await page.getByRole("button", { name: "Cancel" }).first().click();
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByLabel("authorizationSessionId").selectOption(firstOption);
      const sessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      assert.equal(
        sessions.items.find(
          (item) => item.authorizationSessionId === firstOption,
        )?.state,
        "active",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B33 kicking a connection leaves the session valid", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contextA = await launchProfile(runtime);
    const contextB = await chromium.launchPersistentContext(
      join(runtime.workdir, "browser-profile-b"),
      { headless: true, env: browserTrustEnvironment() },
    );
    try {
      const pageA = await contextA.newPage();
      const pageB = await contextB.newPage();
      await openConsoleAsRuntimeAdmin(pageA, runtime);
      const initialSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      const initialIds = new Set(
        initialSessions.items.map((item) => item.authorizationSessionId),
      );
      await openConsoleAsRuntimeAdmin(pageB, runtime);
      const laterSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      const newSessions = new Set(
        laterSessions.items
          .filter(
            (item) =>
              item.binding.clientId === "trellis.console" &&
              !initialIds.has(item.authorizationSessionId),
          )
          .map((item) => item.authorizationSessionId),
      );
      const admitted = await runtime.callAdminRpc("authConnectionsList", {
        state: "admitted",
        page: { limit: 100 },
      });
      const sessionB = admitted.items.find((item) =>
        newSessions.has(item.authorizationSessionId),
      )?.authorizationSessionId;
      assert.ok(sessionB);
      const before = await runtime.callAdminRpc("authConnectionsList", {
        authorizationSessionId: codecs.ulid.decode(sessionB),
        state: "admitted",
        page: { limit: 100 },
      });
      const connectionB = before.items[0]?.attachmentId;
      assert.ok(connectionB);

      await pageA.goto(
        `${runtime.trellisUrl}/console/admin/sessions/kick?attachmentId=${connectionB}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(pageA);
      await pageA
        .getByLabel("attachmentId")
        .and(pageA.locator("input"))
        .waitFor({ state: "visible", timeout: 30_000 });
      await pageA
        .getByRole("button", { name: "Kick attachment", exact: true })
        .click();
      const dialog = pageA.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(connectionB);
      await dialog.getByRole("button", { name: "Kick", exact: true }).click();
      await runtime.waitFor(
        async () => {
          const after = await runtime.callAdminRpc("authConnectionsList", {
            authorizationSessionId: codecs.ulid.decode(sessionB),
            state: "admitted",
            page: { limit: 100 },
          });
          return after.items.every(
            (entry) => entry.attachmentId !== connectionB,
          )
            ? true
            : undefined;
        },
        { timeoutMs: 30_000 },
      );
      await pageA.goto(`${runtime.trellisUrl}/console/admin/sessions/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(pageA);
      const optionB = pageA
        .getByLabel("authorizationSessionId")
        .locator(`option[value="${sessionB}"]`);
      await optionB.waitFor({ state: "attached", timeout: 30_000 });
      await pageA.getByLabel("authorizationSessionId").selectOption(sessionB);
      const sessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        { page: { limit: 100 } },
      );
      assert.equal(
        sessions.items.find((item) => item.authorizationSessionId === sessionB)
          ?.state,
        "active",
      );
    } finally {
      await contextB.close();
      await contextA.close();
    }
  }, browserRuntimeOptions());
});
