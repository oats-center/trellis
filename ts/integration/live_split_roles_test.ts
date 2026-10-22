import { TrellisService } from "@oatscenter/trellis/service";
import { createAuth } from "@oatscenter/trellis";
import { Result } from "@oatscenter/result";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { randomBytes } from "node:crypto";
import { test } from "node:test";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { parse, stringify } from "smol-toml";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { reserveLocalPort } from "../packages/trellis-testkit/src/ports.ts";
import { API as authApi } from "../packages/trellis-testkit/trellis/apis/auth/mod.js";
import { spawnTracked, type ProcessExit } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary used for the platform and each split role. */
function serverBinary(): string {
  return (
    process.env["TRELLIS_TEST_SERVER_BIN"] ??
    fileURLToPath(new URL("../../target/debug/trellis-server", import.meta.url))
  );
}

async function startSplitRole(
  runtime: TrellisTestRuntime,
  role: "health" | "jobs" | "events",
): Promise<{
  status: Promise<ProcessExit>;
  kill: () => void;
}> {
  const configDir = join(runtime.workdir, "config", "trellis");
  const platformConfig = await readFile(join(configDir, "config.toml"), "utf8");
  const [fixture, ...args] = rustFixtureArgv("trellis-runtime-acceptance", [
    "builtin-evidence",
    role,
  ]);
  const { stdout } = await promisify(execFile)(fixture, args);
  const definition: unknown = JSON.parse(stdout);
  assert(
    definition &&
      typeof definition === "object" &&
      "participantId" in definition &&
      "packageEvidence" in definition,
  );
  const input = authApi.actions["rpc:Deployments.Create"].input.decode({
    kind: "service",
    title: `split-${role}`,
    participantId: definition.participantId,
    packageEvidence: definition.packageEvidence,
    idempotencyKey: ulid(),
  });
  const deployment = await runtime.callAdminRpc("authDeploymentsCreate", input);
  const seed = randomBytes(32).toString("base64url");
  const identity = await createAuth({ sessionKeySeed: seed });
  await runtime.callAdminRpc(
    "authServiceInstancesProvision",
    authApi.actions["rpc:ServiceInstances.Provision"].input.decode({
      deploymentId: deployment.deployment.deploymentId,
      identityPublicKey: identity.sessionKey,
      title: `split-${role}`,
      idempotencyKey: ulid(),
    }),
  );
  const root = join(runtime.workdir, "split", role);
  await mkdir(root, { recursive: true, mode: 0o700 });
  const seedFile = join(root, "identity.seed");
  await writeFile(seedFile, seed, { mode: 0o600 });
  const reservation = await reserveLocalPort();
  try {
    const port = reservation.port;
    const configPath = join(root, "config.toml");
    const config = parse(platformConfig);
    const http = config.http;
    assert(http && typeof http === "object" && !Array.isArray(http));
    Object.assign(http, { port });
    const nats = config.nats;
    assert(nats && typeof nats === "object" && "runtime" in nats);
    const transport = nats.runtime;
    assert(
      transport &&
        typeof transport === "object" &&
        "trellis_creds_path" in transport,
    );
    assert.equal(typeof transport.trellis_creds_path, "string");
    const creds = join(root, "operational.creds");
    await writeFile(
      creds,
      await readFile(join(configDir, String(transport.trellis_creds_path))),
      { mode: 0o600 },
    );
    transport.trellis_creds_path = creds;
    // The copied configuration still names the installation's TLS files;
    // resolve them against the original bundle.
    const bundlePath = (value: unknown): string => {
      assert.equal(typeof value, "string");
      return join(configDir, String(value));
    };
    nats.ca_certificate = bundlePath(nats.ca_certificate);
    const tls = config.tls;
    assert(tls && typeof tls === "object" && "generated_ca_certificate" in tls);
    tls.generated_ca_certificate = bundlePath(tls.generated_ca_certificate);
    assert("transport" in http);
    const oauthListener = (config.auth as Record<string, unknown> | undefined)
      ?.oauth as Record<string, unknown> | undefined;
    for (const listener of [http.transport, oauthListener?.transport]) {
      assert(listener && typeof listener === "object");
      const files = listener as Record<string, unknown>;
      files.certificate_chain = bundlePath(files.certificate_chain);
      files.private_key = bundlePath(files.private_key);
    }
    config.paths = {
      data: join(root, "data"),
      state: join(root, "state"),
      cache: join(root, "cache"),
      runtime: join(root, "runtime"),
      logs: join(root, "logs"),
    };
    for (const subsystem of ["platform", "health", "jobs", "events"])
      delete config[subsystem];
    config[role] = {
      storage: {
        kind: "sqlite",
        path: join(root, `${role}.sqlite`),
        journal_mode: "wal",
        single_writer: true,
      },
    };
    config.live_provider_seed_files = { [role]: seedFile };
    const auth = config.auth;
    assert(auth && typeof auth === "object" && !Array.isArray(auth));
    config.auth = Object.fromEntries(
      Object.entries(auth).filter(
        ([key]) =>
          ![
            "authorization",
            "oauth_storage_key_file",
            "local_identity",
          ].includes(key),
      ),
    );
    await writeFile(configPath, stringify(config));
    await reservation.releaseForSpawn();
    const child = spawnTracked(serverBinary(), ["--config", configPath, role], {
      env: {
        PATH: process.env["PATH"] ?? "",
        HOME: join(root, "home"),
        XDG_CONFIG_HOME: join(root, "config"),
        XDG_DATA_HOME: join(root, "data"),
        XDG_STATE_HOME: join(root, "state"),
        XDG_RUNTIME_DIR: join(root, "runtime"),
      },
    });
    child.stdout.pipe(process.stdout);
    child.stderr.pipe(process.stderr);
    try {
      await Promise.race([
        runtime.waitFor(
          async () => {
            try {
              return (await fetch(`https://localhost:${port}/readyz`)).ok;
            } catch {
              return false;
            }
          },
          { timeoutMs: 60_000 },
        ),
        child.exited.then((exit) => {
          throw new Error(
            `split ${role} exited before readiness: ${JSON.stringify(exit)}`,
          );
        }),
      ]);
    } catch (error) {
      child.kill("SIGTERM");
      await child.exited;
      throw error;
    }
    return {
      status: child.exited,
      kill: () => {
        child.kill("SIGTERM");
      },
    };
  } finally {
    await reservation.release();
  }
}

