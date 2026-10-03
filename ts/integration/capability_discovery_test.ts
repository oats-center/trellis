import { assert, assertEquals } from "@std/assert";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as earlier } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("capability discovery preserves exact installed snapshots and distinguishes current API definitions", async () => {
  await withTrellisRuntime(async (runtime) => {
    const previous = await runtime.contracts.install({
      contract: earlier.EventService.participant,
    });
    await runtime.contracts.install({ contract: earlier.Alpha.participant });
    const sourceApi = "runtime-trellis.events@v1";
    const oldCatalog = await runtime.callAdminRpc("authCapabilitiesList", {
      participantId: previous.participantId,
      revision: previous.installedRevision,
      sourceApi,
    });
    assert(oldCatalog.items.length > 0);
    const current = await runtime.contracts.install({
      contract: participants.EventService.participant,
    });
    const currentCatalog = await runtime.callAdminRpc("authCapabilitiesList", {
      participantId: current.participantId,
      revision: current.installedRevision,
      sourceApi,
    });
    assert(currentCatalog.items.length > 0);
    assert(currentCatalog.items[0].apiDigest !== oldCatalog.items[0].apiDigest);
    assert(
      currentCatalog.items[0].allows.length > oldCatalog.items[0].allows.length,
    );
    const retained = await runtime.callAdminRpc("authCapabilitiesList", {
      participantId: previous.participantId,
      revision: previous.installedRevision,
      sourceApi,
    });
    assertEquals(
      retained.items,
      oldCatalog.items,
      "upgrading the participant must not change discovery for its retained revision",
    );
    const latest = await runtime.callAdminRpc("authCapabilitiesList", {
      participantId: current.participantId,
      sourceApi,
    });
    assertEquals(latest.items, currentCatalog.items);

    // The old Alpha installation still references the old definition. Walk one
    // entry per page to prove variants are neither overwritten nor skipped.
    const discovered = [];
    let cursor: string | undefined;
    do {
      const page = await runtime.callAdminRpc("authCapabilitiesList", {
        sourceApi,
        page: { limit: 1, cursor },
      });
      discovered.push(...page.items);
      cursor = page.page.nextCursor ?? undefined;
    } while (cursor);
    assertEquals(
      new Set(discovered.map((item) => item.apiDigest)),
      new Set([
        oldCatalog.items[0].apiDigest,
        currentCatalog.items[0].apiDigest,
      ]),
    );
    assertEquals(
      discovered.length,
      oldCatalog.items.length + currentCatalog.items.length,
    );

    // Once every current installation uses the new API, old definitions leave
    // global discovery but remain available through the historical scope.
    await runtime.contracts.install({
      contract: participants.Alpha.participant,
    });
    const updated = await runtime.callAdminRpc("authCapabilitiesList", {
      sourceApi,
    });
    assertEquals(
      updated.items,
      currentCatalog.items,
      "identical API definitions from multiple participants deduplicate",
    );
    const historical = await runtime.callAdminRpc("authCapabilitiesList", {
      participantId: previous.participantId,
      revision: previous.installedRevision,
      sourceApi,
    });
    assertEquals(historical.items, oldCatalog.items);
  });
});
