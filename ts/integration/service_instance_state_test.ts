/**
 * What `ServiceInstances.List` says about a connected instance that cannot
 * serve yet: the update its build waits on, with its review link, or the
 * required settings it has no value for.
 */
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a connected instance lists the update it waits on, then nothing once it serves", async () => {
  await withTrellisRuntime(async (runtime) => {
    const base = participants.Provider.participant;
    const extended = extendedParticipants.Provider.participant;
    await runtime.deployments.create({ id: "waiting-update", contract: base });
    await runtime.contracts.apply({
      deployment: "waiting-update",
      contract: base,
    });
    const key = await runtime.services.createInstance({
      name: "waiting-update",
      deployment: "waiting-update",
      contract: base,
    });
    const list = async () =>
      (
        await runtime.callAdminRpc("authServiceInstancesList", {
          deploymentId: codecs.ulid.decode(key.deploymentId),
        })
      ).items.find((item) => item.instanceId === key.instanceId);
    const offline = await list();
    assert.equal(offline?.connectedAt, undefined);
    assert.equal(offline?.participantDigest, base.digest);

    // The extended build asks for a KV the deployment never had.
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: extended,
      seed: key.seed,
    }).orThrow();
    const waiting = await runtime.waitFor(async () => {
      const instance = await list();
      return instance?.waiting?.update ? instance : undefined;
    });
    const { items } = await runtime.callAdminRpc("authServiceUpdatesList", {});
    const pending = items.find((update) => update.state === "pending");
    assert(pending);
    assert(waiting.connectedAt);
    assert.equal(waiting.waiting?.update?.updateId, pending.updateId);
    assert.equal(waiting.waiting?.update?.reviewUrl, pending.reviewUrl);
    assert.deepEqual(waiting.waiting?.missingSettings, []);
    // Until approved it still holds the build it had.
    assert.equal(waiting.participantDigest, base.digest);

    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: pending.updateId,
      decision: "approved",
      expectedRevision: pending.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const service = await connecting;
    try {
      const serving = await list();
      assert(serving?.connectedAt);
      assert.equal(serving.waiting, undefined);
      assert.equal(serving.participantDigest, extended.digest);
    } finally {
      await service.stop();
    }
    await runtime.waitFor(
      async () => (await list())?.connectedAt === undefined,
    );
  });
});

test("a connected instance missing required settings names them", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participant = participants.ConfiguredService.participant;
    const key = await runtime.services.createInstance({
      name: "waiting-settings",
      deployment: "waiting-settings",
      contract: participant,
    });
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      seed: key.seed,
    }).orThrow();
    const listed = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc("authServiceInstancesList", {
        deploymentId: codecs.ulid.decode(key.deploymentId),
      });
      return items.find((item) => item.waiting !== undefined);
    });
    assert.equal(listed.waiting?.update, undefined);
    assert.deepEqual(listed.waiting?.missingSettings, ["apiKey", "endpoint"]);
    assert.equal(
      listed.waiting?.settingsUrl,
      `${runtime.consoleUrl}/console/admin/services/${key.deploymentId}#settings`,
    );
    await runtime.callAdminRpc("authConfigSet", {
      target: { deploymentId: codecs.ulid.decode(key.deploymentId) },
      values: { endpoint: "https://one.example" },
      secrets: { apiKey: "k1" },
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const service = await connecting;
    try {
      const { items } = await runtime.callAdminRpc("authServiceInstancesList", {
        deploymentId: codecs.ulid.decode(key.deploymentId),
      });
      assert.equal(items[0]?.waiting, undefined);
    } finally {
      await service.stop();
    }
  });
});
