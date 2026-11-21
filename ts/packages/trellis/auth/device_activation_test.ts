import assert from "node:assert/strict";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { test } from "node:test";

import { createAuth } from "./session_auth.ts";

import {
  buildDeviceActivationPayload,
  deriveDeviceConfirmationCode,
  deriveDeviceIdentity,
  deriveDeviceUserCompanion,
  encodeDeviceActivationPayload,
  parseDeviceActivationPayload,
  requestDeviceEnrollment,
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

test("device enrollment refuses a redirect and sends nothing to its target", async () => {
  const listen = async (server: Server): Promise<string> => {
    await new Promise<void>((resolve) =>
      server.listen(0, "127.0.0.1", resolve),
    );
    return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  };
  const received: string[] = [];
  const elsewhere = createServer((request, response) => {
    received.push(`${request.method} ${request.url}`);
    response.end("{}");
  });
  const elsewhereUrl = await listen(elsewhere);
  const trellis = createServer((request, response) => {
    request.resume();
    response.writeHead(307, { location: `${elsewhereUrl}${request.url}` });
    response.end();
  });
  const trellisUrl = await listen(trellis);
  try {
    const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(3));
    await assert.rejects(
      requestDeviceEnrollment({
        trellisUrl,
        publicIdentityKey: identity.publicIdentityKey,
        identitySeed: identity.identitySeed,
        sessionIdentity: await createAuth({
          sessionKeySeed: identity.identitySeedBase64url,
        }),
        connectionId: "connection",
        participantId: "acme.Sensor",
        packageEvidence: {},
        participantPath: "acme.Sensor",
        packageDigest: "digest",
        challengeDigest: "challenge",
        confirmationCode: "ABCDEFGH",
        provisioningSecret: "provisioning",
      }),
      new RegExp(
        `/auth/device/enroll with a 307 redirect to ${elsewhereUrl}/auth/device/enroll`,
      ),
    );
    assert.deepEqual(received, []);
  } finally {
    trellis.close();
    elsewhere.close();
  }
});
