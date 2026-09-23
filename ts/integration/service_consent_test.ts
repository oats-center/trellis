import { assert, assertRejects } from "@std/assert";

import { TrellisService } from "@oats-center/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("service connection requires an approved participant identity", async (t) => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.contracts.install({
      contract: participants.Provider.participant,
    });
    const pending = await runtime.contracts.requestApply({
      contract: participants.Provider.participant,
    });

    await t.step("deployment apply stops at consent", () => {
      assert(pending.status === "approval_required");
    });
    if (pending.status !== "approval_required") return;

    await t.step(
      "unapproved deployment cannot provision an identity",
      async () => {
        await assertRejects(() => runtime.services.provisionInstanceOnly({}));
      },
    );

    await t.step("approved deployment provisions and connects", async () => {
      const approval = await runtime.contracts.approveApply(pending.pendingId);
      assert(approval.participantId.length > 0);
      const identity = await runtime.services.createInstance({
        name: "provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: "provider",
        seed: identity.seed,
      }).orThrow();
      assert(service.connection !== undefined);
    });
  });
});
