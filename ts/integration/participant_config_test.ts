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
import { deadline } from "@nats-io/nats-core";
import { codecs, type JsonValue } from "@oatscenter/trellis/generated";
import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";

import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { apps as pinnedApps } from "../../integration/fixtures/runtime-pinned/packages/runtime-trellis/index.js";
import { apis as adminApis } from "../packages/trellis-testkit/trellis/index.js";
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
        idempotencyKey: codecs.ulid.decode(ulid()),
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

    // A write based on an old revision is refused and changes nothing.
    await assert.rejects(
      set(deployment, { values: { level: "debug" } }),
      (error) => {
        assert(error instanceof adminApis.auth.AuthError);
        assert.equal(error.data.code, "revision_conflict");
        return true;
      },
    );
    const unchanged = await read(deployment);
    const saved = (view: Awaited<ReturnType<typeof read>>) =>
      view.fields.map(({ name, value, isSet }) => ({ name, value, isSet }));
    assert.equal(unchanged.revision, 1);
    assert.deepEqual(saved(unchanged), saved(first));

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
      idempotencyKey: codecs.ulid.decode(ulid()),
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
        idempotencyKey: codecs.ulid.decode(ulid()),
      }),
      /current revision cannot be deleted/,
    );
    await runtime.callAdminRpc("authConfigRevisionsDelete", {
      target: deployment,
      revision: 1,
      idempotencyKey: codecs.ulid.decode(ulid()),
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
    // Null for a secret that may not be null is kept but doesn't fit, so the
    // instance goes on with the deployment's key, also when it reconnects.
    const misfit = await set(instance, {
      secrets: { apiKey: null },
      expectedRevision: 2,
    });
    assert.match(
      misfit.fields.find((field) => field.name === "apiKey")?.invalid ?? "",
      /apiKey/,
    );
    await service.stop();
    service = await connect();
    assert.equal(service.config.current.apiKey, "k1");
    await service.stop();
  });
});

