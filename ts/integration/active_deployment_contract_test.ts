/**
 * Live acceptance for contract replacement on an active deployment.
 *
 * Applying a changed contract to a running deployment must not require a
 * disable/apply/enable cycle, and it must disconnect only the physical
 * connections whose effective authority the replacement no longer covers.
 * Deployment lifecycle invalidation (disable) must still terminate every native
 * connection that belongs to the deployment.
 */

import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const DEPLOYMENT = "active-apply";

/** Current live presence entries for one deployment. */
async function liveConnections(
  runtime: TrellisTestRuntime,
  deploymentId: string,
) {
  const listed = await runtime.callAdminRpc("authConnectionsList", {
    page: { limit: 100 },
  });
  return listed.items.filter((item) => item.deploymentId === deploymentId);
}

/** Waits until a native instance has a live physical connection. */
function waitForConnection(
  runtime: TrellisTestRuntime,
  deploymentId: string,
  instanceId: string,
) {
  return runtime.waitFor(async () => {
    const connection = (await liveConnections(runtime, deploymentId))
      .find((item) => item.instanceId === instanceId);
    return connection ?? false;
  }, { timeoutMs: 30_000 });
}

Deno.test("active contract replacement keeps covered connections and drops uncovered ones", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.deployments.create({ id: DEPLOYMENT, kind: "service" });

    // Stage A: a narrow provider context from the removed fixture.
    await runtime.contracts.apply({
      deployment: DEPLOYMENT,
      contract: removedParticipants.Provider.participant,
    });
    const instanceA = await runtime.services.createInstance({
      deployment: DEPLOYMENT,
      name: "provider-a",
      contract: removedParticipants.Provider.participant,
    });
    const serviceA = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: removedParticipants.Provider.participant,
      name: "provider-a",
      seed: instanceA.seed,
    }).orThrow();
    const serviceAExit = serviceA.wait().catch((error: unknown) => error);
    await serviceA.handleEcho(({ input }) => Result.ok(input));

    const caller = await runtime.connectClient({
      name: "active-apply-caller",
      contract: removedParticipants.Caller.participant,
    });

    const cleanups: Array<() => Promise<unknown>> = [];
    try {
      const aBefore = await waitForConnection(
        runtime,
        instanceA.deploymentId,
        instanceA.instanceId,
      );
      assertEquals(
        (await caller.echo({ value: "before" }).orThrow()).value,
        "before",
      );

      // Stage B: additive replacement while the deployment stays active.
      await runtime.contracts.apply({
        deployment: DEPLOYMENT,
        contract: participants.Provider.participant,
      });

      const aAfter = await waitForConnection(
        runtime,
        instanceA.deploymentId,
        instanceA.instanceId,
      );
      assertEquals(aAfter.connectionId, aBefore.connectionId);
      assertEquals(aAfter.runtimeConnectionId, aBefore.runtimeConnectionId);
      assertEquals(
        (await caller.echo({ value: "after-additive" }).orThrow()).value,
        "after-additive",
      );

      // Stage C: a broader context issued after the replacement.
      const instanceB = await runtime.services.createInstance({
        deployment: DEPLOYMENT,
        name: "provider-b",
        contract: participants.Provider.participant,
      });
      const serviceB = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: "provider-b",
        seed: instanceB.seed,
      }).orThrow();
      cleanups.push(() => serviceB.stop().catch(() => {}));
      const serviceBExit = serviceB.wait().catch((error: unknown) => error);
      cleanups.push(() => serviceBExit.catch(() => {}));
      await serviceB.handleEcho(({ input }) => Result.ok(input));
      const bBefore = await waitForConnection(
        runtime,
        instanceB.deploymentId,
        instanceB.instanceId,
      );

      // Stage D: authority reduction while still active. The narrower context
      // survives; the broader one loses its physical connection.
      await runtime.contracts.apply({
        deployment: DEPLOYMENT,
        contract: removedParticipants.Provider.participant,
      });

      const aFinal = await waitForConnection(
        runtime,
        instanceA.deploymentId,
        instanceA.instanceId,
      );
      assertEquals(aFinal.connectionId, aBefore.connectionId);
      assertEquals(aFinal.runtimeConnectionId, aBefore.runtimeConnectionId);
      assertEquals(
        (await caller.echo({ value: "after-reduction" }).orThrow()).value,
        "after-reduction",
      );
      // The stale physical attachment carrying excess authority must be gone.
      // The logical runtime connection may legitimately return under the
      // reduced authority with a new physical connection, so assert on the
      // physical identity, not the logical one.
      await runtime.waitFor(
        async () =>
          (await liveConnections(runtime, instanceB.deploymentId)).every(
            (item) => item.connectionId !== bBefore.connectionId,
          ),
        { timeoutMs: 30_000 },
      );

      await serviceB.stop().catch(() => {});
      await serviceBExit.catch(() => {});

      // Deployment disable still terminates every native connection that
      // belonged to the deployment, and new native bootstrap is denied.
      const current = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: instanceA.deploymentId,
      });
      await runtime.callAdminRpc("authDeploymentsDisable", {
        deploymentId: instanceA.deploymentId,
        expectedVersion: current.deployment.version,
        idempotencyKey: crypto.randomUUID(),
        reason: null,
      });
      await runtime.waitFor(
        async () =>
          (await liveConnections(runtime, instanceA.deploymentId)).length === 0,
        {
          timeoutMs: 30_000,
        },
      );

      const denied = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: removedParticipants.Provider.participant,
        name: "provider-a",
        seed: instanceA.seed,
      });
      assert(denied.isErr());

      // Deployment enable restores authority in place: the same seed A
      // bootstraps the same instance and serves RPC without re-applying the
      // contract.
      const disabledRevision = await runtime.callAdminRpc(
        "authDeploymentsGet",
        { deploymentId: instanceA.deploymentId },
      );
      await runtime.callAdminRpc("authDeploymentsEnable", {
        deploymentId: instanceA.deploymentId,
        expectedVersion: disabledRevision.deployment.version,
        idempotencyKey: crypto.randomUUID(),
        reason: null,
      });
      const serviceAEnabled = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: removedParticipants.Provider.participant,
        name: "provider-a",
        seed: instanceA.seed,
      }).orThrow();
      cleanups.push(() => serviceAEnabled.stop().catch(() => {}));
      const serviceAEnabledExit = serviceAEnabled.wait().catch(
        (error: unknown) => error,
      );
      cleanups.push(() => serviceAEnabledExit.catch(() => {}));
      await serviceAEnabled.handleEcho(({ input }) => Result.ok(input));
      const enabledConnection = await waitForConnection(
        runtime,
        instanceA.deploymentId,
        instanceA.instanceId,
      );
      assertEquals(enabledConnection.instanceId, instanceA.instanceId);
      assertEquals(
        (await caller.echo({ value: "after-enable" }).orThrow()).value,
        "after-enable",
      );
    } finally {
      for (const cleanup of cleanups) await cleanup();
      await caller.connection.close();
      await serviceA.stop().catch(() => {});
      await serviceAExit.catch(() => {});
    }
  });
});
