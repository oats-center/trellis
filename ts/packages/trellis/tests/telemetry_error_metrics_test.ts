import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "npm:@opentelemetry/sdk-metrics@^2.7.0";
import { assertEquals, assertExists } from "@std/assert";
import { AsyncResult, err } from "@qlever-llc/result";
import type { NatsConnection } from "@nats-io/nats-core";
import { Type } from "typebox";
import { ulid } from "ulid";
import { AuthError, UnexpectedError } from "../errors/index.ts";
import type { PreparedOutboxRecord } from "../service/outbox_inbox.ts";

const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
const reader = new PeriodicExportingMetricReader({
  exporter,
  exportIntervalMillis: 60_000,
});
metrics.setGlobalMeterProvider(new MeterProvider({ readers: [reader] }));

Deno.test("recordTrellisError emits sanitized low-cardinality attributes", async () => {
  exporter.reset();
  const { buildTrellisErrorMetricAttributes, recordTrellisError } =
    await import(
      `../telemetry/metrics.ts?sanitize=${ulid()}`
    );
  const error = new SerializableRemoteMetricError();

  const attributes = buildTrellisErrorMetricAttributes(error, {
    surface: "rpc",
    direction: "client",
    operation: "rpc.v1.Account.abc123/with-path",
    phase: "remote_error",
    authReason: "user@example.com",
  });

  assertEquals(attributes["exception.type"], "RemoteError");
  assertEquals(attributes["trellis.error.type"], "RemoteError");
  assertEquals(attributes["trellis.remote_error.type"], "AuthError");
  assertEquals(attributes["trellis.surface"], "rpc");
  assertEquals(attributes["trellis.direction"], "client");
  assertEquals(attributes["trellis.phase"], "remote_error");
  assertEquals(attributes["trellis.operation"], undefined);
  assertEquals(attributes["trellis.auth.reason"], undefined);

  assertEquals(
    buildTrellisErrorMetricAttributes(error, {
      operation: "rpc.v1.Account.01HZYJ3SM8Q7C6DBK5PR7H4YBM",
    })["trellis.operation"],
    undefined,
  );
  assertEquals(
    buildTrellisErrorMetricAttributes(error, {
      operation: "550e8400-e29b-41d4-a716-446655440000",
    })["trellis.operation"],
    undefined,
  );
  const hostileError = Object.defineProperties({}, {
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
  });
  assertEquals(
    buildTrellisErrorMetricAttributes(hostileError, {
      surface: "rpc",
      direction: "client",
      phase: "getter_safety",
    })["trellis.error.type"],
    "unknown",
  );

  const authAttributes = buildTrellisErrorMetricAttributes(
    new AuthError({ reason: "missing_session_key" }),
  );
  assertEquals(authAttributes["trellis.auth.reason"], "missing_session_key");

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
  assertExists(dataPoint);
  assertEquals(dataPoint.attributes["trellis.remote_error.type"], "AuthError");
  assertExists(findTrellisErrorDataPoint({
    "trellis.surface": "rpc",
    "trellis.phase": "getter_safety",
  }));
});

Deno.test("RPC client template failures record trellis error metric", async () => {
  exporter.reset();
  const { Trellis } = await import(`../session.ts?rpc=${ulid()}`);
  const api = {
    rpc: {
      "Test.Echo": {
        subject: "rpc.v1.Test.Echo.{id}",
        permission: Object.freeze({
          apiId: "test@v1",
          apiVersion: "v1",
          surfaceKind: "rpc",
          surfaceName: "Test.Echo",
          action: "call",
        }),
        input: Type.Object({}),
        output: Type.Object({ ok: Type.Boolean() }),
        callerCapabilities: [],
      },
    },
    events: {},
  } as const;
  const runtime = new Trellis(
    "test-client",
    fakeNatsConnection(),
    {
      sessionKey: "test-session-key",
      sign: () => new Uint8Array(),
    },
    { api },
  );

  await runtime.request("Test.Echo", {}).take();
  await reader.forceFlush();

  const dataPoint = findTrellisErrorDataPoint({
    "trellis.surface": "rpc",
    "trellis.direction": "client",
    "trellis.operation": "Test.Echo",
    "trellis.phase": "request_encoding",
  });
  assertExists(dataPoint);
  assertEquals(dataPoint.attributes["trellis.error.type"], "ValidationError");
});

