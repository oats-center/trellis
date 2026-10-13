import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { mkdtemp, rm } from "node:fs/promises";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import {
  apps,
  participants,
} from "../../../../integration/fixtures/testkit/packages/trellis-test-fixture/index.js";
import { TrellisDevice, checkDeviceActivation } from "../device.ts";
import { deriveDeviceIdentity } from "../auth/device_activation.ts";
import { discoverNativeOAuth, NativeOAuthClient } from "./oauth.ts";
import { connectNativeClient } from "./connection.ts";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";

const stdin = createInterface({ input: process.stdin });
const messages = stdin[Symbol.asyncIterator]();
async function receive() {
  const line = await messages.next();
  assert(!line.done, "runtime driver exited");
  return JSON.parse(line.value);
}
const config: {
  issuer: string;
  provisioningSecret: string;
  username: string;
  password: string;
} = await receive();
const directory = await mkdtemp("/tmp/opencode/device-interop-");
const rootSecret = crypto.getRandomValues(new Uint8Array(32));
const participant = participants.CompanionDevice.participant;
const request = apps.CompanionDevice_Companion.request;
const enrollment = await checkDeviceActivation({
  trellisUrl: config.issuer,
  participant,
  rootSecret,
  provisioningSecret: config.provisioningSecret,
  stateDir: directory,
});
assert.equal(enrollment.status, "activation_required");
if (enrollment.status !== "activation_required")
  throw new Error("enrollment expected");
const reviewId = codecs.ulid.decode(
  new URL(enrollment.activationUrl).searchParams.get("reviewId"),
);
console.log(
  JSON.stringify({
    stage: "review",
    reviewId,
    confirmationCode: enrollment.confirmationCode,
  }),
);
const review: { clientId: string; revision: string } = await receive();
await enrollment.waitForOnlineApproval({ signal: AbortSignal.timeout(15000) });
const server = (await discoverNativeOAuth(config.issuer)).orThrow();
const identity = await deriveDeviceIdentity(rootSecret);
const options = {
  participant,
  servers: [...server.natsServers],
  trellisInstanceId: server.trust.instanceId,
  audienceNatsAccount: server.trust.account,
  issuers: [server.trust.issuer],
  seed: identity.identitySeedBase64url,
  timeout: 5000,
};
await assert.rejects(TrellisDevice.connect(options).orThrow());
const scope = {
  issuer: server.issuer,
  instanceId: server.trust.instanceId,
  account: server.trust.account,
  clientId: review.clientId,
  credentialHandle: ulid(),
};
async function authorize(credentialHandle: string) {
  const native = (
    await NativeOAuthClient.open({
      directory,
      scope: { ...scope, credentialHandle },
      trust: server.trust,
    })
  ).orThrow();
  return (
    await loginNativeTestClient({
      ...config,
      directory,
      clientId: review.clientId,
      client: native,
      request,
    })
  ).session;
}
const session = await authorize(scope.credentialHandle);
const attachment = await connectNativeClient({
  request,
  session,
  servers: server.natsServers,
}).orThrow();
const association = await attachment
  .deviceCompanionsAttach({
    reviewId,
    confirmationCode: enrollment.confirmationCode,
    expectedRevision: BigInt(review.revision),
    idempotencyKey: ulid(),
  })
  .orThrow();
await attachment.connection.close();
await session.close();
const wrong = await authorize(ulid());
await assert.rejects(
  TrellisDevice.connect({
    ...options,
    companion: { session: wrong, servers: server.natsServers },
  }).orThrow(),
);
const sibling = await connectNativeClient({
  request,
  session: wrong,
  servers: server.natsServers,
}).orThrow();
const resumed = (
  await NativeOAuthClient.open({ directory, scope, trust: server.trust })
).orThrow();
const assigned = (await resumed.resume()).orThrow();
const device = await TrellisDevice.connect({
  ...options,
  companion: { session: assigned, servers: server.natsServers },
}).orThrow();
try {
  const child = device.companion;
  assert(child);
  assert.equal(
    (await child.echo({ value: "typescript companion" }).orThrow()).value,
    "typescript companion",
  );
  await device.state.saved.set({ value: "typescript device" }).orThrow();
  await child.state.saved.set({ value: "typescript user" }).orThrow();
  assert.equal(
    (await device.state.saved.get().orThrow())?.value.value,
    "typescript device",
  );
  assert.equal(
    (await child.state.saved.get().orThrow())?.value.value,
    "typescript user",
  );
  console.log(
    JSON.stringify({
      stage: "revoke",
      instanceId: association.instanceId,
      revision: association.revision.toString(),
    }),
  );
  await receive();
  await assert.rejects(child.refreshAuthority());
  await assert.rejects(child.echo({ value: "revoked" }).orThrow());
  assert.equal(
    (await device.state.saved.get().orThrow())?.value.value,
    "typescript device",
  );
  assert.equal(
    (await sibling.echo({ value: "unrelated grant survives" }).orThrow()).value,
    "unrelated grant survives",
  );
  console.log(JSON.stringify({ stage: "verified" }));
} finally {
  await device.companion?.connection.close();
  await device.connection.close();
  await assigned.close();
  await sibling.connection.close();
  await wrong.close();
  stdin.close();
  await rm(directory, { recursive: true, force: true });
}
