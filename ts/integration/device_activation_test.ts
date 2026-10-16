import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import {
  checkDeviceActivation,
  TrellisDevice,
} from "@oatscenter/trellis/device";
import { TrellisService } from "@oatscenter/trellis/service";
import { deriveDeviceIdentity } from "@oatscenter/trellis/auth";
import {
  connectNativeClient,
  discoverNativeOAuth,
  NativeOAuthClient,
} from "@oatscenter/trellis/native";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import {
  participants,
  types,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("device enrollment and independent companion consent survive restart without granting optional authority", async () => {
  await withTrellisRuntime(async (runtime) => {
    const providerIdentity = await runtime.registerService({
      deployment: "child-provider",
      name: "device-child-provider",
      contract: participants.ChildProvider.participant,
    });
    const provider = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.ChildProvider.participant,
      seed: providerIdentity.seed,
    }).orThrow();
    const providerExit = provider.wait().catch(() => undefined);
    await provider.handleRequired(() => Result.ok({}));
    await provider.handleOptional(() => Result.ok({}));
    const participant = participants.Device.participant;
    const request = participants.Device.Companion.request;
    const deployment = await runtime.contracts.apply({
      deployment: "device",
      contract: participant,
    });
    assert(deployment.deploymentId);
    const deploymentId = types.AuthDeploymentGetRequestCodec.decode({
      deploymentId: deployment.deploymentId,
    }).deploymentId;
    const provisioned = await runtime.devices.provision({
      deploymentId,
      title: "Device",
      idempotencyKey: ulid(),
    });
    assert(provisioned.provisioningSecret);
    const rootSecret = crypto.getRandomValues(new Uint8Array(32));
    const activationOptions = {
      trellisUrl: runtime.oauthIssuer,
      participant,
      rootSecret,
      provisioningSecret: provisioned.provisioningSecret,
      statePath: join(runtime.workdir, "activation.json"),
    };
    const server = (await discoverNativeOAuth(runtime.oauthIssuer)).orThrow();
    const identity = await deriveDeviceIdentity(rootSecret);
    const connectOptions = {
      servers: [...server.natsServers],
      trellisInstanceId: server.trust.instanceId,
      audienceNatsAccount: server.trust.account,
      issuers: [server.trust.issuer],
      seed: identity.identitySeedBase64url,
      participant,
    };
    const directory = join(runtime.workdir, "companion-oauth");
    let device:
      | Awaited<
          ReturnType<
            ReturnType<
              typeof TrellisDevice.connect<typeof participant>
            >["orThrow"]
          >
        >
      | undefined;
    try {
      const activation = await checkDeviceActivation(activationOptions);
      assert.equal(activation.status, "activation_required");
      if (activation.status !== "activation_required")
        throw new Error("Missing review");
      await assert.rejects(() =>
        TrellisDevice.connect(connectOptions).orThrow(),
      );
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
      await assert.rejects(() =>
        TrellisDevice.connect(connectOptions).orThrow(),
      );
      assert.equal(
        (await checkDeviceActivation(activationOptions)).status,
        "activated",
      );
      // Preparing the ordinary test account does not register a substitute OAuth
      // client: the accepted device declaration owns this exact native client.
      const prepared = await runtime.registerClient({
        name: "companion-user-grants",
        contract: request,
      });
      await prepared.session.close();
      const login = await loginNativeTestClient({
        issuer: runtime.oauthIssuer,
        directory,
        clientId: `trellis.device.${deployment.deploymentId}.companion`,
        request,
        username: runtime.adminUsername,
        password: runtime.adminPassword,
        approvedCapabilities: request.requiredCapabilities,
      });
      const scope = login.session.scope;
      const trust = login.session.trust;
      const companion = await connectNativeClient({
        ...login,
        request,
      }).orThrow();
      try {
        assert.deepEqual(await companion.required({}).orThrow(), {});
        assert((await companion.optional({})).isErr());
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
      let custody = (
        await NativeOAuthClient.open({
          directory,
          scope,
          trust,
        })
      ).orThrow();
      let session = (await custody.resume()).orThrow();
      device = await TrellisDevice.connect({
        ...connectOptions,
        companion: { session, servers: login.servers },
      }).orThrow();
      assert(device.companion);
      assert.notEqual(device.companion.connection, device.connection);
      assert.deepEqual(await device.companion.required({}).orThrow(), {});
      assert((await device.companion.optional({})).isErr());
      await device.state.telemetry.set({ value: "device retained" }).orThrow();
      await device.companion.state.shared
        .set({ value: "user retained" })
        .orThrow();
      await device.companion.connection.close();
      await device.connection.close();
      device = undefined;
      await provider.stop();
      await providerExit;
      await runtime.restart();
      assert.equal(
        (await checkDeviceActivation(activationOptions)).status,
        "activated",
      );
      custody = (
        await NativeOAuthClient.open({
          directory,
          scope,
          trust,
        })
      ).orThrow();
      session = (await custody.resume()).orThrow();
      device = await TrellisDevice.connect({
        ...connectOptions,
        companion: { session, servers: login.servers },
      }).orThrow();
      assert.deepEqual((await device.state.telemetry.get().orThrow())?.value, {
        value: "device retained",
      });
      assert.deepEqual(
        (await device.companion.state.shared.get().orThrow())?.value,
        { value: "user retained" },
      );
      const { deployment: current } = await runtime.callAdminRpc(
        "authDeploymentsGet",
        {
          deploymentId,
        },
      );
      await runtime.callAdminRpc("authDeploymentsDisable", {
        deploymentId: current.deploymentId,
        expectedRevision: current.revision,
        idempotencyKey: ulid(),
      });
      await assert.rejects(() =>
        TrellisDevice.connect(connectOptions).orThrow(),
      );
      await assert.rejects(() => checkDeviceActivation(activationOptions), {
        status: 403,
        code: "device_enrollment_denied",
      });
      await device.companion.connection.close();
      await device.connection.close();
      device = undefined;
      // Parent shutdown closes its wrapper, not the independent OAuth grant.
      custody = (
        await NativeOAuthClient.open({ directory, scope, trust })
      ).orThrow();
      session = (await custody.resume()).orThrow();
      const independent = await connectNativeClient({
        session,
        servers: login.servers,
        request,
      }).orThrow();
      try {
        assert.deepEqual(
          (await independent.state.shared.get().orThrow())?.value,
          { value: "user retained" },
        );
      } finally {
        await independent.connection.close();
      }
    } finally {
      await device?.companion?.connection.close();
      await device?.connection.close();
      await provider.stop();
      await providerExit;
    }
  });
});
