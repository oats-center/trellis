import { loadDefaultRuntimeTransport } from "../../runtime_transport.ts";
import { initTelemetry } from "../../telemetry/init.ts";
import type { TrellisServiceRuntimeDeps } from "./runtime.ts";

/** Load the production NATS transport and telemetry owner. */
export async function loadDefaultServiceRuntimeDeps(): Promise<TrellisServiceRuntimeDeps> {
  const transport = await loadDefaultRuntimeTransport();
  return { initTelemetry, connect: transport.connect };
}
