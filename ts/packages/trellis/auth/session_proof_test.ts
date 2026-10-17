import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { trellisCrypto } from "./crypto.ts";
import {
  parseSessionProof,
  type SessionProofInput,
  signSessionProof,
  verifySessionProof,
} from "./session_proof.ts";
import { base64urlDecode, base64urlEncode, sha256 } from "./utils.ts";

async function identity() {
  const privateKey = await (
    await trellisCrypto()
  ).signerFromSeed(crypto.getRandomValues(new Uint8Array(32)));
  return {
    publicKey: privateKey.publicKey,
    privateKey,
    keyId: base64urlEncode(await sha256(base64urlDecode(privateKey.publicKey))),
  };
}

test("WebCrypto device-enrollment signatures bind the complete request in Rust", async () => {
  const owner = await identity();
  const session = await identity();
  const iat = Date.now();
  const input: SessionProofInput = {
    purpose: "deviceEnrollment",
    origin: "https://trellis.example",
    unsignedRequest: {
      identityKeyId: owner.keyId,
      sessionKey: session.publicKey,
      connectionId: ulid(),
      requestId: ulid(),
      iat,
      participantId: "example.device",
      name: "device one",
      extension: { value: 1 },
    },
  };
  const proof = await signSessionProof(
    input,
    owner.privateKey,
    owner.publicKey,
  );
  await verifySessionProof(input, proof, owner.publicKey, iat);
  await assert.rejects(() =>
    signSessionProof(input, session.privateKey, owner.publicKey),
  );
  await assert.rejects(() =>
    verifySessionProof(input, proof, session.publicKey, iat),
  );
  await assert.rejects(() =>
    verifySessionProof(
      { ...input, origin: "https://other.example" },
      proof,
      owner.publicKey,
      iat,
    ),
  );
  for (const [field, value] of Object.entries({
    identityKeyId: session.keyId,
    sessionKey: owner.publicKey,
    connectionId: ulid(),
    requestId: ulid(),
    iat: iat + 1,
    participantId: "example.other-device",
    name: "changed name",
    extension: { value: 2 },
  })) {
    const changed = structuredClone(input);
    changed.unsignedRequest[field] = value;
    await assert.rejects(() =>
      verifySessionProof(changed, proof, owner.publicKey, iat),
    );
  }
  for (const now of [iat - 30_001, iat + 30_001]) {
    await assert.rejects(() =>
      verifySessionProof(input, proof, owner.publicKey, now),
    );
  }
  await assert.rejects(() =>
    verifySessionProof(
      input,
      { ...proof, signature: base64urlEncode(new Uint8Array(64)) },
      owner.publicKey,
      iat,
    ),
  );
  await assert.rejects(() =>
    signSessionProof(
      { ...input, unsignedRequest: { ...input.unsignedRequest, proof } },
      owner.privateKey,
      owner.publicKey,
    ),
  );
  await assert.rejects(() =>
    signSessionProof(
      {
        ...input,
        unsignedRequest: { ...input.unsignedRequest, name: "x".repeat(129) },
      },
      owner.privateKey,
      owner.publicKey,
    ),
  );
  assert.throws(
    () => parseSessionProof({ ...proof, signature: `${proof.signature}=` }),
    Error,
  );
});
