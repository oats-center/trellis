import assert from "node:assert/strict";
import { setTimeout } from "node:timers/promises";
import { ulid } from "ulid";
import { Result } from "../../../ts/packages/result/result.ts";
import { loginNativeTestClient } from "../../../ts/packages/trellis-testkit/src/admin/native_auth.ts";
import {
  apps,
  types,
} from "../../../ts/packages/trellis/internal_sdk/generated/index.js";
import { connectNativeClient } from "../../../ts/packages/trellis/native.ts";
import { TrellisService } from "../../../ts/packages/trellis/service/mod.ts";
import { createAuth } from "../../../ts/packages/trellis/auth/session_auth.ts";
import { participants } from "./packages/runtime-trellis/index.js";

const [issuer, username, password, directory] = process.argv.slice(2);
const login = await loginNativeTestClient({
  issuer,
  username,
  password,
  directory,
  clientId: "trellis.cli",
  request: apps.cli.request,
  administrativeCapabilities: ["trellis.auth.v1:users.manage"],
});
const admin = await connectNativeClient({
  ...login,
  request: apps.cli.request,
}).orThrow();
try {
  const participant = participants.OrdinaryJobs.participant;
  const { deployment } = await admin
    .deploymentsCreate({
      title: "History recovery",
      kind: "service",
      participantId: participant.identity,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        participant.packageEvidence,
      ),
      idempotencyKey: ulid(),
    })
    .orThrow();
  const seed = Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString(
    "base64url",
  );
  const identity = await createAuth({ sessionKeySeed: seed });
  const provisioned = await admin
    .serviceInstancesProvision({
      deploymentId: deployment.deploymentId,
      title: "History worker",
      identityPublicKey: identity.sessionKey,
      idempotencyKey: ulid(),
    })
    .orThrow();
  const service = await TrellisService.connect({
    trellisUrl: issuer,
    participant,
    seed,
    telemetry: false,
  }).orThrow();
  try {
    const job = await service.jobs.work
      .create({ value: "retained history" })
      .orThrow();
    service.jobs.work.handle(async ({ job }) => Result.ok(job.payload));
    const deadline = AbortSignal.timeout(30_000);
    let initial = await admin
      .connectionsList({
        principalId: provisioned.instance.principal.principalId,
        state: "admitted",
      })
      .orThrow();
    while (!initial.items.length) {
      await setTimeout(50, undefined, { signal: deadline });
      initial = await admin
        .connectionsList({
          principalId: provisioned.instance.principal.principalId,
          state: "admitted",
        })
        .orThrow();
    }
    await admin
      .connectionsKick({
        attachmentId: initial.items[0].attachmentId,
        idempotencyKey: ulid(),
      })
      .orThrow();
    for (;;) {
      deadline.throwIfAborted();
      const current = await admin
        .connectionsList({
          principalId: provisioned.instance.principal.principalId,
          state: "admitted",
        })
        .orThrow();
      if (
        current.items.some(
          (item) => item.attachmentId !== initial.items[0].attachmentId,
        )
      )
        break;
      await setTimeout(50, undefined, { signal: deadline });
    }
    // Both the reference and the worker were created against the predecessor.
    assert.equal((await job.get().orThrow()).state, "pending");
    const waiting = service.wait();
    assert.deepEqual((await job.wait().orThrow()).result, {
      value: "retained history",
    });
    assert.equal((await job.get().orThrow()).state, "completed");
    await service.stop();
    await waiting;
  } finally {
    await service.stop();
  }
} finally {
  await admin.connection.close();
}
