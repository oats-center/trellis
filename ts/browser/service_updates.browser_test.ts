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

test("an administrator adds a service that introduced itself from its review link", async () => {
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
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      seed: seed.toString("base64url"),
    }).orThrow();
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
      await page.goto(pending.reviewUrl, { waitUntil: "domcontentloaded" });
      await completeConsoleEntry(page, BROWSER_ADMIN);
      await page.getByRole("heading", { name: "third-party-orders" }).waitFor();
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
      await page.getByRole("heading", { name: "third-party-orders" }).waitFor();
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
});
