// Console service journeys. Real runtime, real generated client.

import { assertEquals } from "@std/assert";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  openConsoleAsRuntimeAdmin,
  seedDeviceDeployment,
  seedProvisionableDeviceDeployment,
  seedServiceWithoutParticipant,
  seedServiceWithResources,
  waitForConsolePage,
  waitForConsoleShell,
} from "./console_test_support.ts";

Deno.test("B01 service profile creation opens a no-participant detail", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/services/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);

      const displayName = `ct-b01-${crypto.randomUUID().slice(0, 8)}`;
      await page.getByLabel("Display name").fill(displayName);
      await page.getByRole("button", { name: "Create deployment profile" })
        .click();

      await waitForConsolePage(page);
      await page.getByText(displayName).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByText("No participant installed").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        new URL(page.url()).pathname.startsWith("/console/admin/services/"),
        true,
        "creating a profile must land on its own detail route",
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B02 installed service resources render from real evidence", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedServiceWithResources(runtime, "b02");
    try {
      const detail = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: seeded.deploymentId,
      });
      const resourceCount = detail.resources.length;
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${seeded.deploymentId}`,
          { waitUntil: "domcontentloaded" },
        );
        await waitForConsoleShell(page);
        await waitForConsolePage(page);

        await page.getByText("Resource evidence").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertEquals(
          await page.locator("table tbody tr").count(),
          resourceCount,
          "expected one rendered row per materialized resource",
        );
        assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      // The live provider must stop before runtime teardown, or its socket
      // surfaces an uncaught transport reset in a later test.
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B03 same-document service navigation replaces A with B", async () => {
  await withTrellisRuntime(async (runtime) => {
    const a = await seedServiceWithResources(runtime, "b03-a");
    try {
      const b = await seedServiceWithoutParticipant(runtime, "b03-b");
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${a.deploymentId}`,
          { waitUntil: "domcontentloaded" },
        );
        await waitForConsolePage(page);
        await page.getByText("Resource evidence").first().waitFor({
          state: "visible",
        });

        await page.evaluate((deploymentId) => {
          document.querySelector('[data-testid="console-ready"]')?.setAttribute(
            "id",
            "same-console-document",
          );
          const link = document.createElement("a");
          link.href = `/console/admin/services/${
            encodeURIComponent(deploymentId)
          }`;
          document.body.append(link);
          link.click();
          link.remove();
        }, b.deploymentId);
        await page.waitForURL((url) =>
          url.pathname.endsWith(`/${b.deploymentId}`)
        );
        await page.getByText(b.displayName).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        await page.getByText("No participant installed").first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assertEquals(
          await page.getByText(a.displayName).count(),
          0,
          "service A's content must not remain after navigating to B",
        );
        assertEquals(
          await page.locator("#same-console-document").count(),
          1,
          "the document and authority shell were reused",
        );
        assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await a.stop();
    }
  }, browserRuntimeOptions());
});

