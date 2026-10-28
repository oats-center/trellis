import { metrics } from "@opentelemetry/api";
import { MeterProvider, MetricReader } from "@opentelemetry/sdk-metrics";
import { writeFile } from "node:fs/promises";

/** Collect ordinary production instruments through a host-owned SDK reader. */
export function captureMetrics(output: string, role: string) {
  const reader = new (class extends MetricReader {
    protected async onShutdown() {}
    protected async onForceFlush() {}
  })();
  const provider = new MeterProvider({ readers: [reader] });
  if (!metrics.setGlobalMeterProvider(provider))
    throw new Error("Benchmark meter provider already installed");
  return async () => {
    const result = await reader.collect();
    if (result.errors.length)
      throw new AggregateError(
        result.errors,
        "Benchmark metric collection failed",
      );
    await writeFile(
      `${output}/metrics-${role}.json`,
      JSON.stringify(result.resourceMetrics.scopeMetrics, null, 2),
    );
    await provider.shutdown();
  };
}
