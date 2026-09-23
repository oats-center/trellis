import { context, metrics, propagation, trace } from "@opentelemetry/api";
import { getEnv } from "./env.ts";

/** Native process roles accepted by the telemetry owner. */
export type TelemetryRole = "server" | "cli" | "service" | "device";

/** Process identity defaults for one telemetry owner. */
export interface TelemetryIdentity {
  /** Stable service name for the deployment. */
  serviceName: string;
  /** Optional service version reported as `service.version`. */
  serviceVersion?: string;
  /** Role of the process that owns the providers. */
  role: TelemetryRole;
}

/** Process telemetry handle returned by the initialization owner. */
export interface TelemetryRuntimeHandle {
  /** Whether managed traces are active. */
  readonly traces: boolean;
  /** Whether managed metrics are active. */
  readonly metrics: boolean;
  /** Flushes owned providers; never rejects business work. */
  forceFlush(): Promise<void>;
  /** Flushes and shuts down owned providers once. */
  shutdown(): Promise<void>;
}

const DISABLED_HANDLE: TelemetryRuntimeHandle = {
  traces: false,
  metrics: false,
  forceFlush: () => Promise.resolve(),
  shutdown: () => Promise.resolve(),
};

type TracingRuntimeModules = {
  NodeTracerProvider:
    typeof import("@opentelemetry/sdk-trace-node").NodeTracerProvider;
  ParentBasedSampler:
    typeof import("@opentelemetry/sdk-trace-base").ParentBasedSampler;
  TraceIdRatioBasedSampler:
    typeof import("@opentelemetry/sdk-trace-base").TraceIdRatioBasedSampler;
  AlwaysOnSampler:
    typeof import("@opentelemetry/sdk-trace-base").AlwaysOnSampler;
  AlwaysOffSampler:
    typeof import("@opentelemetry/sdk-trace-base").AlwaysOffSampler;
  OTLPTraceExporter:
    typeof import("@opentelemetry/exporter-trace-otlp-proto").OTLPTraceExporter;
  BatchSpanProcessor:
    typeof import("@opentelemetry/sdk-trace-base").BatchSpanProcessor;
  ConsoleSpanExporter:
    typeof import("@opentelemetry/sdk-trace-base").ConsoleSpanExporter;
  resourceFromAttributes:
    typeof import("@opentelemetry/resources").resourceFromAttributes;
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
};

/** Default OpenTelemetry export timeout when the environment does not set one. */
const DEFAULT_EXPORT_TIMEOUT_MILLIS = 3_000;
/** Metrics export interval from the observability order. */
const METRICS_INTERVAL_MILLIS = 15_000;
/** Default parent-based trace ratio when the environment does not set one. */
const DEFAULT_TRACE_RATIO = 0.1;
/** Trace batch delay from the observability order. */
const TRACE_BATCH_DELAY_MILLIS = 5_000;
/** Trace queue capacity from the observability order. */
const TRACE_QUEUE_SIZE = 2_048;
/** Trace batch maximum from the observability order. */
const TRACE_BATCH_MAX = 256;

let owner: Promise<TelemetryRuntimeHandle> | undefined;
let ownedTraces = false;
let ownedMetrics = false;
let shutdownDone = false;
let claimedServiceName: string | undefined;
let identityWarned = false;
let envWarned = false;

/** Reads one non-empty environment value. */
function envValue(key: string): string | undefined {
  const value = getEnv(key)?.trim();
  return value ? value : undefined;
}

/** Parses one bounded positive environment duration in milliseconds. */
function positiveEnvMillis(key: string): number | undefined {
  const value = envValue(key);
  if (!value) return undefined;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) return undefined;
  return Math.trunc(parsed);
}

/** Emits one bounded warning for a repeated environment problem. */
function warnEnvOnce(message: string): void {
  if (!envWarned) {
    envWarned = true;
    console.warn(`trellis telemetry: ${message}`);
  }
}

/** Whether the named exporter variable suppresses its signal. */
function exporterDisabled(key: string): boolean {
  return (envValue(key) ?? "").toLowerCase() === "none";
}

