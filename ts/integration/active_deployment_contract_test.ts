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
import assert from "node:assert/strict";
import { test } from "node:test";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  apps as removedApps,
  participants as removedParticipants,
} from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const DEPLOYMENT = "active-apply";

/** Current live presence entries for one deployment. */
async function liveConnections(
  runtime: TrellisTestRuntime,
  deploymentId: string,
) {
  const listed = await runtime.callAdminRpc("authConnectionsList", {
    state: "admitted",
    page: { limit: 100 },
  });
  const sessions = await runtime.callAdminRpc("authAuthorizationSessionsList", {
    deploymentId: codecs.ulid.decode(deploymentId),
    page: { limit: 100 },
  });
  return listed.items.flatMap((attachment) => {
    const session = sessions.items.find(
      (item) =>
        item.authorizationSessionId === attachment.authorizationSessionId,
    );
    return session
      ? [{ ...attachment, instanceId: session.binding.instanceId }]
      : [];
  });
}

/** Waits until a native instance has a live physical connection. */
function waitForConnection(
  runtime: TrellisTestRuntime,
  deploymentId: string,
  instanceId: string,
) {
  return runtime.waitFor(
    async () => {
      const connection = (await liveConnections(runtime, deploymentId)).find(
        (item) => item.instanceId === instanceId,
      );
      return connection ?? false;
    },
    { timeoutMs: 30_000 },
  );
}

test("active contract replacement keeps covered connections and drops uncovered ones", async (context) => {
  await withTrellisRuntime(
    async (runtime) => {
      await runtime.deployments.create({
        id: DEPLOYMENT,
        contract: removedParticipants.Provider.participant,
      });

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
        contract: removedApps.Caller.request,
      });

      const cleanups: Array<() => Promise<unknown>> = [];
      try {
        await context.test(
          "compatible rolling upgrade, original-seed reconnect, and selective retirement preserve RPC and administration",
          async () => {
            const aBefore = await waitForConnection(
              runtime,
              instanceA.deploymentId,
              instanceA.instanceId,
            );
            assert.deepEqual(
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
            assert.deepEqual(aAfter.attachmentId, aBefore.attachmentId);
            assert.deepEqual(
              (await caller.echo({ value: "after-additive" }).orThrow()).value,
              "after-additive",
            );

            const reconnected = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: removedParticipants.Provider.participant,
              name: "provider-a-reconnected",
              seed: instanceA.seed,
            }).orThrow();
            await reconnected.handleEcho(({ input }) => Result.ok(input));
            const reconnectedExit = reconnected.wait();
            try {
              assert.equal(
                (await caller.echo({ value: "old-seed-reconnected" }).orThrow())
                  .value,
                "old-seed-reconnected",
              );
            } finally {
              await reconnected.stop();
              await reconnectedExit;
            }

            // Stage C: a broader context issued after the replacement.
            const instanceB = await runtime.services.createInstance({
              deployment: DEPLOYMENT,
              name: "provider-b",
              contract: participants.Provider.participant,
            });
            const gate = runtime.nativeTransportGate();
            const existingConnections = new Set(
              gate.connections().map(({ id }) => id),
            );
            const serviceB = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: participants.Provider.participant,
              name: "provider-b",
              seed: instanceB.seed,
            }).orThrow();
            cleanups.push(() => serviceB.stop().catch(() => {}));
            const serviceBExit = serviceB
              .wait()
              .catch((error: unknown) => error);
            cleanups.push(() => serviceBExit.catch(() => {}));
            await serviceB.handleEcho(({ input }) => Result.ok(input));
            const bTransport = await runtime.waitFor(
              () =>
                gate
                  .connections()
                  .find(
                    ({ id, closed, subs }) =>
                      !existingConnections.has(id) &&
                      !closed &&
                      subs.some(
                        ({ subject }) => subject === "rpc.v1.runtime.Echo",
                      ),
                  ) ?? false,
            );
            await waitForConnection(
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

            // Apply commits retirement intent; the broker kick is asynchronous.
            // A canonical request sent before B's socket closes can reach B and
            // lose its reply when revocation fences it. Observe the actual close
            // before proving fresh routing through the surviving narrow provider.
            await runtime.waitFor(() => gate.isClosed(bTransport.id), {
              timeoutMs: 30_000,
            });
            assert.equal(
              (
                await caller
                  .echo({ value: "after-removal-before-admin" })
                  .orThrow()
              ).value,
              "after-removal-before-admin",
            );
            await runtime.callAdminRpc("authDeploymentsGet", {
              deploymentId: codecs.ulid.decode(instanceA.deploymentId),
            });
            await liveConnections(runtime, instanceA.deploymentId);

            const aFinal = await waitForConnection(
              runtime,
              instanceA.deploymentId,
              instanceA.instanceId,
            );
            assert.deepEqual(aFinal.attachmentId, aBefore.attachmentId);
            assert.deepEqual(
              (await caller.echo({ value: "after-reduction" }).orThrow()).value,
              "after-reduction",
            );
            // Instance B is pinned to the broad revision whose required
            // `eventConsumer:changes` resource no longer exists under present
            // deployment policy. A broad binary is never downgraded into an
            // incomplete narrow authority, so its attachment is retired and it stays
            // unissuable until it actually becomes the narrow revision.
            await runtime.waitFor(
              async () =>
                (await liveConnections(runtime, instanceB.deploymentId)).every(
                  (item) => item.instanceId !== instanceB.instanceId,
                ),
              { timeoutMs: 30_000 },
            );

            await serviceB.stop().catch(() => {});
            await serviceBExit.catch(() => {});
          },
        );

        // Deployment disable still terminates every native connection that
        // belonged to the deployment, and new native bootstrap is denied.
        const current = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: codecs.ulid.decode(instanceA.deploymentId),
        });
        await runtime.callAdminRpc("authDeploymentsDisable", {
          deploymentId: codecs.ulid.decode(instanceA.deploymentId),
          expectedRevision: current.deployment.revision,
          idempotencyKey: ulid(),
        });
        await runtime.waitFor(
          async () =>
            (await liveConnections(runtime, instanceA.deploymentId)).length ===
            0,
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
          { deploymentId: codecs.ulid.decode(instanceA.deploymentId) },
        );
        await runtime.callAdminRpc("authDeploymentsEnable", {
          deploymentId: codecs.ulid.decode(instanceA.deploymentId),
          expectedRevision: disabledRevision.deployment.revision,
          idempotencyKey: ulid(),
        });
        const serviceAEnabled = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: removedParticipants.Provider.participant,
          name: "provider-a",
          seed: instanceA.seed,
        }).orThrow();
        cleanups.push(() => serviceAEnabled.stop().catch(() => {}));
        const serviceAEnabledExit = serviceAEnabled
          .wait()
          .catch((error: unknown) => error);
        cleanups.push(() => serviceAEnabledExit.catch(() => {}));
        await serviceAEnabled.handleEcho(({ input }) => Result.ok(input));
        const enabledConnection = await waitForConnection(
          runtime,
          instanceA.deploymentId,
          instanceA.instanceId,
        );
        assert.deepEqual(enabledConnection.instanceId, instanceA.instanceId);
        assert.deepEqual(
          (await caller.echo({ value: "after-enable" }).orThrow()).value,
          "after-enable",
        );
      } finally {
        for (const cleanup of cleanups) await cleanup();
        await caller.connection.close();
        await serviceA.stop().catch(() => {});
        await serviceAExit.catch(() => {});
      }
    },
    { interruptibleNativeProxy: true },
  );
});
