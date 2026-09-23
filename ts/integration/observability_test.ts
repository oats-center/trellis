import { Result } from "@oatscenter/trellis";
import { ensureTelemetryRuntime } from "@oatscenter/trellis/telemetry";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary used so each scenario can set its own environment. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

const captureEndpoint = Deno.env.get("TRELLIS_OBS_CAPTURE_ENDPOINT");

/**
 * Telemetry is optional: a runtime started with no OTLP endpoint, with the SDK
 * disabled, with traces only, or with an unreachable Collector must keep
 * serving ordinary generated Jobs and Events work rather than stopping its
 * Jobs or Events subsystems.
 *
 * Each scenario waits for a generated Job result and an Event consumer delivery.
 */
for (
  const scenario of [
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
    ...(captureEndpoint
      ? [{
        name: "collector enabled",
        env: {
          OTEL_EXPORTER_OTLP_ENDPOINT: captureEndpoint,
          OTEL_TRACES_SAMPLER: "always_on",
          OTEL_METRIC_EXPORT_INTERVAL: "1000",
          OTEL_BSP_SCHEDULE_DELAY: "100",
        },
      }]
      : []),
  ]
) {
  Deno.test(
    `generated jobs and events work with ${scenario.name} telemetry`,
    async () => {
      const requests: string[] = [];
      let endpoint = "";
      const collector = scenario.name === "traces only" ||
          scenario.name === "SDK disabled"
        ? Deno.serve({
          hostname: "127.0.0.1",
          port: 0,
          onListen: ({ port }) => {
            endpoint = `http://127.0.0.1:${port}`;
          },
        }, async (request) => {
          requests.push(new URL(request.url).pathname);
          await request.arrayBuffer();
          return new Response(null, { status: 200 });
        })
        : undefined;
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
        await withTrellisRuntime(async (runtime) => {
          const identity = await runtime.registerService({
            name: `obs-${scenario.name.replaceAll(" ", "-")}`,
            contract: participants.Provider.participant,
          });
          // A real authenticated service connection exercises the running
          // runtime's platform, Jobs, and Events wiring end to end.
          const service = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
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
                  gateArrivals.set(key, resolve)
                );
            const holdAtGate = (key: string) =>
              gateReleased.has(key)
                ? Promise.resolve()
                : new Promise<void>((resolve) =>
                  gateReleases.set(key, resolve)
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
                  await service.jobs.work.create({
                    value: input.value,
                  }).orThrow();
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
                    (await service.echo({ value: "operation-downstream" })
                      .orThrow()).value,
                  );
                }
                return await op.complete({ value: `completed ${input.value}` })
                  .orThrow();
              });
              await service.handleWatch(async ({ emit }) => {
                await emit({ value: "rust-feed-from-ts" }).orThrow();
              });
              service.jobs.keyedWork.handle(({ job }) =>
                Promise.resolve(Result.ok(job.payload))
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
                (await service.echo({
                  value: `linked-downstream-${attempt}`,
                }).orThrow()).value,
              );
              return attempt === 1
                ? Result.err(new RetryJobError())
                : Result.ok(job.payload);
            });
            let received: string | undefined;
            await service.onChanged(({ event }) => {
              received = event.value;
              return Result.ok(undefined);
            }).orThrow();
            serviceExit = service.wait().catch((error: unknown) => error);
            const job = await service.jobs.work.create({ value: "observed" })
              .orThrow();
            const terminal = await job.wait().orThrow();
            assertEquals(terminal.state, "completed");
            assertEquals(terminal.result, { value: "observed" });
            if (scenario.name === "collector enabled") {
              const keyed = await service.jobs.keyedWork.create({
                key: "observed",
                value: "observed",
              }).orThrow();
              assertEquals((await keyed.wait().orThrow()).state, "completed");
            }
            await service.publishChanged({ value: "observed" }).orThrow();
            assertEquals(await runtime.waitFor(() => received), "observed");
            if (scenario.name === "collector enabled") {
              const client = await runtime.connectClient({
                name: "observability-caller",
                contract: participants.Caller.participant,
              });
              assertEquals(
                (await client.echo({ value: "observed" }).orThrow()).value,
                "Rust received observed",
              );
              const linkedEcho = client.echo({ value: "linked-job" })
                .orThrow();
              assertEquals(
                (await linkedEcho).value,
                "Rust received linked-job",
              );
              await runtime.waitFor(() =>
                downstreamEchoes.includes(
                  "Rust received linked-downstream-1",
                ) && downstreamEchoes.includes(
                  "Rust received linked-downstream-2",
                ), { timeoutMs: 60_000 });
              assertEquals(linkedAttempts, 2);
              const operation = await client.work({ value: "observed" })
                .start().orThrow();
              const finished = await operation.wait().orThrow();
              assertEquals(finished.state, "completed");
              assertEquals(finished.output?.value, "completed observed");
              const downstreamOperation = await client.work({
                value: "downstream-operation",
              }).start().orThrow();
              await waitForGateArrival("operation");
              releaseGate("operation");
              const downstreamFinished = await downstreamOperation.wait()
                .orThrow();
              assertEquals(downstreamFinished.state, "completed");
              assertEquals(
                downstreamFinished.output?.value,
                "completed downstream-operation",
              );
              await runtime.waitFor(() =>
                downstreamEchoes.includes(
                  "Rust received operation-downstream",
                ), { timeoutMs: 60_000 });
              const child = new Deno.Command("cargo", {
                args: [
                  "run",
                  "--config",
                  `patch.crates-io.trellis-rs.path=${
                    JSON.stringify(
                      fromFileUrl(
                        new URL("../../crates/trellis", import.meta.url),
                      ),
                    )
                  }`,
                  "--bin",
                  "caller",
                  "--manifest-path",
                  fromFileUrl(
                    new URL(
                      "../../integration/fixtures/runtime/Cargo.toml",
                      import.meta.url,
                    ),
                  ),
                ],
                env: {
                  TRELLIS_URL: runtime.trellisUrl,
                  XDG_CONFIG_HOME: join(
                    runtime.workdir,
                    "rust-observability-caller",
                  ),
                  CARGO_TARGET_DIR: fromFileUrl(
                    new URL("../../target", import.meta.url),
                  ),
                },
                stdout: "piped",
                stderr: "inherit",
              }).spawn();
              const reader = child.stdout.pipeThrough(new TextDecoderStream())
                .getReader();
              let output = "";
              while (!output.includes("rust login ")) {
                const chunk = await reader.read();
                assert(!chunk.done, output);
                output += chunk.value;
              }
              const loginUrl = output.match(/rust login (\S+)/)?.[1];
              assert(loginUrl);
              await runtime.completeClientAuth({
                loginUrl,
                sessionKey: "observability-rust-caller",
                mode: "session_key",
              });
              while (!output.includes("rust caller complete")) {
                const chunk = await reader.read();
                assert(!chunk.done, output);
                output += chunk.value;
              }
              assert((await child.status).success, output);
            }
          } finally {
            await service.stop();
            if (serviceExit) assertEquals(await serviceExit, undefined);
            // The case-owned Collector must observe every span before the test
            // process exits; a batch exporter otherwise drops the tail.
            await ensureTelemetryRuntime({
              serviceName: "observability_test",
              role: "service",
            }).then((handle) => handle.forceFlush());
          }
        }, {
          trellis: {
            command: {
              cmd: "env",
              args: [
                ...Object.keys(Deno.env.toObject()).filter((key) =>
                  key.startsWith("OTEL_")
                ).flatMap((key) => ["-u", key]),
                ...Object.entries(env).map(([key, value]) => `${key}=${value}`),
                serverBinary(),
                "--config",
                "{config}",
                "all",
              ],
            },
          },
        });
        if (scenario.name === "traces only") {
          assertEquals(requests.includes("/v1/traces"), true);
          assertEquals(requests.includes("/v1/metrics"), false);
        }
        if (scenario.name === "SDK disabled") assertEquals(requests, []);
      } finally {
        await collector?.shutdown();
      }
    },
  );
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
          Object.entries(subset).every(([key, value]) =>
            point.attributes[key] === value
          )
        ) {
          sum += point.value as number;
        }
      }
    }
    return sum;
  };
  return {
    total,
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

Deno.test("TS service connection observes usable, suspended, resumed, and disposal against real NATS", async () => {
  const capture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    const serviceName = `connection-owner-${Date.now()}`;
    const identity = await runtime.registerService({
      name: serviceName,
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: serviceName,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    const usable = () =>
      capture.total("trellis.connection.count", {
        "trellis.participant.kind": "service",
        "trellis.state": "usable",
      });
    try {
      // A service suspended by an authoritative instance disable keeps
      // refreshing until it is terminal; the transition is observed at the
      // real TypeScript owner rather than inferred from Rust samples.
      await runtime.waitFor(async () => {
        await capture.flush();
        return usable() >= 1;
      }, { timeoutMs: 30_000 });
      await capture.flush();
      // Exactly one logical connection: the shared wrapper adopts the
      // caller's handle instead of leaving an orphan `connecting` entry.
      assertEquals(
        capture.total("trellis.connection.count", {
          "trellis.participant.kind": "service",
          "trellis.state": "connecting",
        }),
        0,
      );
      await runtime.services.disableInstance({
        instanceId: identity.instanceId,
        expectedVersion: 1n,
        idempotencyKey: crypto.randomUUID(),
        reason: "connection owner observability",
      });
      await runtime.waitFor(async () => {
        await capture.flush();
        return capture.total("trellis.auth.refresh.attempts", {
          "trellis.participant.kind": "service",
        }) >= 1;
      }, { timeoutMs: 30_000 });
    } finally {
      await service.stop();
      // A deliberately suspended service settles with the terminal
      // authorization outcome instead of a clean stop; the telemetry
      // transition is the assertion here, not the exit value.
      await serviceExit;
    }
    await runtime.waitFor(async () => {
      await capture.flush();
      return usable() === 0;
    }, { timeoutMs: 30_000 });
    await capture.flush();
    assertEquals(
      capture.total("trellis.connection.transitions", {
        "trellis.participant.kind": "service",
      }) >= 1,
      true,
    );
    assertEquals(
      capture.total("trellis.connection.transitions", {
        "trellis.participant.kind": "service",
        "trellis.reason": "terminal",
      }) >= 1,
      true,
    );
  }, {
    trellis: {
      command: {
        cmd: serverBinary(),
        args: ["--config", "{config}", "all"],
      },
    },
  });
});

Deno.test("TS standalone live telemetry derives exact per-side deltas from one owner", async () => {
  const capture = ensureCapture();
  try {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "live-owner-observability",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
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
        contract: participants.Caller.participant,
      });
      try {
        await capture.flush();
        const before = {
          consumerEnds: capture.total("trellis.live.ends", {
            "trellis.kind": "standalone",
            "trellis.side": "consumer",
          }),
          providerEnds: capture.total("trellis.live.ends", {
            "trellis.kind": "standalone",
            "trellis.side": "provider",
          }),
        };

        // 1. Normal EOF: consume every value and the normal END.
        const normal = await client.watch({}).orThrow();
        const seen: unknown[] = [];
        for await (const value of normal) seen.push(value);
        // Reaching the end of the loop without return/cancel is the normal
        // source EOF proof.
        assertEquals(seen.length, 3);

        // 2. Local cancellation after one value.
        const cancelled = await client.watch({}).orThrow();
        const iterator = cancelled[Symbol.asyncIterator]();
        await iterator.next();
        await iterator.return?.();

        // 3. Prepared cancellation before the first next(): no source start,
        // but one live end still applies.
        const prepared = await client.watch({}).orThrow();
        await prepared[Symbol.asyncIterator]().return?.();

        await runtime.waitFor(async () => {
          await capture.flush();
          return capture.total("trellis.live.ends", {
            "trellis.kind": "standalone",
            "trellis.side": "consumer",
          }) >= before.consumerEnds + 3;
        }, { timeoutMs: 30_000 });
        await capture.flush();

        assertEquals(sourceEnds, 2);
        assertEquals(
          capture.total("trellis.live.ends", {
            "trellis.kind": "standalone",
            "trellis.side": "consumer",
          }) - before.consumerEnds,
          3,
        );
        assertEquals(
          capture.total("trellis.live.ends", {
            "trellis.kind": "standalone",
            "trellis.side": "provider",
          }) - before.providerEnds,
          3,
        );
      } finally {
        await client.connection.close();
        await service.stop();
        assertEquals(await serviceExit, undefined);
      }
    }, {
      trellis: {
        command: {
          cmd: serverBinary(),
          args: ["--config", "{config}", "all"],
        },
      },
    });
  } finally {
    await capture.stop();
  }
});
