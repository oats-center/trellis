import { configureErrorTraceId } from "./result.ts";
import { ensureTelemetryRuntime, type TelemetryIdentity } from "./runtime.ts";
import { getEnv } from "./env.ts";

/** Initializes Trellis telemetry for a native service runtime. */
export function initTelemetry(serviceName: string): Promise<void> {
  configureErrorTraceId();
  const identity: TelemetryIdentity = {
    serviceName,
    role: "service",
    serviceVersion: getEnv("TRELLIS_SERVICE_VERSION"),
  };
  return ensureTelemetryRuntime(identity).then(() => undefined);
}
