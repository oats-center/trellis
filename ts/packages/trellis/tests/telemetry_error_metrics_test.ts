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
import { AuthError } from "../errors/index.ts";

const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
const reader = new PeriodicExportingMetricReader({
  exporter,
  exportIntervalMillis: 60_000,
});
const provider = new MeterProvider({ readers: [reader] });
metrics.setGlobalMeterProvider(provider);
after(() => provider.shutdown());

test("recordTrellisError emits sanitized low-cardinality attributes", async () => {
  exporter.reset();
  const { buildTrellisErrorMetricAttributes, recordTrellisError } =
    await import(`../telemetry/metrics.ts?sanitize=${ulid()}`);
  const error = new SerializableRemoteMetricError();

  const attributes = buildTrellisErrorMetricAttributes(error, {
    surface: "rpc",
    direction: "client",
    operation: "rpc.v1.Account.abc123/with-path",
    phase: "remote_error",
    authReason: "user@example.com",
  });

  assert.deepEqual(attributes["exception.type"], "RemoteError");
  assert.deepEqual(attributes["trellis.error.type"], "RemoteError");
  assert.deepEqual(attributes["trellis.remote_error.type"], "AuthError");
  assert.deepEqual(attributes["trellis.surface"], "rpc");
  assert.deepEqual(attributes["trellis.direction"], "client");
  assert.deepEqual(attributes["trellis.phase"], "remote_error");
  assert.deepEqual(attributes["trellis.operation"], undefined);
  assert.deepEqual(attributes["trellis.auth.reason"], undefined);

  assert.deepEqual(
    buildTrellisErrorMetricAttributes(error, {
      operation: "rpc.v1.Account.01HZYJ3SM8Q7C6DBK5PR7H4YBM",
    })["trellis.operation"],
    undefined,
  );
  assert.deepEqual(
    buildTrellisErrorMetricAttributes(error, {
      operation: "550e8400-e29b-41d4-a716-446655440000",
    })["trellis.operation"],
    undefined,
  );
  const hostileError = Object.defineProperties(
    {},
    {
      type: {
        get() {
          throw new Error("type getter should not escape telemetry");
        },
      },
      toSerializable: {
        get() {
          throw new Error("toSerializable getter should not escape telemetry");
        },
      },
    },
  );
  assert.deepEqual(
    buildTrellisErrorMetricAttributes(hostileError, {
      surface: "rpc",
      direction: "client",
      phase: "getter_safety",
    })["trellis.error.type"],
    "unknown",
  );

  const authAttributes = buildTrellisErrorMetricAttributes(
    new AuthError({ reason: "session_expired" }),
  );
  assert.deepEqual(authAttributes["trellis.auth.reason"], "session_expired");

  recordTrellisError(error, {
    surface: "rpc",
    direction: "client",
    phase: "remote_error",
  });
  recordTrellisError(hostileError, {
    surface: "rpc",
    direction: "client",
    phase: "getter_safety",
  });
  await reader.forceFlush();

  const dataPoint = findTrellisErrorDataPoint({
    "trellis.surface": "rpc",
    "trellis.phase": "remote_error",
  });
  assert(dataPoint);
  assert.deepEqual(
    dataPoint.attributes["trellis.remote_error.type"],
    "AuthError",
  );
  assert(
    findTrellisErrorDataPoint({
      "trellis.surface": "rpc",
      "trellis.phase": "getter_safety",
    }),
  );
});

class SerializableRemoteMetricError extends Error {
  override name = "RemoteError";

  constructor() {
    super("user 123 failed on rpc.v1.Account.abc123");
  }

  toSerializable() {
    return {
      type: "RemoteError",
      message: "user 123 failed on rpc.v1.Account.abc123",
      remoteError: { type: "AuthError" },
      context: {
        reason: "missing_session_key",
        sessionKey: "secret-session-key",
      },
    };
  }
}

type MetricDataPoint = {
  attributes: Record<string, unknown>;
  value: unknown;
};

function findTrellisErrorDataPoint(
  expected: Record<string, string>,
): MetricDataPoint | undefined {
  return trellisErrorDataPoints().find((dataPoint) =>
    Object.entries(expected).every(
      ([key, value]) => dataPoint.attributes[key] === value,
    ),
  );
}

function trellisErrorDataPoints(): MetricDataPoint[] {
  const points: MetricDataPoint[] = [];
  for (const resourceMetrics of exporter.getMetrics()) {
    for (const scopeMetric of resourceMetrics.scopeMetrics) {
      for (const metric of scopeMetric.metrics) {
        if (metric.descriptor.name !== "trellis.errors") continue;
        for (const dataPoint of metric.dataPoints) {
          points.push({
            attributes: dataPoint.attributes,
            value: dataPoint.value,
          });
        }
      }
    }
  }
  return points;
}
