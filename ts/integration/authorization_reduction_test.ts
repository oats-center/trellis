/** Server enforcement for uncooperative native clients and provisioned service authority. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { codecs } from "@oatscenter/trellis/generated";
import { isErr } from "@oatscenter/result";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { admitRawNativeClient } from "./_support/client_session.ts";
import {
  admittedConnections,
  readRuntimeBrokerInventory,
} from "./_support/broker_inventory.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("login revocation removes an uncooperative native attachment and denies replay of its admission", async () => {
  await withTrellisRuntime(async (runtime) => {
    const raw = await admitRawNativeClient(
      await runtime.registerClient({
        name: "uncooperative-native",
        contract: adminParticipant,
      }),
    );
    const closed = raw.nc.closed().catch(() => undefined);
    try {
      const info = (await raw.nc.request("$SYS.REQ.USER.INFO")).json<{
        data?: { user?: string };
      }>();
      const user = info.data?.user;
      assert(user && user.startsWith("trellis.auth.v1:"));
      const attachmentId = user.slice("trellis.auth.v1:".length);
      const ids = new Set([attachmentId]);
      assert.equal(
        admittedConnections(await readRuntimeBrokerInventory(runtime), ids)
          .length,
        1,
      );
      const loginSessionId = raw.authority.loginSessionId;
      assert(loginSessionId);
      await runtime.callAdminRpc("authSessionsRevoke", {
        loginSessionId: codecs.ulid.decode(loginSessionId),
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(
        async () =>
          raw.nc.isClosed() &&
          admittedConnections(await readRuntimeBrokerInventory(runtime), ids)
            .length === 0,
      );
      await closed;
      let unexpected: Awaited<ReturnType<typeof connect>> | undefined;
      try {
        await assert.rejects(async () => {
          unexpected = await connect({
            servers: runtime.natsUrl,
            authenticator: raw.authenticator,
            inboxPrefix: raw.inboxPrefix,
            reconnect: false,
          });
        });
      } finally {
        await unexpected?.close();
      }
    } finally {
      await raw.nc.close();
      await closed;
    }
  });
});

test("required capability reduction retires native service sockets and fences its original identity until regrant", async () => {
  await withTrellisRuntime(async (runtime) => {
    const targetIdentity = await runtime.registerService({
      deployment: "reduction-target",
      name: "reduction-target",
      contract: participants.TransportGrowthTarget.participant,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.TransportGrowthTarget.participant,
      seed: targetIdentity.seed,
    }).orThrow();
    const targetExit = target.wait().catch((error: unknown) => error);
    let closeSource: (() => Promise<void>) | undefined;
    try {
      let calls = 0;
      await target.handleAdvance(() => {
        calls++;
        return Result.ok({});
      });
      const contract = participants.TransportGrowthSubject.participant;
      const identity = await runtime.registerService({
        deployment: "reduction-subject",
        name: "reduction-subject",
        contract,
      });
      const source = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      closeSource = () => source.connection.close();
      await source.advance({}).orThrow();
      const sessions = await runtime.callAdminRpc(
        "authAuthorizationSessionsList",
        {},
      );
      const own = sessions.items.find(
        (session) =>
          session.state === "active" &&
          session.binding.kind === "service" &&
          session.binding.instanceId === identity.instanceId,
      );
      assert(own);
      const active = await runtime.callAdminRpc("authConnectionsList", {
        authorizationSessionId: own.authorizationSessionId,
        state: "admitted",
      });
      assert(active.items.length > 0);
      const ids = new Set(
        active.items.map((attachment) => attachment.attachmentId),
      );
      assert(
        admittedConnections(await readRuntimeBrokerInventory(runtime), ids)
          .length > 0,
      );
      const capabilityId = "runtime-trellis.transport_growth.v1:growth.advance";
      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        principalId: own.principalId,
        capabilityId,
      });
      assert.equal(grants.items.length, 1);
      const grant = grants.items[0];
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: grant.grantId,
        expectedRevision: grant.revision,
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(
        async () =>
          admittedConnections(await readRuntimeBrokerInventory(runtime), ids)
            .length === 0,
      );
      await source.connection.close();
      const denied = (
        await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: contract,
          seed: identity.seed,
        })
      ).take();
      if (!isErr(denied)) {
        await denied.connection.close();
        assert.fail(
          "the same identity must not regain a missing required capability",
        );
      }
      await runtime.callAdminRpc("authCapabilityGrantsGrant", {
        principalId: own.principalId,
        capabilityId,
        expectedRevision: 0n,
        idempotencyKey: ulid(),
      });
      const restored = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
      closeSource = () => restored.connection.close();
      await restored.advance({}).orThrow();
      assert.equal(calls, 2);
    } finally {
      await closeSource?.();
      await target.connection.close();
      await targetExit;
    }
  });
});
