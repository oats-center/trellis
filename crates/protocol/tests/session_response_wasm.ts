import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createResponseProof } from "../../../ts/packages/trellis/auth/proof.ts";
import { trellisCrypto } from "../../../ts/packages/trellis/auth/crypto.ts";
import { verifySessionResponseWasm } from "../../../ts/packages/trellis/auth/protocol_wasm.ts";
import {
  create_session_authority_handle,
  initSync,
  session_response_signing_digest,
} from "../../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm.js";
import { PROTOCOL_WASM_BASE64 } from "../../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bytes.ts";

initSync({
  module: Uint8Array.from(atob(PROTOCOL_WASM_BASE64), (c) => c.charCodeAt(0)),
});
const fixture = JSON.parse(await readFile(process.argv[2], "utf8"));
const encoder = new TextEncoder();
const signer = await (
  await trellisCrypto()
).signerFromSeed(
  new Uint8Array(Buffer.from(fixture.providerSeed, "base64url")),
);
assert.equal(
  await createResponseProof(
    signer,
    fixture.verification.response,
    encoder.encode(fixture.payload),
  ),
  fixture.verification.proof,
);
const handle = create_session_authority_handle(
  JSON.stringify(fixture.authorityVerification),
  encoder.encode(JSON.stringify(fixture.authority)),
);
try {
  const verify = (input = fixture.verification, payload = fixture.payload) =>
    verifySessionResponseWasm({
      ...input,
      providerHandle: handle,
      payload: encoder.encode(payload),
    });
  const accepted = await verify();
  assert.equal(accepted.ok, true);
  assert.deepEqual(accepted.response, fixture.verification.response);
  assert.equal(
    Buffer.from(
      session_response_signing_digest(
        JSON.stringify(fixture.verification.response),
        encoder.encode(fixture.payload),
      ),
    ).toString("base64url"),
    fixture.digest,
  );
  assert.equal(
    (await verify(fixture.verification, "broker-substituted result")).ok,
    false,
  );
  assert.equal(
    (await verify({ ...fixture.verification, receivedSubject: "_INBOX.other" }))
      .ok,
    false,
  );
  assert.equal(
    (await verify({ ...fixture.verification, knownRevoked: true })).ok,
    false,
  );
  const replayed = structuredClone(fixture.verification);
  replayed.pendingRequest.requestId = "01JY0000000000000000000099";
  assert.equal((await verify(replayed)).ok, false);
  const unimplemented = structuredClone(fixture.verification);
  unimplemented.action.name = "Documents.Delete";
  assert.equal((await verify(unimplemented)).ok, false);
} finally {
  handle.free();
}
