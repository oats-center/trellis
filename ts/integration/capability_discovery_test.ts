import assert from "node:assert/strict";
import { test } from "node:test";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as earlier } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("capability discovery pages the currently accepted API definitions", async () => {
  await withTrellisRuntime(async (runtime) => {
    const apiId = "runtime-trellis.events@v1";
    await runtime.contracts.install({
      contract: earlier.EventService.participant,
    });
    const before = await runtime.callAdminRpc("authCapabilitiesList", {
      apiId,
    });
    const oldInspect = before.items.find(
      (item) => item.identity.capabilityId === `${apiId.replace("@", ".")}:events.inspect`,
    );
    assert(oldInspect);
    await runtime.contracts.install({
      contract: participants.EventService.participant,
    });
    const current = await runtime.callAdminRpc("authCapabilitiesList", {
      apiId,
    });
    const inspect = current.items.find(
      (item) => item.identity.capabilityId === oldInspect.identity.capabilityId,
    );
    assert(inspect);
    assert(
      inspect.allows.length > oldInspect.allows.length,
      "discovery must expose the newly accepted inspection action",
    );
    const discovered = [];
    let cursor: string | undefined;
    do {
      const page = await runtime.callAdminRpc("authCapabilitiesList", {
        apiId,
        page: { limit: 1, cursor },
      });
      discovered.push(...page.items);
      cursor = page.page.nextCursor ?? undefined;
    } while (cursor);
    assert.deepEqual(discovered, current.items);
    assert.equal(
      new Set(
        discovered.map(
          (item) =>
            `${item.identity.capabilityId}@${item.identity.identityGeneration}`,
        ),
      ).size,
      discovered.length,
    );
    await runtime.contracts.install({
      contract: participants.EventService.participant,
    });
    assert.deepEqual(
      (await runtime.callAdminRpc("authCapabilitiesList", { apiId })).items,
      current.items,
      "reinstalling the accepted package must not duplicate capability definitions",
    );
  });
});
