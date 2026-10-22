import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { trellisCrypto } from "./crypto.ts";
import { createProof, createResponseProof } from "./proof.ts";
import {
  sessionRequestSigningDigest,
  sessionResponseSigningDigest,
} from "./message_digests.ts";
import { type SessionRequest, type SessionResponse } from "./protocol_wasm.ts";
import { base64urlDecode, base64urlEncode, toArrayBuffer } from "./utils.ts";

test("TypeScript signatures bind Rust/WASM request bytes and provider error correlation", async () => {
  const backend = await trellisCrypto();
  const signer = await backend.signerFromSeed(backend.randomBytes(32));
  const key = await crypto.subtle.importKey(
    "raw",
    toArrayBuffer(base64urlDecode(signer.publicKey)),
    "Ed25519",
    false,
    ["verify"],
  );
  const verify = (signature: string, digest: Uint8Array) =>
    crypto.subtle.verify(
      "Ed25519",
      key,
      toArrayBuffer(base64urlDecode(signature)),
      toArrayBuffer(digest),
    );
  const authorityDigest = base64urlEncode(
    await backend.sha256(new TextEncoder().encode("signed authority")),
  );
  const request: SessionRequest = {
    authorityDigest,
    apiId: "example.orders@v1",
    apiGeneration: "1",
    acceptedRevision: "2",
    action: "Create",
    subject: "rpc.v1.orders.deployment.Create",
    replySubject: "_INBOX.bound.reply",
    requestId: ulid(),
    issuedAt: Math.floor(Date.now() / 1000),
  };
  const bytes = new TextEncoder().encode('{"amount":5,"extension":"bound"}');
  const signed = await createProof(signer, request, bytes);
  assert.deepEqual(
    await verify(
      signed.proof,
      await sessionRequestSigningDigest(request, bytes),
    ),
    true,
  );
  assert.deepEqual(
    await verify(
      signed.proof,
      await sessionRequestSigningDigest(
        request,
        new TextEncoder().encode('{"amount":5,"extension":"changed"}'),
      ),
    ),
    false,
  );
  assert.deepEqual(
    await verify(
      signed.proof,
      await sessionRequestSigningDigest(
        { ...request, acceptedRevision: "3" },
        bytes,
      ),
    ),
    false,
  );

  const response: SessionResponse = {
    trellisInstanceId: ulid(),
    audienceNatsAccount: "trellis",
    providerAuthorityDigest: authorityDigest,
    providerCertificateDigest: authorityDigest,
    apiId: request.apiId,
    apiGeneration: request.apiGeneration,
    action: request.action,
    requestAuthorityDigest: authorityDigest,
    requestId: request.requestId,
    requestProofDigest: base64urlEncode(signed.requestProofDigest),
    subject: request.replySubject!,
    kind: "error",
    issuedAt: request.issuedAt,
  };
  const errorBytes = new TextEncoder().encode(
    '{"type":"OrderRejected","message":"rejected"}',
  );
  const proof = await createResponseProof(signer, response, errorBytes);
  assert.deepEqual(
    await verify(
      proof,
      await sessionResponseSigningDigest(response, errorBytes),
    ),
    true,
  );
  assert.deepEqual(
    await verify(
      proof,
      await sessionResponseSigningDigest(
        { ...response, requestId: ulid() },
        errorBytes,
      ),
    ),
    false,
  );
  assert.deepEqual(
    await verify(
      proof,
      await sessionResponseSigningDigest(
        { ...response, kind: "success" },
        errorBytes,
      ),
    ),
    false,
  );
});