/** Resolves the trace exporter endpoint; the specific endpoint wins exactly. */
function tracesEndpoint(): string | undefined {
  const specific = envValue("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
  if (specific) return specific;
  const shared = envValue("OTEL_EXPORTER_OTLP_ENDPOINT");
  return shared ? `${shared.replace(/\/$/, "")}/v1/traces` : undefined;
}

/** Resolves the metric exporter endpoint; the specific endpoint wins exactly. */
function metricsEndpoint(): string | undefined {
  const specific = envValue("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
  if (specific) return specific;
  const shared = envValue("OTEL_EXPORTER_OTLP_ENDPOINT");
  return shared ? `${shared.replace(/\/$/, "")}/v1/metrics` : undefined;
}

/** Resolves the export timeout, honoring standard overrides. */
function exportTimeoutMillis(signal: "TRACES" | "METRICS"): number {
  return (
    positiveEnvMillis(`OTEL_EXPORTER_OTLP_${signal}_TIMEOUT`) ??
      positiveEnvMillis("OTEL_EXPORTER_OTLP_TIMEOUT") ??
      DEFAULT_EXPORT_TIMEOUT_MILLIS
  );
}

/** Parses `OTEL_TRACES_SAMPLER_ARG` into a bounded ratio. */
function samplerRatio(): number {
  const value = envValue("OTEL_TRACES_SAMPLER_ARG");
  if (!value) return DEFAULT_TRACE_RATIO;
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0 || parsed > 1) {
    return DEFAULT_TRACE_RATIO;
  }
  return parsed;
}

/** Resolves the process resource attributes with environment overrides. */
function resourceAttributes(
  identity: TelemetryIdentity,
): Record<string, string> {
  const attributes: Record<string, string> = {
    "service.name": identity.serviceName,
    "service.version": identity.serviceVersion ?? "0.0.0",
    "service.namespace": "trellis",
    "deployment.environment.name": envValue("TRELLIS_ENVIRONMENT") ??
      "development",
    "trellis.cluster": envValue("TRELLIS_CLUSTER") ?? "local",
    "trellis.role": identity.role,
  };
  const serviceName = envValue("OTEL_SERVICE_NAME");
  if (serviceName) attributes["service.name"] = serviceName;
  const extra = envValue("OTEL_RESOURCE_ATTRIBUTES");
  if (extra) {
    for (const member of extra.split(",")) {
      const separator = member.indexOf("=");
      if (separator <= 0) continue;
      const key = member.slice(0, separator).trim();
      const value = member.slice(separator + 1).trim();
      if (key && value) attributes[key] = value;
    }
  }
  // Process identity honors a configured value, otherwise it is generated
  // exactly once per process and is never a credential.
  if (attributes["service.instance.id"] === undefined) {
    attributes["service.instance.id"] = crypto.randomUUID();
  }
  return attributes;
}

/** Dynamic import that avoids bundler resolution of native OTel modules. */
function runtimeImport<TModule>(specifier: string): Promise<TModule> {
  return import(specifier) as Promise<TModule>;
}

async function loadTracingRuntime(): Promise<TracingRuntimeModules> {
  const [traceNode, otlp, traceBase, resources] = await Promise.all([
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
  ]);
  return {
    NodeTracerProvider: traceNode.NodeTracerProvider,
    ParentBasedSampler: traceBase.ParentBasedSampler,
    TraceIdRatioBasedSampler: traceBase.TraceIdRatioBasedSampler,
    AlwaysOnSampler: traceBase.AlwaysOnSampler,
    AlwaysOffSampler: traceBase.AlwaysOffSampler,
    OTLPTraceExporter: otlp.OTLPTraceExporter,
    BatchSpanProcessor: traceBase.BatchSpanProcessor,
    ConsoleSpanExporter: traceBase.ConsoleSpanExporter,
    resourceFromAttributes: resources.resourceFromAttributes,
  };
}

async function loadMetricsRuntime(): Promise<MetricsRuntimeModules> {
  const [sdkMetrics, otlp, resources] = await Promise.all([
    runtimeImport<typeof import("@opentelemetry/sdk-metrics")>(
      ["@opentelemetry", "sdk-metrics"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/exporter-metrics-otlp-proto")>(
      ["@opentelemetry", "exporter-metrics-otlp-proto"].join("/"),
    ),
    runtimeImport<typeof import("@opentelemetry/resources")>(
      ["@opentelemetry", "resources"].join("/"),
    ),
  ]);
  return {
    MeterProvider: sdkMetrics.MeterProvider,
    PeriodicExportingMetricReader: sdkMetrics.PeriodicExportingMetricReader,
    ConsoleMetricExporter: sdkMetrics.ConsoleMetricExporter,
    OTLPMetricExporter: otlp.OTLPMetricExporter,
    resourceFromAttributes: resources.resourceFromAttributes,
  };
}

/** Builds the environment-selected trace sampler with a 0.10 default. */
function samplerFromEnv(runtime: TracingRuntimeModules) {
  const ratio = samplerRatio();
  const parentBased = () =>
    new runtime.ParentBasedSampler({
      root: new runtime.TraceIdRatioBasedSampler(ratio),
    });
  const raw = envValue("OTEL_TRACES_SAMPLER");
  switch (raw) {
    case undefined:
      return parentBased();
    case "always_on":
      return new runtime.AlwaysOnSampler();
    case "always_off":
      return new runtime.AlwaysOffSampler();
    case "traceidratio":
      return new runtime.TraceIdRatioBasedSampler(ratio);
    case "parentbased_traceidratio":
      return parentBased();
    default:
      warnEnvOnce(
        `unknown OTEL_TRACES_SAMPLER '${raw}'; using parentbased_traceidratio ${DEFAULT_TRACE_RATIO}`,
      );
      return parentBased();
  }
}

/** Bounded process telemetry flush/shutdown budget in milliseconds. */
const TELEMETRY_BUDGET_MILLIS = 5_000;

/** One owned signal provider with its bounded flush/shutdown operations. */
type OwnedSignalProvider = {
  forceFlush(): Promise<void>;
  shutdown(): Promise<void>;
};

/** Resolves with the provider operation or the budget, never rejects. */
async function withinBudget(operation: () => Promise<void>): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let invoke: Promise<void>;
  try {
    invoke = operation();
  } catch (error) {
    // A synchronous invocation failure is a bounded provider failure, not a
    // process failure.
    console.warn("trellis telemetry: provider operation failed", error);
    return;
  }
  const started = invoke.catch((error) => {
    console.warn("trellis telemetry: provider operation failed", error);
  });
  const timeout = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, TELEMETRY_BUDGET_MILLIS);
  });
  try {
    await Promise.race([started, timeout]);
  } finally {
    // Never leave a pending timer after a successful owner operation.
    if (timer !== undefined) clearTimeout(timer);
  }
}

