/** A deployed service administering accounts through accounts.manage. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a service holding accounts.manage finds and deletes a user with access the service lacks", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "account-janitor",
      contract: participants.AccountJanitor.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.AccountJanitor.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      // The user holds the directory app's capabilities, which the service does not.
      const alice = await runtime.createUser({ name: "Disposable Alice" });
      await runtime.grantUserCapabilities({
        user: alice,
        capabilityIds: apps.DirectoryCaller.request.requiredCapabilities,
      });
      const login = await runtime.registerClient({
        name: "disposable caller",
        contract: apps.DirectoryCaller.request,
        user: alice,
      });
      const caller = await connectNativeClient({
        ...login,
        request: apps.DirectoryCaller.request,
      }).orThrow();
      try {
        // No account is linked to this sign-in, at a provider nobody configured.
        const providerId = codecs.ulid.decode(ulid());
        assert.equal(
          (
            await service
              .usersFind({ identity: { providerId, subject: "auth0|nobody" } })
              .orThrow()
          ).found,
          undefined,
        );
        const userId = codecs.ulid.decode(alice.principalId);

        const found = (await service.usersFind({ userId }).orThrow()).found;
        assert(found?.user.account);
        assert.equal(found.user.principalId, alice.principalId);
        assert.equal(found.user.account.principal.enabled, true);
        assert.deepEqual(found.identities, []);
        const expectedRevision = found.user.account.principal.revision;

        await service
          .usersDelete({
            userId,
            expectedRevision,
            idempotencyKey: ulid(),
          })
          .orThrow();
        // Deleting the account ends every sign-in it had.
        await runtime.waitFor(async () =>
          (
            await runtime.callAdminRpc("authAuthorizationSessionsList", {
              principalId: userId,
            })
          ).items.every((session) => session.state === "revoked"),
        );
        assert.equal(
          (await service.usersFind({ userId }).orThrow()).found,
          undefined,
        );
        // A retry whose earlier reply was lost finds nothing left to delete.
        await assert.rejects(
          () =>
            service
              .usersDelete({
                userId,
                expectedRevision,
                idempotencyKey: ulid(),
              })
              .orThrow(),
          (error) => {
            assert(error instanceof apis.auth.AuthError);
            assert.equal(error.data.code, "not_found");
            return true;
          },
        );
        assert(
          (
            await service.usersFind({
              userId,
              identity: { providerId, subject: "auth0|nobody" },
            })
          ).isErr(),
          "a lookup takes exactly one key",
        );
        assert.equal(
          (
            await service
              .usersFind({ userId: codecs.ulid.decode(ulid()) })
              .orThrow()
          ).found,
          undefined,
        );
      } finally {
        await caller.connection.close();
      }
    } finally {
      await service.stop();
    }
  });
});
