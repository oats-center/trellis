import { decodeTransferGrant, Result } from "@oatscenter/trellis";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { ensureTelemetryRuntime } from "@oatscenter/trellis/telemetry";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { join } from "node:path";
import { Readable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import {
  authorizeRustNativeLogin,
  repoTrellisSource,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

/**
 * Telemetry is optional: a runtime started with no OTLP endpoint, with the SDK
 * disabled, with traces only, or with an unreachable Collector must keep
 * serving ordinary generated Jobs and Events work rather than stopping its
 * Jobs or Events subsystems.
 *
 * Each scenario waits for a generated Job result and an Event consumer delivery.
 */
for (const scenario of [
  {
    name: "no endpoint",
    env: {
      OTEL_TRACES_EXPORTER: "otlp",
      OTEL_METRICS_EXPORTER: "otlp",
    } as Record<string, string>,
  },
  {
    name: "SDK disabled",
    env: {
      OTEL_SDK_DISABLED: "true",
      OTEL_TRACES_EXPORTER: "otlp",
      OTEL_METRICS_EXPORTER: "otlp",
    } as Record<string, string>,
  },
  {
    name: "traces only",
    env: {
      OTEL_TRACES_EXPORTER: "otlp",
      OTEL_METRICS_EXPORTER: "otlp",
    } as Record<string, string>,
  },
  {
    name: "collector unavailable",
    env: {
      OTEL_EXPORTER_OTLP_ENDPOINT: "http://127.0.0.1:49999",
    } as Record<string, string>,
  },
]) {
  test(`generated jobs and events work with ${scenario.name} telemetry`, async () => {
    const requests: string[] = [];
    let endpoint = "";
    const collector =
      scenario.name === "traces only" || scenario.name === "SDK disabled"
        ? createServer(async (request, response) => {
            requests.push(new URL(request.url ?? "/", endpoint).pathname);
            const ended = once(request, "end");
            request.resume();
            await ended;
            response.writeHead(200).end();
          })
        : undefined;
    if (collector) {
      collector.listen(0, "127.0.0.1");
      await once(collector, "listening");
      const address = collector.address();
      assert(address && typeof address !== "string");
      endpoint = `http://127.0.0.1:${address.port}`;
    }
    try {
      const env = {
        ...scenario.env,
        ...(scenario.name === "traces only" && {
          OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: `${endpoint}/v1/traces`,
          OTEL_TRACES_SAMPLER: "always_on",
          OTEL_BSP_SCHEDULE_DELAY: "100",
        }),
        ...(scenario.name === "SDK disabled" && {
          OTEL_EXPORTER_OTLP_ENDPOINT: endpoint,
        }),
      };
      await withTrellisRuntime(
        async (runtime) => {
          const identity = await runtime.registerService({
            name: `obs-${scenario.name.replaceAll(" ", "-")}`,
            contract: participants.Provider.participant,
          });
          // A real authenticated service connection exercises the running
          // runtime's platform, Jobs, and Events wiring end to end.
          const service = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: participants.Provider.participant,
            name: "provider",
            seed: identity.seed,
          }).orThrow();
          let serviceExit: Promise<unknown> | undefined;
          try {
            const gateArrived = new Set<string>();
            const gateArrivals = new Map<string, () => void>();
            const gateReleased = new Set<string>();
            const gateReleases = new Map<string, () => void>();
            const arriveAtGate = (key: string) => {
              gateArrived.add(key);
              gateArrivals.get(key)?.();
              gateArrivals.delete(key);
            };
            const waitForGateArrival = (key: string) =>
              gateArrived.has(key)
                ? Promise.resolve()
                : new Promise<void>((resolve) =>
                    gateArrivals.set(key, resolve),
                  );
            const holdAtGate = (key: string) =>
              gateReleased.has(key)
                ? Promise.resolve()
                : new Promise<void>((resolve) =>
                    gateReleases.set(key, resolve),
                  );
            const releaseGate = (key: string) => {
              gateReleased.add(key);
              gateReleases.get(key)?.();
              gateReleases.delete(key);
            };
            const downstreamEchoes: string[] = [];
            if (scenario.name === "collector enabled") {
              await service.handleEcho(async ({ input }) => {
                if (input.value === "linked-job") {
                  // Create the retrying Job under this Echo handler's span so
                  // both attempts link to the same exported producer. The
                  // attempts themselves are observed by the outer test.
                  await service.jobs.work
                    .create({
                      value: input.value,
                    })
                    .orThrow();
                }
                return Result.ok({ value: `Rust received ${input.value}` });
              });
              await service.handleWork(async ({ input, op }) => {
                await op.started().orThrow();
                if (input.value === "routing-check") {
                  const signal = await op.nextSignal("Continue").orThrow();
                  await op.acknowledgeSignal(signal.sequence).orThrow();
                  return await op.complete({ value: "completed" }).orThrow();
                }
                if (input.value === "downstream-operation") {
                  arriveAtGate("operation");
                  await holdAtGate("operation");
                  downstreamEchoes.push(
                    (
                      await service
                        .echo({ value: "operation-downstream" })
                        .orThrow()
                    ).value,
                  );
                }
                return await op
                  .complete({ value: `completed ${input.value}` })
                  .orThrow();
              });
              await service.handleWatch(async ({ emit }) => {
                await emit({ value: "rust-feed-from-ts" }).orThrow();
              });
              service.jobs.keyedWork.handle(({ job }) =>
                Promise.resolve(Result.ok(job.payload)),
              );
            }
            let linkedAttempts = 0;
            service.jobs.work.handle(async ({ job }) => {
              if (job.payload.value !== "linked-job") {
                return Result.ok(job.payload);
              }
              const attempt = ++linkedAttempts;
              // A controlled business wait before the downstream RPC keeps the
              // attempt open long enough for its start span to be exportable.
              await new Promise((resolve) => setTimeout(resolve, 100));
              downstreamEchoes.push(
                (
                  await service
                    .echo({
                      value: `linked-downstream-${attempt}`,
                    })
                    .orThrow()
                ).value,
              );
              return attempt === 1
                ? Result.err(new RetryJobError())
                : Result.ok(job.payload);
            });
            let received: string | undefined;
            await service
              .onChanged(({ event }) => {
                received = event.value;
                return Result.ok(undefined);
              })
              .orThrow();
            serviceExit = service.wait().catch((error: unknown) => error);
            const job = await service.jobs.work
              .create({ value: "observed" })
              .orThrow();
            const terminal = await job.wait().orThrow();
            assert.deepEqual(terminal.state, "completed");
            assert.deepEqual(terminal.result, { value: "observed" });
            if (scenario.name === "collector enabled") {
              const keyed = await service.jobs.keyedWork
                .create({
                  key: "observed",
                  value: "observed",
                })
                .orThrow();
              assert.deepEqual(
                (await keyed.wait().orThrow()).state,
                "completed",
              );
            }
            await service.publishChanged({ value: "observed" }).orThrow();
            assert.deepEqual(await runtime.waitFor(() => received), "observed");
            if (scenario.name === "collector enabled") {
              const client = await runtime.connectClient({
                name: "observability-caller",
                contract: apps.Caller.request,
              });
              assert.deepEqual(
                (await client.echo({ value: "observed" }).orThrow()).value,
                "Rust received observed",
              );
              const linkedEcho = client.echo({ value: "linked-job" }).orThrow();
              assert.deepEqual(
                (await linkedEcho).value,
                "Rust received linked-job",
              );
              await runtime.waitFor(
                () =>
                  downstreamEchoes.includes(
                    "Rust received linked-downstream-1",
                  ) &&
                  downstreamEchoes.includes(
                    "Rust received linked-downstream-2",
                  ),
                { timeoutMs: 60_000 },
              );
              assert.deepEqual(linkedAttempts, 2);
              const operation = await client
                .work({ value: "observed" })
                .start()
                .orThrow();
              const finished = await operation.wait().orThrow();
              assert.deepEqual(finished.state, "completed");
              assert.deepEqual(finished.output?.value, "completed observed");
              const downstreamOperation = await client
                .work({
                  value: "downstream-operation",
                })
                .start()
                .orThrow();
              await waitForGateArrival("operation");
              releaseGate("operation");
              const downstreamFinished = await downstreamOperation
                .wait()
                .orThrow();
              assert.deepEqual(downstreamFinished.state, "completed");
              assert.deepEqual(
                downstreamFinished.output?.value,
                "completed downstream-operation",
              );
              await runtime.waitFor(
                () =>
                  downstreamEchoes.includes(
                    "Rust received operation-downstream",
                  ),
                { timeoutMs: 60_000 },
              );
              const callerArgv = rustFixtureArgv("caller");
              const child = spawnTracked(callerArgv[0], callerArgv.slice(1), {
                env: {
                  ...process.env,
                  TRELLIS_URL: runtime.oauthIssuer,
                  TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
                  XDG_CONFIG_HOME: join(
                    runtime.workdir,
                    "rust-observability-caller",
                  ),
                },
              });
              child.stderr.pipe(process.stderr);
              const reader = Readable.toWeb(child.stdout)
                .pipeThrough(new TextDecoderStream())
                .getReader();
              let output = "";
              while (!/^NATIVE_LOGIN .+\n/m.test(output)) {
                const chunk = await reader.read();
                assert(!chunk.done, output);
                output += chunk.value;
              }
              await authorizeRustNativeLogin(
                runtime,
                output,
                apps.Caller.request,
              );
              while (!output.includes("rust caller complete")) {
                const chunk = await reader.read();
                assert(!chunk.done, output);
                output += chunk.value;
              }
              assert.equal((await child.exited).code, 0, output);
            }
          } finally {
            await service.stop();
            if (serviceExit) assert.deepEqual(await serviceExit, undefined);
            // The case-owned Collector must observe every span before the test
            // process exits; a batch exporter otherwise drops the tail.
            await ensureTelemetryRuntime({
              serviceName: "observability_test",
              role: "service",
            }).then((handle) => handle.forceFlush());
          }
        },
        {
          trellis: {
            environment: {
              unset: Object.keys(process.env).filter((key) =>
                key.startsWith("OTEL_"),
              ),
              set: env,
            },
          },
        },
      );
      if (scenario.name === "traces only") {
        assert.deepEqual(requests.includes("/v1/traces"), true);
        assert.deepEqual(requests.includes("/v1/metrics"), false);
      }
      if (scenario.name === "SDK disabled") assert.deepEqual(requests, []);
    } finally {
      if (collector)
        await new Promise<void>((resolve, reject) =>
          collector.close((error) => (error ? reject(error) : resolve())),
        );
    }
  });
}

