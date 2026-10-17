// Console service journeys. Real runtime, real generated client.

import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

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
  seedServiceWithResources,
  seedServiceWithoutResources,
  waitForConsoleShell,
} from "./console_test_support.ts";

test("B01 creating a native service deployment opens its persisted implementation", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.contracts.install({
      contract: participants.EchoProvider.participant,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/services/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);

      const displayName = `ct-b01-${ulid()}`;
      await page.getByLabel("Title", { exact: true }).fill(displayName);
      await page
        .getByLabel("Participant identity")
        .fill(participants.EchoProvider.participant.identity);
      await page
        .getByLabel("Native package source evidence")
        .fill(
          JSON.stringify(participants.EchoProvider.participant.packageEvidence),
        );
      await page
        .getByRole("button", { name: "Create deployment", exact: true })
        .click();
      await page
        .getByRole("link", { name: "Open deployment", exact: true })
        .click();

      await page
        .getByRole("heading", { name: displayName, exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      const deploymentId = codecs.ulid.decode(
        new URL(page.url()).pathname.split("/").at(-1),
      );
      const stored = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId,
      });
      assert.equal(stored.deployment.title, displayName);
      assert.equal(
        stored.deployment.participantId,
        participants.EchoProvider.participant.identity,
      );
      assert.deepEqual(
        new URL(page.url()).pathname.startsWith("/console/admin/services/"),
        true,
        "creating a profile must land on its own detail route",
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B02 installed service resources render from real evidence", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedServiceWithResources(runtime, "b02");
    try {
      const detail = await runtime.callAdminRpc("authResourcesList", {
        ownerId: seeded.deploymentId,
      });
      const resourceCount = detail.items.length;
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(`${runtime.trellisUrl}/console/admin/resources`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        await page
          .getByRole("button", { name: "All retained states", exact: true })
          .click();
        const resourceRows = page
          .locator("tbody tr")
          .filter({ hasText: seeded.deploymentId });
        await resourceRows.first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assert.deepEqual(
          await resourceRows.count(),
          resourceCount,
          "expected one rendered row per materialized resource",
        );
        await assertNoBrowserErrors(errors);
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

test("B03 same-document service navigation replaces A with B", async () => {
  await withTrellisRuntime(async (runtime) => {
    const a = await seedServiceWithResources(runtime, "b03-a");
    try {
      const b = await seedServiceWithoutResources(runtime, "b03-b");
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${a.deploymentId}`,
          { waitUntil: "domcontentloaded" },
        );
        await page
          .getByRole("heading", { name: a.displayName, exact: true })
          .waitFor({
            state: "visible",
          });

        await page.evaluate((deploymentId) => {
          document
            .querySelector('[data-testid="console-ready"]')
            ?.setAttribute("id", "same-console-document");
          const link = document.createElement("a");
          link.href = `/console/admin/services/${encodeURIComponent(
            deploymentId,
          )}`;
          document.body.append(link);
          link.click();
          link.remove();
        }, b.deploymentId);
        await page.waitForURL((url) =>
          url.pathname.endsWith(`/${b.deploymentId}`),
        );
        await page.getByText(b.displayName).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assert.deepEqual(
          await page.getByText(a.displayName).count(),
          0,
          "service A's content must not remain after navigating to B",
        );
        assert.deepEqual(
          await page.locator("#same-console-document").count(),
          1,
          "the document and authority shell were reused",
        );
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await a.stop();
    }
  }, browserRuntimeOptions());
});

test("B04 a missing service ID renders unavailable, not a fallback", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/services/${ulid()}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Not found", { exact: false }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByRole("button", { name: "Refresh", exact: true }).waitFor({
        state: "visible",
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B05 service table pages and resets on filter change", async () => {
  await withTrellisRuntime(async (runtime) => {
    for (let index = 0; index < 52; index += 1) {
      await seedServiceWithoutResources(runtime, `b05-${index}`);
    }
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.trellisUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page
        .getByRole("heading", { name: "Service runtime", exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });
      await page.getByText("on this page").first().waitFor({
        state: "visible",
      });
      const rows = page.locator(
        'main tbody a[href^="/console/admin/services/"]',
      );
      const firstPageId = await rows.first().getAttribute("href");
      await page.getByRole("button", { name: "Next", exact: true }).click();
      await page
        .getByRole("button", { name: "Previous", exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.waitForFunction(
        (first) =>
          document
            .querySelector('main tbody a[href^="/console/admin/services/"]')
            ?.getAttribute("href") !== first,
        firstPageId,
      );
      assert.deepEqual(
        (await rows.first().getAttribute("href")) !== firstPageId,
        true,
      );

      // Filtering hides unmatched rows; clearing it must return to the first page.
      await page.getByLabel("Filter by state").selectOption("disabled");
      await runtime.waitFor(async () => (await rows.count()) === 0);
      await page.getByLabel("Filter by state").selectOption("");
      await page.waitForFunction(
        (first) =>
          document
            .querySelector('main tbody a[href^="/console/admin/services/"]')
            ?.getAttribute("href") === first,
        firstPageId,
      );
      assert.deepEqual(
        await page
          .getByRole("button", { name: "Previous", exact: true })
          .isDisabled(),
        true,
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B06 device creation persists the authored participant and returns its allocated identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.contracts.install({
      contract: participants.Device.participant,
    });
    await runtime.contracts.install({
      contract: participants.ChildProvider.participant,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/profiles/new`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);

      const displayName = `ct-b06-${ulid()}`;
      await page.getByLabel("Title", { exact: true }).fill(displayName);
      await page
        .getByLabel("Participant identity")
        .fill(participants.Device.participant.identity);
      await page
        .getByLabel("Native package source evidence")
        .fill(JSON.stringify(participants.Device.participant.packageEvidence));
      await page
        .getByRole("button", { name: "Create deployment", exact: true })
        .click();
      try {
        await page
          .getByText(/^Deployment .* created\. Implementation /)
          .first()
          .waitFor({ state: "visible", timeout: 30_000 });
      } catch (cause) {
        throw new Error(
          `Device creation did not resolve: ${await page.locator("main").innerText()}`,
          { cause },
        );
      }
      const notice = await page
        .getByText(/^Deployment .* created\. Implementation /)
        .first()
        .innerText();
      const createdId = codecs.ulid.decode(
        /^Deployment (\S+) created\./.exec(notice)?.[1],
      );
      const listed = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: createdId,
      });
      assert.equal(listed.deployment.title, displayName);
      assert.equal(
        listed.deployment.participantId,
        participants.Device.participant.identity,
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B07 provisioning deep link selects exactly the requested deployment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const target = await seedDeviceDeployment(runtime, "b07-target");
    const other = await seedDeviceDeployment(runtime, "b07-other");
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
      assert.deepEqual(await field.inputValue(), targetId);
      assert.deepEqual((await field.inputValue()) === otherId, false);
      // A read-only field proves the deep link cannot drift to another target.
      assert.deepEqual((await field.getAttribute("readonly")) !== null, true);

      const missingId = ulid();
      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/provision?deploymentId=${missingId}`,
        { waitUntil: "domcontentloaded" },
      );
      await page
        .getByText(`No deployment matches '${missingId}'`)
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assert.equal(
        await page
          .getByRole("button", { name: "Provision", exact: true })
          .isDisabled(),
        true,
        "an unavailable deployment cannot be provisioned",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("N05 device disable after query navigation affects only the new target", async () => {
  await withTrellisRuntime(async (runtime) => {
    const a = await seedDeviceDeployment(runtime, "n05-a");
    const b = await seedDeviceDeployment(runtime, "n05-b");
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
        link.href = `/console/admin/devices/profiles/disable?deployment=${encodeURIComponent(
          id,
        )}`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, b.deploymentId);
      await page.waitForURL(
        (url) => url.searchParams.get("deployment") === b.deploymentId,
      );
      await page.locator("dialog[open]").waitFor({ state: "hidden" });
      for (const target of [a, b]) {
        const unchanged = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: target.deploymentId,
        });
        assert.equal(
          unchanged.deployment.state,
          "enabled",
          "stale confirmation must disable neither deployment",
        );
      }
      await page.getByLabel("Deployment").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page.getByLabel("Deployment").inputValue(),
        b.deploymentId,
      );
      await page.getByRole("button", { name: "Disable deployment" }).click();
      const dialog = page.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(b.deploymentId);
      await dialog.getByRole("button", { name: "Disable deployment" }).click();
      await page
        .getByText(`Device deployment ${b.deploymentId} is disabled.`)
        .waitFor({ state: "visible", timeout: 30_000 });
      const [unchanged, disabled] = await Promise.all([
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: a.deploymentId,
        }),
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: b.deploymentId,
        }),
      ]);
      assert.deepEqual(unchanged.deployment.state, "enabled");
      assert.deepEqual(disabled.deployment.state, "disabled");
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B08 provisioning receipt shows the returned identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedDeviceDeployment(runtime, "b08-device");
    const deploymentId = seeded.deploymentId;
    const other = await seedDeviceDeployment(runtime, "b08-next");

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/provision?deploymentId=${deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page
        .getByRole("button", { name: "Provision", exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.getByLabel("Instance title").fill("B08 first instance");
      await page
        .getByRole("button", { name: "Provision", exact: true })
        .click();

      await page.getByText("Provisioning receipt").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const receiptText = await page
        .locator('[data-testid="console-page-ready"], main')
        .first()
        .innerText();
      assert.deepEqual(receiptText.includes(deploymentId), true);
      assert.deepEqual(
        (await page.getByText("Store this secret now.").count()) > 0,
        true,
      );
      const firstInstanceId = await page
        .locator("dd.trellis-identifier")
        .nth(1)
        .innerText();
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/devices/instances/provision?deploymentId=${encodeURIComponent(
          id,
        )}`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, other.deploymentId);
      await page.waitForURL(
        (url) => url.searchParams.get("deploymentId") === other.deploymentId,
      );
      await page
        .getByText(
          `This one-time receipt belongs to deployment ${deploymentId}, not the currently requested deployment ${other.deploymentId}.`,
          { exact: false },
        )
        .waitFor({ state: "visible", timeout: 30_000 });

      // Starting another operation clears the previous one-time secret.
      await page
        .getByRole("button", { name: "Provision another instance" })
        .click();
      await page.getByText("Provisioning receipt").first().waitFor({
        state: "hidden",
        timeout: 10_000,
      });
      await page.getByLabel("Instance title").fill("B08 second instance");
      await page
        .getByRole("button", { name: "Provision", exact: true })
        .click();
      await page.getByText("Provisioning receipt").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page.locator("dd.trellis-identifier").first().innerText(),
        other.deploymentId,
      );
      assert.deepEqual(
        (await page.locator("dd.trellis-identifier").nth(1).innerText()) !==
          firstInstanceId,
        true,
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B09 missing device action targets never default to another row", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedDeviceDeployment(runtime, "b09-device");
    const missingId = ulid();
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/profiles/disable?deployment=${missingId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("is not an active deployment").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.goto(
        `${runtime.trellisUrl}/console/admin/devices/instances/disable?instance=${missingId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("is not an active instance").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      const unchanged = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: seeded.deploymentId,
      });
      assert.equal(
        unchanged.deployment.state,
        "enabled",
        "unavailable targets must not mutate the existing deployment",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B12 device back links stay inside the configured console path", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      for (const route of [
        "/console/admin/devices/profiles/new",
        "/console/admin/devices/instances/provision",
      ]) {
        await page.goto(`${runtime.trellisUrl}${route}`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        await page
          .getByRole("link", { name: /Back to (devices|deployments)/ })
          .first()
          .click();
        await page.waitForURL(
          (url) => url.pathname === "/console/admin/devices",
        );
        await waitForConsoleShell(page);
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
