import assert from "node:assert/strict";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { ulid } from "ulid";
import { codecs } from "../../../ts/packages/trellis/generated.ts";
import {
  checkDeviceActivation,
  TrellisDevice,
} from "../../../ts/packages/trellis/device.ts";
import { loginNativeTestClient } from "../../../ts/packages/trellis-testkit/src/admin/native_auth.ts";
import {
  apps,
  types,
} from "../../../ts/packages/trellis/internal_sdk/generated/index.js";
import {
  connectNativeClient,
  discoverNativeOAuth,
} from "../../../ts/packages/trellis/native.ts";
import { deriveDeviceIdentity } from "../../../ts/packages/trellis/auth/device_activation.ts";
import { participants } from "./packages/runtime-trellis/index.js";

const [issuer, username, password, directory] = process.argv.slice(2);
const login = await loginNativeTestClient({
  issuer,
  username,
  password,
  directory,
  clientId: "trellis.cli",
  request: apps.cli.request,
  platformPrivileges: { required: ["principals.manage"] },
});
const admin = await connectNativeClient({
  ...login,
  request: apps.cli.request,
}).orThrow();
try {
  const participant = participants.StateDevice.participant;
  const { deployment } = await admin
    .deploymentsCreate({
      title: "Activation recheck",
      kind: "device",
      participantId: participant.identity,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        participant.packageEvidence,
      ),
      idempotencyKey: ulid(),
    })
    .orThrow();
  const provisioned = await admin
    .devicesProvision({
      deploymentId: deployment.deploymentId,
      title: "Rechecked device",
      idempotencyKey: ulid(),
    })
    .orThrow();
  const args = {
    trellisUrl: issuer,
    participant,
    rootSecret: crypto.getRandomValues(new Uint8Array(32)),
    provisioningSecret: provisioned.provisioningSecret,
    statePath: join(directory, "activation.json"),
  };
  const pending = await checkDeviceActivation(args);
  assert(pending.status === "activation_required");
  const reviewId = codecs.ulid.decode(
    new URL(pending.activationUrl).searchParams.get("reviewId"),
  );
  const { review } = await admin
    .devicesReviewsGet({ reviewId, confirmationCode: pending.confirmationCode })
    .orThrow();
  await admin
    .devicesReviewsDecide({
      reviewId,
      confirmationCode: pending.confirmationCode,
      expectedRevision: review.revision,
      decision: "approved",
      idempotencyKey: ulid(),
    })
    .orThrow();
  await pending.waitForOnlineApproval();
  const before = await readFile(args.statePath, "utf8");
  // Separate helper instances load the persisted completion, as a restarted device does.
  assert.equal((await checkDeviceActivation(args)).status, "activated");
  assert.equal((await checkDeviceActivation(args)).status, "activated");
  assert.equal(await readFile(args.statePath, "utf8"), before);
  const reviews = await admin.devicesReviewsList({}).orThrow();
  assert.deepEqual(
    reviews.items
      .filter((item) => item.instanceId === provisioned.instance.instanceId)
      .map((item) => item.state),
    ["approved"],
  );
  const server = (await discoverNativeOAuth(issuer)).orThrow();
  const identity = await deriveDeviceIdentity(args.rootSecret);
  const connectOptions = {
    servers: [...server.natsServers],
    trellisInstanceId: server.trust.instanceId,
    audienceNatsAccount: server.trust.account,
    issuers: [server.trust.issuer],
    seed: identity.identitySeedBase64url,
    participant,
  };
  const device = await TrellisDevice.connect(connectOptions).orThrow();
  try {
    await device.state.telemetry.set({ value: "approved once" }).orThrow();
    assert.deepEqual((await device.state.telemetry.get().orThrow())?.value, {
      value: "approved once",
    });
  } finally {
    await device.connection.close();
  }
  const current = await admin
    .devicesList({ deploymentId: deployment.deploymentId })
    .orThrow();
  const instance = current.items.find(
    (item) => item.instanceId === provisioned.instance.instanceId,
  );
  assert(instance);
  await admin
    .devicesDisable({
      instanceId: instance.instanceId,
      expectedRevision: instance.revision,
      idempotencyKey: ulid(),
    })
    .orThrow();
  await assert.rejects(() => TrellisDevice.connect(connectOptions).orThrow());
} finally {
  await admin.connection.close();
}
