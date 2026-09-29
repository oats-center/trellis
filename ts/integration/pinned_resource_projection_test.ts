/**
 * Live acceptance for revision-pinned resource projection (plan §11.4).
 *
 * A service instance pinned to the older participant revision keeps its own
 * declaration vocabulary while the deployment advances to a newer revision. Its
 * resource authority is projected from the one current physical materialization:
 * a compatible resource approved later becomes available to the pinned view, and
 * removing the newer revision's own resource retires only the connection
 * admitted with it.
 */

import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as pinnedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const DEPLOYMENT = "pinned-projection";

/** Current live physical connection for one runtime instance, when present. */
async function liveConnection(
  runtime: TrellisTestRuntime,
  instanceId: string,
) {
  const listed = await runtime.callAdminRpc("authConnectionsList", {
    page: { limit: 100 },
  });
  return listed.items.find((item) => item.instanceId === instanceId);
}

Deno.test("a revision-pinned service projects current materialization", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.deployments.create({ id: DEPLOYMENT, kind: "service" });

    const r1 = participants.Provider.participant;
    const r2 = pinnedParticipants.Provider.participant;

    // R1: approve the deployment without the optional `extras` resource.
    await runtime.contracts.install({ contract: r1 });
    const requestedR1 = await runtime.contracts.requestApply({
      deployment: DEPLOYMENT,
      contract: r1,
    });
    assertEquals(requestedR1.status, "approval_required");
    if (requestedR1.status !== "approval_required") return;
    await runtime.contracts.approveApply(requestedR1.pendingId, {
      excludeResources: ["extras"],
    });

    const instanceA = await runtime.services.createInstance({
      deployment: DEPLOYMENT,
      name: "pinned-a",
      contract: r1,
    });
    const serviceA = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: r1,
      name: "pinned-a",
      seed: instanceA.seed,
    }).orThrow();
    const serviceAExit = serviceA.wait().catch((error: unknown) => error);

    const cleanups: Array<() => Promise<unknown>> = [];
    try {
      await serviceA.handleEcho(({ input }) => Result.ok(input));
      const recordsA = serviceA.kv.records;
      assert(recordsA, "the required records KV must be bound at R1");
      await recordsA.put("pinned", { value: "pinned" });
      assertEquals(await recordsA.get("pinned").orThrow(), { value: "pinned" });
      assert(
        serviceA.kv.extras === undefined,
        "the unapproved optional extras must not be bound",
      );

      const aBefore = await runtime.waitFor(
        async () => (await liveConnection(runtime, instanceA.instanceId)) ?? false,
        { timeoutMs: 30_000 },
      );

      // Advance the deployment to R2, still declining the optional `extras`.
      await runtime.contracts.install({ contract: r2 });
      const requestedR2 = await runtime.contracts.requestApply({
        deployment: DEPLOYMENT,
        contract: r2,
      });
      if (requestedR2.status === "approval_required") {
        await runtime.contracts.approveApply(requestedR2.pendingId, {
          excludeResources: ["extras"],
        });
      }

      // The R1-pinned attachment is still covered by R2 and serves its own
      // declared resources without reconnecting.
      const aAfterAdvance = await runtime.waitFor(
        async () => (await liveConnection(runtime, instanceA.instanceId)) ?? false,
        { timeoutMs: 30_000 },
      );
      assertEquals(aAfterAdvance.connectionId, aBefore.connectionId);
      assertEquals(
        await recordsA.get("pinned").orThrow(),
        { value: "pinned" },
        "the R1-pinned resource keeps working across the revision advance",
      );

      // A new R2 connection uses the R2-only `extended` resource.
      const instanceB = await runtime.services.createInstance({
        deployment: DEPLOYMENT,
        name: "pinned-b",
        contract: r2,
      });
      const serviceB = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: r2,
        name: "pinned-b",
        seed: instanceB.seed,
      }).orThrow();
      cleanups.push(() => serviceB.stop().catch(() => {}));
      const serviceBExit = serviceB.wait().catch((error: unknown) => error);
      cleanups.push(() => serviceBExit.catch(() => {}));
      // The ordinary client transport-lifecycle signal: `wait()` settles when
      // the service's NATS connection actually closes.
      let bTransportClosed = false;
      serviceBExit.then(() => {
        bTransportClosed = true;
      });

      const extended = await runtime.waitFor(
        () => serviceB.kv.extended ?? false,
        { timeoutMs: 60_000 },
      );
      await extended.put("r2", { value: "r2" });
      assertEquals(await extended.get("r2").orThrow(), { value: "r2" });

      // Approve the compatible optional resource. The pinned R1 view must become
      // available from the one current materialization.
      await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: r2 });
      const extrasA = await runtime.waitFor(() => serviceA.kv.extras ?? false, {
        timeoutMs: 60_000,
      });
      const beforeAdoption = await extrasA.get("missing");
      assert(beforeAdoption.isErr());
      await serviceA.connection.refreshTransport().orThrow();
      await extrasA.put("after", { value: "after" });
      assertEquals(await extrasA.get("after").orThrow(), { value: "after" });
      assertEquals(
        await recordsA.get("pinned").orThrow(),
        { value: "pinned" },
        "the earlier resource stays usable after adopting the added one",
      );
      // Adopting the added resource replaces A's admitted socket; capture the
      // new physical attachment to prove the later removal does not disturb it.
      const aAfterGrowth = await runtime.waitFor(
        async () => (await liveConnection(runtime, instanceA.instanceId)) ?? false,
        { timeoutMs: 30_000 },
      );

      // Remove the R2-only resource by returning to R1. The R2-pinned
      // attachment loses its required resource and is retired; the R1-pinned
      // attachment stays.
      await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: r1 });
      await runtime.waitFor(
        async () => (await liveConnection(runtime, instanceB.instanceId)) === undefined,
        { timeoutMs: 30_000 },
      );
      const aFinal = await liveConnection(runtime, instanceA.instanceId);
      assert(aFinal, "the R1-pinned attachment must survive the removal");
      assertEquals(aFinal.connectionId, aAfterGrowth.connectionId);

      // The removal must retire B's actual physical transport, observed through
      // the client lifecycle rather than the presence table alone.
      await runtime.waitFor(() => bTransportClosed, { timeoutMs: 30_000 });

      // The surviving R1-pinned attachment is usable, not merely present.
      await recordsA.put("after-removal", { value: "after-removal" });
      assertEquals(
        await recordsA.get("after-removal").orThrow(),
        { value: "after-removal" },
      );
    } finally {
      for (const cleanup of cleanups) await cleanup();
      await serviceA.stop().catch(() => {});
      await serviceAExit;
    }
  });
});
