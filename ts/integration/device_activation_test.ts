import {
  checkDeviceActivation,
  TrellisDevice,
} from "@oatscenter/trellis/device";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals, assertRejects } from "@std/assert";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import type {
  ConsentCapability,
  ConsentResource,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/types/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("device companion requires separate selected consent across restart", async (t) => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.deployments.create({
      id: "device",
      kind: "device",
      reviewMode: "none",
    });
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
    await runtime.contracts.install({
      contract: participants.Device.Companion.participant,
    });
    await runtime.contracts.apply({
      deployment: "device",
      contract: participants.Device.participant,
    });
    await runtime.ensurePortalConsentPolicy(
      participants.Device.Companion.participant.identity,
      [
        "capability:runtime-trellis.device_child@v1::required",
        "capability:runtime-trellis.device_child@v1::optional",
        "capability:trellis.auth@v1::public",
        "capability:trellis.state@v1::public",
      ],
    );
    const provisioned = await runtime.devices.provision({
      deploymentId: "device",
      participantId: participants.Device.participant.identity,
      identityPublicKey: null,
      instanceId: null,
      idempotencyKey: crypto.randomUUID(),
    });
    assert(provisioned.provisioningSecret);
    const rootSecret = crypto.getRandomValues(new Uint8Array(32));
    const activation = await checkDeviceActivation({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Device.participant,
      rootSecret,
      provisioningSecret: provisioned.provisioningSecret,
    });
    assert(activation.status === "activation_required");
    const portal = await runtime.connectClient({
      name: "device-activation-portal",
      contract: participants.DevicePortal.participant,
    });
    await t.step("companion cannot bootstrap directly", async () => {
      await assertRejects(() =>
        runtime.connectClient({
          name: "forbidden-device-companion",
          contract: participants.Device.Companion.participant,
        })
      );
    });

    const flowId = new URL(activation.activationUrl).searchParams.get(
      "flowId",
    )!;
    const pending = await portal.deviceUserAuthoritiesResolve({
      flowId,
      confirmationCode: activation.confirmationCode,
    }).start().orThrow();
    const pendingSnapshot = await runtime.waitFor(async () => {
      const snapshot = await pending.get().orThrow();
      return snapshot.progress?.companionConsent ? snapshot : undefined;
    });
    assertEquals(pendingSnapshot.state, "running");
    const consent = pendingSnapshot.progress?.companionConsent;
    assert(consent);
    assertEquals(
      (await checkDeviceActivation({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Device.participant,
        rootSecret,
      })).status,
      "activation_required",
    );
    const approval = {
      mode: "capabilities" as const,
      installedRevision: consent.installedRevision,
      expectedGrantRevision: consent.expectedGrantRevision,
      decisionDigest: consent.decisionDigest,
      approvedCapabilities: consent.capabilities.filter((
        item: ConsentCapability,
      ) => item.required && item.eligible).map((item: ConsentCapability) => ({
        id: item.id,
        consentDigest: item.consentDigest,
      })),
      approvedResources: consent.resources.filter((item: ConsentResource) =>
        item.required && item.eligible
      ).map((item: ConsentResource) => ({
        kind: item.kind,
        name: item.name,
        commitment: item.requestedCommitment,
      })),
      companionApproved: false,
    };

    const approved = await portal.deviceUserAuthoritiesResolve({
      flowId,
      confirmationCode: activation.confirmationCode,
      companionApproval: approval,
    }).start().orThrow();
    const approvedResult = await runtime.waitFor(async () => {
      const snapshot = await approved.get().orThrow();
      return snapshot.state === "running" ? undefined : snapshot;
    }, { timeoutMs: 60_000 });
    assertEquals(approvedResult.state, "completed");
    const fetch = globalThis.fetch;
    globalThis.fetch = async (input, init) => {
      if (
        new URL(input instanceof Request ? input.url : input).pathname ===
          "/bootstrap/device"
      ) {
        const body = JSON.parse(String(init?.body));
        body.companion.proof =
          (body.companion.proof.startsWith("A") ? "B" : "A") +
          body.companion.proof.slice(1);
        return await fetch(input, { ...init, body: JSON.stringify(body) });
      }
      return await fetch(input, init);
    };
    try {
      await assertRejects(() =>
        TrellisDevice.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Device.participant,
          rootSecret,
        }).orThrow()
      );
    } finally {
      globalThis.fetch = fetch;
    }
    let device = await TrellisDevice.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Device.participant,
      rootSecret,
    }).orThrow();
    const companion = device.companion;
    assert(companion);
    assert(companion.connection !== device.connection);
    await t.step("first companion uses approved authority", async () => {
      assertEquals(companion.connection.status.phase, "connected");
      assertEquals((await companion.required({})).isOk(), true);
      assertEquals((await companion.optional({})).isErr(), true);
    });

    const secondProvisioned = await runtime.devices.provision({
      deploymentId: "device",
      participantId: participants.Device.participant.identity,
      identityPublicKey: null,
      instanceId: null,
      idempotencyKey: crypto.randomUUID(),
    });
    assert(secondProvisioned.provisioningSecret);
    const secondRootSecret = crypto.getRandomValues(new Uint8Array(32));
    const secondActivation = await checkDeviceActivation({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Device.participant,
      rootSecret: secondRootSecret,
      provisioningSecret: secondProvisioned.provisioningSecret,
    });
    assert(secondActivation.status === "activation_required");
    const secondFlowId = new URL(secondActivation.activationUrl).searchParams
      .get(
        "flowId",
      )!;
    const secondPending = await portal.deviceUserAuthoritiesResolve({
      flowId: secondFlowId,
      confirmationCode: secondActivation.confirmationCode,
    }).start().orThrow();
    const secondSnapshot = await runtime.waitFor(async () => {
      const snapshot = await secondPending.get().orThrow();
      const consent = snapshot.progress?.companionConsent;
      return consent?.resources.every((resource: ConsentResource) =>
          !resource.required || resource.actual
        )
        ? snapshot
        : undefined;
    });
    const secondConsent = secondSnapshot.progress?.companionConsent;
    assert(secondConsent);
    assert(BigInt(secondConsent.expectedGrantRevision) > 0n);
    const secondApproved = await portal.deviceUserAuthoritiesResolve({
      flowId: secondFlowId,
      confirmationCode: secondActivation.confirmationCode,
      companionApproval: {
        mode: "capabilities",
        installedRevision: secondConsent.installedRevision,
        expectedGrantRevision: secondConsent.expectedGrantRevision,
        decisionDigest: secondConsent.decisionDigest,
        approvedCapabilities: secondConsent.capabilities.filter((
          item: ConsentCapability,
        ) => item.required && item.eligible && !item.alreadyApproved).map((
          item: ConsentCapability,
        ) => ({
          id: item.id,
          consentDigest: item.consentDigest,
        })),
        approvedResources: secondConsent.resources.filter((
          item: ConsentResource,
        ) => item.required && item.eligible && !item.alreadyApproved).map((
          item: ConsentResource,
        ) => ({
          kind: item.kind,
          name: item.name,
          commitment: item.requestedCommitment,
        })),
        companionApproved: false,
      },
    }).start().orThrow();
    const secondResult = await runtime.waitFor(async () => {
      const snapshot = await secondApproved.get().orThrow();
      return snapshot.state === "running" ? undefined : snapshot;
    }, { timeoutMs: 60_000 });
    assertEquals(
      secondResult.state,
      "completed",
      `second approval state ${secondResult.state}`,
    );
    let secondDevice = await TrellisDevice.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Device.participant,
      rootSecret: secondRootSecret,
    }).orThrow();
    assertEquals((await secondDevice.companion?.required({}))?.isOk(), true);
    await runtime.waitFor(async () => {
      const result = await companion.required({});
      return result.isOk() ? true : undefined;
    });
    await companion.state.shared.set({ value: "same-user" }).orThrow();
    await runtime.waitFor(async () => {
      const value = (await secondDevice.companion?.state.shared.get().orThrow())
        ?.value;
      return value?.value === "same-user" ? true : undefined;
    });
    await companion.connection.close();
    await secondDevice.companion?.connection.close();
    await device.connection.close();
    await secondDevice.connection.close();

    await runtime.restartControlPlane();
    device = await TrellisDevice.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Device.participant,
      rootSecret,
    }).orThrow();
    const restartedCompanion = device.companion;
    assert(restartedCompanion);
    assert(restartedCompanion.connection !== device.connection);
    assertEquals((await restartedCompanion.required({})).isOk(), true);
    secondDevice = await TrellisDevice.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Device.participant,
      rootSecret: secondRootSecret,
    }).orThrow();
    assertEquals((await secondDevice.companion?.required({}))?.isOk(), true);
    await restartedCompanion.logout();
    assertEquals((await secondDevice.companion?.required({}))?.isOk(), true);
    assertEquals(
      (await secondDevice.companion?.state.shared.get().orThrow())?.value,
      { value: "same-user" },
    );
    await secondDevice.companion?.state.shared.delete().orThrow();
    await secondDevice.companion?.connection.close();
    await device.connection.close();
    await secondDevice.connection.close();
    await portal.connection.close();
    await childProvider.connection.close();
  });
});
