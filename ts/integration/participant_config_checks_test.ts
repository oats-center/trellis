/**
 * Live acceptance for settings a TypeScript service checks before they take
 * effect: a check or a derive builder that refuses a revision leaves the
 * values in effect and `onChange` alone, the instance reports the rejection
 * to Auth and in its health, and a later revision that passes takes effect.
 * A derived value is rebuilt only when its own fields change and the one it
 * replaces is disposed after the replacement took effect. A registration that
 * refuses the values a service starts with waits for a fix, and the instance
 * never shows the revision it starts with as applied before it serves. An
 * instance that is not running shows the newest revision as pending.
 */

import { TrellisService } from "@oatscenter/trellis/service";
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
type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];
/** Admin helpers for one deployment's settings and its instance's health. */
async function settingsOf(
  runtime: Runtime,
  key: { deploymentId: string; instanceId: string },
) {
  const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
  const read = () => runtime.callAdminRpc("authConfigGet", { target });
  const observer = await runtime.connectClient({
    name: `settings-observer-${ulid()}`,
    contract: apps.Observer.request,
  });
  return {
    target,
    read,
    async set(change: {
      values?: Record<string, string | null>;
      secrets?: Record<string, string | null>;
    }) {
      const { revision } = await read();
      return await runtime.callAdminRpc("authConfigSet", {
        target,
        ...change,
        ...(revision === undefined ? {} : { expectedRevision: revision }),
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
    },
    /** The instance's settings status once `matches` accepts it. */
    status(
      matches: (
        status: NonNullable<
          Awaited<ReturnType<typeof read>>["instances"][number]["settings"]
        >,
      ) => boolean,
    ) {
      return runtime.waitFor(async () => {
        const status = (await read()).instances.find(
          (instance) => instance.instanceId === key.instanceId,
        )?.settings;
        return status && matches(status) ? status : undefined;
      });
    },
    /** The `settings` check of the instance's latest heartbeat, once `matches`. */
    health(matches: (check: { status: string; summary?: string }) => boolean) {
      return runtime.waitFor(async () => {
        const { instances } = await observer
          .healthInspect({
            contractId: participant.identity,
            participantKind: "service",
          })
          .orThrow();
        const instance = instances.find(
          (entry) => entry.instanceId === key.instanceId,
        );
        const check = instance?.latestSample.checks.find(
          (entry) => entry.name === "settings",
        );
        return check && matches(check)
          ? { check, phase: instance?.latestSample.phase }
          : undefined;
      });
    },
  };
}

test("a check refuses a live change, which leaves the service as it was until a revision passes", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "checked",
      deployment: "checked",
      contract: participant,
      config: {
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "k1" },
      },
    });
    const settings = await settingsOf(runtime, key);
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      seed: key.seed,
    }).orThrow();
    let serving: Promise<void> = Promise.resolve();
    try {
      const changes: string[][] = [];
      service.config.onChange(({ changed }) => changes.push([...changed]));
      await service.config.check((candidate, { reject }) => {
        if (!candidate.endpoint.startsWith("https://"))
          reject(["endpoint"], "must be an https URL");
      });
      // A listener that throws is logged; it never rejects a revision.
      service.config.onChange(() => {
        throw new Error("listener failure");
      });
      serving = service.wait();
      await service.ready;
      await settings.status((status) => status.state === "applied");

      const refused = await settings.set({
        values: { endpoint: "http://plain.example", level: "debug" },
      });
      const rejected = await settings.status(
        (status) =>
          status.state === "rejected" && status.revision === refused.revision,
      );
      assert.deepEqual(rejected.fields, ["endpoint"]);
      assert.equal(rejected.reason, "must be an https URL");
      // Nothing of the refused revision took effect, not even its other field.
      assert.equal(service.config.current.endpoint, "https://one.example");
      assert.equal(service.config.current.level, "info");
      assert.deepEqual(service.config.revision, { deployment: 1 });
      assert.deepEqual(changes, []);
      const degraded = await settings.health(
        (check) => check.status === "degraded",
      );
      assert.equal(
        degraded.check.summary,
        "Settings rejected: endpoint: must be an https URL",
      );
      // A rejected revision stays stored, where an administrator sees it.
      assert.equal((await settings.read()).revision, refused.revision);

      // Rolling it back is a new revision the service accepts.
      const rolledBack = await runtime.callAdminRpc("authConfigRollback", {
        target: settings.target,
        revision: 1,
        expectedRevision: refused.revision!,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      await settings.status(
        (status) =>
          status.state === "applied" && status.revision === rolledBack.revision,
      );
      assert.equal(service.config.current.endpoint, "https://one.example");
      assert.deepEqual(changes, []);

      const fixed = await settings.set({
        values: { endpoint: "https://two.example", level: "debug" },
      });
      await settings.status(
        (status) =>
          status.state === "applied" && status.revision === fixed.revision,
      );
      assert.equal(service.config.current.endpoint, "https://two.example");
      assert.deepEqual(service.config.revision, {
        deployment: fixed.revision,
      });
      assert.deepEqual(changes, [["endpoint", "level"]]);
      await settings.health((check) => check.status === "ok");
    } finally {
      await service.stop();
      await serving;
    }
  });
});