Deno.test("Feed client template failures record trellis error metric", async () => {
  exporter.reset();
  const { Trellis } = await import(`../session.ts?feed=${ulid()}`);
  const api = {
    rpc: {},
    events: {},
    feeds: {
      "Test.Stream": {
        subject: "feeds.v1.Test.Stream.{id}",
        permission: Object.freeze({
          apiId: "test@v1",
          apiVersion: "v1",
          surfaceKind: "feed",
          surfaceName: "Test.Stream",
          action: "subscribe",
        }),
        input: Type.Object({}),
        event: Type.Object({ ok: Type.Boolean() }),
        subscribeCapabilities: [],
      },
    },
  } as const;
  const runtime = new Trellis(
    "test-client",
    fakeNatsConnection(),
    {
      sessionKey: "test-session-key",
      sign: () => new Uint8Array(),
    },
    { api },
  );

  await runtime.feedHandle("Test.Stream").input({}).subscribe().take();
  await reader.forceFlush();

  const dataPoint = findTrellisErrorDataPoint({
    "trellis.surface": "feed",
    "trellis.direction": "client",
    "trellis.operation": "Test.Stream",
    "trellis.phase": "request_template",
  });
  assertExists(dataPoint);
  assertEquals(dataPoint.attributes["trellis.error.type"], "ValidationError");
});

Deno.test("dispatchOutbox records failed publish hook", async () => {
  exporter.reset();
  const { dispatchOutbox, MemoryOutboxRepository } = await import(
    `../service/outbox_inbox.ts?outbox=${ulid()}`
  );
  const repository = new MemoryOutboxRepository();
  await repository.enqueue(prepared("evt_failed"));

  await dispatchOutbox(repository, {
    publishPreparedEvent: () =>
      AsyncResult.from(Promise.resolve(
        err(new UnexpectedError({ cause: new Error("user 123 failed") })),
      )),
  }, {
    now: new Date("2026-05-25T00:00:00.000Z"),
  });
  await reader.forceFlush();

  const dataPoint = findTrellisErrorDataPoint({
    "trellis.surface": "outbox",
    "trellis.direction": "dispatcher",
    "trellis.operation": "Thing.Changed",
    "trellis.phase": "publish",
  });
  assertExists(dataPoint);
  assertEquals(dataPoint.attributes["trellis.error.type"], "UnexpectedError");
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

function fakeNatsConnection(): NatsConnection & {
  options: { inboxPrefix: string };
} {
  return {
    options: { inboxPrefix: "_INBOX.test" },
    info: undefined,
    closed: async () => undefined,
    close: async () => undefined,
    publish: () => {},
    publishMessage: () => {},
    respondMessage: () => true,
    subscribe: () => {
      throw new Error("not used by this test");
    },
    request: async () => {
      throw new Error("not used by this test");
    },
    requestMany: async () =>
      (async function* () {
        return;
      })(),
    flush: async () => {},
    drain: async () => undefined,
    isClosed: () => false,
    isDraining: () => false,
    getServer: () => "nats://127.0.0.1:4222",
    status: () => ({
      async *[Symbol.asyncIterator]() {},
    }),
    stats: () => ({ inBytes: 0, outBytes: 0, inMsgs: 0, outMsgs: 0 }),
    rtt: async () => 0,
    reconnect: async () => {},
    setServers: () => {},
    getServers: () => [],
    [Symbol.asyncDispose]: async () => {},
  };
}

function prepared(id: string): PreparedOutboxRecord {
  const payload = JSON.stringify({
    header: { id, time: "2026-05-25T00:00:00.000Z" },
    value: "test",
  });
  return {
    id,
    kind: "event.publish",
    name: "Thing.Changed",
    subject: "events.v1.Thing.Changed.user.123",
    payload,
    headers: {
      "Nats-Msg-Id": id,
      "Trellis-Event-Descriptor": "v1.dGVzdEB2MQ.Q29ubmVjdGlvbnMuT3BlbmVk.0",
    },
  };
}

type MetricDataPoint = {
  attributes: Record<string, unknown>;
  value: unknown;
};

function findTrellisErrorDataPoint(
  expected: Record<string, string>,
): MetricDataPoint | undefined {
  return trellisErrorDataPoints().find((dataPoint) =>
    Object.entries(expected).every(([key, value]) =>
      dataPoint.attributes[key] === value
    )
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
