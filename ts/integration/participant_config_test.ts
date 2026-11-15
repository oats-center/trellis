/**
 * Live acceptance for settings Trellis keeps for a service: the instance waits
 * for required values, reads secrets the administrator cannot read back,
 * follows live changes, keeps restart-only values until it reconnects, and
 * takes instance overrides over deployment values. An optional secret reads
 * null until it is set, and an instance can be set back to null.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import {
  checkDeviceActivation,
  TrellisDevice,
} from "@oatscenter/trellis/device";
import { deriveDeviceIdentity } from "@oatscenter/trellis/auth";
import { discoverNativeOAuth } from "@oatscenter/trellis/native";
import { join } from "node:path";
import { codecs } from "@oatscenter/trellis/generated";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const participant = participants.ConfiguredService.participant;

test("a service waits for its settings, keeps secrets from administrators, and follows live changes", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "configured",
      deployment: "configured",
      contract: participant,
    });
    const deployment = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const instance = {
      ...deployment,
      instanceId: codecs.ulid.decode(key.instanceId),
    };
    const read = (target: typeof deployment) =>
      runtime.callAdminRpc("authConfigGet", { target });
    const set = (
      target: typeof deployment,
      change: Omit<
        Parameters<typeof runtime.callAdminRpc<"authConfigSet">>[1],
        "target" | "idempotencyKey"
      >,
    ) =>
      runtime.callAdminRpc("authConfigSet", {
        target,
        ...change,
        idempotencyKey: ulid(),
      });

    const connect = () =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
      }).orThrow();

    // Required values are missing, so the instance reads them and waits.
    const connecting = connect();
    const waiting = await runtime.waitFor(async () => {
      const view = await read(deployment);
      return view.instances[0]?.connectedAt === undefined ? undefined : view;
    });
    assert.deepEqual(waiting.missing, ["apiKey", "endpoint"]);
    assert.equal(
      await Promise.race([
        connecting.then(() => "connected"),
        Promise.resolve("waiting"),
      ]),
      "waiting",
    );

    // A setting and a secret go in separately; the administrator sees only
    // that the secret is set.
    await assert.rejects(
      set(deployment, { values: { apiKey: "k1" } }),
      /apiKey is a secret/,
    );
    const first = await set(deployment, {
      values: { endpoint: "https://one.example" },
      secrets: { apiKey: "k1" },
    });
    assert.equal(first.revision, 1);
    const apiKey = first.fields.find((field) => field.name === "apiKey");
    assert.equal(apiKey?.isSet, true);
    assert.equal(apiKey?.value, undefined);
    assert.deepEqual(first.missing, []);

    let service = await connecting;
    assert.deepEqual(service.config.current, {
      apiKey: "k1",
      endpoint: "https://one.example",
      level: "info",
      port: 8080,
      region: null,
      signingKey: null,
    });
    assert.deepEqual(service.config.revision, { deployment: 1 });

    // A value outside the declared type is refused when it is saved.
    await assert.rejects(
      set(deployment, { values: { level: 5 }, expectedRevision: 1 }),
      /level/,
    );
    // A write based on an old revision is refused.
    await assert.rejects(
      set(deployment, { values: { level: "debug" } }),
      /conflict/i,
    );

    // A live field changes in place; a restart-only one waits for reconnect.
    const changed = new Promise<{
      changed: readonly string[];
      level: string;
      port: number;
    }>((resolve) => {
      service.config.onChange(({ changed, current }) =>
        resolve({ changed, level: current.level, port: current.port }),
      );
    });
    await set(deployment, {
      values: { level: "debug", port: 9090 },
      expectedRevision: 1,
    });
    assert.deepEqual(await changed, {
      changed: ["level"],
      level: "debug",
      port: 8080,
    });
    const pending = await read(deployment);
    assert.equal(pending.revision, 2);
    assert.equal(pending.instances[0]?.connectedRevision, 1);

    await service.stop();
    service = await connect();
    assert.equal(service.config.current.port, 9090);
    assert.equal((await read(deployment)).instances[0]?.connectedRevision, 2);

    // An instance override takes precedence over the deployment's value.
    const overridden = new Promise<string>((resolve) => {
      service.config.onChange(({ current }) => resolve(current.level));
    });
    const override = await set(instance, { values: { level: "info" } });
    assert.equal(
      override.fields.find((field) => field.name === "level")?.source,
      "instance",
    );
    assert.equal(await overridden, "info");

    // History lists what changed without ever showing a secret.
    const history = await runtime.callAdminRpc("authConfigRevisionsList", {
      target: deployment,
    });
    assert.deepEqual(
      history.items.map(({ revision, changed, changedSecrets, current }) => ({
        revision,
        changed,
        changedSecrets,
        current,
      })),
      [
        {
          revision: 2,
          changed: ["level", "port"],
          changedSecrets: [],
          current: true,
        },
        {
          revision: 1,
          changed: ["endpoint"],
          changedSecrets: ["apiKey"],
          current: false,
        },
      ],
    );
    assert.ok(history.items.every((item) => !("apiKey" in item.values)));

    // Rolling back writes the earlier values as a new revision; the current
    // revision cannot be deleted, an older one can.
    const rolledBack = await runtime.callAdminRpc("authConfigRollback", {
      target: deployment,
      revision: 1,
      expectedRevision: 2,
      idempotencyKey: ulid(),
    });
    assert.equal(rolledBack.revision, 3);
    assert.equal(
      rolledBack.fields.find((field) => field.name === "port")?.source,
      "default",
    );
    await assert.rejects(
      runtime.callAdminRpc("authConfigRevisionsDelete", {
        target: deployment,
        revision: 3,
        idempotencyKey: ulid(),
      }),
      /current revision cannot be deleted/,
    );
    await runtime.callAdminRpc("authConfigRevisionsDelete", {
      target: deployment,
      revision: 1,
      idempotencyKey: ulid(),
    });
    const remaining = await runtime.callAdminRpc("authConfigRevisionsList", {
      target: deployment,
    });
    assert.deepEqual(
      remaining.items.map((item) => item.revision),
      [3, 2],
    );

    // An optional secret was never waited for and read null; once set it
    // arrives live, and an instance can go back to null on its own.
    const signingKey = (next: () => Promise<unknown>) =>
      new Promise<string | null>((resolve) => {
        service.config.onChange(({ changed, current }) => {
          if (changed.includes("signingKey")) resolve(current.signingKey);
        });
        void next();
      });
    assert.equal(
      await signingKey(() =>
        set(deployment, {
          secrets: { signingKey: "s1" },
          expectedRevision: 3,
        }),
      ),
      "s1",
    );
    assert.equal(
      await signingKey(() =>
        set(instance, { secrets: { signingKey: null }, expectedRevision: 1 }),
      ),
      null,
    );
    const nulled = (await read(instance)).fields.find(
      (field) => field.name === "signingKey",
    );
    assert.equal(nulled?.source, "instance");
    // A secret that may not be null refuses it.
    await assert.rejects(
      set(instance, { secrets: { apiKey: null }, expectedRevision: 2 }),
      /apiKey/,
    );
    await service.stop();
  });
});

test("a device reads its own settings over its type's and follows live changes", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participant = participants.ConfiguredDevice.participant;
    const applied = await runtime.contracts.apply({
      deployment: "configured-device",
      contract: participant,
    });
    assert(applied.deploymentId);
    const deploymentId = codecs.ulid.decode(applied.deploymentId);
    const provisioned = await runtime.devices.provision({
      deploymentId,
      title: "Greenhouse sensor",
      idempotencyKey: ulid(),
    });
    assert(provisioned.provisioningSecret);
    // The device type sets a site for every device; this device overrides it.
    await runtime.callAdminRpc("authConfigSet", {
      target: { deploymentId },
      values: { site: "North field" },
      idempotencyKey: ulid(),
    });
    await runtime.callAdminRpc("authConfigSet", {
      target: { deploymentId, instanceId: provisioned.instance.instanceId },
      values: { site: "Greenhouse 2" },
      idempotencyKey: ulid(),
    });

    const rootSecret = crypto.getRandomValues(new Uint8Array(32));
    const activationOptions = {
      trellisUrl: runtime.trellisUrl,
      participant,
      rootSecret,
      provisioningSecret: provisioned.provisioningSecret,
      statePath: join(runtime.workdir, "activation.json"),
    };
    const activation = await checkDeviceActivation(activationOptions);
    if (activation.status !== "activation_required")
      throw new Error("expected an activation review");
    const reviewId = new URL(activation.activationUrl).searchParams.get(
      "reviewId",
    );
    assert(reviewId);
    const review = {
      reviewId: codecs.ulid.decode(reviewId),
      confirmationCode: activation.confirmationCode,
    };
    const { review: pending } = await runtime.callAdminRpc(
      "authDevicesReviewsGet",
      review,
    );
    await runtime.callAdminRpc("authDevicesReviewsDecide", {
      ...review,
      expectedRevision: pending.revision,
      decision: "approved",
      idempotencyKey: ulid(),
    });
    await activation.waitForOnlineApproval();

    const server = (await discoverNativeOAuth(runtime.trellisUrl)).orThrow();
    const identity = await deriveDeviceIdentity(rootSecret);
    const device = await TrellisDevice.connect({
      servers: [...server.natsServers],
      trellisInstanceId: server.trust.instanceId,
      audienceNatsAccount: server.trust.account,
      issuers: [server.trust.issuer],
      natsSentinel: server.natsSentinel,
      seed: identity.identitySeedBase64url,
      participant,
    }).orThrow();
    try {
      assert.deepEqual(device.config.current, {
        site: "Greenhouse 2",
        interval: 60,
      });
      const changed = new Promise<number>((resolve) => {
        device.config.onChange(({ current }) => resolve(current.interval));
      });
      await runtime.callAdminRpc("authConfigSet", {
        target: { deploymentId },
        values: { interval: 15 },
        expectedRevision: 1,
        idempotencyKey: ulid(),
      });
      assert.equal(await changed, 15);
    } finally {
      await device.connection.close();
    }
  });
});

test("an app reads its own settings, which an administrator changes live", async () => {
  await withTrellisRuntime(async (runtime) => {
    const request = apps.ConfiguredAgent.request;
    // Registering the client attaches the app's declaration, as `trellis
    // clients put --source` does.
    const client = await runtime.connectClient({
      name: "configured-agent",
      contract: request,
    });
    try {
      assert.deepEqual(client.config.current, { greeting: "hello", limit: 10 });
      const target = { clientId: request.id };
      await assert.rejects(
        runtime.callAdminRpc("authConfigSet", {
          target,
          values: { limit: "many" },
          idempotencyKey: ulid(),
        }),
        /limit/,
      );
      const changed = new Promise<string>((resolve) => {
        client.config.onChange(({ current }) => resolve(current.greeting));
      });
      const saved = await runtime.callAdminRpc("authConfigSet", {
        target,
        values: { greeting: "welcome back" },
        idempotencyKey: ulid(),
      });
      assert.equal(saved.revision, 1);
      assert.equal(await changed, "welcome back");
      assert.deepEqual(client.config.revision, { app: 1 });
    } finally {
      await client.connection.close();
    }
  });
});