/** In-process metric capture that rebinds Trellis instruments to a test reader. */
function startMetricCapture() {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const reader = new PeriodicExportingMetricReader({
    exporter,
    exportIntervalMillis: 60_000,
  });
  const provider = new MeterProvider({ readers: [reader] });
  metrics.setGlobalMeterProvider(provider);
  const total = (name: string, subset: Record<string, string> = {}): number => {
    let sum = 0;
    const scopes = exporter.getMetrics().at(-1)?.scopeMetrics ?? [];
    for (const metric of scopes.flatMap((scope) => scope.metrics)) {
      if (metric.descriptor.name !== name) continue;
      for (const point of metric.dataPoints) {
        if (
          Object.entries(subset).every(
            ([key, value]) => point.attributes[key] === value,
          )
        ) {
          sum += point.value as number;
        }
      }
    }
    return sum;
  };
  // Histogram observations are counted, not summed.
  const count = (name: string, subset: Record<string, string>): number => {
    let observations = 0;
    const scopes = exporter.getMetrics().at(-1)?.scopeMetrics ?? [];
    for (const metric of scopes.flatMap((scope) => scope.metrics)) {
      if (metric.descriptor.name !== name) continue;
      for (const point of metric.dataPoints) {
        if (
          Object.entries(subset).every(
            ([key, value]) => point.attributes[key] === value,
          )
        ) {
          observations += (point.value as { count: number }).count;
        }
      }
    }
    return observations;
  };
  return {
    total,
    count,
    flush: () => provider.forceFlush(),
    // Providers stay installed for the process: shutting a previous global
    // provider down poisons later instruments in the same test run.
    stop: () => Promise.resolve(),
  };
}

