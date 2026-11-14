import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import {
  connectNativeClient,
  NativeOAuthClient,
  MemoryNativeOAuthCustody,
} from "@oatscenter/trellis/native";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { apps } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { SessionAuthorityOwner } from "../packages/trellis/auth/authority.ts";
import { authenticateIssuerChain } from "../packages/trellis/auth/issuer_trust.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("administrator issuer rotation is idempotent and retained native custody follows the chain across server restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      deployment: "rotation-provider",
      name: "rotation-provider",
      contract: participants.Provider.participant,
    });
    let service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let serviceExit = service.wait().catch(() => undefined);
    await service.handleEcho(({ input }) => Result.ok(input));
    const prepared = await runtime.registerClient({
      name: "rotation-caller",
      contract: apps.Caller.request,
    });
    await prepared.session.close();
    const credentialCustody = new MemoryNativeOAuthCustody();
    const credentials = await loginNativeTestClient({
      issuer: runtime.trellisUrl,
      custody: credentialCustody,
      clientId: apps.Caller.request.id,
      request: apps.Caller.request,
      username: runtime.adminUsername,
      password: runtime.adminPassword,
    });
    const { scope, trust } = credentials.session;
    const client = await connectNativeClient({
      ...credentials,
      request: apps.Caller.request,
    }).orThrow();
    try {
      assert.deepEqual(
        await client.echo({ value: "before rotation" }).orThrow(),
        { value: "before rotation" },
      );
      const input = {
        expectedCurrentKeyId: trust.issuer.keyId,
        idempotencyKey: ulid(),
      };
      const [rotated, replay] = await Promise.all([
        runtime.callAdminRpc("authIssuersRotate", input),
        runtime.callAdminRpc("authIssuersRotate", input),
      ]);
      assert.equal(replay.keyId, rotated.keyId);
      assert.deepEqual(replay.rotation, rotated.rotation);
      assert.notEqual(rotated.keyId, trust.issuer.keyId);
      await assert.rejects(
        runtime.callAdminRpc("authIssuersRotate", {
          ...input,
          idempotencyKey: ulid(),
        }),
      );
      assert.deepEqual(
        await client.echo({ value: "existing authority" }).orThrow(),
        { value: "existing authority" },
      );
      const second = await runtime.callAdminRpc("authIssuersRotate", {
        expectedCurrentKeyId: rotated.keyId,
        idempotencyKey: ulid(),
      });
      assert.notEqual(second.keyId, rotated.keyId);
      const freshCredentials = await loginNativeTestClient({
        issuer: runtime.trellisUrl,
        clientId: "trellis.cli",
        request: adminParticipant,
        username: runtime.adminUsername,
        password: runtime.adminPassword,
        administrativeCapabilities: ["trellis.auth.v1:accounts.manage"],
      });
      const freshClient = await connectNativeClient({
        ...freshCredentials,
        request: adminParticipant,
      }).orThrow();
      try {
        // The first fresh Auth opening must recover its provider's retired
        // signer before dispatch, without requiring a successful warm-up call.
        const users = await freshClient.usersList({}).orThrow();
        assert(users.items.length > 0);
      } finally {
        await freshClient.connection.close();
        await freshCredentials.session.close();
      }
      await client.connection.close();
      await service.stop();
      await serviceExit;
      await runtime.restart();
      service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
      }).orThrow();
      serviceExit = service.wait().catch(() => undefined);
      await service.handleEcho(({ input }) => Result.ok(input));
      const custody = (
        await NativeOAuthClient.open({
          custody: credentialCustody,
          scope,
          trust,
        })
      ).orThrow();
      try {
        const session = (await custody.resume()).orThrow();
        assert.equal(session.trust.issuer.keyId, second.keyId);
        assert.equal(
          BigInt(session.trust.rotationSequence),
          BigInt(trust.rotationSequence) + 2n,
        );
        const restored = await connectNativeClient({
          session,
          servers: credentials.servers,
          request: apps.Caller.request,
        }).orThrow();
        try {
          assert.deepEqual(
            await restored
              .echo({ value: "authenticated successor after restart" })
              .orThrow(),
            { value: "authenticated successor after restart" },
          );
          const retained = await runtime.callAdminRpc(
            "authIssuersRotate",
            input,
          );
          assert.equal(retained.keyId, rotated.keyId);
          assert.deepEqual(retained.rotation, rotated.rotation);

          // Rotation tells every session to renew, so the service holds
          // authority the new key signed before the old one is revoked.
          // Revoking it then retires user sessions the old key admitted but
          // not the service, which signs in with its own key.
          const serviceSession = async () => {
            const listed = await runtime.callAdminRpc(
              "authAuthorizationSessionsList",
              {
                deploymentId: codecs.ulid.decode(identity.deploymentId),
                page: { limit: 100 },
              },
            );
            // The service process from before the restart left a session
            // behind; the current process holds the newest one.
            const found = listed.items
              .filter((item) => item.binding.instanceId === identity.instanceId)
              .sort((a, b) => b.createdAt.localeCompare(a.createdAt))[0];
            assert(found);
            return found;
          };
          const renewedFrom = await serviceSession();
          // Expiry has whole-second precision. A renewal in the second of
          // the last issuance would carry the same expiry, so the rotation
          // waits for the clock to pass that second.
          const issuedBy = Math.floor(Date.now() / 1000);
          await runtime.waitFor(
            async () => Math.floor(Date.now() / 1000) > issuedBy,
          );
          const third = await runtime.callAdminRpc("authIssuersRotate", {
            expectedCurrentKeyId: second.keyId,
            idempotencyKey: ulid(),
          });
          await runtime.waitFor(
            async () =>
              ((await serviceSession()).maximumContextExpiresAt ?? 0) >
              (renewedFrom.maximumContextExpiresAt ?? 0),
          );
          await runtime.callAdminRpc("authIssuersRevoke", {
            keyId: second.keyId,
            idempotencyKey: ulid(),
          });
          const userSession = async () =>
            (
              await runtime.callAdminRpc("authAuthorizationSessionsList", {
                clientId: apps.Caller.request.id,
                page: { limit: 100 },
              })
            ).items.find((item) => item.runtimeId === session.runtimeId);
          await runtime.waitFor(
            async () => (await userSession())?.state === "revoked",
          );
          const kept = await serviceSession();
          assert.equal(
            kept.authorizationSessionId,
            renewedFrom.authorizationSessionId,
          );
          assert.equal(kept.state, "active");
          assert.notEqual(third.keyId, second.keyId);
        } finally {
          await restored.connection.close();
        }
      } finally {
        await custody.close();
      }
    } finally {
      await client.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

test("startup refuses replaced OAuth custody and restored custody retains client access", async () => {
  await withTrellisRuntime(async (runtime) => {
    const account = (
      await runtime.callAdminRpc("authUsersCreate", {
        username: "retained-custody",
        displayName: "Retained custody",
        idempotencyKey: ulid(),
      })
    ).user;
    const key = join(runtime.workdir, "config", "trellis", "secrets.toml");
    const original = await readFile(key, "utf8");
    try {
      await writeFile(
        key,
        original.replace(
          /^oauth_storage_key = ".*"$/m,
          `oauth_storage_key = "${randomBytes(32).toString("base64url")}"`,
        ),
      );
      await assert.rejects(runtime.restart());
      await writeFile(key, original);
      await runtime.restart();
      assert.deepEqual(
        (
          await runtime.callAdminRpc("authUsersGet", {
            userId: account.principalId,
          })
        ).user,
        account,
      );
    } finally {
      await writeFile(key, original);
    }
  });
});

test("authenticating the issuer chain accepts links its shared owner already holds", async () => {
  await withTrellisRuntime(async (runtime) => {
    const credentials = await loginNativeTestClient({
      issuer: runtime.trellisUrl,
      custody: new MemoryNativeOAuthCustody(),
      clientId: "trellis.cli",
      request: adminParticipant,
      username: runtime.adminUsername,
      password: runtime.adminPassword,
    });
    const anchor = credentials.session.trust;
    await credentials.session.close();
    const rotated = await runtime.callAdminRpc("authIssuersRotate", {
      expectedCurrentKeyId: anchor.issuer.keyId,
      idempotencyKey: ulid(),
    });
    const link: { nextPublicKey: string } = JSON.parse(
      new TextDecoder().decode(rotated.rotation),
    );
    const target = {
      ...anchor,
      issuer: {
        keyId: rotated.keyId,
        publicKey: link.nextPublicKey,
        state: "active" as const,
      },
      rotationSequence: (BigInt(anchor.rotationSequence) + 1n).toString(),
    };
    // A connected session installs the link on Auth's issuer notice, while
    // the credential still names the older pin. A concurrent refresh then
    // authenticates the chain from that pin through the same owner.
    const owner = new SessionAuthorityOwner({
      instanceId: anchor.instanceId,
      account: anchor.account,
      issuers: [anchor.issuer],
      rotationSequence: anchor.rotationSequence,
    });
    try {
      await owner.acceptIssuerRotation(rotated.rotation);
      assert.equal(
        await authenticateIssuerChain(
          runtime.trellisUrl,
          anchor,
          target,
          undefined,
          owner,
        ),
        owner,
      );
      assert.deepEqual(owner.trustForCredential(anchor), target);
    } finally {
      owner.stop();
    }
  });
});
