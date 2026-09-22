import { metrics } from "@opentelemetry/api";
import { getEnv } from "./env.ts";

let initialized = false;
let provider: { register(): void } | undefined;
let metricProvider: unknown;

type TracingRuntimeModules = {
  NodeTracerProvider:
    typeof import("@opentelemetry/sdk-trace-node").NodeTracerProvider;
  OTLPTraceExporter:
    typeof import("@opentelemetry/exporter-trace-otlp-proto").OTLPTraceExporter;
  BatchSpanProcessor:
    typeof import("@opentelemetry/sdk-trace-base").BatchSpanProcessor;
  ConsoleSpanExporter:
    typeof import("@opentelemetry/sdk-trace-base").ConsoleSpanExporter;
  resourceFromAttributes:
    typeof import("@opentelemetry/resources").resourceFromAttributes;
  ATTR_SERVICE_NAME:
    typeof import("@opentelemetry/semantic-conventions").ATTR_SERVICE_NAME;
};

type MetricsRuntimeModules = {
  MeterProvider: typeof import("@opentelemetry/sdk-metrics").MeterProvider;
  PeriodicExportingMetricReader:
    typeof import("@opentelemetry/sdk-metrics").PeriodicExportingMetricReader;
  ConsoleMetricExporter:
    typeof import("@opentelemetry/sdk-metrics").ConsoleMetricExporter;
  OTLPMetricExporter:
    typeof import("@opentelemetry/exporter-metrics-otlp-proto").OTLPMetricExporter;
  resourceFromAttributes:
    typeof import("@opentelemetry/resources").resourceFromAttributes;
  ATTR_SERVICE_NAME:
    typeof import("@opentelemetry/semantic-conventions").ATTR_SERVICE_NAME;
};

function runtimeImport<TModule>(specifier: string): Promise<TModule> {
  return import(specifier) as Promise<TModule>;
}

