//! Browser-safe Trellis telemetry entrypoint.
//!
//! This module imports only OpenTelemetry API, resources, and browser
//! WebTracerProvider/metrics modules. It never imports Node builtins, async
//! local storage, filesystem, or collector credentials. Built-in and
//! third-party browser apps opt in explicitly and supply a same-origin
//! relative OTLP path; the deployment's restricted relay forwards it to the
//! isolated browser Collector listener.

import { metrics } from "@opentelemetry/api";

import { randomUuid } from "../auth/crypto.ts";

export {
  buildTrellisErrorMetricAttributes,
  recordCatalogCounter,
  recordCatalogDuration,
  recordCatalogUpDown,
  recordTrellisDuration,
} from "./metrics.ts";
export type {
  TrellisDurationMetricAttributes,
  TrellisDurationMetricName,
} from "./metrics.ts";
export { extractTraceContext, injectTraceContext } from "./carrier.ts";

/** Built-in application identity accepted by the browser initializer. */
export type BrowserTelemetryApp = "console" | "portal" | string;

/** Explicit opt-in configuration for one browser document owner. */
export interface BrowserTelemetryOptions {
  /** Built-in or third-party application label; never a user or session ID. */
  app: BrowserTelemetryApp;
  /** Same-origin relative OTLP prefix such as `/otel`. */
  endpoint: string;
  /** Optional service name; defaults to `trellis-browser`. */
  serviceName?: string;
  /** Optional service version. */
  serviceVersion?: string;
  /** Trace ratio in `0..1`; defaults to 0.05. */
  traceRatio?: number;
}

/** Process handle for the browser document owner. */
export interface BrowserTelemetryHandle {
  /** Whether browser telemetry was installed. */
  readonly enabled: boolean;
  /** Best-effort flush, used at most once per visibility transition. */
  forceFlush(): Promise<void>;
  /** Flushes and shuts down the document providers. */
  shutdown(): Promise<void>;
}

/** One overall owner-operation budget in milliseconds. */
const BROWSER_OWNER_BUDGET_MILLIS = 5_000;

/**
 * Runs one owner operation best-effort under the overall budget.
 *
 * A synchronous or asynchronous provider failure never rejects and never
 * prevents attempting the remaining signal within the budget.
 */
async function runWithinBudget(
  work: Promise<unknown>,
  deadline: number,
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, Math.max(0, deadline - performance.now()));
  });
  try {
    await Promise.race([work, timeout]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** One document owner serializes raw provider work independently of caller waits. @internal */
export function browserOwner(
  tracerProvider: {
    forceFlush(): Promise<unknown>;
    shutdown(): Promise<unknown>;
  },
  meterProvider: {
    forceFlush(): Promise<unknown>;
    shutdown(): Promise<unknown>;
  },
): BrowserTelemetryHandle {
  let closing = false;
  let active: Promise<void> | undefined;
  let shutdownWait: Promise<void> | undefined;
  const run = async (
    operations: Array<() => Promise<unknown>>,
    deadline: number,
  ) => {
    for (const operation of operations) {
      if (performance.now() >= deadline) break;
      try {
        await operation();
      } catch (error) {
        console.warn(
          "trellis telemetry: browser provider operation failed",
          error,
        );
      }
    }
  };
  return {
    enabled: true,
    forceFlush: () => {
      const deadline = performance.now() + BROWSER_OWNER_BUDGET_MILLIS;
      if (closing) return Promise.resolve();
      if (active === undefined) {
        const work = run([
          () => tracerProvider.forceFlush(),
          () => meterProvider.forceFlush(),
        ], deadline);
        active = work;
        void work.finally(() => {
          if (active === work) active = undefined;
        });
      }
      return runWithinBudget(active, deadline);
    },
    shutdown: () => {
      if (closing) return shutdownWait ?? Promise.resolve();
      const deadline = performance.now() + BROWSER_OWNER_BUDGET_MILLIS;
      closing = true;
      const previous = active;
      const work = (async () => {
        if (previous) await previous;
        await run([
          () => tracerProvider.shutdown(),
          () => meterProvider.shutdown(),
        ], deadline);
      })();
      active = work;
      void work.finally(() => {
        if (active === work) active = undefined;
      });
      shutdownWait = runWithinBudget(work, deadline);
      return shutdownWait;
    },
  };
}

/** Default browser trace ratio from the observability order. */
const DEFAULT_BROWSER_TRACE_RATIO = 0.05;
/** Browser metrics export interval in milliseconds. */
const BROWSER_METRICS_INTERVAL_MILLIS = 15_000;

let owner: Promise<BrowserTelemetryHandle> | undefined;

/**
 * Reads and sanitizes the same-origin relative OTLP prefix.
 *
 * Only a plain same-origin path is accepted: scheme-relative references,
 * backslashes, credentials, queries, and fragments are rejected so the browser
 * never sends telemetry to another origin.
 */
export function sanitizeBrowserEndpoint(
  endpoint: string | undefined,
): string | undefined {
  if (!endpoint) return undefined;
  const trimmed = endpoint.trim();
  if (!trimmed.startsWith("/")) return undefined;
  // Control characters can be stripped or normalized by URL parsing and turn a
  // relative path into a cross-origin reference (for example "/\t/host").
  for (const character of trimmed) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20 || code === 0x7f) return undefined;
  }
  // A network-path reference (`//host`) is cross-origin, not a local path.
  if (trimmed.startsWith("//")) return undefined;
  if (
    trimmed.includes("://") || trimmed.includes("?") || trimmed.includes("#") ||
    trimmed.includes("@") || trimmed.includes("\\")
  ) {
    return undefined;
  }
  // Validate the resolved origin after normalization: the final exporter URL
  // must stay on the document origin. Outside a document there is no origin to
  // compare against, so the structural checks above remain the contract.
  const origin = (globalThis as { location?: { origin?: string } }).location
    ?.origin;
  if (origin !== undefined) {
    const resolved = new URL(trimmed.replace(/\/+$/, ""), origin);
    if (resolved.origin !== origin) return undefined;
    if (resolved.username || resolved.password) return undefined;
    if (resolved.search || resolved.hash) return undefined;
  }
  return trimmed.replace(/\/+$/, "");
}

