import { configureErrorTraceId } from "./result.ts";
import { initTelemetryRuntime } from "./runtime.ts";

/** Initializes Trellis telemetry for a service runtime. */
export function initTelemetry(serviceName: string): void {
  configureErrorTraceId();
  initTelemetryRuntime(serviceName);
}
