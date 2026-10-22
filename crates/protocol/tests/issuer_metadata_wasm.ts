import assert from "node:assert/strict";
import "../../../ts/packages/trellis/auth/protocol_wasm_provider.ts";
import { initializeProtocolWasmSync } from "../../../ts/packages/trellis/auth/protocol_wasm.ts";
import { readFile } from "node:fs/promises";
import { verify_issuer_metadata } from "../../../ts/packages/trellis/auth/protocol_wasm/provider/trellis_protocol_wasm.js";

initializeProtocolWasmSync();
const fixture = JSON.parse(await readFile(process.argv[2], "utf8"));
const verify = (signed = fixture.signed, verification = fixture.verification) =>
  JSON.parse(
    verify_issuer_metadata(
      JSON.stringify(verification),
      new TextEncoder().encode(JSON.stringify(signed)),
    ),
  );
const accepted = verify();
assert.equal(accepted.ok, true);
assert.deepEqual(accepted.response.result, fixture.signed.response.result);
const forgedAbsence = structuredClone(fixture.signed);
forgedAbsence.response.result = { kind: "missing" };
assert.equal(verify(forgedAbsence).ok, false);
const swapped = structuredClone(fixture.verification);
swapped.request.query.contextDigest = Buffer.alloc(32, 42).toString(
  "base64url",
);
assert.equal(verify(fixture.signed, swapped).ok, false);
const revoked = structuredClone(fixture.verification);
revoked.issuer.state = "revoked";
assert.equal(verify(fixture.signed, revoked).ok, false);
assert.equal(
  verify(fixture.signed, {
    ...fixture.verification,
    replySubject: "_INBOX.other",
  }).ok,
  false,
);
assert.equal(
  verify(fixture.signed, {
    ...fixture.verification,
    nowUnixSeconds: fixture.signed.response.expiresAt,
  }).ok,
  false,
);
