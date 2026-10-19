import assert from "node:assert/strict";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

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
        new URL(runtime.oauthIssuer).origin,
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
