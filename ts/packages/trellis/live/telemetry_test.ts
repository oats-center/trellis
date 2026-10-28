import assert from "node:assert/strict";
import { after, test } from "node:test";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";

import { LiveTelemetryOwner } from "./telemetry.ts";
import { LiveEnd } from "./types.ts";

/** In-process capture rebinding Trellis instruments to a test reader. */
function startCapture() {
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
  return {
    shutdown: () => provider.shutdown(),
    total,
    flush: async () => {
      await provider.forceFlush();
    },
  };
}

const capture = startCapture();
after(() => capture.shutdown());

test("prepared failure records one live end", async () => {
  const beforeLiveEnds = capture.total("trellis.live.ends");
  const owner = new LiveTelemetryOwner("standalone", "consumer");
  owner.prepared();
  owner.end(new LiveEnd("setup_timeout"));
  // Repeated commits are ignored.
  owner.end(new LiveEnd("complete"));
  owner.cleanupFinished();
  await capture.flush();
  assert.deepEqual(capture.total("trellis.live.ends") - beforeLiveEnds, 1);
});

test("active Live moves the session to active", async () => {
  const beforeSessions = capture.total("trellis.live.sessions", {
    "trellis.phase": "active",
  });
  const owner = new LiveTelemetryOwner("standalone", "provider");
  owner.prepared();
  owner.activating();
  owner.active();
  await capture.flush();
  assert.deepEqual(
    capture.total("trellis.live.sessions", { "trellis.phase": "active" }) -
      beforeSessions,
    1,
  );
  owner.end(new LiveEnd("complete"));
  owner.cleanupFinished();
  await capture.flush();
});

test("Operation sessions report the operation kind", async () => {
  const beforeLive = capture.total("trellis.live.sessions", {
    "trellis.kind": "operation_watch",
    "trellis.phase": "active",
  });
  const owner = new LiveTelemetryOwner("operation", "consumer");
  owner.prepared();
  owner.activating();
  owner.active();
  await capture.flush();
  assert.deepEqual(
    capture.total("trellis.live.sessions", {
      "trellis.kind": "operation_watch",
      "trellis.phase": "active",
    }) - beforeLive,
    1,
  );
  owner.cleanupFinished();
  await capture.flush();
});

test("cleanup pending is a paired gauge with the live session", async () => {
  const owner = new LiveTelemetryOwner("standalone", "provider");
  owner.prepared();
  owner.cleanupExceededGrace();
  await capture.flush();
  const pending = capture.total("trellis.live.cleanup.pending", {
    "trellis.side": "provider",
  });
  assert.deepEqual(pending >= 1, true);
  owner.cleanupFinished();
  await capture.flush();
  assert.deepEqual(
    capture.total("trellis.live.cleanup.pending", {
      "trellis.side": "provider",
    }),
    pending - 1,
  );
});
