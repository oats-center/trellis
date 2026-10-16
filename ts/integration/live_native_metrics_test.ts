import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { buffer } from "node:stream/consumers";
import { test } from "node:test";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary whose environment this case controls. */

const LIVE_FAMILIES = [
  "trellis.live.sessions",
  "trellis.live.ends",
  "trellis.live.handshake.duration",
  "trellis.live.buffered.bytes",
  "trellis.live.frames",
  "trellis.live.rejections",
  "trellis.live.cleanup.pending",
];

/**
 * The Rust provider engine owns built-in Health sessions. Opening a real
 * `Health.Watch` must make that owner export its `trellis.live.*` families
 * through the ordinary OTLP HTTP exporter, and the Feed-only families
 * must project from the same session.
 */
test("M01 Rust provider owner exports live families over the real OTLP wire", async () => {
  const bodies: Uint8Array[] = [];
  const collector = createServer(async (request, response) => {
    const body = await buffer(request);
    if (request.url === "/v1/metrics") bodies.push(body);
    response.writeHead(200).end();
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
          () =>
            bodies.some((body) =>
              new TextDecoder().decode(body).includes("trellis.live.ends"),
            ),
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
  const decoded = bodies
    .map((body) => new TextDecoder("utf-8", { fatal: false }).decode(body))
    .join("\n");
  assert(bodies.length > 0, "the server exported no metrics over the wire");
  const live = LIVE_FAMILIES.filter((name) => decoded.includes(name));
  // Every family this ordinary session touches must appear; the durable
  // rejection and retained-cleanup families only appear on their own events.
  for (const required of [
    "trellis.live.sessions",
    "trellis.live.ends",
    "trellis.live.handshake.duration",
    "trellis.live.frames",
  ]) {
    assert(
      decoded.includes(required),
      `missing ${required}; exported live families: ${live.join(", ")}`,
    );
  }
});
