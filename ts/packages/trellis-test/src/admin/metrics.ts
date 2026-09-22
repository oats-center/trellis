import { recordTrellisDuration as recordOpenTelemetryDuration } from "@qlever-llc/trellis/telemetry";

export function recordTrellisDuration(
  name: Parameters<typeof recordOpenTelemetryDuration>[0],
  durationMs: number,
  attributes?: Parameters<typeof recordOpenTelemetryDuration>[2] & {
    deployment?: string;
    participantId?: string;
  },
): void {
  if (attributes === undefined) {
    recordOpenTelemetryDuration(name, durationMs);
  } else {
    const {
      deployment: _deployment,
      participantId: _participantId,
      ...otelAttributes
    } = attributes;
    recordOpenTelemetryDuration(name, durationMs, otelAttributes);
  }
}