/**
 * Installs owned providers and returns the process handle.
 *
 * Trace and metric initialization are independent: if one signal fails, the
 * successful provider is retained and its handle is returned. Provider
 * operations resolve within the process budget and never reject.
 */
async function initialize(
  identity: TelemetryIdentity,
): Promise<TelemetryRuntimeHandle> {
  if (shutdownDone) return DISABLED_HANDLE;
  if ((envValue("OTEL_SDK_DISABLED") ?? "").toLowerCase() === "true") {
    return DISABLED_HANDLE;
  }
  const tracesEnabled = !exporterDisabled("OTEL_TRACES_EXPORTER") &&
    tracesEndpoint() !== undefined;
  const metricsEnabled = !exporterDisabled("OTEL_METRICS_EXPORTER") &&
    metricsEndpoint() !== undefined;
  if (!tracesEnabled && !metricsEnabled) return DISABLED_HANDLE;

  const attributes = resourceAttributes(identity);
  const owned: {
    tracerProvider?: OwnedSignalProvider;
    meterProvider?: OwnedSignalProvider;
  } = {};

  if (tracesEnabled) {
    try {
      const runtime = await loadTracingRuntime();
      const endpoint = tracesEndpoint();
      const spanProcessors = endpoint
        ? [
          new runtime.BatchSpanProcessor(
            new runtime.OTLPTraceExporter({
              url: endpoint,
              timeoutMillis: exportTimeoutMillis("TRACES"),
            }),
            {
              maxQueueSize: TRACE_QUEUE_SIZE,
              maxExportBatchSize: TRACE_BATCH_MAX,
              scheduledDelayMillis: TRACE_BATCH_DELAY_MILLIS,
            },
          ),
        ]
        : [];
      if (getEnv("OTEL_TRACES_CONSOLE") === "true") {
        spanProcessors.push(
          new runtime.BatchSpanProcessor(new runtime.ConsoleSpanExporter()),
        );
      }
      const provider = new runtime.NodeTracerProvider({
        resource: runtime.resourceFromAttributes(attributes),
        sampler: samplerFromEnv(runtime),
        ...(spanProcessors.length > 0 ? { spanProcessors } : {}),
      });
      trace.setGlobalTracerProvider(provider);
      owned.tracerProvider = provider;
      ownedTraces = true;
    } catch (error) {
      console.warn("trellis telemetry: trace export disabled", error);
    }
  }

  if (metricsEnabled) {
    try {
      const runtime = await loadMetricsRuntime();
      const endpoint = metricsEndpoint();
      const readers = endpoint
        ? [
          new runtime.PeriodicExportingMetricReader({
            exporter: new runtime.OTLPMetricExporter({
              url: endpoint,
              timeoutMillis: exportTimeoutMillis("METRICS"),
            }),
            exportIntervalMillis:
              positiveEnvMillis("OTEL_METRIC_EXPORT_INTERVAL") ??
                METRICS_INTERVAL_MILLIS,
          }),
        ]
        : [];
      if (getEnv("TRELLIS_METRICS_CONSOLE") === "true") {
        readers.push(
          new runtime.PeriodicExportingMetricReader({
            exporter: new runtime.ConsoleMetricExporter(),
          }),
        );
      }
      const meterProvider = new runtime.MeterProvider({
        resource: runtime.resourceFromAttributes(attributes),
        ...(readers.length > 0 ? { readers } : {}),
      });
      metrics.setGlobalMeterProvider(meterProvider);
      owned.meterProvider = meterProvider;
      ownedMetrics = true;
    } catch (error) {
      console.warn("trellis telemetry: metric export disabled", error);
    }
  }

  if (ownedTraces || ownedMetrics) {
    // Install the native async context manager and W3C propagator only when
    // Trellis owns providers; a host keeps its own registrations.
    await installAsyncContextManager();
    await installW3cPropagator();
  }

  const handle: TelemetryRuntimeHandle = {
    traces: ownedTraces,
    metrics: ownedMetrics,
    forceFlush: async () => {
      await withinBudget(async () => {
        await owned.tracerProvider?.forceFlush();
        await owned.meterProvider?.forceFlush();
      });
    },
    shutdown: async () => {
      if (shutdownDone) return;
      shutdownDone = true;
      await withinBudget(async () => {
        await owned.tracerProvider?.shutdown();
        await owned.meterProvider?.shutdown();
      });
    },
  };
  return handle;
}

