/**
 * Real-boundary live lifecycle acceptance.
 *
 * These cases exercise prepared/opening ownership, cancellation, source
 * settlement and truthfulness of the telemetry owner against an actual
 * Trellis runtime, an actual provider service and an actual caller.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { test } from "node:test";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { repoTrellisSource, withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary supplied by the live test harness. */

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
    trellisUrl: runtime.oauthIssuer,
    participant: participants.Provider.participant,
    name: `live-lifecycle-${crypto.randomUUID()}`,
    seed: identity.seed,
  }).orThrow();
  const exit = service.wait().catch((error: unknown) => error);
  await service.handleWatch(source);
  return { service, exit };
}

const runtimeOptions = {
  trellis: { source: repoTrellisSource() },
};

test("connection close synchronously cancels active and prepared Live consumers", async () => {
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    const { service, exit } = await startProvider(
      runtime,
      async ({ emit, signal }) => {
        starts += 1;
        await emit({ value: "active" }).orThrow();
        if (!signal.aborted) {
          await new Promise<void>((resolve) =>
            signal.addEventListener("abort", () => resolve(), { once: true }),
          );
        }
      },
    );
    const client = await runtime.connectClient({
      name: "live-close-owner-caller",
      contract: apps.Caller.request,
    });
    try {
      const active = await client.runtimeWatch({}).orThrow();
      assert.deepEqual((await active.next()).done, false);
      const prepared = await client.runtimeWatch({}).orThrow();
      const closing = client.connection.close();
      // A resolved local end must win immediately, without waiting for physical
      // transport closure or authorization-registry teardown to notify consumers.
      for (const handle of [active, prepared]) {
        const end = await Promise.race([
          handle.closed,
          Promise.resolve(undefined),
        ]);
        assert.deepEqual(end?.reason, "cancelled");
        assert.deepEqual(await handle.next(), { done: true, value: undefined });
      }
      await closing;
      assert.deepEqual(
        starts,
        1,
        "the prepared consumer never activates its source",
      );
    } finally {
      await client.connection.close();
      await service.stop();
      assert.deepEqual(await exit, undefined);
    }
  }, runtimeOptions);
});

test("L19/L20 prepared return never starts a provider source", async () => {
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
      contract: apps.Caller.request,
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
      const prepared = await client.runtimeWatch({}).orThrow();
      await prepared[Symbol.asyncIterator]().return?.();
      await new Promise((resolve) => setTimeout(resolve, 2_000));
      await metricsCapture.flush();

      assert.deepEqual(starts, 0);
      assert.deepEqual(cleanups, 0);
      assert.deepEqual(
        metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
        }) - beforeEnds,
        1,
      );
      assert.deepEqual(
        metricsCapture.total("trellis.live.sessions", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
          "trellis.phase": "active",
        }),
        beforeActive,
      );
      assert.deepEqual(
        metricsCapture.total("trellis.live.cleanup.pending"),
        beforePending,
      );
    } finally {
      await client.connection.close();
      await service.stop();
      assert.deepEqual(await exit, undefined);
    }
  }, runtimeOptions);
});

test("L24/L25 finite source delivers ordered values and one normal end", async () => {
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
      contract: apps.Caller.request,
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

      const handle = await client.runtimeWatch({}).orThrow();
      const seen: unknown[] = [];
      for await (const value of handle) seen.push(value);
      assert.deepEqual(seen.length, 4);
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 10_000 });
      await metricsCapture.flush();

      assert.deepEqual(starts, 1);
      assert.deepEqual(cleanups, 1);
      assert.deepEqual(
        metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "consumer",
        }) - beforeConsumerEnds,
        1,
      );
      assert.deepEqual(
        metricsCapture.total("trellis.live.ends", {
          "trellis.kind": "standalone",
          "trellis.side": "provider",
        }) - beforeProviderEnds,
        1,
      );
    } finally {
      await client.connection.close();
      await service.stop();
      assert.deepEqual(await exit, undefined);
    }
  }, runtimeOptions);
});

test("L28 cancelling a blocked source settles provider cleanup", async () => {
  const metricsCapture = ensureCapture();
  await withTrellisRuntime(async (runtime) => {
    let starts = 0;
    let cleanups = 0;
    let cleanupAborted = false;
    const { service, exit } = await startProvider(
      runtime,
      async ({ emit, signal }) => {
        starts += 1;
        try {
          while (!signal.aborted) {
            await emit({ value: "blocked" }).orThrow();
          }
        } finally {
          cleanupAborted = signal.aborted;
          cleanups += 1;
        }
      },
    );
    const client = await runtime.connectClient({
      name: "live-lifecycle-caller",
      contract: apps.Caller.request,
    });
    try {
      await metricsCapture.flush();
      const beforePending = metricsCapture.total(
        "trellis.live.cleanup.pending",
      );
      const handle = await client.runtimeWatch({}).orThrow();
      const iterator = handle[Symbol.asyncIterator]();
      await iterator.next();
      const [receipt, concurrentReceipt] = await Promise.all([
        handle.close().orThrow(),
        handle.close().orThrow(),
      ]);
      assert.deepEqual(receipt.remote, "confirmed");
      assert.deepEqual(receipt.cleanup, "complete");
      assert.deepEqual(receipt.end.reason, "cancelled");
      assert.deepEqual(concurrentReceipt, receipt);
      assert.deepEqual(await handle.close().orThrow(), receipt);
      assert.deepEqual(await iterator.next(), { done: true, value: undefined });
      await iterator.return?.();
      await runtime.waitFor(() => cleanups === 1, { timeoutMs: 10_000 });
      await metricsCapture.flush();

      assert.deepEqual(starts, 1);
      assert.deepEqual(cleanups, 1);
      assert(cleanupAborted);
      // A settled cancellation leaves no retained cleanup behind.
      assert.deepEqual(
        metricsCapture.total("trellis.live.cleanup.pending"),
        beforePending,
      );
    } finally {
      await client.connection.close();
      await service.stop();
      assert.deepEqual(await exit, undefined);
    }
  }, runtimeOptions);
});