let sharedCapture: ReturnType<typeof startMetricCapture> | undefined;

/** One process-wide capture keeps instrument binding stable across both tests. */
function ensureCapture(): ReturnType<typeof startMetricCapture> {
  sharedCapture ??= startMetricCapture();
  return sharedCapture;
}

test("TS standalone live telemetry derives exact per-side deltas from one owner", async () => {
  const capture = ensureCapture();
  try {
    await withTrellisRuntime(
      async (runtime) => {
        const identity = await runtime.registerService({
          name: "live-owner-observability",
          contract: participants.Provider.participant,
        });
        const service = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.Provider.participant,
          name: "live-owner-observability",
          seed: identity.seed,
        }).orThrow();
        const serviceExit = service.wait().catch((error: unknown) => error);
        let sourceEnds = 0;
        await service.handleWatch(async ({ emit }) => {
          // A finite source returns normally so the consumer sees a normal END.
          for (let frame = 1; frame <= 3; frame++) {
            await emit({ value: `owner-live-${frame}` }).orThrow();
          }
          sourceEnds += 1;
        });
        const client = await runtime.connectClient({
          name: "live-owner-caller",
          contract: apps.Caller.request,
        });
        try {
          await capture.flush();
          const before = {
            consumerEnds: capture.total("trellis.live.ends", {
              "trellis.kind": "live",
              "trellis.side": "consumer",
            }),
            providerEnds: capture.total("trellis.live.ends", {
              "trellis.kind": "live",
              "trellis.side": "provider",
            }),
          };

          // 1. Normal EOF: consume every value and the normal END.
          const normal = await client.runtimeWatch({}).orThrow();
          const seen: unknown[] = [];
          for await (const value of normal) seen.push(value);
          // Reaching the end of the loop without return/cancel is the normal
          // source EOF proof.
          assert.deepEqual(seen.length, 3);

          // 2. Local cancellation after one value.
          const cancelled = await client.runtimeWatch({}).orThrow();
          const iterator = cancelled[Symbol.asyncIterator]();
          await iterator.next();
          await iterator.return?.();

          // 3. Prepared cancellation before the first next(): no source start,
          // but one live end still applies.
          const prepared = await client.runtimeWatch({}).orThrow();
          await prepared[Symbol.asyncIterator]().return?.();

          await runtime.waitFor(
            async () => {
              await capture.flush();
              return (
                capture.total("trellis.live.ends", {
                  "trellis.kind": "live",
                  "trellis.side": "consumer",
                }) >=
                before.consumerEnds + 3
              );
            },
            { timeoutMs: 30_000 },
          );
          await capture.flush();

          assert.deepEqual(sourceEnds, 2);
          assert.deepEqual(
            capture.total("trellis.live.ends", {
              "trellis.kind": "live",
              "trellis.side": "consumer",
            }) - before.consumerEnds,
            3,
          );
          assert.deepEqual(
            capture.total("trellis.live.ends", {
              "trellis.kind": "live",
              "trellis.side": "provider",
            }) - before.providerEnds,
            3,
          );
        } finally {
          await client.connection.close();
          await service.stop();
          assert.deepEqual(await serviceExit, undefined);
        }
      },
      {
        trellis: { source: repoTrellisSource() },
      },
    );
  } finally {
    await capture.stop();
  }
});

