import assert from "node:assert/strict";
import { test } from "node:test";
import { Result } from "@oatscenter/result";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const id = <const N extends string>(name: N) =>
  `runtime-trellis.transport_growth.v1:${name}` as const;

test("an app requesting a group sees each member the user holds without the whole group", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "group-member-target",
      contract: participants.TransportGrowthTarget.participant,
    });
    const target = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.TransportGrowthTarget.participant,
      seed: identity.seed,
    }).orThrow();
    try {
      await target.handleAdvance(() => Result.ok({}));
      await target.handleExtend(() => Result.ok({}));
      await target.handleExtend2(() => Result.ok({}));
      // A role grants one member directly, not the whole group.
      const user = await runtime.createUser({ name: "Member Holder" });
      await runtime.grantUserCapabilities({
        user,
        capabilityIds: [id("growth.advance"), id("growth.extend")],
      });
      const caller = await runtime.connectClient({
        name: "group member caller",
        contract: apps.GroupMemberCaller.request,
        user,
      });
      const held = await runtime.waitFor(() => {
        const { capabilities } = caller.availability();
        return capabilities[id("growth.extend")] && capabilities;
      });
      assert.deepEqual(held, {
        [id("extras")]: false,
        [id("growth.extend")]: true,
        [id("growth.extendAgain")]: false,
      });
      // The keys gate exactly what the backend allows.
      assert.deepEqual(await caller.extend({}).orThrow(), {});
      assert((await caller.extend2({})).isErr());
    } finally {
      await target.stop();
    }
  });
});
