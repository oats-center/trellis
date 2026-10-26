/**
 * Live acceptance for service-submitted participant updates.
 *
 * A running service presents its own build. A build that asks for a resource
 * the deployment never had waits for an administrator; a build that only gives
 * access up installs at once; declaring a dropped resource again waits too and
 * reattaches its retained data; starting to provide an API waits for approval;
 * and answering an API another service serves also needs the administrator to
 * acknowledge that requests will be split between them.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

const DEPLOYMENT = "service-updates";

test("a service update waits for approval only when it asks for new access", async () => {
  await withTrellisRuntime(async (runtime) => {
    const base = participants.Provider.participant;
    const extended = extendedParticipants.Provider.participant;
    await runtime.deployments.create({ id: DEPLOYMENT, contract: base });
    await runtime.contracts.apply({ deployment: DEPLOYMENT, contract: base });
    const instance = await runtime.services.createInstance({
      name: "service-updates",
      deployment: DEPLOYMENT,
      contract: base,
    });
    const connect = <P extends typeof base | typeof extended>(participant: P) =>
      TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant,
        seed: instance.seed,
      }).orThrow();

    // The extended build declares a KV the deployment never had.
    const waiting = connect(extended);
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(pending.changes.addedResources, [
      { kind: "kv", name: "extended" },
    ]);
    assert.deepEqual(pending.changes.addedCapabilities, []);
    assert.equal(
      pending.reviewUrl,
      `${runtime.trellisUrl}/console/admin/services/${pending.deploymentId}?update=${pending.updateId}`,
    );
    const { update: approved } = await runtime.callAdminRpc(
      "authServiceUpdatesDecide",
      {
        updateId: pending.updateId,
        decision: "approved",
        expectedRevision: pending.revision,
        idempotencyKey: ulid(),
      },
    );
    assert.equal(approved.state, "applied");
    const first = await waiting;
    await first.kv.extended.put("kept", { value: "before rollback" }).orThrow();
    await first.kv.records.put("record", { value: "durable" }).orThrow();
    await first.stop();

    // Rolling back only drops the KV, so it installs without an administrator.
    const rolledBack = await connect(base);
    assert.deepEqual(await rolledBack.kv.records.get("record").orThrow(), {
      value: "durable",
    });
    await rolledBack.stop();
    const { items } = await runtime.callAdminRpc("authServiceUpdatesList", {});
    const rollback = items[0];
    assert(rollback);
    assert.equal(rollback.state, "applied");
    assert.deepEqual(rollback.changes.removedResources, [
      { kind: "kv", name: "extended" },
    ]);

    // The fix declares the KV again: reattaching it waits for approval too,
    // and then brings its retained data back.
    const reattaching = connect(extended);
    const reattach = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(reattach.changes.reattachedResources, [
      { kind: "kv", name: "extended" },
    ]);
    assert.deepEqual(reattach.changes.addedResources, []);
    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: reattach.updateId,
      decision: "approved",
      expectedRevision: reattach.revision,
      idempotencyKey: ulid(),
    });
    const fixed = await reattaching;
    assert.deepEqual(await fixed.kv.extended.get("kept").orThrow(), {
      value: "before rollback",
    });
    await fixed.stop();
  });
});

test("a Rust service waits for its update's approval, then connects under it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const base = participants.Provider.participant;
    await runtime.deployments.create({
      id: "rust-service-update",
      contract: base,
    });
    await runtime.contracts.apply({
      deployment: "rust-service-update",
      contract: base,
    });
    const instance = await runtime.services.createInstance({
      name: "rust-service-update",
      deployment: "rust-service-update",
      contract: base,
    });
    const argv = rustFixtureArgv("service_update");
    const child = spawnTracked(argv[0], argv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.oauthIssuer,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: instance.seed,
      },
    });
    let output = "";
    let errors = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (errors += chunk.toString()));
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(pending.changes.addedResources, [
      { kind: "kv", name: "extended" },
    ]);
    // Waiting, it shows the administrator where to approve it.
    await runtime.waitFor(() => errors.includes(pending.reviewUrl));
    assert(!output.includes("service update connected"), output);
    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: pending.updateId,
      decision: "approved",
      expectedRevision: pending.revision,
      idempotencyKey: ulid(),
    });
    const exit = await child.exited;
    assert.equal(exit.code, 0, errors);
    assert.match(output, /service update stored approved/);
  });
});

test("providing a new API waits for approval; sharing one another service serves needs an acknowledgement", async () => {
  await withTrellisRuntime(async (runtime) => {
    // Another deployment serves the runtime API.
    await runtime.deployments.create({
      id: "runtime-owner",
      contract: participants.EchoProvider.participant,
    });
    await runtime.contracts.apply({
      deployment: "runtime-owner",
      contract: participants.EchoProvider.participant,
    });
    const resources = participants.ResourceProvider.participant;
    await runtime.deployments.create({ id: "takeover", contract: resources });
    await runtime.contracts.apply({
      deployment: "takeover",
      contract: resources,
    });
    const takeover = await runtime.services.createInstance({
      name: "takeover",
      deployment: "takeover",
      contract: resources,
    });
    const sharing = TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: extendedParticipants.ResourceProvider.participant,
      seed: takeover.seed,
    }).orThrow();
    const shared = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(shared.changes.sharedApis, ["runtime-trellis.runtime@v1"]);
    // Approving needs the explicit acknowledgement that requests get split.
    await assert.rejects(
      runtime.callAdminRpc("authServiceUpdatesDecide", {
        updateId: shared.updateId,
        decision: "approved",
        expectedRevision: shared.revision,
        idempotencyKey: ulid(),
      }),
      /callers' requests would be split/,
    );
    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: shared.updateId,
      decision: "approved",
      acknowledgeSharedApis: true,
      expectedRevision: shared.revision,
      idempotencyKey: ulid(),
    });
    await (await sharing).stop();

    // A brand-new API is new access: it waits, then installs on approval.
    const jobs = participants.OrdinaryJobs.participant;
    await runtime.deployments.create({ id: "new-api", contract: jobs });
    await runtime.contracts.apply({ deployment: "new-api", contract: jobs });
    const instance = await runtime.services.createInstance({
      name: "new-api",
      deployment: "new-api",
      contract: jobs,
    });
    const connecting = TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: extendedParticipants.OrdinaryJobs.participant,
      seed: instance.seed,
    }).orThrow();
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(pending.changes.addedApis, ["runtime-trellis.added@v1"]);
    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: pending.updateId,
      decision: "approved",
      expectedRevision: pending.revision,
      idempotencyKey: ulid(),
    });
    const service = await connecting;
    await service.stop();
  });
});