test("a derived value is rebuilt only for its own fields and disposed after its replacement takes effect", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "derived",
      deployment: "derived",
      contract: participant,
      config: {
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "k1" },
      },
    });
    const settings = await settingsOf(runtime, key);
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      seed: key.seed,
    }).orThrow();
    let serving: Promise<void> = Promise.resolve();
    try {
      let builds = 0;
      const disposed: { key: string; current: string }[] = [];
      const client = await service.config.derive(
        ["apiKey"],
        (candidate) => {
          if (candidate.apiKey === "bad") throw new Error("the key is refused");
          builds += 1;
          return { key: candidate.apiKey };
        },
        {
          // What is in effect when the old value goes.
          dispose: (old) =>
            void disposed.push({ key: old.key, current: client.current.key }),
        },
      );
      assert.equal(client.current.key, "k1");
      assert.equal(builds, 1);
      serving = service.wait();
      await service.ready;

      // Another field changing leaves the derived value as it is.
      const level = await settings.set({ values: { level: "debug" } });
      await settings.status(
        (status) =>
          status.state === "applied" && status.revision === level.revision,
      );
      assert.equal(service.config.current.level, "debug");
      assert.equal(builds, 1);

      // A builder that fails rejects its fields and keeps the old value.
      const bad = await settings.set({ secrets: { apiKey: "bad" } });
      const rejected = await settings.status(
        (status) =>
          status.state === "rejected" && status.revision === bad.revision,
      );
      assert.deepEqual(rejected.fields, ["apiKey"]);
      assert.equal(rejected.reason, "the key is refused");
      assert.equal(client.current.key, "k1");
      assert.equal(service.config.current.apiKey, "k1");
      assert.deepEqual(disposed, []);

      const good = await settings.set({ secrets: { apiKey: "k2" } });
      await settings.status(
        (status) =>
          status.state === "applied" && status.revision === good.revision,
      );
      assert.equal(client.current.key, "k2");
      assert.equal(builds, 2);
      await runtime.waitFor(async () => disposed.length > 0 || undefined);
      assert.deepEqual(disposed, [{ key: "k1", current: "k2" }]);
    } finally {
      await service.stop();
      await serving;
    }
  });
});

