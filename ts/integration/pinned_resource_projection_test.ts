/**
 * Live acceptance for revision-pinned resource projection.
 *
 * A service instance pinned to the older participant revision keeps its own
 * declaration vocabulary while the deployment advances to a newer revision. Its
 * resource authority is projected from the one current physical materialization:
 * retained resources remain usable across an additive apply and original-seed
 * reconnect; removing a required resource fences the broader pinned provider.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as pinnedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const DEPLOYMENT = "pinned-projection";

test("a revision-pinned service projects current materialization", async () => {
  await withTrellisRuntime(async (runtime) => {
    const r1 = participants.Provider.participant;
    const r2 = pinnedParticipants.Provider.participant;
    await runtime.deployments.create({ id: DEPLOYMENT, contract: r1 });
    await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: r1 });
    const instanceA = await runtime.services.createInstance({
      name: "pinned-a",
      deployment: DEPLOYMENT,
      contract: r1,
    });
    const serviceA = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: r1,
      seed: instanceA.seed,
    }).orThrow();
    const aExit = serviceA.wait().catch((error: unknown) => error);
    let closeB: (() => Promise<void>) | undefined;
    try {
      const records = serviceA.kv.records;
      const extras = serviceA.kv.extras;
      assert(records && extras);
      await records.put("pinned", { value: "retained-r1" }).orThrow();
      await extras.put("optional", { value: "optional-r1" }).orThrow();
      await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: r2 });
      const instanceB = await runtime.services.createInstance({
        name: "pinned-b",
        deployment: DEPLOYMENT,
        contract: r2,
      });
      const serviceB = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: r2,
        seed: instanceB.seed,
      }).orThrow();
      const bExit = serviceB.wait().catch((error: unknown) => error);
      closeB = async () => {
        await serviceB.stop();
        await bExit;
      };
      await serviceB.kv.extended.put("new", { value: "r2-only" }).orThrow();
      assert.deepEqual(await serviceB.kv.extended.get("new").orThrow(), {
        value: "r2-only",
      });
      assert.deepEqual(await records.get("pinned").orThrow(), {
        value: "retained-r1",
      });
      assert.deepEqual(await extras.get("optional").orThrow(), {
        value: "optional-r1",
      });
      // The original identity admits its pinned revision against the current
      // materialization; reconnect does not replace its declared vocabulary.
      await serviceA.stop();
      await aExit;
      const reconnected = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: r1,
        seed: instanceA.seed,
      }).orThrow();
      const reconnectExit = reconnected.wait().catch((error: unknown) => error);
      try {
        assert.deepEqual(await reconnected.kv.records.get("pinned").orThrow(), {
          value: "retained-r1",
        });
        await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: r1 });
        await reconnected.kv.records
          .put("after-removal", { value: "still-usable" })
          .orThrow();
        assert.deepEqual(
          await reconnected.kv.records.get("after-removal").orThrow(),
          { value: "still-usable" },
        );
        await runtime.waitFor(async () =>
          (await serviceB.kv.extended.get("new")).isErr(),
        );
      } finally {
        await reconnected.stop();
        await reconnectExit;
      }
    } finally {
      await closeB?.();
      await serviceA.stop();
      await aExit;
    }
  });
});
