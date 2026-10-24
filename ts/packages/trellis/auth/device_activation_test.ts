import assert from "node:assert/strict";
import { test } from "node:test";

import {
  buildDeviceActivationPayload,
  deriveDeviceConfirmationCode,
  deriveDeviceIdentity,
  deriveDeviceUserCompanion,
  encodeDeviceActivationPayload,
  parseDeviceActivationPayload,
  verifyDeviceConfirmationCode,
} from "./device_activation.ts";

test("device companion derivation binds canonical origin and exact child", async () => {
  const companion = await deriveDeviceUserCompanion(
    new Uint8Array(32).fill(7),
    "https://example.com/path",
    "acme.Sensor.Companion",
  );
  assert.deepEqual(
    companion.installationSeedBase64url,
    "vIo7nClzQFE0Ir1R7AQz3aqO4NJ5uU2-wXJLaqcyVss",
  );
  assert.deepEqual(
    companion.installationPublicKey,
    "vGf_UmbZSDEhpK6sxpO1esqkNwuof8fwvxvaNG20xZw",
  );
  assert.notDeepEqual(
    companion.installationPublicKey,
    (
      await deriveDeviceUserCompanion(
        new Uint8Array(32).fill(7),
        "https://other.example.com",
        "acme.Sensor.Companion",
      )
    ).installationPublicKey,
  );
});

test("device activation payload helpers round-trip encoded payloads", async () => {
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(7));
  const payload = await buildDeviceActivationPayload({
    activationKey: identity.activationKey,
    publicIdentityKey: identity.publicIdentityKey,
    nonce: "nonce_123",
  });
  assert.deepEqual(
    parseDeviceActivationPayload(encodeDeviceActivationPayload(payload)),
    payload,
  );
});

test("device confirmation codes verify only their bound challenge", async () => {
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(9));
  const challenge = {
    activationKey: identity.activationKey,
    publicIdentityKey: identity.publicIdentityKey,
    nonce: "nonce_123",
  };
  const confirmationCode = await deriveDeviceConfirmationCode(challenge);
  assert(
    await verifyDeviceConfirmationCode({
      ...challenge,
      confirmationCode: confirmationCode.toLowerCase(),
    }),
  );
  assert(
    !(await verifyDeviceConfirmationCode({
      ...challenge,
      nonce: "other_challenge",
      confirmationCode,
    })),
  );
});
