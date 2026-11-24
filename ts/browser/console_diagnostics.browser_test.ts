import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { chromium } from "playwright";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { types } from "../packages/trellis-testkit/trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  fixtureName,
  openConsoleAsRuntimeAdmin,
  waitForConsoleShell,
} from "./console_test_support.ts";

// An administrator opens Diagnostics from the sidebar and sees a healthy
// Trellis with its version and certificate. A service then connects without
// its required settings; checking again names the deployment and the settings
// it waits for. Two deployments with one title each keep their own row, told
// apart by ID, across checks. A user without the diagnostics capability gets
// no Diagnostics item. This catches a page that shows a stale or partial
// report, misses a waiting instance, merges or swaps rows that share a title,
// or is offered to users who cannot read it.
test("Diagnostics shows the installation's state and names what needs attention", async () => {
  await withTrellisRuntime(async (runtime) => {
    const credentials = {
      username: "diagnostics-outsider",
      password: "diagnostics-outsider-password",
    };
    await createOrdinaryUserWithPassword(runtime, credentials);
    const context = await launchProfile(runtime);
    let waiting:
      | {
          deploymentId: string;
          connecting: Promise<{ stop(): Promise<void> }>;
        }
      | undefined;
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await waitForConsoleShell(page);
      await page.getByRole("link", { name: "Diagnostics" }).first().click();
      const overall = page.getByTestId("diagnostics-status");
      // A just-started Trellis warns until its own instance first reports.
      const fine = overall.getByText("Everything looks fine.");
      await overall.getByText(/Checked/).waitFor();
      await runtime.waitFor(
        async () => {
          if ((await fine.count()) > 0) return true;
          await page.getByRole("button", { name: "Check again" }).click();
          await page
            .locator("button:not([disabled])", { hasText: "Check again" })
            .waitFor();
          return (await fine.count()) > 0;
        },
        { timeoutMs: 30_000 },
      );
      await page
        .getByTestId("diagnostics-server")
        .getByText(/^\d+\.\d+\.\d+/)
        .waitFor();
      await page
        .getByTestId("diagnostics-certificates")
        .getByText("Served", { exact: true })
        .waitFor();

      const name = fixtureName("waiting-settings");
      const key = await runtime.services.createInstance({
        name,
        deployment: name,
        contract: participants.ConfiguredService.participant,
      });
      waiting = {
        deploymentId: key.deploymentId,
        connecting: TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.ConfiguredService.participant,
          seed: key.seed,
        }).orThrow(),
      };
      const health = page.getByTestId("diagnostics-health");
      const finding = health.getByText(
        `${name}: instance ${key.instanceId} waits for settings: apiKey, endpoint`,
        { exact: false },
      );
      await runtime.waitFor(
        async () => {
          await page.getByRole("button", { name: "Check again" }).click();
          await overall.getByText(/Checked/).waitFor();
          return (await finding.count()) > 0;
        },
        { timeoutMs: 60_000 },
      );
      await overall.getByText("Some things need fixing.").waitFor();
      await health.getByText("Problem", { exact: true }).waitFor();
      await health.getByText("Waiting for settings").waitFor();

      // Two deployments share a title; only the second runs an instance.
      // Each keeps its own row, told apart by its ID, through every check.
      const shared = fixtureName("same-title");
      const running = await runtime.registerService({
        name: shared,
        deployment: shared,
        contract: participants.EchoProvider.participant,
      });
      const idle = await runtime.callAdminRpc("authDeploymentsCreate", {
        title: shared,
        kind: "service",
        participantId: participants.EchoProvider.participant.identity,
        packageEvidence: types.AuthPackageEvidenceCodec.decode(
          participants.EchoProvider.participant.packageEvidence,
        ),
        acknowledgeSharedApis: true,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const echo = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.EchoProvider.participant,
        name: shared,
        seed: running.seed,
      }).orThrow();
      try {
        const row = (deploymentId: string) =>
          health
            .locator("dt", { hasText: deploymentId })
            .locator("xpath=following-sibling::dd[1]");
        const idleRow = row(idle.deployment.deploymentId);
        const runningRow = row(running.deploymentId);
        await runtime.waitFor(
          async () => {
            await page.getByRole("button", { name: "Check again" }).click();
            await page
              .locator("button:not([disabled])", { hasText: "Check again" })
              .waitFor();
            return (await runningRow.textContent())?.trim() === "Healthy";
          },
          { timeoutMs: 60_000 },
        );
        for (let check = 0; check < 2; check++) {
          await page.getByRole("button", { name: "Check again" }).click();
          await page
            .locator("button:not([disabled])", { hasText: "Check again" })
            .waitFor();
          assert.equal(
            await health.locator("dt", { hasText: shared }).count(),
            2,
          );
          assert.equal((await idleRow.textContent())?.trim(), "No instances");
          assert.equal((await runningRow.textContent())?.trim(), "Healthy");
        }
      } finally {
        await echo.stop();
      }

      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }

    // A profile of its own, so the administrator's sign-in is not reused.
    const outsider = await chromium.launchPersistentContext(
      join(runtime.workdir, "browser-profile-outsider"),
      { headless: true, env: browserTrustEnvironment() },
    );
    try {
      const page = await outsider.newPage();
      await openConsole(page, runtime, credentials);
      await waitForConsoleShell(page);
      // The ordinary user's sidebar is there, without Diagnostics.
      await page.getByRole("link", { name: "Account", exact: true }).waitFor();
      assert.equal(
        await page.getByRole("link", { name: "Diagnostics" }).count(),
        0,
      );
    } finally {
      await outsider.close();
    }
    // Once its settings are set, the waiting service serves and stops.
    if (waiting) {
      await runtime.callAdminRpc("authConfigSet", {
        target: { deploymentId: codecs.ulid.decode(waiting.deploymentId) },
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "k1" },
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      await (await waiting.connecting).stop();
    }
  }, browserRuntimeOptions());
});
