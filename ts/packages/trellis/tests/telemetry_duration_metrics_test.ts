import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";
import { assertEquals, assertExists } from "@std/assert";
import { ulid } from "ulid";

const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
const reader = new PeriodicExportingMetricReader({
  exporter,
  exportIntervalMillis: 60_000,
});
metrics.setGlobalMeterProvider(new MeterProvider({ readers: [reader] }));

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

Deno.test("Records histogram value and unit", async () => {
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
  assertExists(points[0]);
  assertEquals(points[0].unit, "s");
  assertEquals(points[0].value.count, 1);
  assertEquals(points[0].value.sum, 0.15);
});

Deno.test("Ignores negative duration", async () => {
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
  assertEquals(points.length, 0);
});

Deno.test("Ignores NaN duration", async () => {
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
  assertEquals(points.length, 0);
});

Deno.test("Ignores infinite duration", async () => {
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
  assertEquals(points.length, 0);
});

Deno.test("Sanitizes attributes — drops bad values, keeps good ones", async () => {
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
  assertEquals(points.length, 1);
  assertEquals(points[0].attributes["trellis.outcome"], "ok");
  assertEquals(points[0].attributes["trellis.session.kind"], "user");
  assertEquals(points[0].attributes["trellis.participant.kind"], "device");
});

Deno.test("Sanitizes attributes — drops high-cardinality values", async () => {
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
  assertEquals(points.length, 1);
  assertEquals(points[0].attributes["trellis.outcome"], "ok");
  assertEquals(points[0].attributes["trellis.participant.kind"], undefined);
});

Deno.test("Maps camelCase to OTel dot-separated names", async () => {
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
  assertEquals(points.length, 1);
  assertEquals(points[0].attributes["trellis.auth.flow"], "device");
  assertEquals(points[0].attributes["trellis.session.kind"], "device");
  assertEquals(points[0].attributes["trellis.authority.present"], "true");
  assertEquals(points[0].attributes["authFlow"], undefined);
  assertEquals(points[0].attributes["sessionKind"], undefined);
  assertEquals(points[0].attributes["authorityPresent"], undefined);
});

Deno.test("Caches histogram instrument per metric name", async () => {
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
  assertEquals(connectPoints.length, 1);
  assertEquals(connectSecondPoints.length, 1);
  assertEquals(authPoints.length, 1);
});

Deno.test("No-op without meter provider does not throw", async () => {
  metrics.setGlobalMeterProvider(new MeterProvider());

  const { recordTrellisDuration } = await import(
    `../telemetry/metrics.ts?noop=${ulid()}`
  );

  recordTrellisDuration("trellis.connect.duration", 100);
});