test("a restarted service whose check refuses its starting values never shows them applied, waits for a fix, then serves", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "refused-start",
      deployment: "refused-start",
      contract: participant,
      config: {
        values: { endpoint: "http://plain.example" },
        secrets: { apiKey: "k1" },
      },
    });
    const settings = await settingsOf(runtime, key);
    const connect = () =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
      }).orThrow();
    // A first run without the check serves and applies revision 1.
    const first = await connect();
    const firstServing = first.wait();
    await first.ready;
    await settings.status(
      (status) => status.state === "applied" && status.revision === 1,
    );
    await first.stop();
    await firstServing;

    // The next run adds a check that refuses the same revision.
    const service = await connect();
    // Every status from connect until the rejection, read as fast as Auth
    // answers.
    const seen: string[] = [];
    const watching = (async () => {
      for (;;) {
        const status = (await settings.read()).instances.find(
          (instance) => instance.instanceId === key.instanceId,
        )?.settings;
        if (status) seen.push(`${status.state} r${status.revision}`);
        if (status?.state === "rejected") return;
      }
    })();
    const printed: string[] = [];
    let serving: Promise<void> = Promise.resolve();
    const error = console.error;
    console.error = (...args: unknown[]) => {
      printed.push(args.map(String).join(" "));
    };
    try {
      // The check registers only after Auth has been asked at least once.
      await runtime.waitFor(async () => seen.length > 0 || undefined);
      let registered = false;
      const registering = service.config
        .check((candidate, { reject }) => {
          if (!candidate.endpoint.startsWith("https://"))
            reject(["endpoint"], "must be an https URL");
        })
        .then(() => {
          registered = true;
        });
      // A failure below stops the service, which ends this wait; the
      // failure is the one to report.
      registering.catch(() => {});
      await watching;
      assert.ok(
        !seen.includes("applied r1"),
        `revision 1 showed as applied before the check ran: ${seen.join(", ")}`,
      );
      const rejected = await settings.status(() => true);
      assert.equal(rejected.state, "rejected");
      assert.equal(rejected.revision, 1);
      assert.deepEqual(rejected.fields, ["endpoint"]);
      const waiting = await settings.health(
        (check) => check.status === "degraded",
      );
      // It never served: its heartbeat still says it is connecting.
      assert.equal(waiting.phase, "connecting");
      assert.equal(registered, false);
      // The operator reads why and where to fix it.
      const message = printed.find((line) =>
        line.includes("rejected its settings"),
      );
      assert.ok(message, printed.join("\n"));
      assert.ok(message.includes("endpoint: must be an https URL"), message);
      assert.ok(
        message.includes(
          `${runtime.consoleUrl}/console/admin/services/${key.deploymentId}#settings`,
        ),
        message,
      );

      const fixed = await settings.set({
        values: { endpoint: "https://one.example" },
      });
      await registering;
      assert.equal(service.config.current.endpoint, "https://one.example");
      // Accepted, but the service has not served yet.
      const accepted = await settings.status(() => true);
      assert.equal(accepted.state, "pending");
      assert.equal(accepted.revision, fixed.revision);
      serving = service.wait();
      await service.ready;
      await settings.status(
        (status) =>
          status.state === "applied" && status.revision === fixed.revision,
      );
      await settings.health((check) => check.status === "ok");
    } finally {
      console.error = error;
      await service.stop();
      await serving;
    }
  });
});

test("an instance that is not running shows the newest revision as pending until it reads it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "stopped",
      deployment: "stopped",
      contract: participant,
      config: {
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "k1" },
      },
    });
    const settings = await settingsOf(runtime, key);
    // Before the instance ever reads its settings, they are pending.
    const never = await settings.status(() => true);
    assert.equal(never.state, "pending");
    assert.equal(never.revision, 1);

    const connect = () =>
      TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
      }).orThrow();
    let service = await connect();
    let serving = service.wait();
    await service.ready;
    await settings.status(
      (status) => status.state === "applied" && status.revision === 1,
    );
    await service.stop();
    await serving;

    const saved = await settings.set({ values: { level: "debug" } });
    const pending = await settings.status(() => true);
    assert.equal(pending.state, "pending");
    assert.equal(pending.revision, saved.revision);
    assert.ok(pending.at);

    service = await connect();
    try {
      serving = service.wait();
      await service.ready;
      await settings.status(
        (status) =>
          status.state === "applied" && status.revision === saved.revision,
      );
    } finally {
      await service.stop();
      await serving;
    }
  });
});
