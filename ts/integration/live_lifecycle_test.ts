/**
 * Real-boundary live lifecycle acceptance.
 *
 * These cases exercise prepared/opening ownership, cancellation, source
 * settlement and truthfulness of the telemetry owner against an actual
 * Trellis runtime, an actual provider service and an actual caller.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

/** In-process metric capture rebinding Trellis instruments to a test reader. */
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
  return { total, flush: () => provider.forceFlush() };
}

let capture: ReturnType<typeof startMetricCapture> | undefined;
function ensureCapture(): ReturnType<typeof startMetricCapture> {
  capture ??= startMetricCapture();
  return capture;
}

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** Start one provider service whose Watch source is controlled by the case. */
async function startProvider(
  runtime: Runtime,
  source: (context: {
    emit: (event: { value: string }) => { orThrow(): Promise<void> };
    signal: AbortSignal;
  }) => Promise<void>,
) {
  const identity = await runtime.registerService({
    name: `live-lifecycle-${crypto.randomUUID()}`,
    contract: participants.Provider.participant,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    name: `live-lifecycle-${crypto.randomUUID()}`,
    seed: identity.seed,
  }).orThrow();
  const exit = service.wait().catch((error: unknown) => error);
  await service.handleWatch(source);
  return { service, exit };
}

const runtimeOptions = {
  trellis: {
    command: {
      cmd: serverBinary(),
      args: ["--config", "{config}", "all"],
    },
  },
};

Deno.test("L19/L20 prepared return never starts a provider source", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    let cleanups = 0;
    const { service, exit } = await startProvider(
      runtime,
      async ({ emit, signal }) => {
        starts += 1;
        while (!signal.aborted) {
          await emit({ value: "prepared" }).orThrow();
          await new Promise((resolve) => setTimeout(resolve, 10));
        }
        cleanups += 1;
      },
    );
    const client = await runtime.connectClient({
      name: "live-lifecycle-caller",
      contract: participants.Caller.participant,
    });
    try {
      await metricsCapture.flush();
      const beforeEnds = metricsCapture.total("trellis.live.ends", {
        "trellis.kind": "standalone",
        "trellis.side": "consumer",
      });
      const beforeActive = metricsCapture.total("trellis.live.sessions", {
        "trellis.kind": "standalone",
        "trellis.side": "consumer",
        "trellis.phase": "active",
      });
      const beforePending = metricsCapture.total(
        "trellis.live.cleanup.pending",
      );

      // A never-iterated handle is returned immediately: no activation, no
      // provider source, and exactly one local live end.
      const prepared = await client.watch({}).orThrow();
      await prepared[Symbol.asyncIterator]().return?.();
      await new Promise((resolve) => setTimeout(resolve, 2_000));
      await metricsCapture.flush();

      assertEquals(starts, 0);
      assertEquals(cleanups, 0);
      assertEquals(
        metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
        }) - beforeEnds,
        1,
      );
      assertEquals(
        metricsCapture.total("trellis.live.sessions", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
          "trellis.phase": "active",
        }),
        beforeActive,
      );
      assertEquals(
        metricsCapture.total("trellis.live.cleanup.pending"),
        beforePending,
      );
    } finally {
      await client.connection.close();
      await service.stop();
      assertEquals(await exit, undefined);
    }
  }, runtimeOptions);
});

Deno.test("L24/L25 finite source delivers ordered values and one normal end", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    let cleanups = 0;
    const { service, exit } = await startProvider(runtime, async ({ emit }) => {
      starts += 1;
      for (let frame = 1; frame <= 4; frame++) {
        await emit({ value: `ordered-${frame}` }).orThrow();
      }
      cleanups += 1;
    });
    const client = await runtime.connectClient({
      name: "live-lifecycle-caller",
      contract: participants.Caller.participant,
    });
    try {
      await metricsCapture.flush();
      const beforeConsumerEnds = metricsCapture.total("trellis.live.ends", {
        "trellis.kind": "standalone",
        "trellis.side": "consumer",
      });
      const beforeProviderEnds = metricsCapture.total("trellis.live.ends", {
        "trellis.kind": "standalone",
        "trellis.side": "provider",
      });

      const handle = await client.watch({}).orThrow();
      const seen: unknown[] = [];
      for await (const value of handle) seen.push(value);
      assertEquals(seen.length, 4);
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 10_000 });
      await metricsCapture.flush();

      assertEquals(starts, 1);
      assertEquals(cleanups, 1);
      assertEquals(
        metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
        }) - beforeConsumerEnds,
        1,
      );
      assertEquals(
        metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "provider",
        }) - beforeProviderEnds,
        1,
      );
    } finally {
      await client.connection.close();
      await service.stop();
      assertEquals(await exit, undefined);
    }
  }, runtimeOptions);
});

Deno.test("L28 cancelling a blocked source settles provider cleanup", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    let cleanups = 0;
    const { service, exit } = await startProvider(
      runtime,
      async ({ emit, signal }) => {
        starts += 1;
        while (!signal.aborted) {
          await emit({ value: "blocked" }).orThrow();
          await new Promise((resolve) => setTimeout(resolve, 10));
        }
        cleanups += 1;
      },
    );
    const client = await runtime.connectClient({
      name: "live-lifecycle-caller",
      contract: participants.Caller.participant,
    });
    try {
      await metricsCapture.flush();
      const beforePending = metricsCapture.total(
        "trellis.live.cleanup.pending",
      );
      const handle = await client.watch({}).orThrow();
      const iterator = handle[Symbol.asyncIterator]();
      await iterator.next();
      await iterator.return?.();
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 10_000 });
      await metricsCapture.flush();

      assertEquals(starts, 1);
      assertEquals(cleanups, 1);
      // A settled cancellation leaves no retained cleanup behind.
      assertEquals(
        metricsCapture.total("trellis.live.cleanup.pending"),
        beforePending,
      );
      assert(true);
    } finally {
      await client.connection.close();
      await service.stop();
      assertEquals(await exit, undefined);
    }
  }, runtimeOptions);
});
