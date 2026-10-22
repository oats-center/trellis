import assert from "node:assert/strict";
import "../../../ts/packages/trellis/auth/protocol_wasm_provider.ts";
import { initializeProtocolWasmSync } from "../../../ts/packages/trellis/auth/protocol_wasm.ts";
import { readFile } from "node:fs/promises";
import { createEventProof } from "../../../ts/packages/trellis/auth/proof.ts";
import { trellisCrypto } from "../../../ts/packages/trellis/auth/crypto.ts";
import { createAuth } from "../../../ts/packages/trellis/auth/session_auth.ts";
import {
  create_session_authority_handle,
  verify_session_event,
  verify_session_request,
} from "../../../ts/packages/trellis/auth/protocol_wasm/provider/trellis_protocol_wasm.js";

initializeProtocolWasmSync();

const fixture = JSON.parse(await readFile(process.argv[2], "utf8"));
const signer = await (
  await trellisCrypto()
).signerFromSeed(new Uint8Array(Buffer.from(fixture.runtimeSeed, "base64url")));
const auth = await createAuth({
  sessionKeySeed: fixture.runtimeSeed,
  contextDigest: fixture.request.authorityDigest,
});
const signed = await auth.createProof(
  fixture.request,
  new TextEncoder().encode(fixture.payload),
);
assert.equal(signed.proof, fixture.proof);
assert.equal(
  Buffer.from(signed.requestProofDigest).toString("base64url"),
  fixture.requestProofDigest,
);
assert.equal(
  await createEventProof(
    signer,
    fixture.event,
    new TextEncoder().encode(fixture.payload),
  ),
  fixture.eventProof,
);
const handle = create_session_authority_handle(
  JSON.stringify(fixture.verification),
  new TextEncoder().encode(JSON.stringify(fixture.authority)),
);
try {
  const verify = (request: unknown, payload: string) =>
    JSON.parse(
      verify_session_request(
        handle,
        JSON.stringify({
          request,
          proof: fixture.proof,
          policy: fixture.verification.policy,
          knownRevoked: false,
        }),
        new TextEncoder().encode(payload),
      ),
    );
  const result = verify(fixture.request, fixture.payload);
  assert.deepEqual(result.ok, true);
  assert.deepEqual(result.requestProofDigest, fixture.requestProofDigest);
  assert.deepEqual(result.caller, fixture.caller);
  assert(
    !verify(
      { ...fixture.request, subject: `${fixture.request.subject}.other` },
      fixture.payload,
    ).ok,
  );
  assert(
    !verify({ ...fixture.request, acceptedRevision: "4" }, fixture.payload).ok,
  );
  assert(!verify(fixture.request, fixture.payload.replace("true", "false")).ok);
  const changed = structuredClone(fixture.authority);
  changed.binding.origin = "https://other.example";
  assert.throws(() =>
    create_session_authority_handle(
      JSON.stringify(fixture.verification),
      new TextEncoder().encode(JSON.stringify(changed)),
    ),
  );
} finally {
  handle.free();
}

const historical = create_session_authority_handle(
  JSON.stringify(fixture.eventVerification),
  new TextEncoder().encode(JSON.stringify(fixture.authority)),
);
try {
  const verify = (
    event: unknown,
    originalPublication: unknown,
    payload = fixture.payload,
    proof = fixture.eventProof,
  ) =>
    JSON.parse(
      verify_session_event(
        historical,
        JSON.stringify({
          event,
          proof,
          policy: fixture.eventVerification.policy,
          originalPublication,
          knownRevoked: true,
        }),
        new TextEncoder().encode(payload),
      ),
    );
  const retained = verify(fixture.event, fixture.originalPublication);
  assert.deepEqual(retained.ok, true);
  assert.deepEqual(retained.eventProofDigest, fixture.eventProofDigest);
  assert.deepEqual(retained.publisherAuthority, fixture.publisherAuthority);
  assert.deepEqual(retained.originalPublication, fixture.originalPublication);
  for (const candidate of fixture.signedTimeCases) {
    assert.deepEqual(
      verify(
        candidate.event,
        fixture.originalPublication,
        fixture.payload,
        candidate.proof,
      ).ok,
      candidate.accepted,
      `signed event time cutoff: ${candidate.event.eventTime}`,
    );
  }
  assert(!verify(fixture.event, null).ok);
  assert(
    !verify(fixture.event, {
      ...fixture.originalPublication,
      publishedAt: "1970-01-01T00:20:00Z",
      streamSequence: "18",
    }).ok,
  );
  assert(
    !verify(
      { ...fixture.event, action: "Documents" },
      fixture.originalPublication,
    ).ok,
  );
  assert(
    !verify(
      fixture.event,
      fixture.originalPublication,
      fixture.payload.replace("true", "false"),
    ).ok,
  );
} finally {
  historical.free();
}
