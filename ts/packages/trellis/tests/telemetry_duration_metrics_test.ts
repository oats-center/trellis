import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import assert from "node:assert/strict";
import { after, test } from "node:test";
import { ulid } from "ulid";

const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
const reader = new PeriodicExportingMetricReader({
  exporter,
  exportIntervalMillis: 60_000,
});
const provider = new MeterProvider({ readers: [reader] });
metrics.setGlobalMeterProvider(provider);
after(() => provider.shutdown());

type HistogramValue = {
  sum: number;
  count: number;
  min: number;
  max: number;
};

type HistogramPoint = {
  name: string;
  unit: string;
  value: HistogramValue;
  attributes: Record<string, unknown>;
};

function collectHistogramDataPoints(): HistogramPoint[] {
  const points: HistogramPoint[] = [];
  for (const resourceMetrics of exporter.getMetrics()) {
    for (const scopeMetric of resourceMetrics.scopeMetrics) {
      for (const metric of scopeMetric.metrics) {
        for (const dataPoint of metric.dataPoints) {
          points.push({
            name: metric.descriptor.name,
            unit: metric.descriptor.unit,
            value: dataPoint.value as HistogramValue,
            attributes: dataPoint.attributes as Record<string, unknown>,
          });
        }
      }
    }
  }
  return points;
}

test("Records histogram value and unit", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?record=${ulid()}`
  );

  recordTrellisDuration("trellis.connect.duration", 150, {
    phase: "test_record",
    outcome: "ok",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.connect.duration" &&
      p.attributes["trellis.phase"] === "test_record",
  );
  assert(points[0]);
  assert.deepEqual(points[0].unit, "s");
  assert.deepEqual(points[0].value.count, 1);
  assert.deepEqual(points[0].value.sum, 0.15);
});

test("Ignores negative duration", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?neg=${ulid()}`
  );

  recordTrellisDuration("trellis.connect.duration", -100, {
    phase: "test_neg",
    outcome: "ok",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.connect.duration" &&
      p.attributes["trellis.phase"] === "test_neg",
  );
  assert.deepEqual(points.length, 0);
});

test("Ignores NaN duration", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?nan=${ulid()}`
  );

  recordTrellisDuration("trellis.connect.duration", NaN, {
    phase: "test_nan",
    outcome: "ok",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.connect.duration" &&
      p.attributes["trellis.phase"] === "test_nan",
  );
  assert.deepEqual(points.length, 0);
});

test("Ignores infinite duration", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?inf=${ulid()}`
  );

  recordTrellisDuration("trellis.connect.duration", Infinity, {
    phase: "test_inf",
    outcome: "ok",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.connect.duration" &&
      p.attributes["trellis.phase"] === "test_inf",
  );
  assert.deepEqual(points.length, 0);
});

test("Sanitizes attributes — drops bad values, keeps good ones", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?sanitize=${ulid()}`
  );

  recordTrellisDuration("trellis.auth.callout.duration", 100, {
    phase: "test_sanitize",
    outcome: "ok",
    sessionKind: "user",
    participantKind: "device",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.auth.callout.duration" &&
      p.attributes["trellis.phase"] === "test_sanitize",
  );
  assert.deepEqual(points.length, 1);
  assert.deepEqual(points[0].attributes["trellis.outcome"], "ok");
  assert.deepEqual(points[0].attributes["trellis.session.kind"], "user");
  assert.deepEqual(points[0].attributes["trellis.participant.kind"], "device");
});

test("Sanitizes attributes — drops high-cardinality values", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?highcard=${ulid()}`
  );

  recordTrellisDuration("trellis.auth.callout.duration", 100, {
    phase: "test_highcard",
    outcome: "ok",
    participantKind: "550e8400-e29b-41d4-a716-446655440000",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.auth.callout.duration" &&
      p.attributes["trellis.phase"] === "test_highcard",
  );
  assert.deepEqual(points.length, 1);
  assert.deepEqual(points[0].attributes["trellis.outcome"], "ok");
  assert.deepEqual(points[0].attributes["trellis.participant.kind"], undefined);
});

test("Maps camelCase to OTel dot-separated names", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?camel=${ulid()}`
  );

  recordTrellisDuration("trellis.auth.flow.duration", 100, {
    phase: "test_camel",
    authFlow: "device",
    sessionKind: "device",
    authorityPresent: true,
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints().filter(
    (p) =>
      p.name === "trellis.auth.flow.duration" &&
      p.attributes["trellis.phase"] === "test_camel",
  );
  assert.deepEqual(points.length, 1);
  assert.deepEqual(points[0].attributes["trellis.auth.flow"], "device");
  assert.deepEqual(points[0].attributes["trellis.session.kind"], "device");
  assert.deepEqual(points[0].attributes["trellis.authority.present"], "true");
  assert.deepEqual(points[0].attributes["authFlow"], undefined);
  assert.deepEqual(points[0].attributes["sessionKind"], undefined);
  assert.deepEqual(points[0].attributes["authorityPresent"], undefined);
});

test("Duration recording preserves independent attribute series", async () => {
  exporter.reset();
  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?cache=${ulid()}`
  );

  recordTrellisDuration("trellis.connect.duration", 100, {
    phase: "cache_first",
  });
  recordTrellisDuration("trellis.connect.duration", 200, {
    phase: "cache_second",
  });
  recordTrellisDuration("trellis.auth.flow.duration", 300, {
    phase: "cache_other",
  });
  await reader.forceFlush();

  const points = collectHistogramDataPoints();
  const connectPoints = points.filter(
    (p) =>
      p.name === "trellis.connect.duration" &&
      p.attributes["trellis.phase"] === "cache_first",
  );
  const connectSecondPoints = points.filter(
    (p) =>
      p.name === "trellis.connect.duration" &&
      p.attributes["trellis.phase"] === "cache_second",
  );
  const authPoints = points.filter(
    (p) =>
      p.name === "trellis.auth.flow.duration" &&
      p.attributes["trellis.phase"] === "cache_other",
  );
  assert.deepEqual(connectPoints.length, 1);
  assert.deepEqual(connectSecondPoints.length, 1);
  assert.deepEqual(authPoints.length, 1);
});