Deno.test("B04 a missing service ID renders unavailable, not a fallback", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/services/dep_missing_${crypto.randomUUID()}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Deployment unavailable").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByRole("button", { name: "Retry" }).waitFor({
        state: "visible",
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B05 service table pages and resets on filter change", async () => {
  await withTrellisRuntime(async (runtime) => {
    for (let index = 0; index < 52; index += 1) {
      await seedServiceWithoutParticipant(runtime, `b05-${index}`);
    }
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page.getByRole("heading", { name: "Service runtime", exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });
      await page.getByText("on this page").first().waitFor({
        state: "visible",
      });
      const rows = page.locator('main a[href^="/console/admin/services/dep_"]');
      const firstPageId = await rows.first().getAttribute("href");
      await page.getByRole("button", { name: "Next", exact: true }).click();
      await page.getByRole("button", { name: "Previous", exact: true }).waitFor(
        {
          state: "visible",
          timeout: 30_000,
        },
      );
      await page.waitForFunction(
        (first) =>
          document.querySelector('main a[href^="/console/admin/services/dep_"]')
            ?.getAttribute("href") !== first,
        firstPageId,
      );
      assertEquals(
        await rows.first().getAttribute("href") !== firstPageId,
        true,
      );

      // Filtering by a state with no rows resets the cursor and shows the empty
      // state instead of leaving the previous page's rows.
      await page.getByLabel("Filter by state").selectOption("revoked");
      await page.getByText("No service deployments").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByLabel("Filter by state").selectOption("");
      await page.waitForFunction(
        (first) =>
          document.querySelector('main a[href^="/console/admin/services/dep_"]')
            ?.getAttribute("href") === first,
        firstPageId,
      );
      assertEquals(
        await page.getByRole("button", { name: "Previous", exact: true })
          .isDisabled(),
        true,
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B06 device creation preserves review and delegation settings", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/profiles/new`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);

      const displayName = `ct-b06-${crypto.randomUUID().slice(0, 8)}`;
      let createdId = "";
      await page.getByLabel("Display name").fill(displayName);
      await page.getByLabel("Review mode").selectOption("required");
      await page.getByLabel("Require user delegation").check();
      await page.getByRole("button", { name: "Create deployment profile" })
        .click();
      await page.getByText("Last confirmed device deployment profile:").first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      const notice = await page.getByText(
        "Last confirmed device deployment profile:",
      ).first()
        .innerText();
      const idMatch = /dep_[A-Za-z0-9]+/.exec(notice);
      assertEquals(
        idMatch !== null,
        true,
        "receipt shows the allocated deployment ID",
      );
      createdId = idMatch?.[0] ?? "";

      // Verify the persisted deployment through the generated client: the
      // chosen review mode and delegation flag must round-trip exactly.
      const listed = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: createdId,
      });
      assertEquals(listed.deployment.displayName, displayName);
      assertEquals(listed.deployment.reviewMode instanceof Uint8Array, true);
      assertEquals(
        new TextDecoder().decode(listed.deployment.reviewMode as Uint8Array),
        '"required"',
      );
      assertEquals(listed.deployment.requiresDeviceDelegation, true);
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B07 provisioning deep link selects exactly the requested deployment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const target = await seedProvisionableDeviceDeployment(
      runtime,
      "b07-target",
    );
    const other = await seedProvisionableDeviceDeployment(runtime, "b07-other");
    const targetId = target.deploymentId;
    const otherId = other.deploymentId;

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/provision?deploymentId=${targetId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      const field = page.getByLabel("Deployment");
      await field.waitFor({ state: "visible", timeout: 30_000 });
      assertEquals(await field.inputValue(), targetId);
      assertEquals(await field.inputValue() === otherId, false);
      // A read-only field proves the deep link cannot drift to another target.
      assertEquals(await field.getAttribute("readonly") !== null, true);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/provision?deploymentId=dep_missing`,
        { waitUntil: "domcontentloaded" },
      );
      await page.getByText("No deployment matches 'dep_missing'").first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assertEquals(
        await page.getByRole("button", { name: "Provision", exact: true })
          .isDisabled(),
        true,
        "provisioning an unavailable deployment must be disabled",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("N05 device disable after query navigation affects only the new target", async () => {
  await withTrellisRuntime(async (runtime) => {
    const a = await seedProvisionableDeviceDeployment(runtime, "n05-a");
    const b = await seedProvisionableDeviceDeployment(runtime, "n05-b");
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/profiles/disable?deployment=${a.deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Deployment").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByRole("button", { name: "Disable deployment" }).click();
      await page.locator("dialog[open]").waitFor({ state: "visible" });
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/devices/profiles/disable?deployment=${
          encodeURIComponent(id)
        }`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, b.deploymentId);
      await page.waitForURL((url) =>
        url.searchParams.get("deployment") === b.deploymentId
      );
      await page.locator("dialog[open]").waitFor({ state: "hidden" });
      await page.getByLabel("Deployment").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.getByLabel("Deployment").inputValue(),
        b.deploymentId,
      );
      await page.getByRole("button", { name: "Disable deployment" }).click();
      const dialog = page.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(b.deploymentId);
      await dialog.getByRole("button", { name: "Disable deployment" }).click();
      await page.getByText(`Device deployment ${b.deploymentId} is disabled.`)
        .waitFor({ state: "visible", timeout: 30_000 });
      const [unchanged, disabled] = await Promise.all([
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: a.deploymentId,
        }),
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: b.deploymentId,
        }),
      ]);
      assertEquals(unchanged.deployment.state, "active");
      assertEquals(disabled.deployment.state, "disabled");
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B08 provisioning receipt shows the returned identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedProvisionableDeviceDeployment(
      runtime,
      "b08-device",
    );
    const deploymentId = seeded.deploymentId;
    const other = await seedProvisionableDeviceDeployment(runtime, "b08-next");

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/provision?deploymentId=${deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByRole("button", { name: "Provision", exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.getByRole("button", { name: "Provision", exact: true })
        .click();

      await page.getByText("Provisioning receipt").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const receiptText = await page.locator(
        '[data-testid="console-page-ready"], main',
      ).first().innerText();
      assertEquals(receiptText.includes(deploymentId), true);
      assertEquals(
        await page.getByText("Store this secret now.").count() > 0,
        true,
      );
      const firstInstanceId = await page.locator("dd.trellis-identifier")
        .nth(1).innerText();
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/devices/instances/provision?deploymentId=${
          encodeURIComponent(id)
        }`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, other.deploymentId);
      await page.waitForURL((url) =>
        url.searchParams.get("deploymentId") === other.deploymentId
      );
      await page.getByText(
        `This one-time receipt belongs to deployment ${deploymentId}, not the currently requested deployment ${other.deploymentId}.`,
        { exact: false },
      ).waitFor({ state: "visible", timeout: 30_000 });

      // Starting another operation clears the previous one-time secret.
      await page.getByRole("button", { name: "Provision another instance" })
        .click();
      await page.getByText("Provisioning receipt").first().waitFor({
        state: "hidden",
        timeout: 10_000,
      });
      await page.getByRole("button", { name: "Provision", exact: true })
        .click();
      await page.getByText("Provisioning receipt").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertEquals(
        await page.locator("dd.trellis-identifier").first().innerText(),
        other.deploymentId,
      );
      assertEquals(
        (await page.locator("dd.trellis-identifier").nth(1).innerText()) !==
          firstInstanceId,
        true,
      );
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B09 missing device action targets never default to another row", async () => {
  await withTrellisRuntime(async (runtime) => {
    await seedDeviceDeployment(runtime, "b09-device");
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/profiles/disable?deployment=dep_missing_b09`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("is not an active deployment").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/disable?instance=dev_missing_b09`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("is not an active instance").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/activations/revoke?instance=dev_missing_b09`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("has no active activation").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

Deno.test("B12 device back links stay inside the configured console path", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      for (
        const route of [
          "/console/admin/devices/profiles/new",
          "/console/admin/devices/instances/provision",
          "/console/admin/devices/reviews/decide",
        ]
      ) {
        await page.goto(`${runtime.trellisUrl}${route}`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const links = await page.getByRole("link", { name: /Back to devices/ })
          .all();
        for (const link of links) {
          const href = await link.getAttribute("href");
          assertEquals(
            href?.startsWith("/console/") ?? false,
            true,
            `back link ${href} escaped the console path on ${route}`,
          );
        }
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
