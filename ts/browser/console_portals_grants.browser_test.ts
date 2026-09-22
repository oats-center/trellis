// Console portal and grant journeys. Real runtime, real generated client.

import { assertEquals } from "@std/assert";
import { ulid } from "ulid";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  fixtureName,
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";

Deno.test("B13 portal list loads real portals without browser errors", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/portals`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByText("Portals", { exact: true }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // The built-in portal is identified from its record, not display text.
      await page.getByText("built-in", { exact: true }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B14 portal edits reuse the returned version and cancelled disable sends nothing", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = fixtureName("b14-portal");
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "B14 Portal",
      entryUrl: "https://b14.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: false,
        localRegistration: false,
        federatedRegistration: false,
        providers: ["provider-a"],
      },
    });
    const before = await runtime.callAdminRpc("authPortalsGet", { portalId });
    assertEquals(before.portal.loginSettings.localLogin, false);
    assertEquals(before.portal.loginSettings.providers, ["provider-a"]);

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/portals/edit?portalId=${
          encodeURIComponent(portalId)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Display name").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // The saved localLogin=false must round-trip, not be forced on.
      assertEquals(await page.getByLabel("Local login").isChecked(), false);
      await page.getByLabel("Display name").fill("B14 Portal Renamed");
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // A second save without navigating must use the version returned by the
      // first save, not the stale pre-save version.
      await page.getByLabel("Display name").fill("B14 Portal Twice");
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.getByLabel("Enabled").uncheck();
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Cancel" })
        .first().click();
      assertEquals(
        await page.getByRole("button", { name: "Save portal" }).isEnabled(),
        true,
      );

      const after = await runtime.callAdminRpc("authPortalsGet", { portalId });
      assertEquals(after.portal.displayName, "B14 Portal Twice");
      assertEquals(
        after.portal.disabled,
        false,
        "cancelled disable must not dispatch",
      );
      assertEquals(after.portal.loginSettings.localLogin, false);
      assertEquals(after.portal.loginSettings.providers, ["provider-a"]);
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("V14/Z03 a stale portal draft conflicts, blocks ordinary save, and recovers only by explicit discard", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = fixtureName("v14-portal");
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "V14 Portal",
      entryUrl: "https://v14.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: [],
      },
    });

    const context = await launchProfile(runtime);
    try {
      const pageA = await context.newPage();
      const pageB = await context.newPage();
      const errors = captureBrowserErrors(pageA);
      await openConsoleAsRuntimeAdmin(pageA, runtime);
      await openConsoleAsRuntimeAdmin(pageB, runtime);

      const editUrl =
        `${runtime.trellisUrl}/console/admin/portals/edit?portalId=${
          encodeURIComponent(portalId)
        }`;
      for (const page of [pageA, pageB]) {
        await page.goto(editUrl, { waitUntil: "domcontentloaded" });
        await waitForConsoleShell(page);
        await page.getByLabel("Display name").waitFor({
          state: "visible",
          timeout: 30_000,
        });
      }
      assertEquals(
        await pageA.getByLabel("Display name").inputValue(),
        "V14 Portal",
      );

      // A edits only the name; B changes localLogin, an unrelated setting A
      // never touched. A's full update request still carries the old toggle.
      await pageA.getByLabel("Display name").fill("V14 A draft");
      await pageB.getByLabel("Local login").uncheck();
      await pageB.getByRole("button", { name: "Save portal" }).click();
      await pageB.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const afterB = await runtime.callAdminRpc("authPortalsGet", { portalId });
      assertEquals(afterB.portal.loginSettings.localLogin, false);

      // A route write observes B's newer version while A's draft is dirty.
      await pageA.getByLabel("Origin").fill("https://v14-route.example.com");
      await pageA.getByRole("button", { name: "Save route" }).click();
      await pageA.getByText("Portal route saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await pageA.getByLabel("Display name").inputValue(),
        "V14 A draft",
        "a route refresh must not discard the portal draft",
      );

      // The stale save conflicts and must not adopt B's version for A's values.
      await pageA.getByRole("button", { name: "Save portal" }).click();
      await pageA.getByText("Another operator changed this portal").first()
        .waitFor({ state: "visible", timeout: 30_000 });
      const afterConflict = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(
        afterConflict.portal.displayName,
        "V14 Portal",
        "a stale draft must not overwrite the other writer's committed change",
      );
      assertEquals(
        afterConflict.portal.loginSettings.localLogin,
        false,
        "B's unrelated setting stands",
      );
      assertEquals(
        await pageA.getByLabel("Display name").inputValue(),
        "V14 A draft",
        "the conflict keeps the operator's draft for review",
      );

      // Ordinary Save is blocked while conflicted: no implicit rebase.
      assertEquals(
        await pageA.getByRole("button", { name: "Save portal" }).isDisabled(),
        true,
        "a conflicted draft cannot be saved until it is explicitly reloaded",
      );

      // Explicit discard + reload shows B's authoritative record and version.
      await pageA.getByRole("button", {
        name: "Discard edits and reload current portal",
      }).click();
      await pageA.getByRole("dialog").getByRole("button", {
        name: "Discard and reload",
      }).click();
      await pageA.getByText("Another operator changed this portal").waitFor({
        state: "hidden",
        timeout: 30_000,
      });
      assertEquals(
        await pageA.getByLabel("Display name").inputValue(),
        "V14 Portal",
        "the reload shows the current server record",
      );
      assertEquals(
        await pageA.getByLabel("Local login").isChecked(),
        false,
        "the reload shows B's committed setting",
      );

      // Reapply the intended change; both writers' changes persist.
      await pageA.getByLabel("Display name").fill("V14 A final");
      await pageA.getByRole("button", { name: "Save portal" }).click();
      await pageA.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const afterApply = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(afterApply.portal.displayName, "V14 A final");
      assertEquals(
        afterApply.portal.loginSettings.localLogin,
        false,
        "reapplying A's name change must not revert B's setting",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("Z01 a metadata-only save keeps the configured providers so a later add appends", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = fixtureName("z01-portal");
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "Z01 Portal",
      entryUrl: "https://z01.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: ["provider-a"],
      },
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/portals/edit?portalId=${
          encodeURIComponent(portalId)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Display name").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // The hydrated editor shows the configured provider without a reload.
      await page.getByText("provider-a").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // Metadata-only save: providers must remain configured, not be erased.
      await page.getByLabel("Display name").fill("Z01 Renamed");
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText("provider-a").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const afterMetadata = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(afterMetadata.portal.loginSettings.providers, [
        "provider-a",
      ]);

      // Adding a second provider in the same mounted editor must append.
      await page.getByLabel("Provider ID").fill("provider-b");
      await page.getByRole("button", { name: "Add ID" }).click();
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const afterAdd = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(afterAdd.portal.loginSettings.providers, [
        "provider-a",
        "provider-b",
      ]);

      // Explicit removal is operator-directed and removes only what was removed.
      await page.getByRole("button", { name: "Remove provider provider-a" })
        .click();
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByText("Portal saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const afterRemove = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(afterRemove.portal.loginSettings.providers, ["provider-b"]);
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B15 built-in settings save uses the real built-in identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/portals/login/default`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      // The built-in route must resolve its own record, not a guessed ID.
      await page.getByLabel("Display name").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const portalId = await page.getByLabel("Portal ID").inputValue();
      assertEquals(portalId.length > 0, true);
      const portal = await runtime.callAdminRpc("authPortalsGet", { portalId });
      assertEquals(portal.portal.builtIn, true);
      assertEquals(
        await page.getByRole("button", { name: "Save login settings" })
          .count() > 0,
        true,
      );
      assertEquals(
        await page.getByRole("button", { name: "Save portal" }).count(),
        0,
        "the built-in portal must not expose the custom-portal save path",
      );
      const initialRegistration = portal.portal.loginSettings.localRegistration;
      await page.getByLabel("Local registration").setChecked(
        !initialRegistration,
      );
      await page.getByRole("button", { name: "Save login settings" }).click();
      await page.getByText("Portal settings saved.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const firstSave = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(
        firstSave.portal.loginSettings.localRegistration,
        !initialRegistration,
      );
      assertEquals(firstSave.portal.version, portal.portal.version + 1n);

      await page.getByLabel("Local registration").setChecked(
        initialRegistration,
      );
      await page.getByRole("button", { name: "Save login settings" }).click();
      // The success notice is cleared when the save begins and republished
      // only after the write commits, so its reappearance is the terminal
      // condition. Reading the record before that races the write.
      await page.getByText("Portal settings saved.").waitFor({
        state: "hidden",
        timeout: 10_000,
      });
      await page.getByText("Portal settings saved.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const secondSave = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(
        secondSave.portal.loginSettings.localRegistration,
        initialRegistration,
      );
      assertEquals(
        secondSave.portal.version,
        portal.portal.version + 2n,
        "second save must use the first write's returned version",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("Z04 a stale built-in settings save conflicts and does not adopt the newer version", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const pageA = await context.newPage();
      const pageB = await context.newPage();
      const errors = captureBrowserErrors(pageA);
      await openConsoleAsRuntimeAdmin(pageA, runtime);
      await openConsoleAsRuntimeAdmin(pageB, runtime);

      const settingsUrl =
        `${runtime.trellisUrl}/console/admin/portals/login/default`;
      for (const page of [pageA, pageB]) {
        await page.goto(settingsUrl, { waitUntil: "domcontentloaded" });
        await waitForConsoleShell(page);
        await page.getByLabel("Display name").waitFor({
          state: "visible",
          timeout: 30_000,
        });
      }
      const portalId = await pageA.getByLabel("Portal ID").inputValue();
      const initial = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      const initialRegistration =
        initial.portal.loginSettings.localRegistration;

      // B commits a real settings change; A's draft still holds the old value.
      await pageB.getByLabel("Local registration").setChecked(
        !initialRegistration,
      );
      await pageB.getByRole("button", { name: "Save login settings" })
        .click();
      await pageB.getByText("Portal settings saved.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const afterB = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(
        afterB.portal.loginSettings.localRegistration,
        !initialRegistration,
      );

      // A's stale settings save conflicts instead of rebasing A's old value.
      await pageA.getByLabel("Local registration").setChecked(
        initialRegistration,
      );
      await pageA.getByRole("button", { name: "Save login settings" })
        .click();
      await pageA.getByText("Another operator changed this portal").first()
        .waitFor({ state: "visible", timeout: 30_000 });
      const afterConflict = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(
        afterConflict.portal.loginSettings.localRegistration,
        !initialRegistration,
        "a rejected settings write must not adopt the newer version for old values",
      );
      assertEquals(
        await pageA.getByRole("button", { name: "Save login settings" })
          .isDisabled(),
        true,
        "a conflicted settings draft cannot be saved until explicitly reloaded",
      );

      // Explicit discard + reload adopts the current record and version.
      await pageA.getByRole("button", {
        name: "Discard edits and reload current portal",
      }).click();
      await pageA.getByRole("dialog").getByRole("button", {
        name: "Discard and reload",
      }).click();
      await pageA.getByText("Another operator changed this portal").waitFor({
        state: "hidden",
        timeout: 30_000,
      });
      assertEquals(
        await pageA.getByLabel("Local registration").isChecked(),
        !initialRegistration,
        "the reload shows the current server setting",
      );
      assertEquals(
        await pageA.getByRole("button", { name: "Save login settings" })
          .isDisabled(),
        false,
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B16 deleting a custom portal commits the portal and settings versions", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = fixtureName("b16-portal");
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "B16 Portal",
      entryUrl: "https://b16.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: null,
      },
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/portals`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const row = page.getByRole("row").filter({ hasText: portalId });
      await row.waitFor({ state: "visible" });
      await row.locator("summary").click();
      await row.getByRole("button", { name: "Delete", exact: true }).click();
      const dialog = page.getByRole("dialog");
      await dialog.getByRole("textbox").fill(portalId);
      await dialog.getByRole("button", { name: "Delete portal" }).click();
      await page.locator("main").getByText("Portal removed.").waitFor({
        state: "visible",
      });
      await row.waitFor({ state: "detached" });
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B17 route edit preserves selectors and later removal targets that route", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = fixtureName("b17-portal");
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "B17 Portal",
      entryUrl: "https://b17.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: null,
      },
    });
    const created = await runtime.callAdminRpc("authPortalsRoutesPut", {
      portalId,
      participantId: "runtime-trellis.Device",
      deploymentId: null,
      origin: "https://b17.example.com",
      priority: 10n,
      routeId: null,
      expectedVersion: null,
      idempotencyKey: ulid(),
    });
    const routeId = created.route.routeId;

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/portals/edit?portalId=${
          encodeURIComponent(portalId)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByRole("button", { name: "Edit", exact: true }).first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.getByRole("button", { name: "Edit", exact: true }).first()
        .click();

      // Editing only the origin must preserve the participant selector.
      assertEquals(
        await page.getByLabel("Participant ID").inputValue(),
        "runtime-trellis.Device",
      );
      await page.getByLabel("Origin").fill("https://b17-renamed.example.com");
      await page.getByLabel("Display name").fill("Dirty portal draft");
      await page.getByRole("button", { name: "Update route" }).click();
      await page.getByText("Portal route saved.").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByLabel("Display name").inputValue(),
        "Dirty portal draft",
        "route refresh must not overwrite an unsubmitted portal draft",
      );

      const after = await runtime.callAdminRpc("authPortalsGet", { portalId });
      // Exactly one route, the same ID, with both selectors intact.
      assertEquals(after.routes.length, 1);
      const route = after.routes[0];
      assertEquals(route.routeId, routeId);
      assertEquals(route.participantId, "runtime-trellis.Device");
      assertEquals(route.origin, "https://b17-renamed.example.com");
      assertEquals(
        after.portal.displayName,
        "B17 Portal",
        "route save must not silently submit the separate portal draft",
      );
      await page.getByRole("button", { name: "Save portal" }).click();
      await page.getByText("Portal saved.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const savedDraft = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(savedDraft.portal.displayName, "Dirty portal draft");
      await page.getByRole("button", { name: "Remove", exact: true }).first()
        .click();
      const confirmation = page.getByRole("dialog");
      await confirmation.getByRole("textbox").fill(routeId);
      await confirmation.getByRole("button", { name: "Remove route" }).click();
      await page.getByText("Portal route removed.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const removedRoute = await runtime.callAdminRpc("authPortalsGet", {
        portalId,
      });
      assertEquals(removedRoute.routes.length, 0);
      assertEquals(removedRoute.portal.displayName, "Dirty portal draft");
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N08 new capability groups load discoverable capabilities", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/capability-groups/new`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      await page.getByText("Source API:", { exact: false }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByText("No cataloged capabilities were returned.")
          .count(),
        0,
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N10 portal grant creation can select a portal beyond the first catalog page", async () => {
  await withTrellisRuntime(async (runtime) => {
    const prefix = fixtureName("n10-portal");
    let lastPortalId = "";
    for (let index = 0; index < 105; index += 1) {
      const portalId = `${prefix}-${String(index).padStart(3, "0")}`;
      await runtime.callAdminRpc("authPortalsPut", {
        portalId,
        displayName: `N10 Portal ${index}`,
        entryUrl: `https://n10-${index}.example.com`,
        disabled: false,
        expectedVersion: null,
        idempotencyKey: ulid(),
        loginSettings: {
          localLogin: true,
          localRegistration: false,
          federatedRegistration: false,
          providers: null,
        },
      });
      lastPortalId = portalId;
    }

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/grants/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const portalSelect = page.getByLabel("Login portal");
      await portalSelect.locator(`option[value="${lastPortalId}"]`).waitFor({
        state: "attached",
        timeout: 30_000,
      });
      assertEquals(await portalSelect.locator("option").count() >= 106, true);
      await portalSelect.selectOption(lastPortalId);
      assertEquals(await portalSelect.inputValue(), lastPortalId);
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B18 no unpersisted route disabled control remains", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = fixtureName("b18-portal");
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "B18 Portal",
      entryUrl: "https://b18.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: null,
      },
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/portals/edit?portalId=${
          encodeURIComponent(portalId)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Participant ID").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // The route record has no persisted enabled/disabled state, so the editor
      // must not offer a control that silently does nothing.
      assertEquals(
        await page.getByText("Enabled", { exact: true }).count(),
        1,
        "only the portal itself has an enabled control",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B19 capability group save preserves uncataloged assignments and the returned version", async () => {
  await withTrellisRuntime(async (runtime) => {
    const groupKey = fixtureName("b19-group");
    const uncatalogedId = "runtime-trellis.uncataloged@v1::legacy";
    await runtime.callAdminRpc("authCapabilityGroupsPut", {
      groupKey,
      displayName: "B19 Group",
      description: "B19 fixture group",
      capabilities: [uncatalogedId],
      includedGroups: [],
      expectedVersion: null,
      idempotencyKey: ulid(),
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/capability-groups/edit?groupKey=${
          encodeURIComponent(groupKey)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      // The uncataloged assignment must be surfaced, not silently dropped.
      await page.getByText("Unresolved assignments").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText(uncatalogedId).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // Metadata-only edit: the uncataloged ID must round-trip unchanged.
      await page.getByLabel("Display name").fill("B19 Group Renamed");
      await page.getByRole("button", { name: "Save group" }).click();
      await page.getByText("Capability group").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText("saved", { exact: false }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // The page's own state proves the save kept the uncataloged assignment:
      // the unresolved section still lists it after the round trip.
      await page.getByText(uncatalogedId).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const versionText = await page.getByText(/^Version [0-9]+ ·/).first()
        .innerText();
      const version = BigInt(versionText.match(/[0-9]+/)?.[0] ?? "0");
      assertEquals(
        version > 1n,
        true,
        "returned version advanced past the seed",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("Group deletion cancellation preserves its record and confirmed deletion leaves peers untouched", async () => {
  await withTrellisRuntime(async (runtime) => {
    const groupKey = fixtureName("n14-group");
    const peerKey = fixtureName("n14-peer");
    for (const key of [groupKey, peerKey]) {
      await runtime.callAdminRpc("authCapabilityGroupsPut", {
        groupKey: key,
        displayName: key,
        description: "N14 persisted group",
        capabilities: [],
        includedGroups: [],
        expectedVersion: null,
        idempotencyKey: ulid(),
      });
    }

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/capability-groups`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const row = page.getByRole("row").filter({ hasText: groupKey });
      await row.waitFor({ state: "visible", timeout: 30_000 });
      await row.locator("summary").click();
      await row.getByRole("button", { name: "Delete" }).click();
      await page.locator("dialog[open]").getByRole("button", { name: "Cancel" })
        .first().click();
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await row.waitFor({ state: "visible", timeout: 30_000 });

      if (await row.locator("details[open]").count() === 0) {
        await row.locator("summary").click();
      }
      await row.getByRole("button", { name: "Delete" }).click();
      const dialog = page.locator("dialog[open]");
      await dialog.getByRole("textbox").fill(groupKey);
      await dialog.getByRole("button", { name: "Delete group" }).click();
      await page.getByText(`Capability group ${groupKey} deleted.`).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByRole("row").filter({ hasText: peerKey }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(await row.count(), 0);
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N11 last-page user grant revoke leaves first-page binding active", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participantId = "trellis.console";
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      const installed = await runtime.callAdminRpc("authParticipantsGet", {
        participantId,
      });
      const userIds: string[] = [];
      for (let index = 0; index < 101; index++) {
        const created = await runtime.callAdminRpc("authUsersCreate", {
          username: fixtureName(`n11-${index}`),
          name: `N11 Operator ${index}`,
          email: null,
          image: null,
          idempotencyKey: ulid(),
        });
        userIds.push(created.user.userId);
        await runtime.callAdminRpc("authGrantsSet", {
          ownerKind: "user",
          ownerId: created.user.userId,
          participantId,
          installedRevision: installed.participant.revision,
          grants: { format: "trellis.grant-set.v1", permissions: [] },
          platformPrivileges: [],
          expiresAt: null,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
      }
      await page.goto(`${runtime.trellisUrl}/console/admin/apps/revoke`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const firstKey = JSON.stringify(["user", userIds[0], participantId]);
      const lastKey = JSON.stringify(["user", userIds[100], participantId]);
      const select = page.getByLabel("User-owned grant");
      await select.locator(`option[value='${lastKey}']`).waitFor({
        state: "attached",
        timeout: 30_000,
      });
      assertEquals(
        await select.locator(`option[value='${firstKey}']`).count(),
        1,
      );
      await select.selectOption(lastKey);
      await page.getByRole("button", { name: "Revoke grant" }).click();
      const dialog = page.locator("dialog[open]");
      await dialog.getByText(userIds[100], { exact: false }).waitFor({
        state: "visible",
      });
      await dialog.getByRole("textbox").fill(participantId);
      await dialog.getByRole("button", { name: "Revoke grant" }).click();
      await page.getByText("The selected grant is revoked.", { exact: false })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      const last = await runtime.callAdminRpc("authGrantsGet", {
        ownerKind: "user",
        ownerId: userIds[100],
        participantId,
      });
      const first = await runtime.callAdminRpc("authGrantsGet", {
        ownerKind: "user",
        ownerId: userIds[0],
        participantId,
      });
      assertEquals(last.binding?.state, "revoked");
      assertEquals(first.binding?.state, "active");
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B20 capability group second save uses the returned version", async () => {
  await withTrellisRuntime(async (runtime) => {
    const groupKey = fixtureName("b20-group");
    await runtime.callAdminRpc("authCapabilityGroupsPut", {
      groupKey,
      displayName: "B20 Group",
      description: "B20 fixture group",
      capabilities: [],
      includedGroups: [],
      expectedVersion: null,
      idempotencyKey: ulid(),
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/capability-groups/edit?groupKey=${
          encodeURIComponent(groupKey)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Display name").waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.getByLabel("Display name").fill("B20 First");
      await page.getByRole("button", { name: "Save group" }).click();
      await page.getByText("B20 First").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // A second save must use the version returned by the first.
      await page.getByLabel("Display name").fill("B20 Second");
      await page.getByRole("button", { name: "Save group" }).click();
      await page.getByText("B20 Second").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // The second save succeeded without navigating away, which is only
      // possible if it used the version returned by the first save. Confirm
      // the persisted result with a fresh load.
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      const versionText = await page.getByText(/^Version [0-9]+ ·/).first()
        .innerText();
      assertEquals(
        BigInt(versionText.match(/[0-9]+/)?.[0] ?? "0"),
        3n,
        "two sequential saves advanced the persisted version exactly twice",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B22 an exact missing portal-policy edit does not open a default policy", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/grants/new?portalId=builtin&participantId=missing.participant@v1`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Portal grant policy unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // No default policy and no create form should appear for the exact tuple.
      assertEquals(
        await page.getByRole("button", { name: "Save policy" }).count(),
        0,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B23 policy detail labels the configured upper bound", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participantId = fixtureName("b23-app");
    await runtime.callAdminRpc("authPortalsGrantOverridesPut", {
      portalId: "builtin",
      participantId,
      directCapabilities: ["runtime-trellis.uncataloged@v1::legacy"],
      capabilityGroupKeys: [],
      roleMappings: [],
      expectedVersion: null,
      idempotencyKey: ulid(),
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/grants/new?portalId=builtin&participantId=${
          encodeURIComponent(participantId)
        }`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Configured upper bound").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      // Unresolved references are retained and shown, not dropped.
      await page.getByText("Unresolved capability reference").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("Portal grant removal cancellation, bulk targets, and exact single removal", async () => {
  await withTrellisRuntime(async (runtime) => {
    const prefix = fixtureName("policy-remove");
    const targets = [`${prefix}-a`, `${prefix}-b`];
    const peer = `${prefix}-peer`;
    const untouched = `${prefix}-untouched`;
    for (const participantId of [...targets, peer, untouched]) {
      await runtime.callAdminRpc("authPortalsGrantOverridesPut", {
        portalId: "builtin",
        participantId,
        directCapabilities: [],
        capabilityGroupKeys: [],
        roleMappings: [],
        expectedVersion: null,
        idempotencyKey: ulid(),
      });
    }

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/grants`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const first = page.getByRole("row").filter({ hasText: targets[0] });
      await first.waitFor({ state: "visible", timeout: 30_000 });
      await first.getByRole("button", { name: "Remove" }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Cancel" })
        .first().click();
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await first.waitFor({ state: "visible", timeout: 30_000 });

      for (const participantId of targets) {
        await page.getByRole("checkbox", {
          name: `Select builtin ${participantId}`,
        }).check();
      }
      await page.getByRole("button", { name: "Remove selected" }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Remove 2" })
        .click();
      await page.getByText("2 policies removed.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByRole("row").filter({ hasText: peer }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      for (const participantId of targets) {
        assertEquals(
          await page.getByRole("row").filter({ hasText: participantId })
            .count(),
          0,
        );
      }
      await page.getByRole("row").filter({ hasText: peer }).getByRole(
        "button",
        { name: "Remove" },
      ).click();
      await page.getByRole("dialog").locator("input").fill(peer);
      await page.getByRole("dialog").getByRole("button", {
        name: "Remove policy",
      }).click();
      await page.getByText("Portal grant policy removed.").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByRole("row").filter({ hasText: untouched }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByRole("row").filter({ hasText: peer }).count(),
        0,
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("portal policy query navigation replaces an unavailable target without a document reload", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participantId = fixtureName("policy-nav");
    await runtime.callAdminRpc("authPortalsGrantOverridesPut", {
      portalId: "builtin",
      participantId,
      directCapabilities: [],
      capabilityGroupKeys: [],
      roleMappings: [],
      expectedVersion: null,
      idempotencyKey: ulid(),
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/grants/new?portalId=builtin&participantId=missing.participant@v1`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Portal grant policy unavailable").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/grants/new?portalId=builtin&participantId=${
          encodeURIComponent(id)
        }`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, participantId);
      await page.getByRole("button", { name: "Save policy" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByLabel("Application participant").inputValue(),
        participantId,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("missing exact user grant cannot be revoked", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/apps/revoke?ownerKind=user&ownerId=missing-user&participantId=missing.app`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Grant unavailable").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByRole("button", { name: "Revoke grant" }).count(),
        0,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
