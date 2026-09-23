import { assert, assertEquals } from "@std/assert";

import { Result } from "@oatscenter/result";
import {
  checkDeviceActivation,
  TrellisDevice,
} from "@oatscenter/trellis/device";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  signInIfPrompted,
} from "./browser_test_support.ts";

Deno.test("device agent activation completes through the browser portal", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    await runtime.deployments.create({ id: "child-provider", kind: "service" });
    const childProviderIdentity = await runtime.services.createInstance({
      deployment: "child-provider",
      name: "device-child-provider",
      contract: participants.ChildProvider.participant,
    });
    const childProvider = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.ChildProvider.participant,
      seed: childProviderIdentity.seed,
    }).orThrow();
    await childProvider.handleRequired(() => Result.ok({}));
    await childProvider.handleOptional(() => Result.ok({}));
    await runtime.deployments.create({
      id: "agent-device",
      kind: "device",
      reviewMode: "none",
    });
    await runtime.contracts.install({
      contract: participants.AgentDevice.Companion.participant,
    });
    await runtime.contracts.apply({
      deployment: "agent-device",
      contract: participants.AgentDevice.participant,
    });
    await runtime.ensurePortalConsentPolicy(
      participants.AgentDevice.Companion.participant.identity,
      [
        "capability:runtime-trellis.device_child@v1::required",
        "capability:runtime-trellis.device_child@v1::optional",
        "capability:trellis.auth@v1::public",
        "capability:trellis.state@v1::public",
      ],
    );
    const provisioned = await runtime.devices.provision({
      deploymentId: "agent-device",
      participantId: participants.AgentDevice.participant.identity,
      identityPublicKey: null,
      instanceId: null,
      idempotencyKey: crypto.randomUUID(),
    });
    assert(provisioned.provisioningSecret);
    const rootSecret = crypto.getRandomValues(new Uint8Array(32));
    const activation = await checkDeviceActivation({
      trellisUrl: runtime.trellisUrl,
      participant: participants.AgentDevice.participant,
      rootSecret,
      provisioningSecret: provisioned.provisioningSecret,
    });
    assert(activation.status === "activation_required");

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const pageErrors: string[] = [];
      page.on("pageerror", (error) => pageErrors.push(String(error)));
      await page.goto(activation.activationUrl, {
        waitUntil: "domcontentloaded",
      });
      const continueButton = page.getByRole("button", {
        name: "Continue to sign in",
      });
      if (
        await continueButton.waitFor({ state: "visible", timeout: 3_000 })
          .then(() => true)
          .catch(() => false)
      ) {
        await continueButton.click();
      }
      await signInIfPrompted(
        page,
        { username: runtime.adminUsername, password: runtime.adminPassword },
      );
      const confirmationCode = page.getByLabel("Confirmation code");
      await confirmationCode.waitFor({ state: "visible", timeout: 30_000 });
      if ((await confirmationCode.inputValue()).trim() === "") {
        await confirmationCode.fill(activation.confirmationCode);
      }
      await page.getByRole("button", { name: "Approve device" }).click();
      await page
        .getByText("Approval complete.")
        .waitFor({ state: "visible", timeout: 60_000 });

      const device = await TrellisDevice.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.AgentDevice.participant,
        rootSecret,
      }).orThrow();
      const companion = device.companion;
      assert(companion);

      await page
        .getByText("Approval complete.")
        .waitFor({ state: "visible", timeout: 60_000 });
      assertEquals(companion.connection.status.phase, "connected");
      assertEquals(pageErrors, []);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
