import assert from "node:assert/strict";
import { after, test } from "node:test";
import { headers } from "@nats-io/nats-core";
import { metrics, SpanKind } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import {
  InMemorySpanExporter,
  SimpleSpanProcessor,
} from "@opentelemetry/sdk-trace-base";
import { NodeTracerProvider } from "@opentelemetry/sdk-trace-node";
import {
  createNatsHeaderCarrier,
  extractTraceContext,
  injectTraceContext,
  startClientSpan,
  startServerSpan,
} from "../telemetry.ts";

const spans = new InMemorySpanExporter();
const tracer = new NodeTracerProvider({
  spanProcessors: [new SimpleSpanProcessor(spans)],
});
tracer.register();
const metricExporter = new InMemoryMetricExporter(
  AggregationTemporality.CUMULATIVE,
);
const meter = new MeterProvider({
  readers: [
    new PeriodicExportingMetricReader({
      exporter: metricExporter,
      exportIntervalMillis: 60_000,
    }),
  ],
});
metrics.setGlobalMeterProvider(meter);
after(() => Promise.all([tracer.shutdown(), meter.shutdown()]));

test("NATS trace headers preserve actual client/server parentage in exported spans", async () => {
  const client = startClientSpan("orders.Create");
  const carrier = createNatsHeaderCarrier(headers());
  injectTraceContext(carrier, client);
  const server = startServerSpan("orders.Create", extractTraceContext(carrier));
  server.end();
  client.end();
  await tracer.forceFlush();
  const exported = spans.getFinishedSpans();
  const clientRecord = exported.find(
    (span) => span.spanContext().spanId === client.spanContext().spanId,
  );
  const serverRecord = exported.find(
    (span) => span.spanContext().spanId === server.spanContext().spanId,
  );
  assert(clientRecord && serverRecord);
  assert.equal(clientRecord.kind, SpanKind.CLIENT);
  assert.equal(serverRecord.kind, SpanKind.SERVER);
  assert.equal(
    serverRecord.parentSpanContext?.spanId,
    clientRecord.spanContext().spanId,
  );
  assert.equal(
    serverRecord.spanContext().traceId,
    clientRecord.spanContext().traceId,
  );
  assert.notEqual(
    serverRecord.spanContext().spanId,
    clientRecord.spanContext().spanId,
  );
});

test("catalog route tokens and recorders stay bounded", async (t) => {
  const {
    recordCatalogCounter,
    recordCatalogDuration,
    recordCatalogUpDown,
    routeToken,
  } = await import("../telemetry.ts");

  await t.test("registered route labels remain stable", () => {
    const first = routeToken("rpc", "auth.Sessions.Me");
    assert.deepEqual(first, "auth.Sessions.Me");
    assert.deepEqual(routeToken("rpc", "auth.Sessions.Me"), first);
  });

  await t.test("overlong and overflow tokens map to _other", () => {
    assert.deepEqual(routeToken("http", "x".repeat(129)), "_other");
    for (let index = 0; index < 200; index += 1) {
      routeToken("job", `bounded.job.${index}`);
    }
    assert.deepEqual(routeToken("job", "bounded.job.overflow"), "_other");
  });

  await t.test("recorders accept only bounded attributes", async () => {
    recordCatalogDuration("trellis.rpc.client.duration", 12.5, {
      "trellis.route": "auth.Sessions.Me",
      "trellis.outcome": "ok",
      "messaging.destination": "rpc.v1.secret",
    });
    recordCatalogCounter("trellis.rpc.client.attempts", 1, {
      "trellis.route": "auth.Sessions.Me",
      "trellis.outcome": "ok",
    });
    recordCatalogUpDown("trellis.rpc.server.inflight", 1, {
      "trellis.route": "auth.Sessions.Me",
    });
    recordCatalogDuration("trellis.rpc.client.duration", 1, {
      "trellis.route": routeToken("rpc", "x".repeat(129)),
    });
    await meter.forceFlush();
    const attributes = metricExporter
      .getMetrics()
      .flatMap((resource) =>
        resource.scopeMetrics.flatMap((scope) =>
          scope.metrics.flatMap((metric) =>
            metric.dataPoints.map((point) => point.attributes),
          ),
        ),
      );
    assert(attributes.length > 0);
    for (const attrs of attributes)
      assert.equal(attrs["messaging.destination"], undefined);
    assert(
      attributes.some((attrs) => attrs["trellis.route"] === "auth.Sessions.Me"),
    );
    assert(attributes.some((attrs) => attrs["trellis.route"] === "_other"));
  });
});

