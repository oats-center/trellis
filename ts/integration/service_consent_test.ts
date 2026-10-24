import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { types } from "../packages/trellis-testkit/trellis/index.js";

import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("service provisioning requires an installed deployment and admits its identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    await assert.rejects(() => runtime.services.provisionInstanceOnly({}));
    const identity = await runtime.registerService({
      name: "provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.Provider.participant,
      name: "provider",
      seed: identity.seed,
    }).orThrow();
    const caller = await runtime.connectClient({
      name: "provisioned-identity-caller",
      contract: apps.Caller.request,
    });
    try {
      await service.handleEcho(({ input }) => Result.ok(input));
      assert.deepEqual(await caller.echo({ value: "admitted" }).orThrow(), {
        value: "admitted",
      });
    } finally {
      await caller.connection.close();
      await service.stop();
    }
  });
});

test("a stale deployment apply cannot overwrite the accepted implementation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const created = await runtime.callAdminRpc("authDeploymentsCreate", {
      title: "concurrent deployment apply",
      kind: "service",
      participantId: contract.identity,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        contract.packageEvidence,
      ),
      idempotencyKey: ulid(),
    });
    const accepted = await runtime.callAdminRpc("authDeploymentsApply", {
      deploymentId: created.deployment.deploymentId,
      expectedRevision: created.deployment.revision,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        removedParticipants.Provider.participant.packageEvidence,
      ),
      idempotencyKey: ulid(),
    });
    await assert.rejects(() =>
      runtime.callAdminRpc("authDeploymentsApply", {
        deploymentId: created.deployment.deploymentId,
        expectedRevision: created.deployment.revision,
        packageEvidence: types.AuthPackageEvidenceCodec.decode(
          contract.packageEvidence,
        ),
        idempotencyKey: ulid(),
      }),
    );
    const current = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: created.deployment.deploymentId,
    });
    assert.deepEqual(current.deployment, accepted.deployment);
  });
});
