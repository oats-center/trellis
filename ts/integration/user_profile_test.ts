import assert from "node:assert/strict";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "administrative profile CAS rolls back lifecycle edits without overwriting owner changes",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const owner = await runtime.createUser({ name: "Original profile" });
      await runtime.grantUserCapabilities({
        user: owner,
        capabilityIds: apps.DirectoryCaller.request.requiredCapabilities,
      });
      const login = await runtime.registerClient({
        name: "Profile CAS owner",
        user: owner,
        contract: apps.DirectoryCaller.request,
      });
      const client = await connectNativeClient({
        ...login,
        request: apps.DirectoryCaller.request,
      }).orThrow();
      try {
        const userId = codecs.ulid.decode(owner.principalId);
        const loaded = (await runtime.callAdminRpc("authUsersGet", { userId }))
          .user;
        assert(loaded.account);
        const edited = (
          await client
            .usersProfileUpdate({
              displayName: "Owner's newer profile",
              email: "owner@example.com",
              expectedRevision: loaded.account.profileRevision,
              idempotencyKey: ulid(),
            })
            .orThrow()
        ).user;
        await assert.rejects(
          runtime.callAdminRpc("authUsersUpdate", {
            userId,
            displayName: loaded.displayName,
            email: "stale-admin@example.com",
            enabled: false,
            expectedRevision: loaded.account.principal.revision,
            expectedProfileRevision: loaded.account.profileRevision,
            idempotencyKey: ulid(),
          }),
        );
        const after = (await runtime.callAdminRpc("authUsersGet", { userId }))
          .user;
        assert(after.account);
        assert.equal(after.account.principal.enabled, true);
        assert.equal(after.displayName, edited.displayName);
        assert.equal(after.email, edited.email);
        assert.equal(
          (await client.usersResolve({}).orThrow()).user.displayName,
          edited.displayName,
        );
        await assert.rejects(
          runtime.callAdminRpc("authUsersUpdate", {
            userId,
            displayName: "Stale profile-only edit",
            enabled: true,
            expectedRevision: loaded.account.principal.revision,
            expectedProfileRevision: loaded.account.profileRevision,
            idempotencyKey: ulid(),
          }),
        );
        const lifecycleInput = {
          userId,
          enabled: false,
          expectedRevision: loaded.account.principal.revision,
          idempotencyKey: ulid(),
        };
        const lifecycle = (
          await runtime.callAdminRpc("authUsersUpdate", lifecycleInput)
        ).user;
        assert(lifecycle.account);
        assert.equal(lifecycle.account.principal.enabled, false);
        assert.equal(lifecycle.displayName, edited.displayName);
        assert.equal(lifecycle.email, edited.email);
        assert.deepEqual(
          (await runtime.callAdminRpc("authUsersUpdate", lifecycleInput)).user,
          lifecycle,
        );
        const current = (await runtime.callAdminRpc("authUsersGet", { userId }))
          .user;
        assert(current.account);
        const freshInput = {
          userId,
          enabled: true,
          displayName: "Fresh administrator edit",
          email: "fresh-admin@example.com",
          expectedRevision: current.account.principal.revision,
          expectedProfileRevision: current.account.profileRevision,
          idempotencyKey: ulid(),
        };
        await runtime.callAdminRpc("authUsersUpdate", freshInput);
        const fresh = (await runtime.callAdminRpc("authUsersGet", { userId }))
          .user;
        assert(fresh.account);
        assert.equal(fresh.displayName, freshInput.displayName);
        assert.equal(fresh.email, freshInput.email);
        assert.equal(fresh.account.principal.enabled, true);
        for (const expectedProfileRevision of [undefined, 0n]) {
          await assert.rejects(
            runtime.callAdminRpc("authUsersUpdate", {
              userId,
              enabled: false,
              displayName: "Missing or zero profile CAS",
              expectedRevision: fresh.account.principal.revision,
              ...(expectedProfileRevision === undefined
                ? {}
                : { expectedProfileRevision }),
              idempotencyKey: ulid(),
            }),
          );
        }
        const unchanged = (
          await runtime.callAdminRpc("authUsersGet", { userId })
        ).user;
        assert.equal(unchanged.displayName, fresh.displayName);
        assert.equal(unchanged.account?.principal.enabled, true);
      } finally {
        await client.connection.close();
      }
    });
  },
);