test("browser endpoint sanitizer rejects normalization escapes", async (t) => {
  const { sanitizeBrowserEndpoint } = await import("../telemetry/browser.ts");

  await t.test("accepts the same-origin relative default", () => {
    assert.deepEqual(sanitizeBrowserEndpoint("/otel"), "/otel");
    assert.deepEqual(sanitizeBrowserEndpoint("/otel/"), "/otel");
  });

  await t.test("rejects cross-origin and normalization escapes", () => {
    assert.deepEqual(
      sanitizeBrowserEndpoint("//example.invalid/otel"),
      undefined,
    );
    assert.deepEqual(
      sanitizeBrowserEndpoint("/\t/example.invalid/otel"),
      undefined,
    );
    assert.deepEqual(
      sanitizeBrowserEndpoint("/\n/example.invalid/otel"),
      undefined,
    );
    assert.deepEqual(
      sanitizeBrowserEndpoint("https://example.invalid/otel"),
      undefined,
    );
    assert.deepEqual(sanitizeBrowserEndpoint("/otel?x=1"), undefined);
    assert.deepEqual(sanitizeBrowserEndpoint("/otel#frag"), undefined);
    assert.deepEqual(sanitizeBrowserEndpoint("/otel@host"), undefined);
    assert.deepEqual(sanitizeBrowserEndpoint("otel"), undefined);
    assert.deepEqual(sanitizeBrowserEndpoint(undefined), undefined);
  });
});

test("browser owner retains work after timeout and serializes shutdown", async () => {
  const { browserOwner } = await import("../telemetry/browser.ts");
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => (release = resolve));
  const events: string[] = [];
  const handle = browserOwner(
    {
      forceFlush: async () => {
        events.push("trace flush");
        await blocked;
      },
      shutdown: async () => {
        events.push("trace shutdown");
      },
    },
    {
      forceFlush: async () => {
        events.push("metric flush");
      },
      shutdown: async () => {
        events.push("metric shutdown");
      },
    },
  );
  void handle.forceFlush();
  await handle.forceFlush();
  const firstShutdown = handle.shutdown();
  const secondShutdown = handle.shutdown();
  try {
    assert.deepEqual(events, ["trace flush"]);
  } finally {
    release();
    await Promise.all([firstShutdown, secondShutdown]);
  }
  assert.deepEqual(events, [
    "trace flush",
    "trace shutdown",
    "metric shutdown",
  ]);
});

test("browser shutdown joins active work without starting its pending flush", async () => {
  const { browserOwner } = await import("../telemetry/browser.ts");
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => (release = resolve));
  const events: string[] = [];
  const handle = browserOwner(
    {
      forceFlush: async () => {
        events.push("trace flush");
        await blocked;
      },
      shutdown: async () => {
        events.push("trace shutdown");
      },
    },
    {
      forceFlush: async () => {
        events.push("metric flush");
      },
      shutdown: async () => {
        events.push("metric shutdown");
      },
    },
  );
  const flushing = handle.forceFlush();
  const firstShutdown = handle.shutdown();
  const secondShutdown = handle.shutdown();
  try {
    await handle.forceFlush();
    assert.deepEqual(events, ["trace flush"]);
  } finally {
    release();
    await Promise.all([flushing, firstShutdown, secondShutdown]);
  }
  assert.deepEqual(events, [
    "trace flush",
    "trace shutdown",
    "metric shutdown",
  ]);
});

test("browser owner coalesces a caller that stops waiting", async () => {
  const { browserOwner } = await import("../telemetry/browser.ts");
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => (release = resolve));
  let flushes = 0;
  const provider = {
    forceFlush: async () => {
      flushes++;
      await blocked;
    },
    shutdown: async () => {},
  };
  const handle = browserOwner(provider, provider);
  void handle.forceFlush();
  const joining = handle.forceFlush();
  assert.deepEqual(flushes, 1);
  release();
  await joining;
  assert.deepEqual(flushes, 2);
});
