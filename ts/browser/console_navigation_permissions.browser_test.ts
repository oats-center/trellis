// Console navigation, permission, and base-path journeys.

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { assertEquals } from "@std/assert";

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

const CONSOLE_ROUTES = [
  "/console/admin",
  "/console/admin/services",
  "/console/admin/services/new",
  "/console/admin/devices",
  "/console/admin/devices/profiles/new",
  "/console/admin/devices/instances/provision",
  "/console/admin/devices/reviews/decide",
  "/console/admin/devices/activations/revoke",
  "/console/admin/portals",
  "/console/admin/portals/new",
  "/console/admin/portals/login",
  "/console/admin/portals/login/default",
  "/console/admin/portals/login/selection",
  "/console/admin/portals/devices",
  "/console/admin/portals/devices/default",
  "/console/admin/portals/devices/selection",
  "/console/admin/users",
  "/console/admin/users/new",
  "/console/admin/sessions",
  "/console/admin/jobs",
  "/console/admin/events",
  "/console/admin/health-events",
  "/console/admin/grants",
  "/console/admin/apps",
  "/console/admin/apps/revoke",
  "/console/admin/capability-groups",
  "/console/admin/capability-groups/new",
  "/console/profile",
];

Deno.test("B48 every console route direct-loads without browser errors", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      for (const route of CONSOLE_ROUTES) {
        await page.goto(`${runtime.trellisUrl}${route}`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
      }
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("V02/B49 a restricted direct-load mounts the page and renders the authoritative denial", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const credentials = {
      username: "b49-restricted",
      password: "b49-restricted-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsole(page, runtime, credentials);
      // Direct-load an admin route this user cannot use. The route must mount
      // independently of the local session snapshot: the page dispatches its
      // real primary RPC and renders whatever Trellis answers.
      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByRole("heading", { name: "Service runtime" }).waitFor({
        state: "visible",
        timeout: 60_000,
      });
      // Trellis answers the primary read with its authoritative denial, and the
      // console shows a permission denial rather than a sign-in loop.
      await page.getByText("You do not have permission for this operation.")
        .first().waitFor({ state: "visible", timeout: 60_000 });
      assertEquals(
        new URL(page.url()).pathname,
        "/console/admin/services",
        "an operation-level denial must not redirect to sign-in",
      );
      assertEquals(
        await page.getByText("Sign in required").count(),
        0,
        "an operation-level denial is not session expiry",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B53 a missing deployment target is contained by the console", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/services/does-not-exist`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);
      // A missing target is a contained in-page state, not a crash: the shell
      // stays usable and the route explains what is unavailable.
      await page.getByText("Deployment unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByRole("link", { name: "Overview" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B50 a restricted user reaches permitted routes; denial does not erase permitted data", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    // A real ordinary user with a real password, created through the harness's
    // established bootstrap flow. It has no admin privilege or admin grants.
    const credentials = {
      username: "b50-restricted",
      password: "b50-restricted-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsole(page, runtime, credentials);
      await page.goto(`${runtime.trellisUrl}/console/profile`, {
        waitUntil: "domcontentloaded",
      });
      // The permitted self-service route reaches Trellis and renders its data.
      await page.getByText("How you sign in").first().waitFor({
        state: "visible",
        timeout: 60_000,
      });
      await waitForConsoleShell(page);
      // A denied administrative primary read renders the server denial while
      // the shell stays usable and the console remains on the route.
      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await page.getByText("You do not have permission for this operation.")
        .first().waitFor({ state: "visible", timeout: 60_000 });
      await page.getByRole("link", { name: "Account" }).waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByText("Sign in required").count(),
        0,
        "a denied operation must not become a login loop",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B51 an operation-level denial stays in the console", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const credentials = {
      username: "b51-restricted",
      password: "b51-restricted-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, credentials);
      await page.goto(`${runtime.trellisUrl}/console/profile`, {
        waitUntil: "domcontentloaded",
      });
      await page.getByText("How you sign in").first().waitFor({
        state: "visible",
        timeout: 60_000,
      });
      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      // The denial is an operation result, not session loss: the route stays
      // mounted and the user is not sent through login.
      await page.getByText("You do not have permission for this operation.")
        .first().waitFor({ state: "visible", timeout: 60_000 });
      assertEquals(
        new URL(page.url()).pathname,
        "/console/admin/services",
      );
      assertEquals(await page.getByText("Sign in required").count(), 0);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B52 default /console mount keeps internal links under its base path", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const main = page.locator("main");
      // Every internal link stays inside the configured console path, and there
      // is no accidental double prefix from the resolver.
      const links = await main.locator('a[href^="/"]').evaluateAll((nodes) =>
        nodes.map((node) => node.getAttribute("href") ?? "")
      );
      for (const href of links) {
        assertEquals(
          href.startsWith("/console/") || href === "/console",
          true,
          `internal link ${href} must stay inside the console path`,
        );
        assertEquals(
          href.includes("/console/console"),
          false,
          `internal link ${href} must not be double-prefixed`,
        );
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B54 reload, back/forward, and filter changes keep only valid state", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByLabel("Filter by state").selectOption("revoked");
      await page.getByText("No service deployments").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      // A reload re-reads and does not preserve the previous page's rows.
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      await page.getByRole("heading", { name: "Service runtime", exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });

      // Back/forward returns to a route that still exists.
      await page.goto(`${runtime.trellisUrl}/console/admin/users`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.goBack({ waitUntil: "domcontentloaded" });
      await waitForConsoleShell(page);
      assertEquals(
        new URL(page.url()).pathname.startsWith("/console/"),
        true,
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
