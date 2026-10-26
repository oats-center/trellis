import assert from "node:assert/strict";
import { test } from "node:test";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
} from "./browser_test_support.ts";

const DEPLOYMENT = "console-service-update";

test("an administrator approves a waiting service update from its review link", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const base = participants.Provider.participant;
    await runtime.deployments.create({ id: DEPLOYMENT, contract: base });
    await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: base });
    const instance = await runtime.services.createInstance({
      name: DEPLOYMENT,
      deployment: DEPLOYMENT,
      contract: base,
    });
    // The new build asks for a KV the deployment never had, so it waits.
    const connecting = TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: extendedParticipants.Provider.participant,
      seed: instance.seed,
    }).orThrow();
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });

    const admin = await launchProfile(runtime);
    try {
      const page = await admin.newPage();
      // The link the service printed, opened by a signed-out administrator.
      await page.goto(pending.reviewUrl, { waitUntil: "domcontentloaded" });
      await completeConsoleEntry(page, BROWSER_ADMIN);
      const review = page.getByRole("region", { name: "Service update" });
      await review
        .getByRole("heading", { name: "A new build wants more access" })
        .waitFor();
      await review.getByText("Key-value store “extended”").waitFor();
      await review
        .getByRole("button", { name: "Approve update", exact: true })
        .click();
      await page.getByText(/^Approved\./).waitFor();
      await review.getByText("Installed").waitFor();
    } finally {
      await admin.close();
    }

    const service = await connecting;
    try {
      await service.kv.extended
        .put("approved", { value: "from the Console" })
        .orThrow();
      assert.deepEqual(await service.kv.extended.get("approved").orThrow(), {
        value: "from the Console",
      });
    } finally {
      await service.stop();
    }
  }, browserRuntimeOptions());
});
