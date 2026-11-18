import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { test } from "node:test";
import nacl from "tweetnacl";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
  startedServices,
} from "./browser_test_support.ts";

const DEPLOYMENT = "console-service-update";

test("an administrator approves a waiting service update from its review link", async () => {
  const services = startedServices();
  try {
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
        trellisUrl: runtime.trellisUrl,
        participant: extendedParticipants.Provider.participant,
        seed: instance.seed,
        signal: services.signal,
      }).orThrow();
      services.add(connecting);
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
        assert(pending.reviewUrl);
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
        // The decision's outcome, or whatever the page shows instead.
        await page
          .getByText(/^Approved\./)
          .waitFor()
          .catch(async (cause: unknown) => {
            const shown = await page.locator("main").innerText();
            throw new Error(`no approval notice; the page shows: ${shown}`, {
              cause,
            });
          });
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
  } finally {
    // Aborts a connect still waiting for approval, then stops the rest.
    await services.stop();
  }
});

test("a service's page shows the build applied after an earlier self-update", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const deployment = "console-applied-build";
      const base = participants.Provider.participant;
      await runtime.deployments.create({ id: deployment, contract: base });
      await runtime.contracts.apply({ deployment, contract: base });
      const first = await runtime.services.createInstance({
        name: deployment,
        deployment,
        contract: base,
      });
      // The instance updates itself to a new build, approved as usual.
      const updating = TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: extendedParticipants.Provider.participant,
        seed: first.seed,
        signal: services.signal,
      }).orThrow();
      services.add(updating);
      const pending = await runtime.waitFor(async () => {
        const { items } = await runtime.callAdminRpc(
          "authServiceUpdatesList",
          {},
        );
        return items.find((update) => update.state === "pending");
      });
      await runtime.callAdminRpc("authServiceUpdatesDecide", {
        updateId: pending.updateId,
        decision: "approved",
        expectedRevision: pending.revision,
        idempotencyKey: ulid(),
      });
      await (await updating).stop();

      // Then an administrator applies a build directly, which leaves no
      // update in the history, and a new instance connects running it.
      await runtime.contracts.apply({ deployment, contract: base });
      const second = await runtime.services.createInstance({
        name: deployment,
        deployment,
        contract: base,
      });
      const running = TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: base,
        seed: second.seed,
        signal: services.signal,
      }).orThrow();
      services.add(running);
      await running;

      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${first.deploymentId}`,
          { waitUntil: "domcontentloaded" },
        );
        await completeConsoleEntry(page, BROWSER_ADMIN);
        const row = page.locator(`tr[data-instance-id="${second.instanceId}"]`);
        // Health is read once per load, so reload until it reports the
        // instance connected.
        await runtime.waitFor(async () => {
          await row.waitFor();
          if ((await row.getByText(/^Seen /).count()) > 0) return true;
          await page.reload({ waitUntil: "domcontentloaded" });
          return false;
        });
        const summary = page.locator("p", { hasText: /instances? on/ }).first();
        // The heading names the applied build, the one the instance reports,
        // not the self-update's; no update installed it, so it has no date.
        const updated = pending.participantDigest.slice(0, 8);
        const text = await summary.innerText();
        const build = await row.locator("td").nth(3).innerText();
        assert.equal(text.includes(build), true, `${text} / ${build}`);
        assert.equal(text.includes(updated), false, text);
        assert.equal(text.includes("installed"), false, text);
        assert.equal(await row.getByText("Runs an older build").count(), 0);
      } finally {
        await admin.close();
      }
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
  }
});

test("an administrator adds a service that introduced itself from its review link", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const seed = randomBytes(32);
      const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
        title: "third-party-orders",
        instanceTitle: "third-party-orders",
        identityPublicKey: Buffer.from(
          nacl.sign.keyPair.fromSeed(seed).publicKey,
        ).toString("base64url"),
        idempotencyKey: ulid(),
      });
      const connecting = TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: seed.toString("base64url"),
        signal: services.signal,
      }).orThrow();
      services.add(connecting);
      const pending = await runtime.waitFor(async () => {
        const { items } = await runtime.callAdminRpc(
          "authServiceUpdatesList",
          {},
        );
        return items.find(
          (update) =>
            update.deploymentId === claim.claimId && update.state === "pending",
        );
      });

      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        assert(pending.reviewUrl);
        await page.goto(pending.reviewUrl, { waitUntil: "domcontentloaded" });
        await completeConsoleEntry(page, BROWSER_ADMIN);
        await page
          .getByRole("heading", { name: "third-party-orders" })
          .waitFor();
        await page.getByText("Waiting for its service").waitFor();
        const review = page.getByRole("region", { name: "Service update" });
        await review
          .getByRole("button", { name: "Approve update", exact: true })
          .click();
        // Approving adds the service: the page moves to the new deployment.
        await page.waitForURL(
          (url) =>
            url.pathname.includes("/admin/services/") &&
            !url.pathname.includes(claim.claimId),
        );
        await page
          .getByRole("heading", { name: "third-party-orders" })
          .waitFor();
      } finally {
        await admin.close();
      }

      const service = await connecting;
      try {
        await service.kv.records
          .put("added", { value: "from the Console" })
          .orThrow();
        assert.deepEqual(await service.kv.records.get("added").orThrow(), {
          value: "from the Console",
        });
      } finally {
        await service.stop();
      }
    }, browserRuntimeOptions());
  } finally {
    // Aborts a connect still waiting for approval, then stops the rest.
    await services.stop();
  }
});
