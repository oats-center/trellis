import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import type { AppRequestDescriptor } from "@oatscenter/trellis/generated";
import {
  connectNativeClient,
  NativeOAuthClient,
  MemoryNativeOAuthCustody,
} from "@oatscenter/trellis/native";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { apps as narrow } from "../../integration/fixtures/client-upgrade-narrow/packages/client-upgrade/index.js";
import {
  apps as revised,
  participants as revisedProviders,
} from "../../integration/fixtures/client-upgrade-consent/packages/client-upgrade/index.js";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("native request expansion and revised capability meaning require fresh explicit OAuth consent", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      deployment: "upgrade-provider",
      name: "upgrade-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch(() => undefined);
    await service.handleEcho(({ input }) => Result.ok(input));
    await service.handleExtra(({ input }) => Result.ok(input));
    const clients = new Set<{ connection: { close(): Promise<void> } }>();
    const login = async <
      T extends AppRequestDescriptor & { readonly kind: "native" },
    >(
      request: T,
    ) => {
      const prepared = await runtime.registerClient({
        name: "upgrade-caller",
        contract: request,
      });
      await prepared.session.close();
      const custody = new MemoryNativeOAuthCustody();
      const credentials = await loginNativeTestClient({
        issuer: runtime.trellisUrl,
        custody,
        clientId: request.id,
        request,
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      const scope = credentials.session.scope;
      const trust = credentials.session.trust;
      const grantId = credentials.session.oauthGrantId;
      const client = await connectNativeClient({
        ...credentials,
        request,
      }).orThrow();
      clients.add(client);
      return {
        custody,
        scope,
        trust,
        grantId,
        servers: credentials.servers,
        client,
      };
    };
    try {
      const old = await login(narrow.Caller.request);
      assert.deepEqual(await old.client.echo({ value: "original" }).orThrow(), {
        value: "original",
      });
      await old.client.connection.close();
      const expanded = await login(apps.Caller.request);
      assert.notEqual(expanded.grantId, old.grantId);
      assert.deepEqual(
        await expanded.client.extra({ value: "explicit consent" }).orThrow(),
        { value: "explicit consent" },
      );
      await expanded.client.connection.close();
      // Broader code and broader account eligibility cannot widen an old grant.
      let custody = (await NativeOAuthClient.open(old)).orThrow();
      let session = (await custody.resume()).orThrow();
      const unapproved = await connectNativeClient({
        session,
        servers: old.servers,
        request: apps.Caller.request,
      }).orThrow();
      clients.add(unapproved);
      assert((await unapproved.extra({ value: "not approved" })).isErr());
      assert.deepEqual(
        await unapproved.echo({ value: "still narrow" }).orThrow(),
        { value: "still narrow" },
      );
      await unapproved.connection.close();
      await runtime.contracts.install({
        contract: revisedProviders.Provider.participant,
      });
      custody = (await NativeOAuthClient.open(expanded)).orThrow();
      try {
        assert(
          (await custody.resume()).isErr(),
          "old consent must not authorize the revised capability meaning",
        );
      } finally {
        await custody.close();
      }
      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        capabilityId: apps.Caller.request.requiredCapabilities[0],
      });
      assert.equal(grants.items.length, 1);
      const grant = grants.items[0];
      await runtime.callAdminRpc("authCapabilityGrantsGrant", {
        principalId: grant.principalId,
        capabilityId: apps.Caller.request.requiredCapabilities[0],
        expectedRevision: grant.revision,
        idempotencyKey: ulid(),
      });
      const current = await login(revised.Caller.request);
      assert.notEqual(current.grantId, expanded.grantId);
      assert.deepEqual(
        await current.client.extra({ value: "revised consent" }).orThrow(),
        { value: "revised consent" },
      );
      custody = (await NativeOAuthClient.open(old)).orThrow();
      session = (await custody.resume()).orThrow();
      const retained = await connectNativeClient({
        session,
        servers: old.servers,
        request: narrow.Caller.request,
      }).orThrow();
      clients.add(retained);
      assert.deepEqual(
        await retained.echo({ value: "unchanged capability" }).orThrow(),
        { value: "unchanged capability" },
      );
    } finally {
      await Promise.all(
        [...clients].map((client) => client.connection.close()),
      );
      await service.stop();
      await serviceExit;
    }
  });
});
