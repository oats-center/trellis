import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TransportError } from "../packages/trellis/errors/index.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/**
 * Proves a service acquires a resource approved after it connected without
 * replacing its physical attachment.
 *
 * The Provider deployment starts with its optional `extras` KV declined, so the
 * first application authorization `A` and the desired authority `D` both omit
 * it. Approving `extras` afterwards grows `D`; the same attachment keeps serving
 * `records` while `extras` is reported as a pending transport condition, and one
 * explicit refresh makes it usable without disturbing `records`.
 */
Deno.test("a service adopts a resource approved after connect on the same attachment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    assertEquals(requested.status, "approval_required");
    if (requested.status !== "approval_required") return;
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "resource-growth-provider",
      contract,
    });

    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "resource-growth-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    try {
      await service.handleEcho(({ input }) => Result.ok(input));
      const records = service.kv.records;
      assert(records, "the required KV must be bound");
      await records.put("before", { value: "before" });
      assertEquals(await records.get("before").orThrow(), { value: "before" });
      assertEquals(
        service.kv.extras,
        undefined,
        "a declined optional resource must not be bound",
      );

      // Grow authority: approve the optional resource the deployment declined.
      await runtime.contracts.apply({ contract });

      // The refreshed application authorization D now grants `extras` while the
      // admitted attachment A still does not.
      const extras = await runtime.waitFor(() => service.kv.extras ?? false, {
        timeoutMs: 60_000,
      });
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "the already-admitted resource keeps working across the growth",
      );

      const pending = await extras.get("missing");
      assert(pending.isErr());
      assertEquals(
        pending.error instanceof TransportError && pending.error.code,
        "transport_upgrade_required",
      );

      await service.connection.refreshTransport().orThrow();

      await extras.put("after", { value: "after" });
      assertEquals(await extras.get("after").orThrow(), { value: "after" });
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "the earlier resource stays usable after adoption",
      );
    } finally {
      await service.stop();
      await serviceExit;
    }
  });
});
