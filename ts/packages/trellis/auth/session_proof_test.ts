import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { ulid } from "ulid";
import {
  importEd25519PrivateKeyFromSeedBase64url,
  publicKeyBase64urlFromSeed,
} from "./keys.ts";
import {
  parseSessionProof,
  SESSION_PROOF_FORMAT_V1,
  type SessionProofInput,
  sessionProofRequestDigest,
  signSessionProof,
  verifySessionProof,
} from "./session_proof.ts";
import { base64urlDecode, base64urlEncode, sha256 } from "./utils.ts";

async function identity() {
  const seed = crypto.getRandomValues(new Uint8Array(32));
  const publicKey = publicKeyBase64urlFromSeed(seed);
  return {
    publicKey,
    privateKey: await importEd25519PrivateKeyFromSeedBase64url(
      base64urlEncode(seed),
    ),
    keyId: base64urlEncode(await sha256(base64urlDecode(publicKey))),
    seed,
  };
}

Deno.test("WebCrypto native signatures are verified by Rust and bind the complete bootstrap", async () => {
  const owner = await identity();
  const session = await identity();
  const iat = Date.now();
  for (
    const purpose of [
      "serviceBootstrap",
      "deviceBootstrap",
      "deviceEnrollment",
    ] as const
  ) {
    const input: SessionProofInput = {
      purpose,
      origin: "https://trellis.example",
      unsignedRequest: {
        identityKeyId: owner.keyId,
        sessionKey: session.publicKey,
        connectionId: ulid(),
        requestId: ulid(),
        iat,
        participantId: "example.device",
        name: "native replica",
        extension: { value: 1 },
      },
    };
    const proof = await signSessionProof(
      input,
      owner.privateKey,
      owner.publicKey,
    );
    await verifySessionProof(input, proof, owner.publicKey, iat);
    await assertRejects(() =>
      signSessionProof(input, session.privateKey, owner.publicKey)
    );
    await assertRejects(() =>
      verifySessionProof(input, proof, session.publicKey, iat)
    );
    await assertRejects(() =>
      verifySessionProof(
        { ...input, origin: "https://other.example" },
        proof,
        owner.publicKey,
        iat,
      )
    );
    await assertRejects(() =>
      verifySessionProof(
        {
          ...input,
          purpose: purpose === "deviceBootstrap"
            ? "serviceBootstrap"
            : "deviceBootstrap",
        },
        proof,
        owner.publicKey,
        iat,
      )
    );
    for (
      const [field, value] of Object.entries({
        identityKeyId: session.keyId,
        sessionKey: owner.publicKey,
        connectionId: ulid(),
        requestId: ulid(),
        iat: iat + 1,
        participantId: "example.other-device",
        name: "changed name",
        extension: { value: 2 },
      })
    ) {
      const changed = structuredClone(input);
      changed.unsignedRequest[field] = value;
      await assertRejects(() =>
        verifySessionProof(changed, proof, owner.publicKey, iat)
      );
    }
    for (const now of [iat - 30_001, iat + 30_001]) {
      await assertRejects(() =>
        verifySessionProof(input, proof, owner.publicKey, now)
      );
    }
    await assertRejects(() =>
      verifySessionProof(
        input,
        { ...proof, signature: base64urlEncode(new Uint8Array(64)) },
        owner.publicKey,
        iat,
      )
    );
    await assertRejects(() =>
      signSessionProof(
        { ...input, unsignedRequest: { ...input.unsignedRequest, proof } },
        owner.privateKey,
        owner.publicKey,
      )
    );
    await assertRejects(() =>
      signSessionProof(
        {
          ...input,
          unsignedRequest: { ...input.unsignedRequest, name: "x".repeat(129) },
        },
        owner.privateKey,
        owner.publicKey,
      )
    );
    assertThrows(() =>
      parseSessionProof({ ...proof, signature: `${proof.signature}=` })
    );
  }
});

Deno.test("browser request and final bind retain key, flow, redirect, and raw payload integrity", async () => {
  const owner = await identity();
  const iat = Date.now();
  const requestId = ulid();
  const request = {
    requestId,
    issuedAt: iat,
    extra: { a: 1, b: true },
    proof: { format: SESSION_PROOF_FORMAT_V1, signature: "" },
  };
  const requestDigest = await sessionProofRequestDigest(request);
  assertEquals(
    await sessionProofRequestDigest({
      ...request,
      proof: { ...request.proof, signature: "ignored" },
    }),
    requestDigest,
  );
  const bind: SessionProofInput = {
    purpose: "userAuthBind",
    origin: "https://trellis.example",
    flowId: ulid(),
    sessionPublicKey: owner.publicKey,
    unsignedRequest: { requestId, issuedAt: iat, extra: request.extra },
  };
  const bound = await signSessionProof(bind, owner.privateKey, owner.publicKey);
  await verifySessionProof(bind, bound, owner.publicKey, iat);
  await assertRejects(() =>
    verifySessionProof({ ...bind, flowId: ulid() }, bound, owner.publicKey, iat)
  );
  await assertRejects(async () =>
    verifySessionProof(
      {
        ...bind,
        unsignedRequest: {
          ...bind.unsignedRequest,
          extra: { a: 2, b: true },
        },
      },
      bound,
      owner.publicKey,
      iat,
    )
  );
  await assertRejects(() =>
    sessionProofRequestDigest({
      ...request,
      extra: { counter: 9_007_199_254_740_992 },
    })
  );
  const auth: SessionProofInput = {
    purpose: "userAuthRequest",
    origin: "https://trellis.example",
    unsignedRequest: {
      requestId,
      issuedAt: iat,
      sessionPublicKey: owner.publicKey,
      participantId: "test.browser",
      redirectTarget: "https://app.example/return",
    },
  };
  const proof = await signSessionProof(auth, owner.privateKey, owner.publicKey);
  await verifySessionProof(auth, proof, owner.publicKey, iat);
  await assertRejects(() =>
    verifySessionProof(
      {
        ...auth,
        unsignedRequest: {
          ...auth.unsignedRequest,
          redirectTarget: "https://evil.example",
        },
      },
      proof,
      owner.publicKey,
      iat,
    )
  );
  await assertRejects(() =>
    verifySessionProof(
      {
        ...auth,
        unsignedRequest: {
          ...auth.unsignedRequest,
          participantId: "other.browser",
        },
      },
      proof,
      owner.publicKey,
      iat,
    )
  );
  await assertRejects(() =>
    signSessionProof(
      {
        ...auth,
        unsignedRequest: {
          ...auth.unsignedRequest,
          sessionPublicKey: `${owner.publicKey}=`,
        },
      },
      owner.privateKey,
      owner.publicKey,
    )
  );
});
