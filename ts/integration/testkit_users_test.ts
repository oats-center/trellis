import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/result";
import { connectNativeClient } from "@oatscenter/trellis/native";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestConnectedClient } from "@oatscenter/trellis-testkit";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";

test(
  "one registered app authenticates distinct ordinary users without mixing object ownership, grants, or credential families",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "owned-object-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        seed: identity.seed,
        name: "owned-object-provider",
      }).orThrow();
      const clients: TrellisTestConnectedClient<typeof apps.Caller.request>[] =
        [];
      try {
        let owner: string | undefined;
        await service.handleEcho(async ({ input, context: { caller } }) => {
          assert(caller.type === "verified");
          if (input.value === "profile")
            return Result.ok({ value: JSON.stringify(caller.user ?? null) });
          if (input.value === "client") {
            assert(caller.binding.kind === "native");
            return Result.ok({ value: caller.binding.clientId });
          }
          if (input.value === "create") {
            if (owner !== undefined && owner !== caller.principalId)
              return Result.ok({ value: "forbidden" });
            owner = caller.principalId;
            return Result.ok({ value: "created" });
          }
          if (input.value === "read")
            return Result.ok({
              value:
                owner === caller.principalId ? "alice-secret" : "forbidden",
            });
          return Result.ok({ value: caller.principalId });
        });
        const alice = await runtime.createUser({
          name: "Alice",
          email: "alice@example.test",
        });
        const bob = await runtime.createUser({ name: "Bob" });
        const capabilityIds = apps.Caller.request.requiredCapabilities;
        await runtime.grantUserCapabilities({ user: alice, capabilityIds });
        await runtime.grantUserCapabilities({ user: bob, capabilityIds });
        const logins = [];
        for (const user of [alice, alice, bob]) {
          const login = await runtime.registerClient({
            name: "one shared app",
            contract: apps.Caller.request,
            user,
          });
          logins.push(login);
          clients.push(
            await connectNativeClient({
              ...login,
              request: apps.Caller.request,
            }).orThrow(),
          );
        }
        assert(
          logins.every(
            ({ session }) => session.scope.clientId === apps.Caller.request.id,
          ),
        );
        assert.equal(
          new Set(logins.map(({ session }) => session.durableDpopJkt)).size,
          3,
        );
        const [a1, a2, b1] = clients;
        assert(a1 && a2 && b1);
        for (const client of clients) {
          assert.equal(
            (await client.echo({ value: "client" }).orThrow()).value,
            apps.Caller.request.id,
          );
        }
        assert.equal(
          (await a1.echo({ value: "who" }).orThrow()).value,
          alice.principalId,
        );
        assert.equal(
          (await a2.echo({ value: "who" }).orThrow()).value,
          alice.principalId,
        );
        assert.equal(
          (await b1.echo({ value: "who" }).orThrow()).value,
          bob.principalId,
        );
        assert.notEqual(bob.principalId, alice.principalId);
        // A handler sees each caller's own profile, without directory access.
        assert.deepEqual(
          JSON.parse((await a1.echo({ value: "profile" }).orThrow()).value),
          {
            displayName: "Alice",
            email: "alice@example.test",
            emailVerified: false,
          },
        );
        assert.deepEqual(
          JSON.parse((await b1.echo({ value: "profile" }).orThrow()).value),
          { displayName: "Bob", emailVerified: false },
        );
        assert.equal(
          (await a1.echo({ value: "create" }).orThrow()).value,
          "created",
        );
        assert.equal(
          (await b1.echo({ value: "read" }).orThrow()).value,
          "forbidden",
        );
        assert.equal(
          (await b1.echo({ value: "create" }).orThrow()).value,
          "forbidden",
        );
        assert.equal(
          (await a2.echo({ value: "read" }).orThrow()).value,
          "alice-secret",
        );
        await assert.rejects(
          () =>
            runtime.registerClient({
              name: "forged handle",
              contract: apps.Caller.request,
              user: { ...alice },
            }),
          /does not belong to this runtime/u,
        );

        const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
          principalId: codecs.ulid.decode(bob.principalId),
          capabilityId: capabilityIds[0],
        });
        const grant = grants.items[0];
        assert(grant);
        await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
          grantId: grant.grantId,
          expectedRevision: grant.revision,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(async () =>
          (await b1.echo({ value: "read" })).isErr(),
        );
        await assert.rejects(() =>
          runtime.registerClient({
            name: "no implicit regrant",
            contract: apps.Caller.request,
            user: bob,
          }),
        );
        assert.equal(
          (await a1.echo({ value: "read" }).orThrow()).value,
          "alice-secret",
        );
        assert.equal(
          (await a2.echo({ value: "read" }).orThrow()).value,
          "alice-secret",
        );
        await runtime.grantUserCapabilities({ user: bob, capabilityIds });
        const restored = await runtime.connectClient({
          name: "explicit Bob regrant",
          contract: apps.Caller.request,
          user: bob,
        });
        clients.push(restored);
        assert.equal(
          (await restored.echo({ value: "who" }).orThrow()).value,
          bob.principalId,
        );
        assert.equal(
          (await restored.echo({ value: "read" }).orThrow()).value,
          "forbidden",
        );
        await assert.rejects(() =>
          loginNativeTestClient({
            issuer: runtime.oauthIssuer,
            clientId: apps.Caller.request.id,
            request: apps.Caller.request,
            username: bob.username,
            password: `incorrect-${ulid()}`,
          }),
        );
        const userId = codecs.ulid.decode(bob.principalId);
        const { user: current } = await runtime.callAdminRpc("authUsersGet", {
          userId,
        });
        assert(current.account);
        await runtime.callAdminRpc("authUsersUpdate", {
          userId,
          enabled: false,
          expectedRevision: current.account.principal.revision,
          idempotencyKey: ulid(),
        });
        await assert.rejects(() =>
          runtime.registerClient({
            name: "disabled Bob",
            contract: apps.Caller.request,
            user: bob,
          }),
        );
        assert.equal(
          (await a2.echo({ value: "read" }).orThrow()).value,
          "alice-secret",
        );
      } finally {
        await Promise.all(clients.map((client) => client.connection.close()));
        await service.stop();
        await service.wait();
      }
    });
  },
);
