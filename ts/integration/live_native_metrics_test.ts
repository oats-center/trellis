import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { createServer as createNetServer, type Socket } from "node:net";
import { buffer } from "node:stream/consumers";
import { test } from "node:test";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { decodeMetrics, type Metric } from "./_support/otlp.ts";
import { startTrellisRuntime, withTrellisRuntime } from "./_support/runtime.ts";

/**
 * Verify exported Live completion and Auth's resource snapshot after ordinary
 * deployment materialization, not just registration of metric names.
 */
test("M01 Rust owners export Live completion and materialized resource counts over OTLP", async () => {
  let latest: Metric[] = [];
  let collectorError: unknown;
  const total = (name: string, kind: "sum" | "gauge") =>
    latest
      .find((metric) => metric.name === name)
      ?.[kind]?.dataPoints.reduce(
        (sum, point) => sum + Number(point.asInt ?? point.asDouble),
        0,
      );
  const collector = createServer(async (request, response) => {
    try {
      const body = await buffer(request);
      if (request.url === "/v1/metrics") {
        latest = decodeMetrics(body)
          .resourceMetrics.flatMap((resource) => resource.scopeMetrics)
          .flatMap((scope) => scope.metrics);
      }
      response.writeHead(200).end();
    } catch (error) {
      collectorError = error;
      response.writeHead(400).end();
    }
  });
  collector.listen(0, "127.0.0.1");
  await once(collector, "listening");
  const address = collector.address();
  assert(address && typeof address !== "string");
  const endpoint = `http://127.0.0.1:${address.port}`;
  try {
    await withTrellisRuntime(
      async (runtime) => {
        const client = await runtime.connectClient({
          name: "native-metrics-console",
          contract: apps.Observer.request,
        });
        const feed = await client.healthWatch({}).orThrow();
        const first = await feed[Symbol.asyncIterator]().next();
        assert(!first.done, "Health.Watch ended without an activation frame");
        await feed[Symbol.asyncIterator]().return?.();
        await client.connection.close();
        await runtime.waitFor(
          () => (total("trellis.live.ends", "sum") ?? 0) >= 1,
          { timeoutMs: 30_000 },
        );
        assert((total("trellis.live.frames", "sum") ?? 0) >= 1);
        await runtime.waitFor(
          () => total("trellis.resource.bindings", "gauge") !== undefined,
          { timeoutMs: 30_000 },
        );
        const before = total("trellis.resource.bindings", "gauge");
        assert(before !== undefined);
        await runtime.registerService({
          name: "native-metrics-resource-provider",
          contract: participants.Provider.participant,
        });
        await runtime.waitFor(
          () => (total("trellis.resource.bindings", "gauge") ?? 0) > before,
          { timeoutMs: 30_000 },
        );
      },
      {
        trellis: {
          environment: {
            unset: Object.keys(process.env).filter((key) =>
              key.startsWith("OTEL_"),
            ),
            set: {
              OTEL_METRICS_EXPORTER: "otlp",
              OTEL_EXPORTER_OTLP_ENDPOINT: endpoint,
              OTEL_METRIC_EXPORT_INTERVAL: "500",
            },
          },
        },
      },
    );
  } finally {
    await new Promise<void>((resolve, reject) =>
      collector.close((error) => (error ? reject(error) : resolve())),
    );
  }
  assert.equal(collectorError, undefined);
});

test("M02 a server whose OTLP collector never answers stops within its shutdown bound", async () => {
  // The collector accepts connections and never replies, so every export
  // waits out its whole timeout, including the final one at shutdown.
  const sockets = new Set<Socket>();
  let exporting = false;
  const collector = createNetServer((socket) => {
    exporting = true;
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.resume();
  });
  collector.listen(0, "127.0.0.1");
  await once(collector, "listening");
  const address = collector.address();
  assert(address && typeof address !== "string");
  const shutdownMs = 10_000;
  try {
    const runtime = await startTrellisRuntime({
      timeouts: { shutdownMs },
      trellis: {
        environment: {
          unset: Object.keys(process.env).filter((key) =>
            key.startsWith("OTEL_"),
          ),
          set: {
            OTEL_METRICS_EXPORTER: "otlp",
            OTEL_TRACES_EXPORTER: "otlp",
            OTEL_TRACES_SAMPLER: "always_on",
            OTEL_EXPORTER_OTLP_ENDPOINT: `http://127.0.0.1:${address.port}`,
            OTEL_EXPORTER_OTLP_TIMEOUT: "60000",
            OTEL_METRIC_EXPORT_INTERVAL: "200",
            OTEL_BSP_SCHEDULE_DELAY: "100",
          },
        },
      },
    });
    try {
      // An exporter that never connects fails here, not at the job timeout.
      await runtime
        .waitFor(() => exporting, { timeoutMs: 30_000 })
        .catch((error: unknown) => {
          throw new Error(
            `the OTLP exporter never connected to the collector: ${String(error)}\n${runtime.controlPlaneOutput()}`,
          );
        });
      // A served request leaves spans queued for export too.
      await runtime.callAdminRpc("authUsersResolve", {});
      const started = Date.now();
      await runtime.stop();
      const elapsed = Date.now() - started;
      // A host still running at the bound is killed, which takes the whole bound.
      assert(
        elapsed < shutdownMs,
        `stopping took ${elapsed}ms\n${runtime.controlPlaneOutput()}`,
      );
    } finally {
      // Stopping again is a no-op once the timed stop ran.
      await runtime.stop();
    }
  } finally {
    for (const socket of sockets) socket.destroy();
    collector.close();
  }
});
