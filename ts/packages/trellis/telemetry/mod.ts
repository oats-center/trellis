export type { HeaderCarrier } from "./carrier.ts";
export {
  createMapCarrier,
  extractTraceContext,
  injectTraceContext,
} from "./carrier.ts";
export type { Context, Span } from "./core.ts";
export {
  context,
  getActiveSpan,
  getTracer,
  SpanKind,
  SpanStatusCode,
  trace,
  withSpan,
  withSpanAsync,
} from "./core.ts";
export { getEnv } from "./env.ts";
export { initTelemetry } from "./init.ts";
export type { TelemetryObservation } from "./lifecycle.ts";
export { startObservation, withObservation } from "./lifecycle.ts";
export type {
  TelemetryIdentity,
  TelemetryRole,
  TelemetryRuntimeHandle,
} from "./runtime.ts";
export {
  ensureTelemetryRuntime,
  initTelemetryRuntime,
  useHostTelemetryProviders,
} from "./runtime.ts";
export {
  buildTrellisDurationMetricAttributes,
  buildTrellisErrorMetricAttributes,
  getTrellisMeter,
  recordCatalogCounter,
  recordCatalogDuration,
  recordCatalogUpDown,
  recordRpcAttempt,
  recordTrellisDuration,
  recordTrellisError,
  routeToken,
  UNKNOWN_ROUTE,
} from "./metrics.ts";
export type {
  TrellisCatalogCounter,
  TrellisCatalogDuration,
  TrellisCatalogUpDown,
  TrellisRouteFamily,
} from "./metrics.ts";
export type {
  TrellisDurationMetricAttributes,
  TrellisDurationMetricName,
} from "./metrics.ts";
export type { TrellisErrorMetricAttributes } from "./metrics.ts";
export type { NatsHeadersLike } from "./nats.ts";
export { createNatsHeaderCarrier } from "./nats.ts";
export { configureErrorTraceId } from "./result.ts";
export {
  getTrellisTracer,
  startClientSpan,
  startServerSpan,
  trellisRoute,
} from "./trellis.ts";