test("aborting a service connect while its settings read is outstanding ends the connect", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const key = await runtime.services.createInstance({
        name: "configured-abort",
        deployment: "configured-abort",
        contract: participant,
      });
      // Auth's answer to the settings read is withheld, so cancellation lands
      // while the read is still outstanding.
      const hold = runtime
        .nativeTransportGate()
        .armResponseHold(
          `rpc.v1.${Buffer.from("trellis.auth@v1").toString("base64url")}.Config.Own.Get`,
        );
      const controller = new AbortController();
      const connecting = TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
        signal: controller.signal,
      });
      try {
        await deadline(hold.held, 30_000);
        const reason = new Error("connect cancelled");
        controller.abort(reason);
        let timer: ReturnType<typeof setTimeout> | undefined;
        const settled = await Promise.race([
          connecting,
          new Promise<"pending">((resolve) => {
            timer = setTimeout(() => resolve("pending"), 5_000);
          }),
        ]);
        clearTimeout(timer);
        assert.notEqual(settled, "pending", "connect ignored the abort");
        assert(settled !== "pending" && settled.isErr());
        // The connect reports the cancellation, not the closed transport the
        // cancellation leaves behind.
        assert.equal(settled.error.cause, reason);
      } finally {
        await hold.release();
        const late = await connecting.orThrow().catch(() => undefined);
        await late?.stop();
      }
    },
    { interruptibleNativeProxy: true },
  );
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
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    assert(provisioned.provisioningSecret);
    // The device type sets a site for every device; this device overrides it.
    await runtime.callAdminRpc("authConfigSet", {
      target: { deploymentId },
      values: { site: "North field" },
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await runtime.callAdminRpc("authConfigSet", {
      target: { deploymentId, instanceId: provisioned.instance.instanceId },
      values: { site: "Greenhouse 2" },
      idempotencyKey: codecs.ulid.decode(ulid()),
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
      idempotencyKey: codecs.ulid.decode(ulid()),
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
        idempotencyKey: codecs.ulid.decode(ulid()),
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
      // A value that doesn't fit the declared type is kept and reported, and
      // the app goes on reading what it had.
      const misfit = await runtime.callAdminRpc("authConfigSet", {
        target,
        values: { limit: "many" },
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const limit = misfit.fields.find((field) => field.name === "limit");
      assert.match(limit?.invalid ?? "", /limit/);
      assert.equal(limit?.value, 10);
      const changed = new Promise<{ changed: readonly string[] }>((resolve) => {
        client.config.onChange(({ changed }) => resolve({ changed }));
      });
      const saved = await runtime.callAdminRpc("authConfigSet", {
        target,
        values: { greeting: "welcome back" },
        expectedRevision: 1,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      assert.equal(saved.revision, 2);
      // The first change the app hears of is the greeting: the misfit limit
      // never reached it.
      assert.deepEqual(await changed, { changed: ["greeting"] });
      assert.equal(client.config.current.greeting, "welcome back");
      assert.equal(client.config.current.limit, 10);
      assert.deepEqual(client.config.revision, { app: 2 });
      // A later build's declaration changes a default no one overrode; the
      // connected app follows it and keeps the stored greeting.
      const redeclared = new Promise<number>((resolve) => {
        client.config.onChange(({ current }) => resolve(current.limit));
      });
      const later = pinnedApps.ConfiguredAgent.request;
      assert.equal(later.id, request.id);
      await runtime.callAdminRpc("authConfigAppsDeclare", {
        clientId: request.id,
        packageEvidence: later.packageEvidence as JsonValue,
        requestId: later.id,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      assert.equal(await redeclared, 20);
      assert.equal(client.config.current.greeting, "welcome back");
    } finally {
      await client.connection.close();
    }
  });
});

test("deleting a revision that holds a secret takes the capability that sets secrets", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "guarded",
      deployment: "guarded",
      contract: participant,
    });
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const set = (
      change: Omit<
        Parameters<typeof runtime.callAdminRpc<"authConfigSet">>[1],
        "target" | "idempotencyKey"
      >,
    ) =>
      runtime.callAdminRpc("authConfigSet", {
        target,
        ...change,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    // Revision 1 has no secrets; 2 and 3 hold the API key.
    await set({ values: { endpoint: "https://one.example" } });
    await set({ secrets: { apiKey: "k1" }, expectedRevision: 1 });
    await set({ values: { level: "debug" }, expectedRevision: 2 });

    const tunerIdentity = await runtime.registerService({
      name: "config-tuner",
      contract: participants.ConfigTuner.participant,
    });
    const tuner = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.ConfigTuner.participant,
      seed: tunerIdentity.seed,
    }).orThrow();
    const keeperIdentity = await runtime.registerService({
      name: "config-keeper",
      contract: participants.ConfigKeeper.participant,
    });
    const keeper = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.ConfigKeeper.participant,
      seed: keeperIdentity.seed,
    }).orThrow();
    try {
      // A role that can't set secrets can't destroy them either, and is told
      // what it would need.
      await assert.rejects(
        tuner
          .configRevisionsDelete({
            target,
            revision: 2,
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow(),
        (error) => {
          assert(error instanceof apis.auth.AuthError);
          assert.equal(error.data.code, "access_denied");
          assert.match(
            error.message,
            /trellis\.auth\.v1:config\.manageSecrets/,
          );
          return true;
        },
      );
      // A revision without secrets is the settings role's to delete, and
      // the secrets role deletes the one it was refused.
      await tuner
        .configRevisionsDelete({
          target,
          revision: 1,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      await keeper
        .configRevisionsDelete({
          target,
          revision: 2,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      const { items } = await runtime.callAdminRpc("authConfigRevisionsList", {
        target,
      });
      assert.deepEqual(
        items.map((item) => item.revision),
        [3],
      );
    } finally {
      await tuner.stop();
      await keeper.stop();
    }
  });
});