test(
  "ordinary owners update only their own durable profile with revision and replay protection",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await using resources = new AsyncDisposableStack();
      const users = [
        await runtime.createUser({ name: "Profile owner" }),
        await runtime.createUser({ name: "Other owner" }),
      ];
      const clients = [];
      for (const user of users) {
        await runtime.grantUserCapabilities({
          user,
          capabilityIds: apps.DirectoryCaller.request.requiredCapabilities,
        });
        const login = await runtime.registerClient({
          name: "Profile application",
          user,
          contract: apps.DirectoryCaller.request,
        });
        const client = await connectNativeClient({
          ...login,
          request: apps.DirectoryCaller.request,
        }).orThrow();
        resources.defer(() => client.connection.close());
        clients.push(client);
      }
      const [alice, bob] = clients;
      const [owner, other] = users;
      assert(alice && bob && owner && other);
      const original = (await alice.usersResolve({}).orThrow()).user;
      assert(original.account);
      const input = {
        displayName: "Chosen name",
        givenName: "Alice",
        familyName: "Owner",
        email: "shared@example.com",
        gravatarEnabled: false,
        userId: codecs.ulid.decode(other.principalId),
        expectedRevision: original.account.profileRevision,
        idempotencyKey: ulid(),
      };
      const saved = (await alice.usersProfileUpdate(input).orThrow()).user;
      assert.equal(saved.principalId, owner.principalId);
      assert.equal(saved.displayName, "Chosen name");
      assert.equal(saved.email, "shared@example.com");
      assert.equal(saved.emailVerified, false);
      assert(saved.account);
      assert.equal(saved.account.gravatarEnabled, false);
      assert.equal(
        (await alice.usersProfileUpdate(input).orThrow()).user.account
          ?.profileRevision,
        saved.account.profileRevision,
      );
      assert(
        (
          await alice.usersProfileUpdate({
            ...input,
            displayName: "Stale overwrite",
            idempotencyKey: ulid(),
          })
        ).isErr(),
      );
      assert.equal(
        (await bob.usersResolve({}).orThrow()).user.displayName,
        "Other owner",
      );
      const bobOwn = (await bob.usersResolve({}).orThrow()).user;
      assert(bobOwn.account);
      await bob
        .usersProfileUpdate({
          email: "shared@example.com",
          expectedRevision: bobOwn.account.profileRevision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      const found = await alice
        .usersList({ search: "SHARED@EXAMPLE" })
        .orThrow();
      assert.deepEqual(
        found.items.map((user) => user.principalId).sort(),
        users.map((user) => user.principalId).sort(),
      );
      assert(
        found.items.every(
          (user) => user.account === undefined && user.emailVerified === false,
        ),
      );
      assert.equal(
        (
          await bob
            .usersGet({ userId: codecs.ulid.decode(owner.principalId) })
            .orThrow()
        ).user.givenName,
        "Alice",
      );
      assert.equal(
        (await bob.usersList({ search: "Owner" }).orThrow()).items.filter(
          (user) => user.principalId === owner.principalId,
        ).length,
        1,
      );
      assert(
        (
          await alice.usersProfileUpdate({
            email: "changed@example.com",
            clearEmail: true,
            expectedRevision: saved.account.profileRevision,
            idempotencyKey: ulid(),
          })
        ).isErr(),
      );
      assert(
        (
          await alice.usersProfileUpdate({
            avatarOverrideUrl: "https://127.0.0.1/avatar.png",
            expectedRevision: saved.account.profileRevision,
            idempotencyKey: ulid(),
          })
        ).isErr(),
      );
      const cleared = (
        await alice
          .usersProfileUpdate({
            clearEmail: true,
            expectedRevision: saved.account.profileRevision,
            idempotencyKey: ulid(),
          })
          .orThrow()
      ).user;
      assert.equal(cleared.email, undefined);
      assert.equal(cleared.emailVerified, false);
      assert(cleared.account);
      const picture = (
        await alice
          .usersProfileUpdate({
            avatarOverrideUrl: "https://images.invalid/owner.png",
            expectedRevision: cleared.account.profileRevision,
            idempotencyKey: ulid(),
          })
          .orThrow()
      ).user;
      assert(picture.avatarUrl);
      assert.equal(
        new URL(picture.avatarUrl).origin,
        new URL(runtime.trellisUrl).origin,
      );
      assert(
        new URL(picture.avatarUrl).pathname.startsWith("/profile/avatars/"),
      );
      assert(!picture.avatarUrl.includes("images.invalid"));
      const directoryPhoto = (
        await bob
          .usersGet({ userId: codecs.ulid.decode(owner.principalId) })
          .orThrow()
      ).user;
      assert(directoryPhoto.avatarUrl);
      assert.equal(directoryPhoto.account, undefined);
      assert.equal((await fetch(picture.avatarUrl)).status, 404);
      assert.equal(
        (await alice.usersResolve({}).orThrow()).user.displayName,
        "Chosen name",
      );
      assert(picture.account);
      await alice
        .usersProfileUpdate({
          clearAvatarOverride: true,
          expectedRevision: picture.account.profileRevision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      assert.equal((await fetch(picture.avatarUrl)).status, 404);
      await Promise.all(clients.map((client) => client.connection.close()));
      await runtime.restart();
      const login = await runtime.registerClient({
        name: "Profile application",
        user: owner,
        contract: apps.DirectoryCaller.request,
      });
      const retained = await connectNativeClient({
        ...login,
        request: apps.DirectoryCaller.request,
      }).orThrow();
      resources.defer(() => retained.connection.close());
      const profile = (await retained.usersResolve({}).orThrow()).user;
      assert.equal(profile.principalId, owner.principalId);
      assert.equal(profile.displayName, "Chosen name");
      assert.equal(profile.familyName, "Owner");
      assert.equal(profile.email, undefined);
      assert.equal(profile.account?.gravatarEnabled, false);
    });
  },
);
