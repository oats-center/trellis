import assert from "node:assert/strict";
import { codecs } from "@oatscenter/trellis/generated";
import { ulid } from "ulid";
import { API as authApi } from "../packages/trellis-testkit/trellis/apis/auth/mod.js";
import { test } from "node:test";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("resource evidence is paginated, current, and isolated by Jobs/Events capability", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.registerService({
      name: "evidence-owner",
      contract: participants.Provider.participant,
    });
    const jobs = await runtime.connectClient({
      name: "job-evidence",
      contract: apps.JobEvidenceReader.request,
    });
    const events = await runtime.connectClient({
      name: "event-evidence",
      contract: apps.EventEvidenceReader.request,
    });
    try {
      const first = await jobs
        .resourceBindingsQuery({
          kind: "jobQueue",
          ownerId: codecs.ulid.decode(key.deploymentId),
          page: { limit: 1 },
        })
        .orThrow();
      assert.equal(first.items.length, 1);
      assert(first.page.nextCursor);
      assert(first.items[0].job?.publishPrefix);
      assert(first.items[0].resourceBindingsDigest);
      const second = await jobs
        .resourceBindingsQuery({
          kind: "jobQueue",
          ownerId: codecs.ulid.decode(key.deploymentId),
          page: { limit: 1, cursor: first.page.nextCursor },
        })
        .orThrow();
      assert.equal(second.items.length, 1);
      assert.notEqual(second.items[0].resourceId, first.items[0].resourceId);
      const consumer = await events
        .resourceBindingsQuery({
          kind: "eventConsumer",
          ownerId: codecs.ulid.decode(key.deploymentId),
        })
        .orThrow();
      assert.equal(consumer.items.length, 1);
      assert.equal(consumer.items[0].localName, "changes");
      assert(consumer.items[0].eventConsumer?.consumer);
      assert.equal(
        consumer.items[0].resourceBindingsDigest,
        first.items[0].resourceBindingsDigest,
      );
      assert.equal(
        (await jobs.resourceBindingsQuery({ kind: "eventConsumer" })).isErr(),
        true,
      );
      assert.equal(
        (await events.resourceBindingsQuery({ kind: "jobQueue" })).isErr(),
        true,
      );
      const { deployment } = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: codecs.ulid.decode(key.deploymentId),
      });
      await runtime.callAdminRpc(
        "authDeploymentsDisable",
        authApi.actions["rpc:Deployments.Disable"].input.decode({
          deploymentId: key.deploymentId,
          expectedRevision: deployment.revision.toString(),
          idempotencyKey: ulid(),
        }),
      );
      const disabled = await jobs
        .resourceBindingsQuery({
          kind: "jobQueue",
          ownerId: codecs.ulid.decode(key.deploymentId),
        })
        .orThrow();
      assert(
        disabled.items.length > 0,
        "retained evidence must remain discoverable",
      );
      assert(
        disabled.items.every((row) => row.resourceBindingsDigest === undefined),
        "disabled deployment evidence cannot authorize current resource use",
      );
    } finally {
      await jobs.connection.close();
      await events.connection.close();
    }
  });
});
