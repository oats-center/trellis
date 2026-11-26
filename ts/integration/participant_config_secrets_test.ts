/**
 * Live acceptance that what a TypeScript or Rust service's checks and derive
 * builders give as a reason reaches other users only as the service meant it.
 * A check or builder that fails unexpectedly (a throw, or an error converted
 * with `?`) is reported to `Config.Get` and the instance's health by fixed
 * text, whatever the build declares: the error's name and message are the
 * failing code's own text and may carry any credential it read, here a
 * secret setting's value, its base64, and a token from the environment the
 * build doesn't declare. Only the service's own log and output, which its
 * operator reads, show the error, with each secret setting's value replaced.
 * An explicit rejection keeps its text, with every secret value in it
 * replaced. No secret setting's value appears anywhere.
 */

import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { execPath } from "node:process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked, type TrackedProcess } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

const participant = participants.ConfiguredService.participant;
type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/**
 * Run the fixture `start` launches, with `credential` in its environment,
 * through every way it fails, and check what each surface shows.
 */
async function exercise(
  runtime: Runtime,
  start: (seed: string, credential: string) => TrackedProcess,
): Promise<void> {
  const keys = {
    starting: `check-throws-${ulid()}`,
    good: `good-${ulid()}`,
    explicit: `explicit-${ulid()}`,
    checked: `check-throws-${ulid()}`,
    built: `build-throws-${ulid()}`,
    fixed: `fixed-${ulid()}`,
  };
  // A credential-looking token the build doesn't declare, so nothing knows
  // to replace it.
  const credential = `ghp_${ulid()}`;
  const secrets = Object.values(keys);
  const errorTexts = [
    credential,
    ...secrets.map((key) => Buffer.from(key).toString("base64")),
  ];
  const found = (text: string, candidates: readonly string[]) =>
    candidates.filter((candidate) => text.includes(candidate));
  // What other users read never shows a secret or any text of an error.
  const shared = [...secrets, ...errorTexts];
  const text = (value: unknown) =>
    JSON.stringify(value, (_key, item: unknown) =>
      typeof item === "bigint" ? item.toString() : item,
    );
  const key = await runtime.services.createInstance({
    name: "secrets",
    deployment: "secrets",
    contract: participant,
    config: {
      values: { endpoint: "https://one.example" },
      secrets: { apiKey: keys.starting },
    },
  });
  const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
  const read = () => runtime.callAdminRpc("authConfigGet", { target });
  const set = async (change: {
    values?: Record<string, string | null>;
    secrets?: Record<string, string | null>;
  }) => {
    const { revision } = await read();
    return await runtime.callAdminRpc("authConfigSet", {
      target,
      ...change,
      ...(revision === undefined ? {} : { expectedRevision: revision }),
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
  };
  const observer = await runtime.connectClient({
    name: `secrets-observer-${ulid()}`,
    contract: apps.Observer.request,
  });
  /** The instance's settings status once it is `state` for `revision`. */
  const status = (state: string, revision: number | undefined) =>
    runtime.waitFor(async () => {
      const shown = await read();
      assert.deepEqual(found(text(shown), shared), []);
      const settings = shown.instances.find(
        (instance) => instance.instanceId === key.instanceId,
      )?.settings;
      return settings?.state === state && settings.revision === revision
        ? settings
        : undefined;
    });
  /** The `settings` health check once its summary is `summary`. */
  const health = (summary: string | undefined) =>
    runtime.waitFor(async () => {
      const { instances } = await observer
        .healthInspect({
          contractId: participant.identity,
          participantKind: "service",
        })
        .orThrow();
      const sample = instances.find(
        (entry) => entry.instanceId === key.instanceId,
      )?.latestSample;
      assert.deepEqual(found(text(sample ?? {}), shared), []);
      const check = sample?.checks.find((entry) => entry.name === "settings");
      return check && check.summary === summary ? check : undefined;
    });
  /**
   * Save `change`, then return the instance's rejection of it once its
   * health shows `summary`, by default the rejection Auth stored.
   */
  const refused = async (
    change: Parameters<typeof set>[0],
    summary?: string,
  ) => {
    const saved = await set(change);
    const rejected = await status("rejected", saved.revision);
    await health(
      summary ??
        `Settings rejected: ${[rejected.fields.join(", "), rejected.reason].filter(Boolean).join(": ")}`,
    );
    return rejected;
  };

  const child = start(key.seed, credential);
  let output = "";
  child.stdout?.on("data", (chunk: Buffer) => (output += chunk.toString()));
  child.stderr?.on("data", (chunk: Buffer) => (output += chunk.toString()));
  try {
    // The check refuses the starting key at registration and the service
    // waits, saying why, and printing the error for its operator.
    const starting = await status("rejected", (await read()).revision);
    assert.deepEqual(starting.fields, []);
    assert.equal(starting.reason, "the check failed");
    await runtime.waitFor(() =>
      output.includes("rejected its settings: the check failed"),
    );
    await runtime.waitFor(() => output.includes("the endpoint refused"));
    const good = await set({ secrets: { apiKey: keys.good } });
    await runtime.waitFor(() => output.includes("READY"));
    await status("applied", good.revision);
    await health(undefined);

    // A check that fails on a plain field's change, quoting the key it
    // reads but nobody changed and a credential from its environment, in
    // the error's message and, in TypeScript, its name: status and health
    // show the fixed reason, and the service's own output the error, with
    // the key replaced.
    const plain = await refused({ values: { region: "plain" } });
    assert.deepEqual(plain.fields, ["region"]);
    assert.equal(plain.reason, "the check failed");
    await runtime.waitFor(() =>
      output.includes(`region plain is not served for [redacted]`),
    );
    assert.ok(output.includes(credential), output);

    // An explicit reason keeps its text, with the candidate key and the
    // key in effect replaced.
    const explicit = await refused({
      values: { region: "explicit" },
      secrets: { apiKey: keys.explicit },
    });
    assert.deepEqual(explicit.fields, ["region"]);
    assert.equal(
      explicit.reason,
      "region conflicts with key [redacted], was [redacted]",
    );

    // Auth keeps a reason's first KiB, cut at a character, and the health
    // check, whose summary is limited to 1024 characters, still reaches
    // Trellis with the reason cut.
    const cut = await refused(
      { values: { region: "long" } },
      `Settings rejected: region: ${"é".repeat(996)}…`,
    );
    assert.equal(cut.reason, `${"é".repeat(510)}…`);

    // A check that fails while the key changes, and a builder of the key
    // that fails, give fixed reasons too.
    const checked = await refused({
      values: { region: "east" },
      secrets: { apiKey: keys.checked },
    });
    assert.deepEqual(checked.fields, ["apiKey", "region"]);
    assert.equal(checked.reason, "the check failed");
    const built = await refused({ secrets: { apiKey: keys.built } });
    assert.deepEqual(built.fields, ["apiKey"]);
    assert.equal(built.reason, "building the value failed");
    await runtime.waitFor(() =>
      output.includes("cannot sign in with [redacted]"),
    );

    const fixed = await set({ secrets: { apiKey: keys.fixed } });
    await status("applied", fixed.revision);
    await health(undefined);
    // No secret setting's value was logged or printed.
    assert.deepEqual(found(output, secrets), []);
  } catch (error) {
    throw new Error(`service output:\n${output.slice(-4000)}`, {
      cause: error,
    });
  } finally {
    child.kill();
    await child.exited;
  }
}

test("a service whose build declares no secret reports a throw by fixed text", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.ConfiguredGrowthService.participant;
    const key = await runtime.registerService({
      name: "no-secrets",
      contract,
    });
    const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: contract,
      seed: key.seed,
    }).orThrow();
    try {
      await service.config.check((candidate) => {
        if (candidate.level === "debug") {
          const error = new Error("level debug is not served");
          error.name = "LevelError";
          throw error;
        }
      });
      const { revision } = await runtime.callAdminRpc("authConfigSet", {
        target,
        values: { level: "debug" },
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      const rejected = await runtime.waitFor(async () => {
        const { instances } = await runtime.callAdminRpc("authConfigGet", {
          target,
        });
        const settings = instances[0]?.settings;
        return settings?.state === "rejected" && settings.revision === revision
          ? settings
          : undefined;
      });
      assert.deepEqual(rejected.fields, ["level"]);
      assert.equal(rejected.reason, "the check failed");
    } finally {
      await service.stop();
    }
  });
});

test("a TypeScript service's rejection reasons show only what it meant", async () => {
  await withTrellisRuntime(async (runtime) => {
    await exercise(runtime, (seed, credential) =>
      spawnTracked(
        execPath,
        [
          fileURLToPath(
            new URL(
              "../../integration/fixtures/runtime/configured_secrets.ts",
              import.meta.url,
            ),
          ),
          runtime.trellisUrl,
          seed,
        ],
        {
          env: {
            ...process.env,
            NODE_EXTRA_CA_CERTS: runtime.caCertificatePath,
            CONFIGURED_SECRETS_CREDENTIAL: credential,
          },
        },
      ),
    );
  });
});

test("a Rust service's rejection reasons show only what it meant", async () => {
  await withTrellisRuntime(async (runtime) => {
    const argv = rustFixtureArgv("configured_secrets");
    await exercise(runtime, (seed, credential) =>
      spawnTracked(argv[0]!, argv.slice(1), {
        env: {
          ...process.env,
          RUST_LOG: "info",
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificatePath,
          TRELLIS_IDENTITY_SEED: seed,
          CONFIGURED_SECRETS_CREDENTIAL: credential,
        },
      }),
    );
  });
});
