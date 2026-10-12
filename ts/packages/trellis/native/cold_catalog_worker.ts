import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { connect } from "@nats-io/transport-node";
import {
  SessionAuthorityOwner,
  type AuthorityRegistryBinding,
  type RuntimeAuthorizationTrust,
} from "../auth/authority.ts";
import type { CatalogActionIdentity } from "../auth/protocol_wasm.ts";

// The runtime adapter supplies unchanged production-issued artifacts, never
// a synthesized catalog, verified root, or responder.
const input: {
  servers: string[];
  trust: RuntimeAuthorizationTrust;
  registry: AuthorityRegistryBinding;
  authority: string;
  runtimeId: string;
  sessionPublicKey: string;
  apiId: string;
  action: CatalogActionIdentity;
  historicalKeyId: string;
  historicalRotation: string;
  forwardRotation: string;
  successorTrust: RuntimeAuthorizationTrust;
} = JSON.parse(await readFile(process.argv[2], "utf8"));
const nats = await connect({ servers: input.servers });
const owner = new SessionAuthorityOwner({
  instanceId: input.trust.instanceId,
  account: input.trust.account,
  issuers: [input.trust.issuer],
  rotationSequence: input.trust.rotationSequence,
});
try {
  await owner.attach(nats, input.registry);
  const own = await owner.installOwn(
    new TextEncoder().encode(input.authority),
    {
      runtimeId: input.runtimeId,
      sessionPublicKey: input.sessionPublicKey,
    },
  );
  try {
    const catalog = await own.authenticatedCatalog(input.apiId);
    assert.ok(catalog);
    assert.equal(catalog.issuerKeyId, input.historicalKeyId);
    await own.authorize(input.apiId, input.action);
    const retained = owner.trustForCredential(input.trust);
    assert.deepEqual(retained, input.trust);
    await assert.rejects(() =>
      owner.acceptIssuerRotation(
        new TextEncoder().encode(input.historicalRotation),
      ),
    );
    assert.deepEqual(owner.trustSnapshot, input.trust);
    assert.deepEqual(owner.trustForCredential(input.trust), input.trust);
    await owner.acceptIssuerRotation(
      new TextEncoder().encode(input.forwardRotation),
    );
    assert.deepEqual(owner.trustSnapshot, input.successorTrust);
    assert.deepEqual(
      owner.trustForCredential(input.trust),
      input.successorTrust,
    );
    console.log(
      JSON.stringify({
        historicalCatalogAuthorized: true,
        historicalRotationRejected: true,
        forwardRotationAccepted: true,
        trust: owner.trustSnapshot,
      }),
    );
  } finally {
    own.release();
  }
} finally {
  owner.stop();
  await nats.close();
}
