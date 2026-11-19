/**
 * An administrator watches an upgrade from the Console and the CLI: the new
 * build's instance waits for approval, then serves the current build beside
 * the old instance's older one, and the old instance leaves when it stops.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium } from "playwright";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  loginCliAsAdministrator,
} from "./browser_test_support.ts";
import { openConsoleAsRuntimeAdmin } from "./console_test_support.ts";

type Listed = {
  instanceId: string;
  status: {
    state: string;
    build: { digest?: string; current: boolean };
    connectedAt?: string;
    lastHeartbeat?: string;
    reviewUrl?: string;
  };
};

test("the Console and the CLI show both instances and their states while one build takes over from another", async () => {
  await withTrellisRuntime(async (runtime) => {
    const base = participants.Provider.participant;
    const extended = extendedParticipants.Provider.participant;
    await runtime.deployments.create({ id: "takeover", contract: base });
    await runtime.contracts.apply({ deployment: "takeover", contract: base });
    const oldKey = await runtime.services.createInstance({
      name: "takeover-old",
      deployment: "takeover",
      contract: base,
    });
    const newKey = await runtime.services.createInstance({
      name: "takeover-new",
      deployment: "takeover",
      contract: base,
    });
    const deploymentId = oldKey.deploymentId;
    const old = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: base,
      seed: oldKey.seed,
    }).orThrow();
    const oldExit = old.wait().catch((error: unknown) => error);
    let fresh: Awaited<ReturnType<typeof connectNew>> | undefined;
    const connectNew = () =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: extended,
        seed: newKey.seed,
      }).orThrow();
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    const context = await launchProfile(runtime);
    try {
      const { run } = await loginCliAsAdministrator(
        runtime,
        await browser.newPage(),
      );
      const listed = async () =>
        (
          (await run("svc", deploymentId, "instances")) as {
            instances: Listed[];
          }
        ).instances;
      const status = async (instanceId: string) =>
        (await listed()).find((item) => item.instanceId === instanceId)?.status;
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.consoleUrl}/console/admin/services/${deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      const row = (instanceId: string) =>
        page.locator(`tr[data-instance-id="${instanceId}"]`);
      const runtimeState = (instanceId: string) =>
        row(instanceId).locator("[data-runtime-state]");
      const shows = (instanceId: string, state: string) =>
        runtime.waitFor(
          async () =>
            (await runtimeState(instanceId).getAttribute(
              "data-runtime-state",
            )) === state,
          { timeoutMs: 30_000 },
        );

      // The old build serves; the new instance has never connected.
      await shows(oldKey.instanceId, "serving");
      await shows(newKey.instanceId, "offline");
      await row(oldKey.instanceId)
        .getByText("Current", { exact: true })
        .waitFor();

      // The new build asks for a KV, so it waits for an administrator.
      const connecting = connectNew();
      await shows(newKey.instanceId, "waitingForApproval");
      const review = row(newKey.instanceId).getByRole("link", {
        name: "Review the update",
      });
      const reviewUrl = await review.getAttribute("href");
      const waiting = await status(newKey.instanceId);
      assert.equal(waiting?.state, "waitingForApproval");
      assert.equal(waiting?.reviewUrl, reviewUrl);
      assert(waiting?.connectedAt);
      assert.equal((await status(oldKey.instanceId))?.state, "serving");

      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      const pending = items.find((update) => update.state === "pending");
      assert(pending);
      await runtime.callAdminRpc("authServiceUpdatesDecide", {
        updateId: pending.updateId,
        decision: "approved",
        expectedRevision: pending.revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      fresh = await connecting;
      const freshExit = fresh.wait().catch((error: unknown) => error);

      // Both serve: the new instance the current build, the old one an older.
      await shows(newKey.instanceId, "serving");
      await row(newKey.instanceId)
        .getByText("Current", { exact: true })
        .waitFor();
      await row(oldKey.instanceId)
        .getByText("Older", { exact: true })
        .waitFor();
      const both = await listed();
      const byId = new Map(both.map((item) => [item.instanceId, item.status]));
      assert.equal(byId.get(newKey.instanceId)?.state, "serving");
      assert.equal(byId.get(newKey.instanceId)?.build.digest, extended.digest);
      assert.equal(byId.get(newKey.instanceId)?.build.current, true);
      assert.equal(byId.get(oldKey.instanceId)?.state, "serving");
      assert.equal(byId.get(oldKey.instanceId)?.build.digest, base.digest);
      assert.equal(byId.get(oldKey.instanceId)?.build.current, false);
      assert(byId.get(oldKey.instanceId)?.lastHeartbeat);

      // The old instance stops and leaves.
      await old.stop();
      assert.equal(await oldExit, undefined);
      await shows(oldKey.instanceId, "offline");
      assert.equal((await status(oldKey.instanceId))?.state, "offline");
      assert.equal((await status(newKey.instanceId))?.state, "serving");
      await fresh.stop();
      assert.equal(await freshExit, undefined);
    } finally {
      await context.close();
      await browser.close();
      await old.stop();
      await fresh?.stop();
    }
  }, browserRuntimeOptions());
});