/** Installs the async-local-storage context manager for owned providers. */
async function installAsyncContextManager(): Promise<void> {
  try {
    const hooks = await runtimeImport<
      typeof import("@opentelemetry/context-async-hooks")
    >(["@opentelemetry", "context-async-hooks"].join("/"));
    context.setGlobalContextManager(
      new hooks.AsyncLocalStorageContextManager(),
    );
  } catch (error) {
    console.warn("trellis telemetry: async context unavailable", error);
  }
}

/**
 * Installs the approved W3C trace-context propagator for owned providers.
 *
 * Registering a tracer provider does not register a propagator, so the
 * Trellis-owned mode installs the standard W3C propagator itself. A
 * host-owned mode leaves the host's globals untouched.
 */
async function installW3cPropagator(): Promise<void> {
  try {
    const core = await runtimeImport<typeof import("@opentelemetry/core")>(
      ["@opentelemetry", "core"].join("/"),
    );
    propagation.setGlobalPropagator(new core.W3CTraceContextPropagator());
  } catch (error) {
    console.warn("trellis telemetry: W3C propagator unavailable", error);
  }
}

/** Records one identity claim; a different later identity warns once. */
function claimIdentity(identity: TelemetryIdentity): void {
  if (claimedServiceName === undefined) {
    claimedServiceName = identity.serviceName;
    return;
  }
  if (claimedServiceName !== identity.serviceName && !identityWarned) {
    identityWarned = true;
    console.warn(
      `trellis telemetry: process already initialized as '${claimedServiceName}'; ` +
        `ignoring '${identity.serviceName}'`,
    );
  }
}

/**
 * Awaited single-flight telemetry owner for native processes.
 *
 * Resolves to the shared handle or a disabled no-op handle. Telemetry failure
 * never rejects connection startup, and later connections reuse the first
 * successfully claimed identity.
 */
export function ensureTelemetryRuntime(
  identity: TelemetryIdentity,
): Promise<TelemetryRuntimeHandle> {
  if (owner === undefined) {
    claimIdentity(identity);
    owner = initialize(identity);
  }
  return owner;
}

/**
 * Records an explicit host-owned provider mode without installing providers.
 *
 * Applications that own OpenTelemetry call this after their providers are
 * installed and before their first Trellis connect or mount.
 */
export function useHostTelemetryProviders(options: {
  traces?: boolean;
  metrics?: boolean;
}): TelemetryRuntimeHandle {
  const handle: TelemetryRuntimeHandle = {
    traces: options.traces ?? false,
    metrics: options.metrics ?? false,
    forceFlush: () => Promise.resolve(),
    shutdown: () => Promise.resolve(),
  };
  if (owner === undefined) owner = Promise.resolve(handle);
  return handle;
}

/** Existing public initializer name, now an awaitable owner call. */
export function initTelemetryRuntime(serviceName: string): Promise<void> {
  return ensureTelemetryRuntime({
    serviceName,
    role: "service",
    serviceVersion: getEnv("TRELLIS_SERVICE_VERSION"),
  }).then(() => undefined);
}
