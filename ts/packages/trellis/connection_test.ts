import assert from "node:assert/strict";
import { test } from "node:test";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import { trackConnection, trackCoverage } from "./telemetry/lifecycle.ts";

test("connection and coverage metrics survive collection and clear after disposal", async () => {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const provider = new MeterProvider({
    readers: [
      new PeriodicExportingMetricReader({
        exporter,
        exportIntervalMillis: 60_000,
      }),
    ],
  });
  metrics.setGlobalMeterProvider(provider);
  const first = trackConnection("user");
  const stopFirst = trackCoverage(() => ({
    own: false,
    peerCovered: 0,
    peerUnavailable: 0,
  }));
  const second = trackConnection("service");
  let covered = 2;
  const stopSecond = trackCoverage(() => ({
    own: true,
    peerCovered: covered,
    peerUnavailable: 0,
  }));
  try {
    await provider.forceFlush();
    first.dispose();
    stopFirst();
    second.transition("usable", "connected");
    await provider.forceFlush();
    const points = (name: string) =>
      exporter
        .getMetrics()
        .at(-1)
        ?.scopeMetrics.flatMap((scope) => scope.metrics)
        .find((metric) => metric.descriptor.name === name)?.dataPoints ?? [];
    assert(
      points("trellis.connection.count").some(
        (point) =>
          point.attributes["trellis.participant.kind"] === "service" &&
          point.attributes["trellis.state"] === "usable" &&
          point.value === 1,
      ),
    );
    assert(
      points("trellis.connection.count").some(
        (point) =>
          point.attributes["trellis.participant.kind"] === "user" &&
          point.value === 0,
      ),
    );
    assert(
      points("trellis.auth.coverage.count").some(
        (point) =>
          point.attributes["trellis.kind"] === "peer" &&
          point.attributes["trellis.state"] === "covered" &&
          point.value === 2,
      ),
    );
    covered = 1;
    await provider.forceFlush();
    assert(
      points("trellis.auth.coverage.count").some(
        (point) =>
          point.attributes["trellis.kind"] === "peer" &&
          point.attributes["trellis.state"] === "covered" &&
          point.value === 1,
      ),
    );
    second.dispose();
    stopSecond();
    await provider.forceFlush();
    assert(
      points("trellis.connection.count").some(
        (point) =>
          point.attributes["trellis.participant.kind"] === "service" &&
          point.attributes["trellis.state"] === "usable" &&
          point.value === 0,
      ),
    );
  } finally {
    first.dispose();
    stopFirst();
    second.dispose();
    stopSecond();
    await provider.shutdown();
    metrics.disable();
  }
});