/** Validates the opt-in trace ratio. */
function traceRatio(value: number | undefined): number {
  if (
    value === undefined || !Number.isFinite(value) || value < 0 || value > 1
  ) {
    return DEFAULT_BROWSER_TRACE_RATIO;
  }
  return value;
}

/** Installs one document-owned browser telemetry provider set. */
async function initialize(
  options: BrowserTelemetryOptions,
): Promise<BrowserTelemetryHandle> {
  const base = sanitizeBrowserEndpoint(options.endpoint);
  if (!base) {
    console.warn(
      "trellis telemetry: browser telemetry disabled; endpoint must be a same-origin relative path",
    );
    return {
      enabled: false,
      forceFlush: () => Promise.resolve(),
      shutdown: () => Promise.resolve(),
    };
  }
  try {
    // Literal dynamic imports so the existing bundler resolves the browser
    // dependencies statically instead of relying on an import map.
    const [web, traceBase, resources, sdkMetrics, traceOtlp, metricsOtlp] =
      await Promise.all([
        import("@opentelemetry/sdk-trace-web"),
        import("@opentelemetry/sdk-trace-base"),
        import("@opentelemetry/resources"),
        import("@opentelemetry/sdk-metrics"),
        import("@opentelemetry/exporter-trace-otlp-proto"),
        import("@opentelemetry/exporter-metrics-otlp-proto"),
      ]);
    const attributes = {
      "service.name": options.serviceName ?? "trellis-browser",
      "service.version": options.serviceVersion ?? "0.0.0",
      "service.namespace": "trellis",
      "trellis.role": "browser",
      "trellis.app": options.app,
      // Document-lifetime cumulative-stream identity; never a user identity.
      "service.instance.id": randomUuid(),
    };
    const tracerProvider = new web.WebTracerProvider({
      resource: resources.resourceFromAttributes(attributes),
      sampler: new traceBase.ParentBasedSampler({
        root: new traceBase.TraceIdRatioBasedSampler(
          traceRatio(options.traceRatio),
        ),
      }),
      spanProcessors: [
        new traceBase.BatchSpanProcessor(
          new traceOtlp.OTLPTraceExporter({ url: `${base}/v1/traces` }),
        ),
      ],
    });
    const meterProvider = new sdkMetrics.MeterProvider({
      resource: resources.resourceFromAttributes(attributes),
      readers: [
        new sdkMetrics.PeriodicExportingMetricReader({
          exporter: new metricsOtlp.OTLPMetricExporter({
            url: `${base}/v1/metrics`,
          }),
          exportIntervalMillis: BROWSER_METRICS_INTERVAL_MILLIS,
        }),
      ],
    });
    metrics.setGlobalMeterProvider(meterProvider);
    // A fresh browser document does not run native setup, so the provider's
    // own registration installs the browser-supported context manager together
    // with the approved W3C trace-context propagator. A host-owned mode never
    // reaches this path and keeps its own globals.
    const core = await import("@opentelemetry/core");
    tracerProvider.register({
      propagator: new core.W3CTraceContextPropagator(),
    });
    return browserOwner(tracerProvider, meterProvider);
  } catch (error) {
    console.warn("trellis telemetry: browser initialization failed", error);
    return {
      enabled: false,
      forceFlush: () => Promise.resolve(),
      shutdown: () => Promise.resolve(),
    };
  }
}

/**
 * Initializes browser telemetry once per document.
 *
 * Failure never breaks rendering: the promise always resolves to a handle.
 */
export function initBrowserTelemetry(
  options: BrowserTelemetryOptions,
): Promise<BrowserTelemetryHandle> {
  if (owner === undefined) owner = initialize(options);
  return owner;
}
