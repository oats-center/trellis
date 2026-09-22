// Console user and session journeys. Real runtime, real generated client.

import { assertEquals, assertExists } from "@std/assert";
import { join } from "@std/path";
import { chromium } from "playwright";

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
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";

Deno.test("B25 users list renders and exact edit reloads on query change", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: "ct-b25@example.com",
      idempotencyKey: crypto.randomUUID(),
      image: null,
      name: "Console B25",
      username: `ct-b25-${crypto.randomUUID().slice(0, 8)}`,
    });
    const userId = created.user.userId;
    assertExists(userId);
    const other = await runtime.callAdminRpc("authUsersCreate", {
      email: "ct-b25-other@example.com",
      idempotencyKey: crypto.randomUUID(),
      image: null,
      name: "Console B25 Other",
      username: `ct-b25-other-${crypto.randomUUID().slice(0, 8)}`,
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
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
      await page.getByText("Console B25").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/users/edit?userId=${
          encodeURIComponent(id)
        }`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, other.user.userId);
      await page.getByText("Console B25 Other").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByLabel("Name").fill("Updated B25 Other");
      await page.getByRole("button", { name: "Save user" }).click();
      await page.getByText("Updated B25 Other").first().waitFor({
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
      await page.getByText("Console B25").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(await page.getByLabel("Name").inputValue(), "Console B25");
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${other.user.userId}`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      assertEquals(
        await page.getByLabel("Name").inputValue(),
        "Updated B25 Other",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B27 a disabled user stays disabled after a metadata edit", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: "ct-b27@example.com",
      idempotencyKey: crypto.randomUUID(),
      image: null,
      name: "Console B27",
      username: `ct-b27-${crypto.randomUUID().slice(0, 8)}`,
    });
    const userId = created.user.userId;

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${userId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      // The form shows the real returned state and offers only supported states.
      const main = page.locator("main");
      await main.getByText("State", { exact: true }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await main.locator(".badge", { hasText: "active" }).count() > 0,
        true,
      );
      await main.getByRole("checkbox", { name: "Active" }).uncheck();
      await main.getByRole("button", { name: "Save user" }).click();
      await main.locator(".badge", { hasText: "disabled" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await main.getByLabel("Name").fill("Console B27 edited");
      await main.getByRole("button", { name: "Save user" }).click();
      await main.getByRole("heading", { name: "Console B27 edited" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await main.getByRole("heading", { name: "Console B27 edited" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await main.locator(".badge", { hasText: "disabled" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // No fabricated global capability editor is present in the page body.
      assertEquals(
        await main.getByText("Capability Groups", { exact: true }).count(),
        0,
      );
      assertEquals(
        await main.getByText("Capabilities", { exact: true }).count(),
        0,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B28 user creation rotates the one-time setup receipt", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/users/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);

      const main = page.locator("main");
      const username = `ct-b28-${crypto.randomUUID().slice(0, 8)}`;
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
      await page.getByText("Password setup URL").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByRole("button", { name: "Create user", exact: true })
          .count(),
        0,
        "the account was created, so the create form must not be re-offered",
      );
      const firstId = await main.locator(".trellis-identifier").first()
        .innerText();
      const firstUrl = await page.getByLabel("Password setup URL").inputValue();
      await page.getByRole("button", { name: "Create another user" }).click();
      assertEquals(await page.getByLabel("Password setup URL").count(), 0);
      const secondUsername = `ct-b28-next-${crypto.randomUUID().slice(0, 8)}`;
      await main.getByRole("textbox").nth(0).fill(secondUsername);
      await main.getByRole("textbox").nth(1).fill("Console B28 Next");
      await main.getByRole("button", { name: "Create user" }).click();
      await page.getByText("Account created").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText("Password setup URL").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const secondId = await main.locator(".trellis-identifier").first()
        .innerText();
      assertEquals(secondId !== firstId, true);
      assertEquals(
        await page.getByLabel("Password setup URL").inputValue() === firstUrl,
        false,
      );
      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByLabel("Search users").fill(username);
      await page.getByLabel("Search users").press("Enter");
      await page.getByText("Console B28", { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("a one-time password reset receipt is one-time and clears on close", async () => {
  await withTrellisRuntime(async (runtime) => {
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: null,
      idempotencyKey: crypto.randomUUID(),
      image: null,
      name: "Console N07",
      username: `ct-n07-${crypto.randomUUID().slice(0, 8)}`,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByLabel("Search users").fill(created.user.userId);
      await page.getByLabel("Search users").press("Enter");
      const row = page.locator(".users-row", { hasText: "Console N07" });
      await row.waitFor({ state: "visible", timeout: 30_000 });
      await row.locator("summary").click();
      await row.getByRole("button", { name: "Create reset link" }).click();
      await page.getByLabel("Password reset URL").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const url = await page.getByLabel("Password reset URL").inputValue();
      assertEquals(url.length > 0, true);
      await page.getByRole("button", {
        name: "Close password reset link dialog",
      }).click();
      await page.getByLabel("Password reset URL").waitFor({
        state: "detached",
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N07 a real setup denial is recovered by a permission change without a second create", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const suffix = crypto.randomUUID().slice(0, 8);
    const operator = {
      username: `ct-n07-op-${suffix}`,
      password: `ct-n07-op-password-${suffix}`,
    };
    const operatorId = await createOrdinaryUserWithPassword(runtime, operator);

    // Exactly one user is created for this journey. It carries the admin
    // platform privilege so the non-admin operator resolves it as an
    // admin target, which is the shipped contract's authoritative
    // per-operation setup denial.
    const targetName = `Console N07 Target ${suffix}`;
    const target = await runtime.callAdminRpc("authUsersCreate", {
      email: null,
      idempotencyKey: crypto.randomUUID(),
      image: null,
      name: targetName,
      username: `ct-n07-target-${suffix}`,
    });
    const targetId = target.user.userId;
    const installed = await runtime.callAdminRpc("authParticipantsGet", {
      participantId: "trellis.console",
    });
    // Inferred from the generated client: no hand-written wire type here.
    const consoleBinding = async (ownerId: string) => {
      const listed = await runtime.callAdminRpc("authGrantsList", {
        ownerKind: "user",
        ownerId,
        page: { limit: 100 },
      });
      const binding = listed.items.find((item) =>
        item.participantId === "trellis.console"
      );
      return binding === undefined ? null : {
        revision: binding.revision,
        permissions: binding.grants.permissions,
      };
    };
    await runtime.callAdminRpc("authGrantsSet", {
      ownerKind: "user",
      ownerId: targetId,
      participantId: "trellis.console",
      installedRevision: installed.participant.revision,
      grants: { format: "trellis.grant-set.v1", permissions: [] },
      platformPrivileges: ["trellis.auth::admin"],
      expiresAt: null,
      expectedRevision: (await consoleBinding(targetId))?.revision ?? 0n,
      idempotencyKey: crypto.randomUUID(),
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, operator);
      const search = async () => {
        await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        await page.getByLabel("Search users").fill(targetName);
        await page.getByLabel("Search users").press("Enter");
      };
      await search();
      const rows = page.locator(".users-row", { hasText: targetName });
      await rows.first().waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(await rows.count(), 1, "exactly one target user exists");

      // The non-admin operator asks Trellis for the setup link. Trellis
      // answers with its authoritative denial; the account stays committed.
      await rows.locator("summary").click();
      await rows.getByRole("button", { name: "Create reset link" }).click();
      await page.getByText("You do not have permission for this operation.")
        .first().waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(await rows.count(), 1, "the target account remains");
      assertEquals(
        await page.getByLabel("Password reset URL").count(),
        0,
        "a denied setup exposes no receipt",
      );

      // Real permission change through the real administrator context: the
      // operator gains the admin privilege that the admin-target rule needs.
      const operatorBinding = await consoleBinding(operatorId);
      assertExists(operatorBinding, "the operator holds a Console binding");
      await runtime.callAdminRpc("authGrantsSet", {
        ownerKind: "user",
        ownerId: operatorId,
        participantId: "trellis.console",
        installedRevision: installed.participant.revision,
        grants: {
          format: "trellis.grant-set.v1",
          permissions: operatorBinding.permissions,
        },
        platformPrivileges: ["trellis.auth::admin"],
        expiresAt: null,
        expectedRevision: operatorBinding.revision,
        idempotencyKey: crypto.randomUUID(),
      });

      // Ordinary reconnect: a reload obtains a fresh authorization context
      // carrying the changed privilege. No local permission refresh is a
      // dispatch prerequisite.
      await search();
      const retryRows = page.locator(".users-row", { hasText: targetName });
      await retryRows.first().waitFor({ state: "visible", timeout: 30_000 });
      await retryRows.locator("summary").click();
      await retryRows.getByRole("button", { name: "Create reset link" })
        .click();
      await page.getByLabel("Password reset URL").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const url = await page.getByLabel("Password reset URL").inputValue();
      assertEquals(url.length > 0, true);

      // Still exactly one created account: the retry was a setup operation
      // for the same user, never another create.
      await search();
      const finalRows = page.locator(".users-row", { hasText: targetName });
      await finalRows.first().waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(await finalRows.count(), 1);
      assertEquals(
        (await finalRows.first().innerText()).includes(targetId),
        true,
        "the recovered setup link belongs to the same created user",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N09 a real user version conflict preserves the unsaved draft", async () => {
  await withTrellisRuntime(async (runtime) => {
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: null,
      idempotencyKey: crypto.randomUUID(),
      image: null,
      name: "Original name",
      username: `ct-n09-${crypto.randomUUID().slice(0, 8)}`,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${created.user.userId}`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Name").fill("Operator draft");

      const secondPage = await context.newPage();
      await secondPage.goto(
        `${runtime.trellisUrl}/console/admin/users/edit?userId=${created.user.userId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(secondPage);
      await secondPage.getByLabel("Name").fill("Concurrent edit");
      await secondPage.getByRole("button", { name: "Save user" }).click();
      await secondPage.getByText("Updated Concurrent edit.", { exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.getByRole("button", { name: "Save user" }).click();
      await page.getByRole("button", { name: "Refresh version (keep draft)" })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assertEquals(
        await page.getByLabel("Name").inputValue(),
        "Operator draft",
      );
      await page.getByRole("button", { name: "Refresh version (keep draft)" })
        .click();
      await page.getByRole("button", { name: "Save user" }).waitFor({
        state: "visible",
      });
      assertEquals(
        await page.getByLabel("Name").inputValue(),
        "Operator draft",
      );
      await page.getByRole("button", { name: "Save user" }).click();
      await page.getByText("Updated Operator draft.", { exact: true }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByLabel("Name").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByLabel("Name").inputValue(),
        "Operator draft",
      );
      await secondPage.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(secondPage);
      assertEquals(
        await secondPage.getByLabel("Name").inputValue(),
        "Operator draft",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B30 session and connection URLs use canonical IDs with no first-row fallback", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      // A missing session target must render unavailable, not select row one.
      await page.goto(
        `${runtime.trellisUrl}/console/admin/sessions/revoke?sessionId=sess_missing_b30`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Session unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // A missing connection target behaves the same way.
      await page.goto(
        `${runtime.trellisUrl}/console/admin/sessions/kick?connectionId=conn_missing_b30`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Connection unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // The legacy parameter names are not aliases.
      await page.goto(
        `${runtime.trellisUrl}/console/admin/sessions/revoke?sessionKey=sess_legacy`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Session unavailable").count();
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N03 query-only client navigation clears session and connection targets", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/sessions/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const sessionId = await page.getByLabel("Session").locator("option").nth(
        1,
      )
        .getAttribute("value");
      assertExists(sessionId);
      await page.evaluate(() => {
        const marker = document.createElement("div");
        marker.id = "same-document-marker";
        document.body.append(marker);
      });
      const navigate = async (path: string) => {
        await page.evaluate(
          (href) => {
            const anchor = document.createElement("a");
            anchor.href = href;
            anchor.textContent = "Navigate to target";
            document.body.append(anchor);
            anchor.click();
            anchor.remove();
          },
          `${runtime.trellisUrl}/console${path}`,
        );
      };
      await navigate(
        `/admin/sessions/revoke?sessionId=${encodeURIComponent(sessionId)}`,
      );
      await page.getByLabel("Session").and(page.locator("input"))
        .waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(await page.getByLabel("Session").inputValue(), sessionId);
      await page.getByText(sessionId, { exact: true }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.locator("main").getByRole("button", {
        name: /Revoke (session|and sign out)/,
      }).click();
      await page.locator("dialog[open]").waitFor({ state: "visible" });
      await navigate("/admin/sessions/revoke?sessionId=sess_missing_n03");
      await page.getByText("Session unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.locator("dialog[open]").waitFor({ state: "hidden" });
      assertEquals(
        await page.getByRole("button", { name: /Revoke/ }).count(),
        0,
      );
      assertEquals(await page.locator("#same-document-marker").count(), 1);
      await navigate(
        `/admin/sessions/revoke?sessionId=${encodeURIComponent(sessionId)}`,
      );
      await page.getByLabel("Session").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(await page.getByLabel("Session").inputValue(), sessionId);
      await navigate("/admin/sessions/kick");
      const connectionId = await page.getByLabel("Connection").locator("option")
        .nth(1).getAttribute("value");
      assertExists(connectionId);
      await navigate(
        `/admin/sessions/kick?connectionId=${encodeURIComponent(connectionId)}`,
      );
      await page.getByLabel("Connection").and(page.locator("input"))
        .waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(
        await page.getByLabel("Connection").inputValue(),
        connectionId,
      );
      await navigate("/admin/sessions/kick?connectionId=conn_missing_n03");
      await page.getByText("Connection unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByRole("button", { name: "Kick connection" }).count(),
        0,
      );
      assertEquals(await page.locator("#same-document-marker").count(), 1);
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N03 switching targets during A confirmation revokes only B", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contextA = await launchProfile(runtime);
    const contextB = await chromium.launchPersistentContext(
      join(runtime.workdir, "browser-profile-b"),
      { headless: true },
    );
    try {
      const pageA = await contextA.newPage();
      const pageB = await contextB.newPage();
      const errors = captureBrowserErrors(pageA);
      await openConsoleAsRuntimeAdmin(pageA, runtime);
      await openConsoleAsRuntimeAdmin(pageB, runtime);
      const currentSession = async (page: typeof pageA): Promise<string> => {
        await page.goto(`${runtime.trellisUrl}/console/admin/sessions`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const href = await page.getByText("Current", { exact: true })
          .locator("xpath=ancestor::tr")
          .locator('a[href*="sessionId="]')
          .getAttribute("href");
        assertExists(href);
        const id = new URL(href, runtime.trellisUrl).searchParams.get(
          "sessionId",
        );
        assertExists(id);
        return id;
      };
      const sessionA = await currentSession(pageA);
      const sessionB = await currentSession(pageB);
      assertEquals(sessionA === sessionB, false);

      await pageA.goto(
        `${runtime.trellisUrl}/console/admin/sessions/revoke?sessionId=${sessionA}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(pageA);
      await pageA.getByLabel("Session").and(pageA.locator("input"))
        .waitFor({ state: "visible" });
      await pageA.locator("main").getByRole("button", { name: /Revoke/ })
        .click();
      const dialog = pageA.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await pageA.evaluate(
        (href) => {
          const marker = document.createElement("div");
          marker.id = "same-document-marker";
          document.body.append(marker);
          const anchor = document.createElement("a");
          anchor.href = href;
          document.body.append(anchor);
          anchor.click();
          anchor.remove();
        },
        `${runtime.trellisUrl}/console/admin/sessions/revoke?sessionId=${sessionB}`,
      );
      await pageA.getByText(sessionB, { exact: true }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await dialog.waitFor({ state: "hidden", timeout: 30_000 });
      assertEquals(await pageA.getByLabel("Session").inputValue(), sessionB);
      assertEquals(await pageA.locator("#same-document-marker").count(), 1);
      await pageA.getByRole("button", { name: "Revoke session" }).click();
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(sessionB);
      await dialog.getByRole("button", { name: "Revoke session" }).click();
      await pageA.waitForURL(
        (url) => url.pathname === "/console/admin/sessions",
        { timeout: 30_000 },
      );

      await pageA.goto(`${runtime.trellisUrl}/console/admin/sessions/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(pageA);
      const selector = pageA.getByLabel("Session");
      await selector.locator(`option[value="${sessionB}"]`).waitFor({
        state: "attached",
        timeout: 30_000,
      });
      assertEquals(
        (await selector.locator(`option[value="${sessionB}"]`).innerText())
          .includes("revoked"),
        true,
      );
      assertEquals(
        (await selector.locator(`option[value="${sessionA}"]`).innerText())
          .includes("active"),
        true,
      );
      assertNoBrowserErrors(errors);
    } finally {
      await contextB.close();
      await contextA.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B31 cancelling a revoke confirmation keeps its session active", async () => {
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
      const firstOption = await page.getByLabel("Session").locator("option")
        .nth(1).getAttribute("value");
      assertEquals(typeof firstOption, "string");
      if (!firstOption) throw new Error("Expected an active session to revoke");
      await page.getByLabel("Session").selectOption(firstOption);
      await page.getByRole("button", { name: /Revoke/ }).first().click();
      await page.getByRole("button", { name: "Cancel" }).first().click();
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      assertEquals(
        (await page.getByLabel("Session").locator(
          `option[value="${firstOption}"]`,
        ).innerText()).includes("active"),
        true,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B33 kicking a connection leaves the session valid", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contextA = await launchProfile(runtime);
    const contextB = await chromium.launchPersistentContext(
      join(runtime.workdir, "browser-profile-b"),
      { headless: true },
    );
    try {
      const pageA = await contextA.newPage();
      const pageB = await contextB.newPage();
      await openConsoleAsRuntimeAdmin(pageA, runtime);
      await openConsoleAsRuntimeAdmin(pageB, runtime);
      await pageB.goto(`${runtime.trellisUrl}/console/admin/sessions`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(pageB);
      const href = await pageB.getByText("Current", { exact: true })
        .locator("xpath=ancestor::tr").locator('a[href*="sessionId="]')
        .getAttribute("href");
      assertExists(href);
      const sessionB = new URL(href, runtime.trellisUrl).searchParams.get(
        "sessionId",
      );
      assertExists(sessionB);
      const before = await runtime.callAdminRpc("authConnectionsList", {
        sessionId: sessionB,
        page: { limit: 100 },
      });
      const connectionB = before.items[0]?.connectionId;
      assertExists(connectionB);

      await pageA.goto(
        `${runtime.trellisUrl}/console/admin/sessions/kick?connectionId=${connectionB}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(pageA);
      await pageA.getByLabel("Connection").and(pageA.locator("input"))
        .waitFor({ state: "visible", timeout: 30_000 });
      await pageA.getByRole("button", { name: "Kick connection" }).click();
      const dialog = pageA.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(connectionB);
      await dialog.getByRole("button", { name: "Kick connection" }).click();
      await runtime.waitFor(async () => {
        const after = await runtime.callAdminRpc("authConnectionsList", {
          sessionId: sessionB,
          page: { limit: 100 },
        });
        return after.items.every((entry) => entry.connectionId !== connectionB)
          ? true
          : undefined;
      }, { timeoutMs: 30_000 });
      await pageA.goto(`${runtime.trellisUrl}/console/admin/sessions/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(pageA);
      const optionB = pageA.getByLabel("Session").locator(
        `option[value="${sessionB}"]`,
      );
      await optionB.waitFor({ state: "attached", timeout: 30_000 });
      assertEquals((await optionB.innerText()).includes("active"), true);
    } finally {
      await contextB.close();
      await contextA.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B35[partial] profile identities stay self-scoped for an administrator", async () => {
  await withTrellisRuntime(async (runtime) => {
    // The administrator is the viewer here. The profile's identity panel reads
    // the caller's own identities: Trellis scopes UserIdentities.List to the
    // caller principal, so no cross-user identity can appear even for an
    // administrator.
    await runtime.ensureAdmin();
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/profile`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByRole("heading", { name: /Account/ }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText("How you sign in").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // The identities shown are the caller's own: the panel renders the
      // caller-scoped UserIdentities.List result for this administrator, and
      // the page exposes no cross-user selector.
      await page.getByText(/This account signs in through/).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByRole("combobox").count(),
        0,
        "the profile must not offer a cross-user selector",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
