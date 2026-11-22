/** Events are judged at the time they are signed, not when they were recorded. */
import { createClient, type InValue } from "@libsql/client";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import {
  createSqliteOutboxSchema,
  type SqlExecutor,
  TrellisService,
} from "@oatscenter/trellis/service";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";
import type {
  TrellisTestRuntime,
  TrellisTestServiceKey,
} from "@oatscenter/trellis-testkit";

const tsPublisher = participants.EventService.participant;
const rustPublisher = participants.EventServiceBoth.participant;

/** Registers both publishers before either connects. */
async function registerPublishers(runtime: TrellisTestRuntime) {
  const ts = await runtime.registerService({
    name: "signing-time-ts",
    deployment: "signing-time-ts",
    contract: tsPublisher,
  });
  const rust = await runtime.registerService({
    name: "signing-time-rust",
    deployment: "signing-time-rust",
    contract: rustPublisher,
    acknowledgeSharedApis: true,
  });
  return { ts, rust };
}

/** Connects the TypeScript publisher, whose durable consumer records what it accepts. */
async function connectPublisher(
  runtime: TrellisTestRuntime,
  identity: TrellisTestServiceKey,
) {
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: tsPublisher,
    seed: identity.seed,
  }).orThrow();
  const accepted = new Map<string, number>();
  await service
    .onAlpha(
      ({ event }) => {
        accepted.set(event.value, Date.now());
      },
      {},
      { mode: "durable", group: "events" },
    )
    .orThrow();
  // The consumer group takes deliveries once every event in it has a handler
  // and the service serves.
  await service
    .onBeta(() => {}, {}, { mode: "durable", group: "events" })
    .orThrow();
  const serving = service.wait();
  await service.ready;
  return { service, accepted, serving };
}

/** Starts the Rust publisher, whose durable consumer reports what it accepts. */
async function startRustPublisher(
  runtime: TrellisTestRuntime,
  identity: TrellisTestServiceKey,
) {
  const child = spawnTracked(rustFixtureArgv("events")[0], [], {
    env: {
      ...process.env,
      TRELLIS_URL: runtime.trellisUrl,
      TRELLIS_CA_CERTIFICATE: runtime.caCertificatePath,
      TRELLIS_IDENTITY_SEED: identity.seed,
      SIGNING_TIME: "true",
    },
  });
  child.stderr.pipe(process.stderr);
  const lines: string[] = [];
  createInterface({ input: child.stdout }).on("line", (line) =>
    lines.push(line),
  );
  const line = async (wanted: string) =>
    await runtime
      .waitFor(() => lines.includes(wanted) || undefined, {
        timeoutMs: 120_000,
        intervalMs: 50,
      })
      .catch((cause) => {
        throw new Error(`missing ${wanted}:\n${lines.join("\n")}`, { cause });
      });
  await line("SIGNING_TIME_READY");
  return {
    lines,
    line,
    send: (command: string) => child.stdin.write(`${command}\n`),
    async stop() {
      if (child.exitCode === null && child.signalCode === null)
        child.stdin.end("EXIT\n");
      const status = await child.exited;
      assert.equal(status.code, 0, lines.join("\n"));
    },
  };
}

test(
  "outbox events recorded before a renewal are accepted when dispatched after it, from both SDKs",
  { timeout: 180_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const identities = await registerPublishers(runtime);
        const subscriberIdentity = await runtime.registerService({
          name: "signing-time-subscriber",
          deployment: "signing-time-subscriber",
          contract: participants.EventSubscriberService.participant,
          acknowledgeSharedApis: true,
        });
        const ts = await connectPublisher(runtime, identities.ts);
        const subscriber = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.EventSubscriberService.participant,
          seed: subscriberIdentity.seed,
        }).orThrow();
        const live = new Set<string>();
        await subscriber
          .onAlpha(
            ({ event }) => {
              live.add(event.value);
            },
            {},
            { mode: "ephemeral" },
          )
          .orThrow();
        const directory = await mkdtemp(join(tmpdir(), "signing-time-"));
        const database = createClient({
          url: pathToFileURL(join(directory, "outbox.db")).href,
        });
        const rust = await startRustPublisher(runtime, identities.rust);
        try {
          for (const statement of createSqliteOutboxSchema())
            await database.execute(statement);
          const executor = (
            target: Pick<typeof database, "execute">,
          ): SqlExecutor => {
            const run = (sql: string, params: readonly unknown[]) =>
              target.execute({
                sql,
                args: params.map((value): InValue => {
                  if (
                    value === null ||
                    typeof value === "string" ||
                    typeof value === "number" ||
                    typeof value === "bigint"
                  )
                    return value;
                  throw new Error(`unsupported SQL parameter ${typeof value}`);
                }),
              });
            return {
              async query(sql, params) {
                return (await run(sql, params)).rows;
              },
              async execute(sql, params) {
                await run(sql, params);
              },
            };
          };
          const sessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            { page: { limit: 100 } },
          );
          const session = sessions.items.find(
            (item) =>
              item.binding.kind === "service" &&
              item.binding.instanceId === identities.ts.instanceId &&
              item.state === "active",
          );
          assert(session?.maximumContextExpiresAt);
          // Every authority the session holds now has expired by the time the
          // dispatcher wakes, so the one that signs was issued after the event
          // was recorded.
          const expiresAt = Date.parse(String(session.maximumContextExpiresAt));
          const outbox = ts.service.createSqlOutbox({
            dialect: "sqlite",
            executor: executor(database),
            transaction: async (work) => {
              const tx = await database.transaction("write");
              try {
                const result = await work({ tx, executor: executor(tx) });
                await tx.commit();
                return result;
              } finally {
                tx.close();
              }
            },
            dispatcher: { debounceMs: expiresAt - Date.now() + 2_000 },
          });
          await outbox
            .transaction(async ({ event }) => {
              await event.alpha.alpha
                .enqueue({ site: "signing-time-ts", value: "ts-outbox" })
                .orThrow();
            })
            .orThrow();
          rust.send("DISPATCH");
          const values = ["ts-outbox", "rust-outbox"];
          await runtime
            .waitFor(
              () =>
                values.every(
                  (value) =>
                    ts.accepted.has(value) &&
                    live.has(value) &&
                    rust.lines.includes(`SEEN ${value}`),
                ) || undefined,
              { timeoutMs: 120_000, intervalMs: 100 },
            )
            .catch((cause) => {
              throw new Error(
                JSON.stringify({
                  ts: [...ts.accepted.keys()],
                  live: [...live],
                  rust: rust.lines,
                }),
                { cause },
              );
            });
          assert(ts.accepted.get("ts-outbox")! > expiresAt);
          assert(rust.lines.some((line) => line.startsWith("DISPATCHED ")));
        } finally {
          await rust.stop();
          database.close();
          await rm(directory, { recursive: true, force: true });
          await subscriber.stop();
          await ts.service.stop();
          await ts.serving;
        }
      },
      // The shortest policy the runtime accepts with its 30 s clock skew.
      { authorization: { contextLifetimeSeconds: 30 } },
    );
  },
);

