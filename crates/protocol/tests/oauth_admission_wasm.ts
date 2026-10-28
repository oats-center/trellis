import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { verifyOAuthAdmissionWasm } from "../../../ts/packages/trellis/auth/protocol_wasm.ts";

const fixture = JSON.parse(await readFile(process.argv[2], "utf8"));
const verify = (signed = fixture.signed, verification = fixture.verification) =>
  verifyOAuthAdmissionWasm(
    new TextEncoder().encode(JSON.stringify(signed)),
    verification,
  );

const accepted = await verify();
assert.equal(accepted.ok, true);
if (!accepted.ok) throw new Error("OAuth admission rejected");
assert.deepEqual(accepted.admission, fixture.signed);
// Consumers cannot mutate verified identity after the trust decision.
assert.throws(() =>
  Object.assign(accepted.admission.credential, { principalId: "other" }),
);
assert.throws(() =>
  Object.assign(accepted.admission.credential.binding, {
    nativeDpopJkt: "other",
  }),
);

for (const patch of [
  { trellisInstanceId: "01JY0000000000000000000009" },
  { audienceNatsAccount: "other-account" },
  { ephemeralNkey: "other-key" },
  { nowUnixSeconds: 999 },
  { nowUnixSeconds: 1_060 },
  { maximumLifetimeSeconds: 59 },
  { issuer: { ...fixture.verification.issuer, state: "revoked" } },
]) {
  const result = await verify(fixture.signed, {
    ...fixture.verification,
    ...patch,
  });
  assert.equal(result.ok, false);
  if (!result.ok) assert.equal(result.error.code, "ScopeMismatch");
}
for (const patch of [
  { principalId: "01JY0000000000000000000009" },
  { clientId: "https://other.example/native.json" },
  { runtimeId: Buffer.alloc(16, 42).toString("base64url") },
]) {
  const forged = structuredClone(fixture.signed);
  Object.assign(forged.credential, patch);
  const result = await verify(forged);
  assert.equal(result.ok, false);
  if (!result.ok) assert.equal(result.error.code, "InvalidSignature");
}
// Signed documents are strict even though ordinary discovery JSON is open.
assert.equal(
  (await verify({ ...fixture.signed, unsignedExtension: true })).ok,
  false,
);
assert.equal(
  (await verify(fixture.signed, { ...fixture.verification, extra: true })).ok,
  false,
);