async function loadTracingRuntime(): Promise<TracingRuntimeModules> {
  const [traceNode, otlp, traceBase, resources, semantic] = await Promise.all([
    runtimeImport<typeof import("@opentelemetry/sdk-trace-node")>(
      ["@opentelemetry", "sdk-trace-node"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/exporter-trace-otlp-proto")>(
      ["@opentelemetry", "exporter-trace-otlp-proto"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/sdk-trace-base")>(
      ["@opentelemetry", "sdk-trace-base"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/resources")>(
      ["@opentelemetry", "resources"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/semantic-conventions")>(
      ["@opentelemetry", "semantic-conventions"].join("/"),
    ),
  ]);

  return {
    NodeTracerProvider: traceNode.NodeTracerProvider,
    OTLPTraceExporter: otlp.OTLPTraceExporter,
    BatchSpanProcessor: traceBase.BatchSpanProcessor,
    ConsoleSpanExporter: traceBase.ConsoleSpanExporter,
    resourceFromAttributes: resources.resourceFromAttributes,
    ATTR_SERVICE_NAME: semantic.ATTR_SERVICE_NAME,
  };
}

async function loadMetricsRuntime(): Promise<MetricsRuntimeModules> {
  const [sdkMetrics, otlp, resources, semantic] = await Promise.all([
    runtimeImport<typeof import("@opentelemetry/sdk-metrics")>(
      ["@opentelemetry", "sdk-metrics"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/exporter-metrics-otlp-proto")>(
      ["@opentelemetry", "exporter-metrics-otlp-proto"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/resources")>(
      ["@opentelemetry", "resources"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/semantic-conventions")>(
      ["@opentelemetry", "semantic-conventions"].join("/"),
    ),
  ]);

  return {
    MeterProvider: sdkMetrics.MeterProvider,
    PeriodicExportingMetricReader: sdkMetrics.PeriodicExportingMetricReader,
    ConsoleMetricExporter: sdkMetrics.ConsoleMetricExporter,
    OTLPMetricExporter: otlp.OTLPMetricExporter,
    resourceFromAttributes: resources.resourceFromAttributes,
    ATTR_SERVICE_NAME: semantic.ATTR_SERVICE_NAME,
  };
}

async function initTracingRuntime(serviceName: string): Promise<void> {
  const endpoint = getEnv("OTEL_EXPORTER_OTLP_ENDPOINT");
  const consoleTracing = getEnv("OTEL_TRACES_CONSOLE") === "true";

  if (!endpoint && !consoleTracing) {
    return;
  }

  try {
    const runtime = await loadTracingRuntime();
    const spanProcessors: Array<
      InstanceType<typeof runtime.BatchSpanProcessor>
    > = [];

    if (endpoint) {
      spanProcessors.push(
        new runtime.BatchSpanProcessor(
          new runtime.OTLPTraceExporter({ url: `${endpoint}/v1/traces` }),
        ),
      );
    }

    if (consoleTracing) {
      spanProcessors.push(
        new runtime.BatchSpanProcessor(new runtime.ConsoleSpanExporter()),
      );
    }

    const nextProvider = new runtime.NodeTracerProvider({
      resource: runtime.resourceFromAttributes({
        [runtime.ATTR_SERVICE_NAME]: serviceName,
      }),
      ...(spanProcessors.length > 0 ? { spanProcessors } : {}),
    });

    provider = nextProvider;
    nextProvider.register();
  } catch (error) {
    console.warn("Failed to initialize tracing runtime", error);
  }
}

function metricEndpoint(): string | undefined {
  const metricsEndpoint = getEnv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
  if (metricsEndpoint) return metricsEndpoint;

  const endpoint = getEnv("OTEL_EXPORTER_OTLP_ENDPOINT");
  if (!endpoint) return undefined;

  return `${endpoint.replace(/\/$/, "")}/v1/metrics`;
}

function metricExportIntervalMillis(): number | undefined {
  const configured = getEnv("OTEL_METRIC_EXPORT_INTERVAL");
  if (!configured) return undefined;

  const parsed = Number(configured);
  if (!Number.isFinite(parsed) || parsed <= 0) return undefined;

  return Math.trunc(parsed);
}

async function initMetricsRuntime(serviceName: string): Promise<void> {
  const endpoint = metricEndpoint();
  const consoleMetrics = getEnv("TRELLIS_METRICS_CONSOLE") === "true";

  if (!endpoint && !consoleMetrics) {
    return;
  }

  try {
    const runtime = await loadMetricsRuntime();
    const readers: Array<
      InstanceType<typeof runtime.PeriodicExportingMetricReader>
    > = [];
    const exportIntervalMillis = metricExportIntervalMillis();

    if (endpoint) {
      readers.push(
        new runtime.PeriodicExportingMetricReader({
          exporter: new runtime.OTLPMetricExporter({ url: endpoint }),
          ...(exportIntervalMillis !== undefined
            ? { exportIntervalMillis }
            : {}),
        }),
      );
    }

    if (consoleMetrics) {
      readers.push(
        new runtime.PeriodicExportingMetricReader({
          exporter: new runtime.ConsoleMetricExporter(),
          ...(exportIntervalMillis !== undefined
            ? { exportIntervalMillis }
            : {}),
        }),
      );
    }

    const nextMetricProvider = new runtime.MeterProvider({
      resource: runtime.resourceFromAttributes({
        [runtime.ATTR_SERVICE_NAME]: serviceName,
      }),
      ...(readers.length > 0 ? { readers } : {}),
    });

    metricProvider = nextMetricProvider;
    metrics.setGlobalMeterProvider(nextMetricProvider);
  } catch (error) {
    console.warn("Failed to initialize metrics runtime", error);
  }
}

export function initTelemetryRuntime(serviceName: string): void {
  if (initialized) return;
  initialized = true;

  void initTracingRuntime(serviceName);
  void initMetricsRuntime(serviceName);
}
