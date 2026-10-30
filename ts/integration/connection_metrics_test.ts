import assert from "node:assert/strict";
import { test } from "node:test";
import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";
import { codecs } from "../packages/trellis/generated_support.ts";

test("service metrics observe real admission, authoritative disable, and disposal", async () => {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const provider = new MeterProvider({
    readers: [
      new PeriodicExportingMetricReader({
        exporter,
        exportIntervalMillis: 60_000,
      }),
    ],
  });
  metrics.setGlobalMeterProvider(provider);
  const total = (name: string, subset: Record<string, string>) => {
    let count = 0;
    for (const scope of exporter.getMetrics().at(-1)?.scopeMetrics ?? []) {
      for (const metric of scope.metrics) {
        if (metric.descriptor.name !== name) continue;
        for (const point of metric.dataPoints) {
          if (
            typeof point.value === "number" &&
            Object.entries(subset).every(
              ([key, value]) => point.attributes[key] === value,
            )
          )
            count += point.value;
        }
      }
    }
    return count;
  };
  const labels = { "trellis.participant.kind": "service" };
  try {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "connection-metrics",
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        seed: identity.seed,
        // The real exporter above owns SDK initialization for this process.
        telemetry: false,
      }).orThrow();
      const exit = service.wait().catch((error: unknown) => error);
      try {
        await provider.forceFlush();
        assert.equal(
          total("trellis.connection.count", {
            ...labels,
            "trellis.state": "usable",
          }),
          1,
        );
        assert.equal(
          total("trellis.connection.count", {
            ...labels,
            "trellis.state": "connecting",
          }),
          0,
        );
        await runtime.services.disableInstance({
          instanceId: codecs.ulid.decode(identity.instanceId),
          expectedRevision: 1n,
          idempotencyKey: codecs.ulid.decode(ulid()),
          reason: "connection metrics",
        });
        await runtime.waitFor(async () => {
          await provider.forceFlush();
          return (
            total("trellis.connection.count", {
              ...labels,
              "trellis.state": "suspended",
            }) === 1
          );
        });
      } finally {
        await service.stop();
        await exit;
      }
      await provider.forceFlush();
      assert.equal(
        total("trellis.connection.count", {
          ...labels,
          "trellis.state": "usable",
        }),
        0,
      );
      assert.equal(
        total("trellis.connection.count", {
          ...labels,
          "trellis.state": "suspended",
        }),
        0,
      );
      assert.equal(
        total("trellis.connection.transitions", {
          ...labels,
          "trellis.reason": "terminal",
        }),
        1,
      );
    });
  } finally {
    await provider.shutdown();
    metrics.disable();
  }
});
