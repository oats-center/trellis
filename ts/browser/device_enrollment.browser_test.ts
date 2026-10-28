import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import { ulid } from "ulid";
import { checkDeviceActivation } from "@oatscenter/trellis/device";
import {
  participants,
  types,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";

test("an administrator approves a device from its activation link", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const participant = participants.StateDevice.participant;
    const deployment = await runtime.contracts.apply({
      deployment: "portal-enrolled-device",
      contract: participant,
    });
    const deploymentId = types.AuthDeploymentGetRequestCodec.decode({
      deploymentId: deployment.deploymentId,
    }).deploymentId;
    const provisioned = await runtime.devices.provision({
      deploymentId,
      title: "Loading dock sensor",
      idempotencyKey: ulid(),
    });
    assert(provisioned.provisioningSecret);
    const activation = await checkDeviceActivation({
      trellisUrl: runtime.oauthIssuer,
      participant,
      rootSecret: crypto.getRandomValues(new Uint8Array(32)),
      provisioningSecret: provisioned.provisioningSecret,
      statePath: join(runtime.workdir, "activation.json"),
    });
    assert(activation.status === "activation_required");

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await page.goto(activation.activationUrl);
      await page
        .getByRole("heading", { name: "Approve a device", exact: true })
        .waitFor();
      // A wrong code is remembered through sign-in and then refused.
      await page.getByLabel("Code", { exact: true }).fill("WRONG000");
      await page.getByRole("button", { name: "Continue", exact: true }).click();
      await page
        .getByLabel("Username", { exact: true })
        .fill(runtime.adminUsername);
      await page
        .getByLabel("Password", { exact: true })
        .fill(runtime.adminPassword);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await page.getByText("That code doesn't match").waitFor();

      await page
        .getByLabel("Code", { exact: true })
        .fill(activation.confirmationCode);
      await page.getByRole("button", { name: "Continue", exact: true }).click();
      await page.getByText("Loading dock sensor", { exact: true }).waitFor();
      await page.getByRole("button", { name: "Approve", exact: true }).click();
      await page
        .getByRole("heading", { name: "Device approved", exact: true })
        .waitFor();
    } finally {
      await context.close();
    }

    // The device itself observes the approval and can continue activation.
    await activation.waitForOnlineApproval();
    const reviewId = types.AuthDeviceReviewGetRequestCodec.decode({
      reviewId: new URL(activation.activationUrl).searchParams.get("reviewId"),
      confirmationCode: activation.confirmationCode,
    }).reviewId;
    const { review } = await runtime.callAdminRpc("authDevicesReviewsGet", {
      reviewId,
      confirmationCode: activation.confirmationCode,
    });
    assert.equal(review.state, "approved");
  }, browserRuntimeOptions());
});
