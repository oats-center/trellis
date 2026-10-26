/**
 * Live acceptance for service-submitted participant updates.
 *
 * A running service presents its own build. A build that asks for a resource
 * the deployment never had waits for an administrator; a build that only gives
 * access up installs at once; and declaring a dropped resource again
 * reattaches its retained data without another approval.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const DEPLOYMENT = "service-updates";

test("a service update waits for approval only when it asks for new access", async () => {
  await withTrellisRuntime(async (runtime) => {
    const base = participants.Provider.participant;
    const extended = extendedParticipants.Provider.participant;
    await runtime.deployments.create({ id: DEPLOYMENT, contract: base });
    await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: base });
    const instance = await runtime.services.createInstance({
      name: "service-updates",
      deployment: DEPLOYMENT,
      contract: base,
    });
    const connect = <P extends typeof base | typeof extended>(participant: P) =>
      TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant,
        seed: instance.seed,
      }).orThrow();

    // The extended build declares a KV the deployment never had.
    const waiting = connect(extended);
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(pending.changes.addedResources, [
      { kind: "kv", name: "extended" },
    ]);
    assert.deepEqual(pending.changes.addedCapabilities, []);
    assert.equal(
      pending.reviewUrl,
      `${runtime.oauthIssuer}/console/admin/services/${pending.deploymentId}?update=${pending.updateId}`,
    );
    const { update: approved } = await runtime.callAdminRpc(
      "authServiceUpdatesDecide",
      {
        updateId: pending.updateId,
        decision: "approved",
        expectedRevision: pending.revision,
        idempotencyKey: ulid(),
      },
    );
    assert.equal(approved.state, "applied");
    const first = await waiting;
    await first.kv.extended.put("kept", { value: "before rollback" }).orThrow();
    await first.kv.records.put("record", { value: "durable" }).orThrow();
    await first.stop();

    // Rolling back only drops the KV, so it installs without an administrator.
    const rolledBack = await connect(base);
    assert.deepEqual(await rolledBack.kv.records.get("record").orThrow(), {
      value: "durable",
    });
    await rolledBack.stop();
    const { items } = await runtime.callAdminRpc("authServiceUpdatesList", {});
    const rollback = items[0];
    assert(rollback);
    assert.equal(rollback.state, "applied");
    assert.deepEqual(rollback.changes.removedResources, [
      { kind: "kv", name: "extended" },
    ]);

    // The fix declares the KV again and gets its retained data back.
    const fixed = await connect(extended);
    assert.deepEqual(await fixed.kv.extended.get("kept").orThrow(), {
      value: "before rollback",
    });
    await fixed.stop();
    const after = await runtime.callAdminRpc("authServiceUpdatesList", {});
    assert.equal(
      after.items.filter((update) => update.state === "pending").length,
      0,
    );
  });
});
