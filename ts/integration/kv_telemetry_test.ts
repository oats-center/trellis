import { metrics } from "@opentelemetry/api";
import {
  AggregationTemporality,
  InMemoryMetricExporter,
  MeterProvider,
  PeriodicExportingMetricReader,
} from "@opentelemetry/sdk-metrics";
import { resourceFromAttributes } from "@opentelemetry/resources";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { TypedKV } from "../packages/trellis/kv.ts";
import { fixedTransportProvider } from "../packages/trellis/transport/generations.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("typed KV records one bounded storage duration per caller operation", async () => {
  const exporter = new InMemoryMetricExporter(
    AggregationTemporality.CUMULATIVE,
  );
  const reader = new PeriodicExportingMetricReader({
    exporter,
    exportIntervalMillis: 60_000,
  });
  const provider = new MeterProvider({
    resource: resourceFromAttributes({
      "service.name": "trellis-kv-acceptance",
      "service.instance.id": crypto.randomUUID(),
    }),
    readers: [reader],
  });
  metrics.setGlobalMeterProvider(provider);
  try {
    await withTrellisRuntime(async (runtime) => {
      const nats = await connect({
        servers: runtime.natsUrl,
        authenticator: credsAuthenticator(
          await readFile(
            `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
          ),
        ),
      });
      try {
        let current = true;
        const kv = await TypedKV.open(
          fixedTransportProvider(nats),
          "kv_telemetry",
          {
            version: 1,
            codec: {
              encode: (value: number) => value,
              decode: (value: unknown) => Number(value),
            },
            migrations: {},
          },
          { history: 4, isCurrent: () => current },
        ).orThrow();

        assert.deepEqual(await kv.get("absent").orThrow(), undefined);
        const created = await kv.create("secret-key", 1).orThrow();
        assert.deepEqual(await kv.get("secret-key").orThrow(), 1);
        assert.deepEqual((await kv.getEntry("secret-key").orThrow())?.value, 1);
        assert.deepEqual(
          (await kv.replace("secret-key", created.revision, 2).orThrow()).value,
          2,
        );
        const conflict = await kv.replace("secret-key", created.revision, 3);
        assert.deepEqual(conflict.isErr(), true);
        await kv.put("secret-key", 4).orThrow();
        assert.deepEqual((await kv.history("secret-key").orThrow()).length, 3);
        const keys = await kv.keys().orThrow();
        for await (const key of keys) assert.deepEqual(key, "secret-key");
        await kv.status().orThrow();
        const watcher = await kv.watch("secret-key").orThrow();
        for await (const _entry of watcher) break;
        await kv.delete("secret-key").orThrow();
        current = false;
        assert.deepEqual((await kv.get("secret-key")).isErr(), true);

        await provider.forceFlush();
        const counts = new Map<string, number>();
        for (const resource of exporter.getMetrics()) {
          for (const scope of resource.scopeMetrics) {
            for (const metric of scope.metrics) {
              if (metric.descriptor.name !== "trellis.storage.duration") {
                continue;
              }
              assert.deepEqual(metric.descriptor.unit, "s");
              for (const point of metric.dataPoints) {
                assert.deepEqual(point.attributes["trellis.backend"], "kv");
                assert.deepEqual(Object.keys(point.attributes).sort(), [
                  "trellis.backend",
                  "trellis.operation",
                  "trellis.outcome",
                  "trellis.phase",
                ]);
                assert.deepEqual(point.attributes["trellis.phase"], "total");
                const label = `${point.attributes["trellis.operation"]}:${
                  point.attributes["trellis.outcome"]
                }`;
                if (typeof point.value === "number") {
                  throw new Error("Expected a storage duration histogram");
                }
                counts.set(label, point.value.count);
              }
            }
          }
        }
        assert.deepEqual(Object.fromEntries(counts), {
          "read:ok": 3,
          "read:not_found": 1,
          "read:error": 1,
          "cas:ok": 2,
          "cas:conflict": 1,
          "write:ok": 1,
          "list:ok": 2,
          "delete:ok": 1,
          "watch_setup:ok": 1,
        });
      } finally {
        await nats.close();
      }
    });
  } finally {
    await provider.shutdown();
    metrics.disable();
  }
});

test("typed KV CAS remains intact with telemetry disabled", async () => {
  metrics.disable();
  await withTrellisRuntime(async (runtime) => {
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await readFile(
          `${runtime.workdir}/config/trellis/nats/creds/trellis-auth.creds`,
        ),
      ),
    });
    try {
      const kv = await TypedKV.open(
        fixedTransportProvider(nats),
        "kv_disabled",
        {
          version: 1,
          codec: {
            encode: (value: number) => value,
            decode: (value: unknown) => Number(value),
          },
          migrations: {},
        },
      ).orThrow();
      assert.deepEqual(await kv.get("absent").orThrow(), undefined);
      const created = await kv.create("key", 1).orThrow();
      await kv.replace("key", created.revision, 2).orThrow();
      assert.deepEqual(
        (await kv.replace("key", created.revision, 3)).isErr(),
        true,
      );
      assert.deepEqual(await kv.get("key").orThrow(), 2);
    } finally {
      await nats.close();
    }
  });
});
