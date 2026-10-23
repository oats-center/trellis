import assert from "node:assert/strict";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import {
  apis,
  apps,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "directory consent exposes active profiles, isolates account administration, and is revoked on reconnect",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await using resources = new AsyncDisposableStack();
      const alice = await runtime.createUser({ name: "Directory Twin" });
      const bob = await runtime.createUser({ name: "Directory Twin" });
      const disabled = await runtime.createUser({ name: "Directory Hidden" });
      const aliceId = codecs.ulid.decode(alice.principalId);
      const bobId = codecs.ulid.decode(bob.principalId);
      const disabledId = codecs.ulid.decode(disabled.principalId);
      const required = apps.DirectoryCaller.request.requiredCapabilities;
      for (const user of [alice, bob])
        await runtime.grantUserCapabilities({ user, capabilityIds: required });
      const clients = [];
      for (const user of [alice, bob]) {
        const login = await runtime.registerClient({
          name: "Shared directory",
          contract: apps.DirectoryCaller.request,
          user,
        });
        assert.equal(
          login.session.scope.clientId,
          apps.DirectoryCaller.request.id,
        );
        const client = await connectNativeClient({
          ...login,
          request: apps.DirectoryCaller.request,
        }).orThrow();
        resources.defer(() => client.connection.close());
        clients.push(client);
      }
      const [a, b] = clients;
      assert(a && b);
      const own = (await a.usersResolve({}).orThrow()).user;
      assert.equal(own.principalId, aliceId);
      assert.equal(own.account?.username, alice.username);
      const peer = (await a.usersGet({ userId: bobId }).orThrow()).user;
      assert.equal(peer.principalId, bobId);
      assert.equal(peer.displayName, "Directory Twin");
      assert.equal(peer.account, undefined);
      assert.equal(
        (await b.usersGet({ userId: aliceId }).orThrow()).user.account,
        undefined,
      );
      const ids = [];
      let cursor: string | undefined;
      do {
        const page = await a
          .usersList({ search: "RECTORY TWI", page: { cursor, limit: 1 } })
          .orThrow();
        assert.equal(page.items.length, 1);
        assert.equal(page.items[0].account, undefined);
        ids.push(page.items[0].principalId);
        cursor = page.page.nextCursor;
      } while (cursor);
      assert.deepEqual(ids, [aliceId, bobId].sort());
      assert((await a.usersList({ page: { limit: 201 } })).isErr());
      assert.equal(
        (await a.usersList({ search: bob.username }).orThrow()).items.length,
        0,
      );
      const admin = (
        await runtime.callAdminRpc("authUsersGet", {
          userId: disabledId,
        })
      ).user;
      assert(admin.account);
      await runtime.callAdminRpc("authUsersUpdate", {
        userId: disabledId,
        enabled: false,
        expectedRevision: admin.account.principal.revision,
        idempotencyKey: ulid(),
      });
      assert.equal(
        (await a.usersList({ search: "Directory Hidden" }).orThrow()).items
          .length,
        0,
      );
      for (const userId of [disabledId, codecs.ulid.decode(ulid())]) {
        await assert.rejects(
          () => a.usersGet({ userId }).orThrow(),
          (error) => {
            assert(error instanceof apis.auth.AuthError);
            assert.equal(error.data.code, "not_found");
            return true;
          },
        );
      }
      const managed = await runtime.callAdminRpc("authUsersList", {
        search: disabled.username,
      });
      assert.equal(managed.items.length, 1);
      assert.equal(managed.items[0].account?.principal.enabled, false);
      assert(
        (
          await a.usersCreate({
            username: `forbidden-${ulid()}`,
            displayName: "Forbidden",
            idempotencyKey: ulid(),
          })
        ).isErr(),
      );
      assert(
        (
          await a.usersUpdate({
            userId: bobId,
            enabled: false,
            expectedRevision: 1n,
            idempotencyKey: ulid(),
          })
        ).isErr(),
      );

      // The protected admin's durable privilege is not implicitly delegated to a
      // second ordinary client, even though that same account has full authority.
      const ordinaryAdmin = await runtime.connectClient({
        name: "Unprivileged admin app",
        contract: apps.DirectoryCaller.request,
      });
      assert.equal(
        (await ordinaryAdmin.usersGet({ userId: disabledId })).isErr(),
        true,
      );
      assert.equal(
        (await ordinaryAdmin.usersGet({ userId: bobId }).orThrow()).user
          .account,
        undefined,
      );

      const unentitled = await runtime.createUser({
        name: "No directory grant",
      });
      await runtime.grantUserCapabilities({
        user: unentitled,
        capabilityIds: ["trellis.auth@v1::readAccount"],
      });
      for (const user of [alice, unentitled]) {
        const declined = await runtime.registerClient({
          name: "Declined directory",
          contract: apps.DirectoryOptional.request,
          user,
          approvedCapabilities: ["trellis.auth@v1::readAccount"],
        });
        const noDirectory = await connectNativeClient({
          ...declined,
          request: apps.DirectoryOptional.request,
        }).orThrow();
        resources.defer(() => noDirectory.connection.close());
        assert((await noDirectory.usersList({})).isErr());
        assert((await noDirectory.usersGet({ userId: bobId })).isErr());
        assert.equal(
          (await noDirectory.usersResolve({}).orThrow()).user.principalId,
          user.principalId,
        );
      }

      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        principalId: aliceId,
        capabilityId: "trellis.auth@v1::readUsers",
      });
      assert.equal(grants.items.length, 1);
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: grants.items[0].grantId,
        expectedRevision: grants.items[0].revision,
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(async () =>
        (await a.usersList({}, { timeout: 1000 })).isErr(),
      );
      await assert.rejects(() =>
        runtime.registerClient({
          name: "Revoked directory",
          contract: apps.DirectoryCaller.request,
          user: alice,
        }),
      );
      assert.equal(
        (await b.usersGet({ userId: bobId }).orThrow()).user.principalId,
        bobId,
      );
    });
  },
);