test(
  "a publish that waits across a reconnect is accepted, from both SDKs",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const identities = await registerPublishers(runtime);
        const ts = await connectPublisher(runtime, identities.ts);
        const rust = await startRustPublisher(runtime, identities.rust);
        try {
          await ts.service
            .publishAlpha({ site: "signing-time-ts", value: "ts-before" })
            .orThrow();
          rust.send("PUBLISH rust-before");
          await runtime
            .waitFor(
              () =>
                (ts.accepted.has("ts-before") &&
                  ts.accepted.has("rust-before") &&
                  rust.lines.includes("SEEN ts-before") &&
                  rust.lines.includes("SEEN rust-before")) ||
                undefined,
              { timeoutMs: 30_000, intervalMs: 50 },
            )
            .catch(async (cause) => {
              throw new Error(
                JSON.stringify({
                  ts: [...ts.accepted.keys()],
                  rust: rust.lines,
                  consumers: String(
                    JSON.stringify(
                      await runtime.events.consumersQuery({}),
                      (_k, v) => (typeof v === "bigint" ? String(v) : v),
                    ),
                  ),
                }),
                { cause },
              );
            });
          // Both publishes record their event time while transport is down
          // and are signed on the replacement connection, under an authority
          // issued in a later second.
          const attempts = runtime.nativeConnectionAttempts();
          runtime.interruptNativeTransport();
          // Publish once both services know their connection is gone and are
          // trying to replace it, so each publish waits for the replacement.
          await runtime.waitFor(
            () =>
              (["disconnected", "reconnecting"].includes(
                ts.service.connection.status.phase,
              ) &&
                runtime.nativeConnectionAttempts() >= attempts + 2) ||
              undefined,
            { timeoutMs: 15_000, intervalMs: 20 },
          );
          const started = Math.floor(Date.now() / 1000);
          let tsSent = false;
          const tsPublish = ts.service
            .publishAlpha({ site: "signing-time-ts", value: "ts-across" })
            .orThrow()
            .finally(() => {
              tsSent = true;
            });
          rust.send("PUBLISH rust-across");
          await rust.line("PUBLISH_STARTED rust-across");
          await runtime.waitFor(
            () => Math.floor(Date.now() / 1000) > started || undefined,
            { timeoutMs: 2_000, intervalMs: 20 },
          );
          assert(!tsSent && !rust.lines.includes("PUBLISHED rust-across"));
          runtime.restoreNativeTransport();
          await tsPublish;
          await rust.line("PUBLISHED rust-across");
          await runtime
            .waitFor(
              () =>
                (ts.accepted.has("ts-across") &&
                  ts.accepted.has("rust-across") &&
                  rust.lines.includes("SEEN ts-across") &&
                  rust.lines.includes("SEEN rust-across")) ||
                undefined,
              { timeoutMs: 60_000, intervalMs: 100 },
            )
            .catch((cause) => {
              throw new Error(
                JSON.stringify({
                  ts: [...ts.accepted.keys()],
                  rust: rust.lines,
                }),
                { cause },
              );
            });
        } finally {
          runtime.restoreNativeTransport();
          await rust.stop();
          await ts.service.stop();
          await ts.serving;
        }
      },
      { interruptibleNativeProxy: true },
    );
  },
);
