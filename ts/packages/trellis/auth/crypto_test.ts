import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import { portableBackend } from "./crypto.ts";
import {
  type SessionProofInput,
  signSessionProof,
  verifySessionProof,
} from "./session_proof.ts";
import { base64urlDecode, base64urlEncode, sha256 } from "./utils.ts";
import vectors from "../../../../integration/fixtures/protocol/ed25519/vectors.json" with { type: "json" };

function fromHex(hex: string): Uint8Array {
  return new Uint8Array(
    (hex.match(/../g) ?? []).map((byte) => Number.parseInt(byte, 16)),
  );
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

test("portable Ed25519 interoperates with the Rust protocol boundary", async () => {
  const chain = vectors;
  const portable = portableBackend();

  // A proof produced by the Rust protocol is accepted by the portable verifier.
  assert(
    await portable.verifyEd25519(
      base64urlDecode(chain.sessionPublicKey),
      base64urlDecode(chain.requestProofDigest),
      base64urlDecode(chain.requestProof),
    ),
  );
  // The same signature over a different digest is rejected.
  assert(
    !(await portable.verifyEd25519(
      base64urlDecode(chain.sessionPublicKey),
      base64urlDecode(chain.eventProofDigest),
      base64urlDecode(chain.requestProof),
    )),
  );

  // The public key derived by the portable backend matches Rust's, and the
  // Rust verifier accepts a portable-signed session proof.
  const signer = await portable.signerFromSeed(
    base64urlDecode(chain.sessionSeed),
  );
  assert.deepEqual(signer.publicKey, chain.sessionPublicKey);
  const iat = Date.now();
  const input: SessionProofInput = {
    purpose: "deviceEnrollment",
    origin: "https://trellis.example",
    unsignedRequest: {
      requestId: ulid(),
      iat,
      identityKeyId: base64urlEncode(
        await sha256(base64urlDecode(signer.publicKey)),
      ),
      sessionKey: signer.publicKey,
      connectionId: ulid(),
      participantId: "test.portable",
    },
  };
  const proof = await signSessionProof(input, signer, signer.publicKey);
  await verifySessionProof(input, proof, signer.publicKey, iat);
});

test("portable SHA-256 reproduces the pinned protocol digest", async () => {
  const chain = vectors;
  const portable = portableBackend();
  assert.deepEqual(
    base64urlEncode(await portable.sha256(fromHex(chain.requestProofInputHex))),
    chain.requestProofDigest,
  );
});

test("portable HKDF and HMAC match externally defined RFC vectors", async () => {
  const portable = portableBackend();

  // RFC 5869 HKDF-SHA256 test case 1.
  const okm = await portable.hkdfSha256(
    new Uint8Array(22).fill(0x0b),
    fromHex("000102030405060708090a0b0c"),
    fromHex("f0f1f2f3f4f5f6f7f8f9"),
    42,
  );
  assert.deepEqual(
    toHex(okm),
    "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
  );

  // RFC 4231 HMAC-SHA256 test case 1.
  const mac = await portable.hmacSha256(
    new Uint8Array(20).fill(0x0b),
    new TextEncoder().encode("Hi There"),
  );
  assert.deepEqual(
    toHex(mac),
    "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
  );
});
