/**
 * Live acceptance for settings stored ahead of the build that reads them.
 * Auth keeps a value for any field name; each build reads the fields it
 * declares, checked against its own types, when it connects. A service added
 * without its IDL takes settings before it ever connects, and approving it
 * carries them to its deployment. A value that doesn't fit holds a required
 * field until it is fixed, and the waiting service then starts without a
 * restart. While one build runs, values for its next build are set without
 * disturbing it, and the next build's instances start with them. An instance
 * upgraded to a build that needs a new setting reads as pending, never as
 * what its earlier build did with the same revision.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import { codecs } from "@oatscenter/trellis/generated";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { test } from "node:test";
import nacl from "tweetnacl";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as later } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const v1 = participants.ConfiguredService.participant;
const v2 = later.ConfiguredService.participant;

test("a service added without its IDL starts with the settings set before it connected", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seed = randomBytes(32);
    const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
      title: "configured-claim",
      instanceTitle: "configured-claim",
      identityPublicKey: Buffer.from(
        nacl.sign.keyPair.fromSeed(seed).publicKey,
      ).toString("base64url"),
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const claimTarget = { claimId: claim.claimId };

    // Nothing declares these yet, so Auth stores each as given: a mistyped
    // endpoint, a setting the service never declares, a secret, a key that
    // the service will declare secret, and a region typed in as a secret.
    const stored = await runtime.callAdminRpc("authConfigSet", {
      target: claimTarget,
      values: { endpoint: 42, legacyMode: true, signingKey: "plain-text" },
      secrets: { apiKey: "k1", region: "hidden-region" },
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    assert.equal(stored.declared, false);
    assert.deepEqual(stored.fields, []);
    assert.deepEqual(
      stored.unused
        .map(({ name, secret }) => ({ name, secret }))
        .sort((a, b) => a.name.localeCompare(b.name)),
      [
        { name: "apiKey", secret: true },
        { name: "endpoint", secret: false },
        { name: "legacyMode", secret: false },
        { name: "region", secret: true },
        { name: "signingKey", secret: false },
      ],
    );
    // A secret's value never comes back.
    assert.doesNotMatch(JSON.stringify(stored), /hidden-region|k1/);

    // The service introduces itself; until it is approved, its IDL already
    // types what was stored.
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: v1,
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
    const typed = await runtime.callAdminRpc("authConfigGet", {
      target: claimTarget,
    });
    assert.equal(typed.declared, true);
    assert.deepEqual(typed.missing, ["endpoint"]);

    const { update } = await runtime.callAdminRpc("authServiceUpdatesDecide", {
      updateId: pending.updateId,
      decision: "approved",
      expectedRevision: pending.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const deployment = { deploymentId: update.deploymentId };

    // The claim's settings are the new deployment's now. The mistyped
    // endpoint holds the service; the kind mismatches are reported and never
    // used, and the secret typed in as a region is never shown.
    const waiting = await runtime.waitFor(async () => {
      const view = await runtime.callAdminRpc("authConfigGet", {
        target: deployment,
      });
      return view.instances[0]?.connectedAt === undefined ? undefined : view;
    });
    assert.equal(waiting.revision, 1);
    assert.deepEqual(waiting.missing, ["endpoint"]);
    const field = (name: string) =>
      waiting.fields.find((item) => item.name === name);
    assert.match(field("endpoint")?.invalid ?? "", /endpoint/);
    assert.equal(field("apiKey")?.isSet, true);
    assert.match(field("signingKey")?.invalid ?? "", /plain setting/);
    assert.match(field("region")?.invalid ?? "", /stored as a secret/);
    assert.equal(field("region")?.value, null);
    assert.doesNotMatch(JSON.stringify(waiting), /hidden-region/);
    assert.deepEqual(
      waiting.unused.map(({ name, value }) => ({ name, value })),
      [{ name: "legacyMode", value: true }],
    );
    // The same view is what the claim names now.
    const viaClaim = await runtime.callAdminRpc("authConfigGet", {
      target: claimTarget,
    });
    assert.equal(viaClaim.target.deploymentId, update.deploymentId);
    assert.equal(
      await Promise.race([
        connecting.then(() => "connected"),
        Promise.resolve("waiting"),
      ]),
      "waiting",
    );

    // Fixing the endpoint lets the same waiting connect finish.
    await runtime.callAdminRpc("authConfigSet", {
      target: deployment,
      values: { endpoint: "https://one.example" },
      expectedRevision: 1,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const service = await connecting;
    try {
      assert.deepEqual(service.config.current, {
        apiKey: "k1",
        endpoint: "https://one.example",
        level: "info",
        port: 8080,
        region: null,
        signingKey: null,
      });
      assert.deepEqual(service.config.revision, { deployment: 2 });
    } finally {
      await service.stop();
    }
  });
});

test("settings for the next build are set while the current one runs, and its instances start with them", async () => {
  await withTrellisRuntime(async (runtime) => {
    const first = await runtime.services.createInstance({
      name: "ahead",
      deployment: "ahead",
      contract: v1,
      config: {
        values: { endpoint: "https://one.example", region: "east" },
        secrets: { apiKey: "k1" },
      },
    });
    const deployment = { deploymentId: codecs.ulid.decode(first.deploymentId) };
    const read = () =>
      runtime.callAdminRpc("authConfigGet", { target: deployment });
    const running = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: v1,
      seed: first.seed,
    }).orThrow();
    const services: { stop(): Promise<void> }[] = [running];
    try {
      const heard: string[][] = [];
      running.config.onChange(({ changed }) => heard.push([...changed]));

      // v2 needs a worker count and takes a list of regions. Both are saved
      // while v1 serves.
      const ahead = await runtime.callAdminRpc("authConfigSet", {
        target: deployment,
        values: { workers: 4, region: ["east", "west"] },
        expectedRevision: 1,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      assert.deepEqual(ahead.missing, []);
      assert.deepEqual(
        ahead.unused.map(({ name, value }) => ({ name, value })),
        [{ name: "workers", value: 4 }],
      );
      const region = ahead.fields.find((item) => item.name === "region");
      assert.equal(region?.value, "east");
      assert.match(region?.invalid ?? "", /region/);
      assert.equal(ahead.instances[0]?.restartPending, false);

      // v1 hears of the next change it reads, and nothing before it.
      const level = new Promise<void>((resolve) =>
        running.config.onChange(() => resolve()),
      );
      await runtime.callAdminRpc("authConfigSet", {
        target: deployment,
        values: { level: "debug" },
        expectedRevision: 2,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      await level;
      assert.deepEqual(heard, [["level"]]);
      assert.equal(running.config.current.region, "east");

      // A second instance comes up on v2. Its build only adds settings, so
      // it installs without an administrator and starts with them at once.
      const second = await runtime.services.provisionInstanceOnly({
        deployment: "ahead",
      });
      const next = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: v2,
        seed: second.seed,
      }).orThrow();
      services.push(next);
      assert.deepEqual(next.config.current, {
        apiKey: "k1",
        endpoint: "https://one.example",
        level: "debug",
        port: 8080,
        region: ["east", "west"],
        signingKey: null,
        workers: 4,
      });

      // The deployment reads as v2 now; v1 still runs its own build.
      const upgraded = await read();
      assert.deepEqual(upgraded.unused, []);
      assert.equal(
        upgraded.fields.find((item) => item.name === "workers")?.value,
        4,
      );
      assert.ok(
        upgraded.instances.every((instance) => !instance.restartPending),
      );
      const back = new Promise<string>((resolve) =>
        running.config.onChange(({ current }) => resolve(current.level)),
      );
      await runtime.callAdminRpc("authConfigSet", {
        target: deployment,
        values: { level: "info" },
        expectedRevision: 3,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      assert.equal(await back, "info");
      assert.equal(running.config.current.region, "east");
      assert.deepEqual(heard, [["level"], ["level"]]);
    } finally {
      for (const service of services) await service.stop();
    }
  });
});

test("an instance upgraded to a build that needs a new setting never shows its earlier build's result", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "upgraded",
      deployment: "upgraded",
      contract: v1,
      config: {
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "k1" },
      },
    });
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const instance = async () => {
      const view = await runtime.callAdminRpc("authConfigGet", { target });
      const found = view.instances.find(
        (entry) => entry.instanceId === key.instanceId,
      );
      return { view, instance: found };
    };
    const first = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: v1,
      seed: key.seed,
    }).orThrow();
    const serving = first.wait();
    await first.ready;
    const applied = await runtime.waitFor(async () => {
      const { instance: found } = await instance();
      return found?.settings?.state === "applied" ? found : undefined;
    });
    assert.equal(applied.settings?.revision, 1);
    await first.stop();
    await serving;

    // The same instance comes back on v2, which needs settings v1 never
    // had. Its build only adds settings, so it installs without an
    // administrator; nothing about the stored settings changes.
    const controller = new AbortController();
    const connecting = TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: v2,
      seed: key.seed,
      signal: controller.signal,
    }).orThrow();
    connecting.catch(() => {});
    try {
      const waiting = await runtime.waitFor(async () => {
        const read = await instance();
        return read.instance?.connectedAt !== undefined &&
          read.instance.connectedAt !== applied.connectedAt
          ? read
          : undefined;
      });
      assert.ok(waiting.view.missing.includes("workers"));
      // v2 hasn't applied anything: v1's result for the same revision
      // doesn't stand for it.
      assert.equal(waiting.instance?.settings?.state, "pending");
      assert.equal(waiting.instance?.settings?.revision, 1);

      const fixed = await runtime.callAdminRpc("authConfigSet", {
        target,
        values: { workers: 2, region: ["east"] },
        expectedRevision: 1,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const next = await connecting;
      try {
        const nextServing = next.wait();
        await next.ready;
        await runtime.waitFor(async () => {
          const { instance: found } = await instance();
          return found?.settings?.state === "applied" &&
            found.settings.revision === fixed.revision
            ? found
            : undefined;
        });
        await next.stop();
        await nextServing;
      } finally {
        await next.stop();
      }
    } finally {
      controller.abort();
    }
  });
});

test("an earlier revision a build still reads from can't be deleted until a fitting value replaces it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "fallback",
      deployment: "fallback",
      contract: v1,
      config: {
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "k1" },
      },
    });
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const remove = (revision: number) =>
      runtime.callAdminRpc("authConfigRevisionsDelete", {
        target,
        revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    // An endpoint that doesn't fit the build leaves it on revision 1's.
    const misfit = await runtime.callAdminRpc("authConfigSet", {
      target,
      values: { endpoint: 42 },
      expectedRevision: 1,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    const endpoint = misfit.fields.find((field) => field.name === "endpoint");
    assert.equal(endpoint?.value, "https://one.example");

    // Deleting revision 1 would take that value away without a change
    // anyone hears of, so it is refused, naming the field and the build.
    await assert.rejects(
      remove(1),
      /revision 1 still gives .*ConfiguredService.* build \d+ its value for endpoint/,
    );
    const kept = await runtime.callAdminRpc("authConfigGet", { target });
    assert.equal(
      kept.fields.find((field) => field.name === "endpoint")?.value,
      "https://one.example",
    );

    // Once a fitting value is current, nothing reads revision 1 any more.
    await runtime.callAdminRpc("authConfigSet", {
      target,
      values: { endpoint: "https://two.example" },
      expectedRevision: 2,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await remove(1);
    const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
      target,
    });
    assert.deepEqual(
      items.map((item) => item.revision),
      [3, 2],
    );
  });
});
