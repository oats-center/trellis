/**
 * Concurrent operation-handler registration is safe.
 *
 * Two overlapping registrations for the same operation must both succeed, keep
 * the service healthy, and run the operation exactly once. This exercises the
 * registration path under overlap; it does not claim to distinguish duplicate
 * queue-grouped subscriptions, which is not observable through public behavior.
 */

import { assertEquals } from "@std/assert";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("concurrent operation registration succeeds and runs the operation once", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    if (requested.status === "approval_required") {
      await runtime.contracts.approveApply(requested.pendingId, {});
    }
    const instance = await runtime.services.createInstance({
      name: "parallel-init-provider",
      contract,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      name: "parallel-init-provider",
      seed: instance.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);

    let runs = 0;
    const handler = () => {
      runs += 1;
      return Promise.resolve(Result.ok({ value: "ok" }));
    };
    try {
      // Two overlapping registrations for the same operation.
      await Promise.all([
        service.handleWork(handler),
        service.handleWork(handler),
      ]);
      assertEquals(service.connection.status.phase, "connected");

      const caller = await runtime.connectClient({
        name: "parallel-init-caller",
        contract: participants.Caller.participant,
        timeout: 120_000,
      });
      try {
        const operation = await caller.work({ value: "once" }).start()
          .orThrow();
        const terminal = await operation.wait().orThrow();
        assertEquals(terminal.output, { value: "ok" });
        assertEquals(runs, 1, "the operation handler runs exactly once");
      } finally {
        await caller.connection.close().catch(() => undefined);
      }
    } finally {
      await service.stop();
      await serviceExit;
    }
  });
});