function jsonFrame(frame: unknown): Record<string, unknown> {
  if (frame instanceof Uint8Array) {
    return JSON.parse(new TextDecoder().decode(frame));
  }
  if (frame && typeof frame === "object") {
    return frame as Record<string, unknown>;
  }
  throw new Error(`unexpected watch frame: ${typeof frame}`);
}

const splitOptions = {
  trellis: {
    mode: "platform" as const,
  },
};

test("BI03 split health role serves a real Health.Watch on its own owner", async () => {
  await withTrellisRuntime(async (runtime) => {
    const split = await startSplitRole(runtime, "health");
    try {
      const client = await runtime.connectClient({
        name: "bi03-split-health",
        contract: apps.Observer.request,
      });
      const frame = await runtime.waitFor(
        async () => {
          try {
            const feed = await client.healthWatch({}).orThrow();
            const first = await feed[Symbol.asyncIterator]().next();
            await feed[Symbol.asyncIterator]().return?.();
            return first.done ? undefined : first.value;
          } catch {
            return undefined;
          }
        },
        { timeoutMs: 60_000 },
      );
      assert(frame !== undefined, "split health role never served a frame");
      await client.connection.close();
    } finally {
      split.kill();
      await split.status;
    }
  }, splitOptions);
});

test("BI03 split Jobs role emits queryInvalidated", async () => {
  await withTrellisRuntime(async (runtime) => {
    const health = await startSplitRole(runtime, "health");
    const events = await startSplitRole(runtime, "events");
    const split = await startSplitRole(runtime, "jobs");
    try {
      const key = await runtime.registerService({
        name: "bi03-split-jobs-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        name: "bi03-split-jobs-provider",
        seed: key.seed,
      }).orThrow();
      const client = await runtime.connectClient({
        name: "bi03-split-jobs",
        contract: apps.Observer.request,
      });
      const release = Promise.withResolvers<void>();
      const started = Promise.withResolvers<void>();
      service.jobs.keyedWork.handle(async ({ job }) => {
        started.resolve();
        await release.promise;
        return Result.ok(job.payload);
      });
      const worker = service.wait();
      try {
        const abort = new AbortController();
        try {
          const feed = await client
            .jobsWatch(
              { includeInitial: false },
              {
                signal: abort.signal,
              },
            )
            .orThrow();
          const frames: Record<string, unknown>[] = [];
          const pump = (async () => {
            for await (const frame of feed) frames.push(jsonFrame(frame));
          })();
          await runtime.waitFor(() =>
            frames.some((frame) => frame.kind === "ready"),
          );
          const job = await service.jobs.work
            .create({ value: "split-watch-live" })
            .orThrow();
          assert.equal(
            (await job.get().orThrow()).payload.value,
            "split-watch-live",
          );
          const keyed = await service.jobs.keyedWork
            .create({ key: "same-key", value: "queued" })
            .orThrow();
          await runtime.waitFor(() => started.promise.then(() => true), {
            timeoutMs: 20_000,
          });
          const queued = await service.jobs.keyedWork
            .create({ key: "same-key", value: "waiting" })
            .orThrow();
          const admin = await runtime.connectClient({
            name: "split-key-observer",
            contract: apps.JobsAdmin.request,
          });
          try {
            const snapshot = await runtime.waitFor(async () => {
              const result = await admin.getKey({
                service: participants.Provider.participant.identity,
                type: "keyedWork",
                key: "same-key",
              });
              const value = result.orThrow();
              return value.active.some((entry) => entry.jobId === keyed.id) &&
                value.queued.some((entry) => entry.jobId === queued.id)
                ? value
                : undefined;
            });
            assert(snapshot.active.some((entry) => entry.jobId === keyed.id));
            assert(snapshot.queued.some((entry) => entry.jobId === queued.id));
          } finally {
            await admin.connection.close();
          }
          await runtime.waitFor(
            () => frames.some((frame) => frame.kind === "queryInvalidated"),
            { timeoutMs: 30_000 },
          );
          abort.abort();
          await pump.catch(() => undefined);
        } finally {
          abort.abort();
        }
      } finally {
        release.resolve();
        await client.connection.close();
        await service.stop();
        await worker;
      }
    } finally {
      split.kill();
      await split.status;
      events.kill();
      await events.status;
      health.kill();
      await health.status;
    }
  }, splitOptions);
});

