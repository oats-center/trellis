/**
 * Live acceptance for service-submitted participant updates.
 *
 * A running service presents its own build. A build that asks for a resource
 * the deployment never had waits for an administrator; a build that only gives
 * access up installs at once; declaring a dropped resource again waits too and
 * reattaches its retained data; starting to provide an API waits for approval;
 * and answering an API another service serves also needs the administrator to
 * acknowledge that requests will be split between them. A service claim takes
 * whatever service first connects with its identity; approving that service
 * creates its deployment and accepts the APIs it provides, and the APIs it uses
 * that no provider has brought yet, so services can be added in any order.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { test } from "node:test";
import nacl from "tweetnacl";
import { ulid } from "ulid";

import { participants as breakingProviders } from "../../integration/fixtures/client-upgrade-breaking/packages/client-upgrade/index.js";
import { participants as upgradeProviders } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { participants as extendedParticipants } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { participants as majorTwo } from "../../integration/fixtures/runtime/v2/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { types } from "../packages/trellis-testkit/trellis/index.js";
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
        trellisUrl: runtime.trellisUrl,
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
      `${runtime.consoleUrl}/console/admin/services/${pending.deploymentId}?update=${pending.updateId}`,
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

test("a build that implements a force-replaced API connects and installs itself", async () => {
  await withTrellisRuntime(async (runtime) => {
    const base = upgradeProviders.Provider.participant;
    const breaking = breakingProviders.Provider.participant;
    await runtime.deployments.create({ id: "replaced", contract: base });
    await runtime.contracts.apply({ deployment: "replaced", contract: base });
    const instance = await runtime.services.createInstance({
      name: "replaced",
      deployment: "replaced",
      contract: base,
    });
    const second = await runtime.services.createInstance({
      name: "replaced-second",
      deployment: "replaced",
      contract: base,
    });
    const connect = <P extends typeof base | typeof breaking>(
      participant: P,
      seed = instance.seed,
    ) =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed,
      }).orThrow();
    await (await connect(base)).stop();
    await (await connect(base, second.seed)).stop();

    // An administrator replaces the API the deployment provides with a
    // definition its running build no longer matches.
    const { review } = await runtime.callAdminRpc("authApisReview", {
      apiId: breaking.implements[0].identity,
      packageEvidence: types.AuthPackageEvidenceCodec.decode(
        breaking.packageEvidence,
      ),
    });
    assert.equal(review.compatible, false);
    await runtime.callAdminRpc("authApisForceReplace", {
      reviewId: review.reviewId,
      canonicalDigest: review.canonicalDigest,
      expectedGeneration: review.currentGeneration ?? 0n,
      expectedAcceptedRevision: review.currentAcceptedRevision ?? 0n,
      acknowledgeBreakage: true,
      acknowledgeConsentImpact: true,
      roleEdits: [],
      mappingEdits: [],
      idempotencyKey: ulid(),
    });

    // The build that implements the replacement asks for nothing new, so it
    // installs itself without an administrator.
    const updated = await connect(breaking);
    await updated.stop();
    const { items } = await runtime.callAdminRpc("authServiceUpdatesList", {});
    assert.equal(items[0]?.state, "applied");
    // From then on it connects under its own installed revision.
    await (await connect(breaking)).stop();

    // The rollout's next replica is still pinned to the replaced build, though
    // the deployment is already current. It gets the same way through.
    await (await connect(breaking, second.seed)).stop();
    await (await connect(breaking, second.seed)).stop();
    await (await connect(breaking)).stop();
  });
});

test("a build of an API's next major connects once its package is applied and moves its instance onto it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const v1 = participants.DocumentsProvider.participant;
    const v2 = majorTwo.DocumentsProvider.participant;
    await runtime.deployments.create({ id: "major", contract: v1 });
    await runtime.contracts.apply({ deployment: "major", contract: v1 });
    const instance = await runtime.services.createInstance({
      name: "major",
      deployment: "major",
      contract: v1,
    });
    const connect = <P extends typeof v1 | typeof v2>(participant: P) =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: instance.seed,
      }).orThrow();
    await (await connect(v1)).stop();

    // The administrator installs the v2 package first. The instance is still
    // pinned to the v1 build, whose API major the deployment no longer
    // provides, yet the v2 build connects and installs itself.
    await runtime.contracts.apply({ deployment: "major", contract: v2 });
    await (await connect(v2)).stop();
    const { items } = await runtime.callAdminRpc("authServiceUpdatesList", {});
    assert.equal(items[0]?.state, "applied");
    await (await connect(v2)).stop();
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
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: instance.seed,
      },
    });
    let output = "";
    let errors = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (errors += chunk.toString()));
    try {
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
      const reviewUrl = pending.reviewUrl;
      assert(reviewUrl);
      await runtime.waitFor(() => errors.includes(reviewUrl));
      assert(!output.includes("service update connected"), output);
      await runtime.callAdminRpc("authServiceUpdatesDecide", {
        updateId: pending.updateId,
        decision: "approved",
        expectedRevision: pending.revision,
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(() => output.includes("service update stored"));
      assert.match(output, /service update stored approved/);

      // The administrator then applies a package this build no longer fits.
      // Readmitted only to submit a build, the running service stops instead of
      // lingering without authority.
      await runtime.contracts.apply({
        deployment: "rust-service-update",
        contract: removedParticipants.Provider.participant,
      });
      const exit = await child.exited;
      assert.equal(exit.code, 0, errors);
      assert.match(
        output,
        /service stopped: this build is no longer installed for its instance/,
      );
    } finally {
      child.kill();
    }
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
      trellisUrl: runtime.trellisUrl,
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
      trellisUrl: runtime.trellisUrl,
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

test("an update keeps an administrator's revocation and grants what new resources imply", async () => {
  await withTrellisRuntime(async (runtime) => {
    const keeper = participants.GrantKeeper.participant;
    await runtime.deployments.create({ id: "grant-keeper", contract: keeper });
    await runtime.contracts.apply({
      deployment: "grant-keeper",
      contract: keeper,
    });
    const instance = await runtime.services.createInstance({
      name: "grant-keeper",
      deployment: "grant-keeper",
      contract: keeper,
    });
    const principalId = (
      await runtime.callAdminRpc("authServiceInstancesList", {})
    ).items.find((item) => String(item.instanceId) === instance.instanceId)
      ?.principal.principalId;
    assert(principalId);
    const grantsOf = async () =>
      (await runtime.callAdminRpc("authCapabilityGrantsList", { principalId }))
        .items;
    const read = (await grantsOf()).find(
      (grant) => grant.capability.capabilityId === "trellis.auth.v1:users.read",
    );
    assert(read);
    await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
      grantId: read.grantId,
      expectedRevision: read.revision,
      idempotencyKey: ulid(),
    });

    // A narrower build keeps users.read and drops bindings.readJobs: it asks
    // for nothing new, so it installs, and the revocation stays in force.
    const narrower = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: extendedParticipants.GrantKeeper.participant,
      seed: instance.seed,
    }).orThrow();
    await narrower.stop();
    const held = (await grantsOf()).map(
      (grant) => grant.capability.capabilityId,
    );
    assert(!held.includes("trellis.auth.v1:users.read"), held.join(", "));
    assert(
      !held.includes("trellis.auth.v1:bindings.readJobs"),
      held.join(", "),
    );

    // A build that adds the service's first job queue gets the job history
    // right that queue implies, so it can connect under the new revision.
    const echo = participants.EchoProvider.participant;
    await runtime.deployments.create({ id: "first-job", contract: echo });
    await runtime.contracts.apply({ deployment: "first-job", contract: echo });
    const jobs = await runtime.services.createInstance({
      name: "first-job",
      deployment: "first-job",
      contract: echo,
    });
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: extendedParticipants.EchoProvider.participant,
      seed: jobs.seed,
    }).orThrow();
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find((update) => update.state === "pending");
    });
    assert.deepEqual(pending.changes.addedResources, [
      { kind: "job", name: "work" },
    ]);
    await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: pending.updateId,
      decision: "approved",
      expectedRevision: pending.revision,
      idempotencyKey: ulid(),
    });
    await (await connecting).stop();
  });
});

test("a service claim takes whatever service connects first, once an administrator approves it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = () => {
      const seed = randomBytes(32);
      return {
        seed: seed.toString("base64url"),
        identityPublicKey: Buffer.from(
          nacl.sign.keyPair.fromSeed(seed).publicKey,
        ).toString("base64url"),
      };
    };
    const claimed = identity();
    const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
      title: "third-party",
      instanceTitle: "third-party",
      identityPublicKey: claimed.identityPublicKey,
      idempotencyKey: ulid(),
    });
    assert.equal(claim.state, "waiting");

    // Nothing about the service is known beforehand: it introduces itself.
    const participant = participants.Provider.participant;
    const apiId = participant.implements[0].identity;
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      seed: claimed.seed,
    }).orThrow();
    const pending = await runtime.waitFor(async () => {
      const { items } = await runtime.callAdminRpc(
        "authServiceUpdatesList",
        {},
      );
      return items.find(
        (update) =>
          update.deploymentId === claim.claimId && update.state === "pending",
      );
    });
    assert.equal(pending.participantId, participant.identity);
    assert.deepEqual(pending.changes.addedApis, [apiId]);

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
    assert.notEqual(approved.deploymentId, claim.claimId);
    // The service connects as an instance of the new deployment, serving the
    // API its approval accepted, with the storage its IDL declares.
    const service = await connecting;
    await service.kv.records.put("record", { value: "claimed" }).orThrow();
    assert.deepEqual(await service.kv.records.get("record").orThrow(), {
      value: "claimed",
    });
    await service.stop();
    const { items: claims } = await runtime.callAdminRpc(
      "authServiceClaimsList",
      {},
    );
    const finished = claims.find((item) => item.claimId === claim.claimId);
    assert.equal(finished?.state, "claimed");
    assert.equal(finished?.deploymentId, approved.deploymentId);

    // A removed claim turns its identity away.
    const unwanted = identity();
    const { claim: removable } = await runtime.callAdminRpc(
      "authServiceClaimsCreate",
      {
        title: "unwanted",
        instanceTitle: "unwanted",
        identityPublicKey: unwanted.identityPublicKey,
        idempotencyKey: ulid(),
      },
    );
    await runtime.callAdminRpc("authServiceClaimsRemove", {
      claimId: removable.claimId,
      expectedRevision: removable.revision,
      idempotencyKey: ulid(),
    });
    await assert.rejects(
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: unwanted.seed,
      }).orThrow(),
    );
  });
});

test("a service that uses an API no provider has brought yet is added first, and its provider serves it later", async () => {
  await withTrellisRuntime(async (runtime) => {
    // Each service introduces itself through a claim and waits for approval,
    // the way an installation adds services in whatever order they arrive.
    const introduce = async <
      P extends
        | typeof participants.OperationCaller.participant
        | typeof participants.Provider.participant,
    >(
      title: string,
      participant: P,
    ) => {
      const seed = randomBytes(32);
      const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
        title,
        instanceTitle: title,
        identityPublicKey: Buffer.from(
          nacl.sign.keyPair.fromSeed(seed).publicKey,
        ).toString("base64url"),
        idempotencyKey: ulid(),
      });
      const connecting = TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: seed.toString("base64url"),
      }).orThrow();
      const pending = await runtime.waitFor(async () => {
        const { items } = await runtime.callAdminRpc(
          "authServiceUpdatesList",
          {},
        );
        return items.find(
          (update) =>
            update.deploymentId === claim.claimId && update.state === "pending",
        );
      });
      const approve = async () => {
        const { update } = await runtime.callAdminRpc(
          "authServiceUpdatesDecide",
          {
            updateId: pending.updateId,
            decision: "approved",
            expectedRevision: pending.revision,
            idempotencyKey: ulid(),
          },
        );
        assert.equal(update.state, "applied");
        return await connecting;
      };
      return { changes: pending.changes, approve };
    };
    const consumer = participants.OperationCaller.participant;
    const apiId = consumer.uses[0].api.identity;
    const catalogued = async () => {
      const { items } = await runtime.callAdminRpc("authApisList", {
        page: { limit: 50 },
        search: apiId,
      });
      return items.some((api) => api.apiId === apiId);
    };
    // No provider has brought it, so the catalog doesn't hold it.
    assert.equal(await catalogued(), false);

    const early = await introduce("early-consumer", consumer);
    // The review names the API approval adds to the catalog for it.
    assert.deepEqual(early.changes.acceptedUsedApis, [apiId]);
    const caller = await early.approve();
    try {
      assert.equal(await catalogued(), true);

      // The provider's build carries the definition the catalog now holds, so
      // approving it accepts nothing again; it only starts serving the API.
      const late = await introduce(
        "late-provider",
        participants.Provider.participant,
      );
      assert.deepEqual(late.changes.addedApis, [apiId]);
      assert.deepEqual(late.changes.acceptedApis, []);
      assert.deepEqual(late.changes.acceptedUsedApis, []);
      const provider = await late.approve();
      try {
        await provider.handleWork(async ({ input, op }) => {
          return await op
            .complete({ value: `completed ${input.value}` })
            .orThrow();
        });
        // The caller was admitted before the provider existed. Auth announces
        // the provider, so the caller reaches it well before its scheduled
        // renewal, without reconnecting.
        // Starting uses the API's shared route; following the operation uses
        // the route bound to the provider that took it, which the caller gains
        // when its renewal lands.
        const operation = await caller
          .work({ value: "early" })
          .start()
          .orThrow();
        const terminal = await runtime.waitFor(
          () => operation.wait().orThrow(),
          { timeoutMs: 10_000, intervalMs: 100 },
        );
        assert.equal(terminal.state, "completed");
        assert.deepEqual(terminal.output, { value: "completed early" });
      } finally {
        await provider.stop();
      }
    } finally {
      await caller.stop();
    }
  });
});

test("a running caller keeps working and reaches a provider the testkit registers from another package", async () => {
  await withTrellisRuntime(async (runtime) => {
    const consumer = participants.OperationCaller.participant;
    const callerIdentity = await runtime.registerService({
      name: "running-caller",
      deployment: "running-caller",
      contract: consumer,
    });
    const caller = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: consumer,
      name: "running-caller",
      seed: callerIdentity.seed,
    }).orThrow();
    try {
      const apiId = consumer.uses[0].api.identity;
      const acceptedRevision = async () => {
        const { items } = await runtime.callAdminRpc("authApisList", {
          page: { limit: 50 },
          search: apiId,
        });
        return items.find((api) => api.apiId === apiId)?.acceptedRevision;
      };
      const before = await acceptedRevision();
      // Another package carrying the identical API definition. Accepting it
      // again would advance the revision every session using it pins.
      const late = extendedParticipants.Provider.participant;
      const identity = await runtime.registerService({
        name: "other-package-provider",
        deployment: "other-package-provider",
        contract: late,
      });
      assert.equal(await acceptedRevision(), before);
      const provider = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: late,
        name: "other-package-provider",
        seed: identity.seed,
      }).orThrow();
      try {
        await provider.handleWork(async ({ input, op }) => {
          return await op
            .complete({ value: `completed ${input.value}` })
            .orThrow();
        });
        // Starting uses the API's shared route; following the operation uses
        // the route bound to the provider that took it, which the caller gains
        // when its renewal lands.
        const operation = await caller
          .work({ value: "running" })
          .start()
          .orThrow();
        const terminal = await runtime.waitFor(
          () => operation.wait().orThrow(),
          { timeoutMs: 10_000, intervalMs: 100 },
        );
        assert.deepEqual(terminal.output, { value: "completed running" });
      } finally {
        await provider.stop();
      }
    } finally {
      await caller.stop();
    }
  });
});

test("a Rust service connecting to a claim waits for approval, then runs as the new deployment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seed = randomBytes(32);
    const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
      title: "rust-third-party",
      instanceTitle: "rust-third-party",
      identityPublicKey: Buffer.from(
        nacl.sign.keyPair.fromSeed(seed).publicKey,
      ).toString("base64url"),
      idempotencyKey: ulid(),
    });
    const argv = rustFixtureArgv("service_update");
    const child = spawnTracked(argv[0], argv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: seed.toString("base64url"),
      },
    });
    let output = "";
    let errors = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (errors += chunk.toString()));
    try {
      const pending = await runtime.waitFor(async () => {
        const { items } = await runtime.callAdminRpc(
          "authServiceUpdatesList",
          {},
        );
        return items.find(
          (update) =>
            update.deploymentId === claim.claimId && update.state === "pending",
        );
      });
      const reviewUrl = pending.reviewUrl;
      assert(reviewUrl);
      await runtime.waitFor(() => errors.includes(reviewUrl));
      assert(!output.includes("service update connected"), output);
      await runtime.callAdminRpc("authServiceUpdatesDecide", {
        updateId: pending.updateId,
        decision: "approved",
        expectedRevision: pending.revision,
        idempotencyKey: ulid(),
      });
      try {
        await runtime.waitFor(() => output.includes("service update stored"));
      } catch (cause) {
        throw new Error(
          `the service did not store its value; stdout: ${output}; stderr: ${errors.slice(-1500)}`,
          { cause },
        );
      }
      assert.match(output, /service update stored approved/);
    } finally {
      child.kill();
    }
  });
});
