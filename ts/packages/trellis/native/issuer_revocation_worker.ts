import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { connect } from "@nats-io/transport-node";
import {
  SessionAuthorityOwner,
  type AuthorityRegistryBinding,
  type RuntimeAuthorizationTrust,
} from "../auth/authority.ts";
import type { CatalogActionIdentity } from "../auth/protocol_wasm.ts";

// The runtime adapter supplies production-issued artifacts: authority the
// rotated-out key signed for a service session the revocation spared, and the
// real rotation link that retired that key.
const input: {
  servers: string[];
  trust: RuntimeAuthorizationTrust;
  registry: AuthorityRegistryBinding;
  authority: string;
  runtimeId: string;
  sessionPublicKey: string;
  apiId: string;
  action: CatalogActionIdentity;
  rotation: string;
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
    await owner.acceptIssuerRotation(new TextEncoder().encode(input.rotation));
    // Inside the signed overlap the rotated-out key still authorizes.
    await own.authorize(input.apiId, input.action);
    // Auth's issuer notice prompts this re-read; the key is now revoked.
    await owner.recheckRetiringIssuers();
    await assert.rejects(() => own.authorize(input.apiId, input.action));
    console.log(JSON.stringify({ revokedKeyRefused: true }));
  } finally {
    own.release();
  }
} finally {
  owner.stop();
  await nats.close();
}
