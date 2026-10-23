import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import { ulid } from "ulid";
import { Result } from "@oatscenter/result";
import { deriveDeviceIdentity } from "@oatscenter/trellis/auth";
import {
  checkDeviceActivation,
  TrellisDevice,
} from "@oatscenter/trellis/device";
import {
  connectNativeClient,
  discoverNativeOAuth,
  NativeOAuthClient,
  MemoryNativeOAuthCustody,
} from "@oatscenter/trellis/native";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  participants,
  types,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";

test("an enrolled device uses an independently browser-consented native companion", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerIdentity = await runtime.registerService({
      name: "device-child-provider",
      contract: participants.ChildProvider.participant,
    });
    const provider = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.ChildProvider.participant,
      seed: providerIdentity.seed,
    }).orThrow();
    const providerExit = provider.wait().catch(() => undefined);
    try {
      await provider.handleRequired(() => Result.ok({}));
      await provider.handleOptional(() => Result.ok({}));
      const participant = participants.AgentDevice.participant;
      const request = participants.AgentDevice.Companion.request;
      const deployment = await runtime.contracts.apply({
        deployment: "agent-device",
        contract: participant,
      });
      const deploymentId = types.AuthDeploymentGetRequestCodec.decode({
        deploymentId: deployment.deploymentId,
      }).deploymentId;
      const provisioned = await runtime.devices.provision({
        deploymentId,
        title: "Browser-consented device",
        idempotencyKey: ulid(),
      });
      assert(provisioned.provisioningSecret);
      const rootSecret = crypto.getRandomValues(new Uint8Array(32));
      const activation = await checkDeviceActivation({
        trellisUrl: runtime.oauthIssuer,
        participant,
        rootSecret,
        provisioningSecret: provisioned.provisioningSecret,
        statePath: join(runtime.workdir, "activation.json"),
      });
      assert(activation.status === "activation_required");
      const reviewId = types.AuthDeviceReviewGetRequestCodec.decode({
        reviewId: new URL(activation.activationUrl).searchParams.get(
          "reviewId",
        ),
        confirmationCode: activation.confirmationCode,
      }).reviewId;
      const { review } = await runtime.callAdminRpc("authDevicesReviewsGet", {
        reviewId,
        confirmationCode: activation.confirmationCode,
      });
      const approved = await runtime.callAdminRpc("authDevicesReviewsDecide", {
        reviewId,
        confirmationCode: activation.confirmationCode,
        expectedRevision: review.revision,
        decision: "approved",
        idempotencyKey: ulid(),
      });
      await activation.waitForOnlineApproval();
      const prepared = await runtime.registerClient({
        name: "companion-user-grants",
        contract: request,
      });
      await prepared.session.close();
      const server = (await discoverNativeOAuth(runtime.oauthIssuer)).orThrow();
      const credentialCustody = new MemoryNativeOAuthCustody();
      const custody = (
        await NativeOAuthClient.open({
          custody: credentialCustody,
          scope: {
            issuer: server.issuer,
            instanceId: server.trust.instanceId,
            account: server.trust.account,
            clientId: `trellis.device.${deploymentId}.companion`,
            credentialHandle: ulid(),
          },
          trust: server.trust,
        })
      ).orThrow();
      const context = await launchProfile(runtime);
      try {
        const flow = (await custody.startDevice(request)).orThrow();
        const page = await context.newPage();
        await page.goto(flow.verificationUri);
        await page.getByLabel("Code", { exact: true }).fill(flow.userCode);
        await page
          .getByRole("button", { name: "Continue", exact: true })
          .click();
        await page
          .getByLabel("Username", { exact: true })
          .fill(runtime.adminUsername);
        await page
          .getByLabel("Password", { exact: true })
          .fill(runtime.adminPassword);
        await page
          .getByRole("button", { name: "Sign in", exact: true })
          .click();
        await page
          .getByRole("heading", { name: /wants to use your account/ })
          .waitFor();
        for (const choice of await page.getByRole("checkbox").all())
          if (await choice.isEnabled()) await choice.check();
        await page.getByRole("button", { name: "Allow", exact: true }).click();
        const session = (await flow.complete()).orThrow();
        const scope = session.scope;
        const companion = await connectNativeClient({
          session,
          servers: server.natsServers,
          request,
        }).orThrow();
        try {
          await companion
            .deviceCompanionsAttach({
              reviewId,
              confirmationCode: activation.confirmationCode,
              expectedRevision: approved.review.revision,
              idempotencyKey: ulid(),
            })
            .orThrow();
        } finally {
          await companion.connection.close();
        }
        const resumedCustody = (
          await NativeOAuthClient.open({
            custody: credentialCustody,
            scope,
            trust: server.trust,
          })
        ).orThrow();
        try {
          const resumedSession = (await resumedCustody.resume()).orThrow();
          const identity = await deriveDeviceIdentity(rootSecret);
          const device = await TrellisDevice.connect({
            participant,
            servers: [...server.natsServers],
            trellisInstanceId: server.trust.instanceId,
            audienceNatsAccount: server.trust.account,
            issuers: [server.trust.issuer],
            seed: identity.identitySeedBase64url,
            companion: { session: resumedSession, servers: server.natsServers },
          }).orThrow();
          try {
            assert(device.companion);
            assert.deepEqual(await device.companion.required({}).orThrow(), {});
          } finally {
            await device.companion?.connection.close();
            await device.connection.close();
          }
        } finally {
          await resumedCustody.close();
        }
      } finally {
        await context.close();
        await custody.close();
      }
    } finally {
      await provider.stop();
      await providerExit;
    }
  }, browserRuntimeOptions());
});
