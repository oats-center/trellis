/**
 * Live acceptance for settings across a transport cutover: growing a
 * configured service's authority publishes a new transport generation, the old
 * one drains on its own while the settings watch moves over, and a later live
 * change still reaches the service. TypeScript and Rust each follow settings
 * with their own SDK code, so both are exercised.
 */

import assert from "node:assert/strict";
import { test } from "node:test";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import { spawnTracked } from "./_support/process.ts";

const contract = participants.ConfiguredGrowthService.participant;
const capabilityId = "runtime-trellis.transport_growth.v1:growth.extend";

/**
 * Authority lives 90s and renews about halfway through, which is when a
 * service adopts a grant. The old attachment then has some 45s of authority
 * left, so closing well within that proves it drained rather than expired.
 */
const runtimeOptions = {
  authorization: { contextLifetimeSeconds: 90, allowedClockSkewSeconds: 2 },
};

/**
 * Register the service with its optional capability withheld. `growAndDrain`
 * grants it, waits for the grown attachment, then waits for the old one to
 * close by draining.
 */
async function registerGrowable(runtime: TrellisTestRuntime) {
  const key = await runtime.registerService({
    name: "configured-growth",
    contract,
  });
  const { items } = await runtime.callAdminRpc("authCapabilityGrantsList", {
    capabilityId,
  });
  assert.equal(items.length, 1);
  const grant = items[0];
  await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
    grantId: grant.grantId,
    expectedRevision: grant.revision,
    idempotencyKey: codecs.ulid.decode(ulid()),
  });
  const admitted = async () =>
    (
      await runtime.callAdminRpc("authConnectionsList", {
        principalId: grant.principalId,
        state: "admitted",
      })
    ).items;
  return {
    key,
    /** Grow authority once connected, and wait for the old attachment to go. */
    async growAndDrain(): Promise<void> {
      const initial = await runtime.waitFor(async () => {
        const items = await admitted();
        return items.length === 1 ? items[0] : undefined;
      });
      await runtime.callAdminRpc("authCapabilityGrantsGrant", {
        principalId: grant.principalId,
        capabilityId,
        expectedRevision: 0n,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      // The service adopts the grant when it next renews its authority.
      await runtime.waitFor(
        async () => (await admitted()).length >= 2 || undefined,
        { timeoutMs: 90_000 },
      );
      // The old attachment's authority still has most of its lifetime left,
      // so it can only close this soon by draining.
      await runtime.waitFor(
        async () => {
          const items = await admitted();
          return (
            (items.length === 1 &&
              items[0].attachmentId !== initial.attachmentId) ||
            undefined
          );
        },
        { timeoutMs: 20_000 },
      );
    },
    /** Change the live `level` setting. */
    setLevel: (level: string) =>
      runtime.callAdminRpc("authConfigSet", {
        target: { deploymentId: codecs.ulid.decode(key.deploymentId) },
        values: { level },
        idempotencyKey: codecs.ulid.decode(ulid()),
      }),
  };
}

test("a TypeScript service's settings watch lets the old transport drain and follows changes after it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const growable = await registerGrowable(runtime);
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      seed: growable.key.seed,
    }).orThrow();
    try {
      assert.equal(service.config.current.level, "info");
      await growable.growAndDrain();
      const changed = new Promise<string>((resolve) => {
        service.config.onChange(({ current }) => resolve(current.level));
      });
      await growable.setLevel("debug");
      assert.equal(await changed, "debug");
    } finally {
      await service.stop();
    }
  }, runtimeOptions);
});

test("a Rust service's settings watch lets the old transport drain and follows changes after it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const growable = await registerGrowable(runtime);
    const argv = rustFixtureArgv("configured_growth");
    const child = spawnTracked(argv[0], argv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: growable.key.seed,
      },
    });
    let output = "";
    let errors = "";
    child.stdout.on("data", (chunk: Buffer) => (output += chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => (errors += chunk.toString()));
    try {
      await runtime.waitFor(() => output.includes('config level "info"'));
      await growable.growAndDrain();
      await growable.setLevel("debug");
      await runtime.waitFor(() => output.includes('config level "debug"'), {
        timeoutMs: 20_000,
      });
    } catch (error) {
      throw new Error(`stdout: ${output}\nstderr: ${errors.slice(-2000)}`, {
        cause: error,
      });
    } finally {
      child.kill();
      await child.exited;
    }
  }, runtimeOptions);
});
