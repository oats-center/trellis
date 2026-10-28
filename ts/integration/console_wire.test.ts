import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { apis, types } from "../packages/trellis-testkit/trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("generated login-session pagination retains distinct real sign-ins", async () => {
  await withTrellisRuntime(async (runtime) => {
    const read = async () => {
      const sessions: types.AuthLoginSession[] = [];
      let cursor: string | undefined;
      do {
        const page = await runtime.callAdminRpc("authSessionsList", {
          page: { limit: 1, cursor },
        });
        sessions.push(...page.items);
        cursor = page.page.nextCursor;
      } while (cursor);
      assert.equal(
        new Set(sessions.map((item) => item.loginSessionId)).size,
        sessions.length,
      );
      return sessions;
    };
    const before = new Set((await read()).map((item) => item.loginSessionId));
    for (let i = 0; i < 3; i++) {
      const login = await runtime.registerClient({
        name: `paged-sign-in-${i}`,
        contract: apps.Denied.request,
      });
      await login.session.close();
    }
    const created = (await read()).filter(
      (item) => !before.has(item.loginSessionId),
    );
    assert.equal(created.length, 3);
    assert(created.every((item) => item.state === "active"));
  });
});

function revisionConflict(error: unknown): boolean {
  assert(error instanceof apis.auth.AuthError);
  assert.equal(error.data.code, "revision_conflict");
  return true;
}

test("generated deployment mutations preserve replay and reject stale revisions", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.deployments.create({ id: "catalog-source", contract });
    const input = {
      title: "Replay deployment",
      kind: contract.kind,
      participantId: contract.identity,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        contract.packageEvidence,
      ),
      // "catalog-source" already serves this API.
      acknowledgeSharedApis: true,
      idempotencyKey: ulid(),
    };
    const { deployment: created } = await runtime.callAdminRpc(
      "authDeploymentsCreate",
      input,
    );
    const { deployment: replayed } = await runtime.callAdminRpc(
      "authDeploymentsCreate",
      input,
    );
    assert.deepEqual(replayed, created);
    const { deployment: fetched } = await runtime.callAdminRpc(
      "authDeploymentsGet",
      {
        deploymentId: created.deploymentId,
      },
    );
    assert.deepEqual(fetched, created);
    await assert.rejects(
      () =>
        runtime.callAdminRpc("authDeploymentsCreate", {
          ...input,
          title: "Changed intent",
        }),
      revisionConflict,
    );
    await runtime.callAdminRpc("authDeploymentsDisable", {
      deploymentId: created.deploymentId,
      expectedRevision: created.revision,
      idempotencyKey: ulid(),
    });
    await assert.rejects(
      () =>
        runtime.callAdminRpc("authDeploymentsEnable", {
          deploymentId: created.deploymentId,
          expectedRevision: created.revision,
          idempotencyKey: ulid(),
        }),
      revisionConflict,
    );
    const { deployment: disabled } = await runtime.callAdminRpc(
      "authDeploymentsGet",
      {
        deploymentId: created.deploymentId,
      },
    );
    assert.equal(disabled.state, "disabled");
    assert(disabled.revision > created.revision);
  });
});

test("generated user creation replays the original account and rejects changed intent", async () => {
  await withTrellisRuntime(async (runtime) => {
    const input = {
      username: `replay-${ulid()}`,
      displayName: "Replay User",
      idempotencyKey: ulid(),
    };
    const created = await runtime.callAdminRpc("authUsersCreate", input);
    const replayed = await runtime.callAdminRpc("authUsersCreate", input);
    assert.deepEqual(replayed.user, created.user);
    await assert.rejects(
      () =>
        runtime.callAdminRpc("authUsersCreate", {
          ...input,
          displayName: "Changed intent",
        }),
      revisionConflict,
    );
    assert.deepEqual(
      (await runtime.callAdminRpc("authUsersCreate", input)).user,
      created.user,
    );
  });
});

test("generated Auth errors survive the real missing-deployment response", async () => {
  await withTrellisRuntime(async (runtime) => {
    await assert.rejects(
      () =>
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: codecs.ulid.decode(ulid()),
        }),
      (error: unknown) => {
        assert(error instanceof apis.auth.AuthError);
        assert.equal(error.data.code, "not_found");
        const roundTrip = apis.auth.AuthError.fromSerializable(
          error.toSerializable(),
        );
        assert.equal(roundTrip.data.code, error.data.code);
        assert.equal(roundTrip.id, error.id);
        return true;
      },
    );
  });
});
