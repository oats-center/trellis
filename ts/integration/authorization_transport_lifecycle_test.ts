/** Real broker expiration and enforcement after ordinary runtime recovery. */
import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { loginNativeTestClient } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { admitRawNativeClient } from "./_support/client_session.ts";
import {
  admittedConnections,
  brokerConnectionKey,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a non-renewing native attachment cannot outlive its required capability grants", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const username = `bounded-${ulid().toLowerCase()}`;
      const password = `bounded-password-${ulid()}`;
      const clientId = `bounded-${ulid()}`;
      const created = await runtime.callAdminRpc("authUsersCreate", {
        username,
        displayName: "Bounded caller",
        password,
        idempotencyKey: ulid(),
      });
      await runtime.callAdminRpc("authOAuthClientsPut", {
        clientId,
        kind: "native",
        title: "Bounded caller",
        description: "Deadline acceptance",
        redirectUris: ["http://127.0.0.1/callback"],
        development: true,
        impliedConsent: [],
        expectedRevision: 0n,
        idempotencyKey: ulid(),
      });
      const expiresAt = codecs.timestamp.decode(
        new Date(Date.now() + 105_000).toISOString(),
      );
      for (const capabilityId of adminParticipant.requiredCapabilities)
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: created.user.principal.principalId,
          capabilityId,
          expiresAt,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
      const login = await loginNativeTestClient({
        issuer: runtime.oauthIssuer,
        directory: join(runtime.workdir, "bounded-native"),
        clientId,
        request: adminParticipant,
        username,
        password,
        approvedCapabilities: [...adminParticipant.requiredCapabilities],
      });
      let raw: Awaited<ReturnType<typeof admitRawNativeClient>> | undefined;
      try {
        raw = await admitRawNativeClient(login);
        assert(raw.authority.expiresAt * 1000 <= Date.parse(expiresAt));
        const connection = raw.nc;
        const closed = connection.closed().catch(() => undefined);
        const info = (await connection.request("$SYS.REQ.USER.INFO")).json<{
          data?: { user?: string };
        }>();
        const user = info.data?.user;
        assert(user && user.startsWith("trellis.auth.v1:"));
        const ids = new Set([user.slice("trellis.auth.v1:".length)]);
        assert.equal(
          admittedConnections(await readRuntimeBrokerInventory(runtime), ids)
            .length,
          1,
        );
        await runtime.waitFor(
          async () =>
            connection.isClosed() &&
            admittedConnections(await readRuntimeBrokerInventory(runtime), ids)
              .length === 0,
          { timeoutMs: 115_000 },
        );
        await closed;
      } finally {
        await raw?.nc.close();
        await login.session.close();
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 125,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    },
  );
});

test("recovered service attachments remain subject to server enforcement after restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.EchoProvider.participant;
    const identity = await runtime.registerService({
      name: "restart-provider",
      deployment: "restart-provider",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: contract,
      seed: identity.seed,
    }).orThrow();
    const exited = service.wait().catch((error: unknown) => error);
    const caller = await runtime.connectClient({
      name: "restart-caller",
      contract: apps.EchoCaller.request,
    });
    try {
      let calls = 0;
      await service.handleEcho(({ input }) => {
        calls++;
        return Result.ok(input);
      });
      assert.deepEqual(await caller.echo({ value: "before" }).orThrow(), {
        value: "before",
      });
      const beforeSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        {},
      );
      const before = beforeSessions.items.find(
        (session) =>
          session.state === "active" &&
          session.binding.kind === "service" &&
          session.binding.instanceId === identity.instanceId,
      );
      assert(before);
      const beforeAttachments = await runtime.callAdminRpc(
        "authConnectionsList",
        {
          authorizationSessionId: before.authorizationSessionId,
          state: "admitted",
        },
      );
      const beforeIds = new Set(
        beforeAttachments.items.map((attachment) => attachment.attachmentId),
      );
      const beforeSockets = admittedConnections(
        await readRuntimeBrokerInventory(runtime),
        beforeIds,
      );
      assert(beforeSockets.length > 0);
      await runtime.restart();
      await runtime.waitFor(
        () =>
          service.connection.status.phase === "connected" &&
          caller.connection.status.phase === "connected",
      );
      assert.deepEqual(await caller.echo({ value: "after" }).orThrow(), {
        value: "after",
      });
      assert.equal(calls, 2);
      const afterSessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        {},
      );
      const after = afterSessions.items.find(
        (session) =>
          session.state === "active" &&
          session.binding.kind === "service" &&
          session.binding.instanceId === identity.instanceId,
      );
      assert(after);
      assert.equal(after.runtimeId, before.runtimeId);
      const attachments = await runtime.callAdminRpc("authConnectionsList", {
        authorizationSessionId: after.authorizationSessionId,
        state: "admitted",
      });
      const ids = new Set(
        attachments.items.map((attachment) => attachment.attachmentId),
      );
      const recovered = admittedConnections(
        await readRuntimeBrokerInventory(runtime),
        ids,
      );
      assert(recovered.length > 0);
      assert(
        recovered.every(
          (socket) =>
            !beforeSockets.some((old) => old.server === socket.server),
        ),
      );
      const keys = new Set(recovered.map(brokerConnectionKey));
      await runtime.services.disableInstance({
        instanceId: codecs.ulid.decode(identity.instanceId),
        expectedRevision: 1n,
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(
        async () =>
          !(await readRuntimeBrokerInventory(runtime)).some((socket) =>
            keys.has(brokerConnectionKey(socket)),
          ),
      );
    } finally {
      await caller.connection.close();
      await service.connection.close();
      await exited;
    }
  });
});
