import { assert, assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
  connectedAt: bigint;
};

async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items
    .filter((item) => item.participantId === participantId)
    .sort((a, b) => Number(b.connectedAt - a.connectedAt));
}

/**
 * Proves a service owns physical transport generations when a resource approved
 * after connect grows its desired authority, with provider-aware resource
 * handles that follow the current generation.
 *
 * The Provider deployment starts with its optional `extras` KV declined. When
 * `extras` is approved afterwards the service must open a wider physical
 * generation automatically — no explicit refresh — and the same public `records`
 * and `extras` handles must keep working across the rollover.
 */
Deno.test("a service automatically adopts a new generation for a resource approved after connect", async () => {
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

      const [initial] = await attachmentsFor(runtime, contract.identity);
      assert(initial, "the service must have a physical attachment");
      const logical = initial.runtimeConnectionId;
      const physical = initial.connectionId;

      // Grow authority: approve the optional resource the deployment declined.
      await runtime.contracts.apply({ contract });

      // The service adopts a wider physical generation automatically.
      const adopted = await runtime.waitFor(async () => {
        const items = (await attachmentsFor(runtime, contract.identity)).filter(
          (item) => item.runtimeConnectionId === logical,
        );
        return items.length >= 2 ? items : false;
      }, { timeoutMs: 90_000 });
      assert(
        adopted.some((item) => item.connectionId !== physical),
        "authority growth must adopt a new physical generation",
      );

      // The already-admitted required resource keeps working across the growth.
      assertEquals(
        await records.get("before").orThrow(),
        { value: "before" },
        "the already-admitted resource keeps working across the growth",
      );
      await records.put("after", { value: "after" });
      assertEquals(await records.get("after").orThrow(), { value: "after" });

      // The newly approved optional resource materializes and its handle
      // acquires the new generation rather than the original attachment.
      const extras = await runtime.waitFor(() => service.kv.extras ?? false, {
        timeoutMs: 60_000,
      });
      await extras.put("grown", { value: "grown" });
      assertEquals(await extras.get("grown").orThrow(), { value: "grown" });
      assertEquals(service.connection.status.phase, "connected");
    } finally {
      await service.stop();
      await serviceExit;
    }
  });
});