test("BI03 split Events role emits published events", async () => {
  await withTrellisRuntime(async (runtime) => {
    const health = await startSplitRole(runtime, "health");
    const split = await startSplitRole(runtime, "events");
    const jobs = await startSplitRole(runtime, "jobs");
    try {
      const key = await runtime.registerService({
        name: "bi03-split-events-provider",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        name: "bi03-split-events-provider",
        seed: key.seed,
      }).orThrow();
      const client = await runtime.connectClient({
        name: "bi03-split-events",
        contract: apps.Observer.request,
      });
      try {
        const abort = new AbortController();
        try {
          const feed = await client
            .eventsWatch({}, { signal: abort.signal })
            .orThrow();
          const frames: Record<string, unknown>[] = [];
          const pump = (async () => {
            for await (const frame of feed) frames.push(jsonFrame(frame));
          })();
          await runtime.waitFor(
            async () => {
              if (
                frames.some(
                  (frame) =>
                    Array.isArray(frame.events) && frame.events.length > 0,
                )
              ) {
                return true;
              }
              await service
                .publishChanged({ value: "split-watch-live" })
                .orThrow();
              return false;
            },
            { timeoutMs: 30_000 },
          );
          abort.abort();
          await pump.catch(() => undefined);
        } finally {
          abort.abort();
        }
      } finally {
        await client.connection.close();
        await service.stop();
      }
    } finally {
      jobs.kill();
      await jobs.status;
      split.kill();
      await split.status;
      health.kill();
      await health.status;
    }
  }, splitOptions);
});
