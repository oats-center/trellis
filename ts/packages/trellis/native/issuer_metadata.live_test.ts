import assert from "node:assert/strict";
import { test } from "node:test";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { apps } from "../../trellis-testkit/trellis/index.js";
import {
  AuthAuthorityResolveResponseCodec,
  AuthAuthorityStatusResponseCodec,
  AuthIssuerChainResponseCodec,
} from "../../trellis-testkit/trellis/types/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { connectNativeSession } from "../native.ts";
import { createSessionAuthorityHandleWasm } from "../auth/protocol_wasm.ts";

test(
  "an admitted client without administrative authority resolves signed verification metadata",
  { timeout: 30_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;

      const login = await loginNativeTestClient({
        issuer,
        username,
        password,
        clientId: "trellis.cli",
        request: apps.cli.request,
        approvedCapabilities: apps.cli.request.requiredCapabilities,
        signal: context.signal,
      });
      const connected = await connectNativeSession({
        ...login,
        signal: context.signal,
      }).orThrow();
      try {
        const lease = connected.owner.own();
        try {
          assert.deepEqual(lease.authority.platformPrivileges, []);
          const status = await connected.owner.readIssuerMetadata({
            kind: "authorityStatus",
            authorizationSessionId: lease.authority.authorizationSessionId,
          });
          assert.equal(status.result.kind, "value");
          if (status.result.kind !== "value")
            throw new Error("signed status unavailable");
          assert.equal(
            AuthAuthorityStatusResponseCodec.decode(
              JSON.parse(status.result.payload),
            ).authorizationSessionId,
            lease.authority.authorizationSessionId,
          );
          const resolved = await connected.owner.readIssuerMetadata({
            kind: "authorityResolve",
            contextDigest: lease.digest,
          });
          assert.equal(resolved.result.kind, "value");
          if (resolved.result.kind !== "value")
            throw new Error("signed authority unavailable");
          const material = AuthAuthorityResolveResponseCodec.decode(
            JSON.parse(resolved.result.payload),
          );
          const verified = await createSessionAuthorityHandleWasm({
            authorityBytes: material.authority,
            verification: {
              issuer: connected.owner.trustSnapshot.issuer,
              trellisInstanceId: connected.owner.instanceId,
              audienceNatsAccount: connected.owner.account,
              policy: connected.owner.policy(),
              purpose: "live",
              revocationCutoff: null,
            },
          });
          try {
            assert.equal(verified.verified.authorityDigest, lease.digest);
          } finally {
            verified.handle.free();
          }
          const chain = await connected.owner.readIssuerMetadata({
            kind: "issuerChain",
            pinnedKeyId: lease.authority.issuerKeyId,
            targetKeyId: lease.authority.issuerKeyId,
          });
          assert.equal(chain.result.kind, "value");
          if (chain.result.kind !== "value")
            throw new Error("signed issuer chain unavailable");
          AuthIssuerChainResponseCodec.decode(JSON.parse(chain.result.payload));
          lease.assertCurrent();
        } finally {
          lease.release();
        }
      } finally {
        await connected.close();
      }
    });
  },
);