test("TS transfer telemetry labels each endpoint by direction and side", async () => {
  const capture = ensureCapture();
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "transfer-observability",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.Provider.participant,
        name: "transfer-observability",
        seed: identity.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);
      await service.handleUpload(async ({ input, transfer, op }) => {
        assert(transfer);
        const store = await service.store.files.open().orThrow();
        await store
          .put(input.value, await transfer.stream().orThrow())
          .orThrow();
        return await op.complete(input).orThrow();
      });
      await service.handleDownload(async ({ input, context }) => {
        assert(context.caller.type === "verified");
        const transfer = await service
          .createTransfer({
            direction: "receive",
            store: "files",
            key: input.value,
            context,
          })
          .orThrow();
        return Result.ok({ ...input, transfer });
      });
      const client = await runtime.connectClient({
        name: "transfer-observability-caller",
        contract: apps.TransferCaller.request,
      });
      const endpoints = ["upload", "download"].flatMap((direction) =>
        ["client", "provider"].map((side) => ({
          "trellis.direction": direction,
          "trellis.side": side,
        })),
      );
      const measure = () =>
        endpoints.map((labels) => ({
          frames: capture.total("trellis.transfer.frames", labels),
          bytes: capture.total("trellis.transfer.payload.bytes", labels),
          wire: capture.total("trellis.transfer.wire.bytes", labels),
          control: capture.total("trellis.transfer.control.bytes", labels),
          credit: capture.total("trellis.transfer.credit.controls", labels),
          completed: capture.count("trellis.transfer.duration", {
            ...labels,
            "trellis.outcome": "ok",
          }),
        }));
      try {
        // Several frames, so credit is exchanged in both directions.
        const bytes = Uint8Array.from(
          { length: 600 * 1024 },
          (_, index) => index % 251,
        );
        await capture.flush();
        const before = measure();
        const uploaded = await client
          .upload({ value: "observed" })
          .transfer(bytes)
          .start()
          .orThrow();
        await uploaded.wait().orThrow();
        const download = await client.download({ value: "observed" }).orThrow();
        const grant = decodeTransferGrant(download.transfer).orThrow();
        assert(grant.direction === "receive");
        assert.deepEqual(await client.transfer(grant).bytes().orThrow(), bytes);

        // Each endpoint counts the object's DATA once under its own side,
        // and records one successful duration, in the catalog vocabulary.
        await runtime.waitFor(
          async () => {
            await capture.flush();
            return measure().every(
              (after, index) => after.completed > before[index]!.completed,
            );
          },
          { timeoutMs: 30_000 },
        );
        const after = measure();
        for (const [index, labels] of endpoints.entries()) {
          const delta = after[index]!;
          const base = before[index]!;
          assert(delta.frames > base.frames, JSON.stringify(labels));
          assert.equal(delta.bytes - base.bytes, bytes.length);
          // Serialized DATA carries its proof headers on top of the payload,
          // and every endpoint sends or verifies non-DATA protocol messages.
          assert(delta.wire - base.wire > bytes.length, JSON.stringify(labels));
          assert(delta.control > base.control, JSON.stringify(labels));
          assert(delta.credit > base.credit, JSON.stringify(labels));
          assert.equal(delta.completed - base.completed, 1);
        }
      } finally {
        await client.connection.close();
        await service.stop();
        assert.deepEqual(await serviceExit, undefined);
      }
    },
    {
      trellis: { source: repoTrellisSource() },
    },
  );
});
