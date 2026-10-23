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
import { TrellisService } from "@oatscenter/trellis/service";
import { apps } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("administrator issuer rotation is idempotent and retained native custody follows the chain across server restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      deployment: "rotation-provider",
      name: "rotation-provider",
      contract: participants.Provider.participant,
    });
    let service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
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
      issuer: runtime.oauthIssuer,
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
        issuer: runtime.oauthIssuer,
        clientId: "trellis.cli",
        request: adminParticipant,
        username: runtime.adminUsername,
        password: runtime.adminPassword,
        administrativeCapabilities: ["trellis.managePrincipals"],
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
        trellisUrl: runtime.oauthIssuer,
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
    const key = join(
      runtime.workdir,
      "config",
      "trellis",
      "auth",
      "oauth-storage.key",
    );
    const original = await readFile(key);
    try {
      await writeFile(key, `${randomBytes(32).toString("base64url")}\n`);
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
