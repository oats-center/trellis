import { assert } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { participants as webParticipants } from "trellis-web-generated";

import { withTrellisRuntime } from "./_support/runtime.ts";

/** Built server binary whose environment this case controls. */
function serverBinary(): string {
  return Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
    fromFileUrl(
      new URL("../../target/debug/trellis-server", import.meta.url),
    );
}

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
Deno.test("M01 Rust provider owner exports live families over the real OTLP wire", async () => {
  const bodies: Uint8Array[] = [];
  let endpoint = "";
  const collector = Deno.serve({
    hostname: "127.0.0.1",
    port: 0,
    onListen: ({ port }) => {
      endpoint = `http://127.0.0.1:${port}`;
    },
  }, async (request) => {
    const body = new Uint8Array(await request.arrayBuffer());
    if (new URL(request.url).pathname === "/v1/metrics") bodies.push(body);
    return new Response(null, { status: 200 });
  });
  try {
    await withTrellisRuntime(async (runtime) => {
      const client = await runtime.connectClient({
        name: "native-metrics-console",
        contract: webParticipants.Console.participant,
      });
      const feed = await client.healthWatch({}).orThrow();
      const first = await feed[Symbol.asyncIterator]().next();
      assert(!first.done, "Health.Watch ended without an activation frame");
      await feed[Symbol.asyncIterator]().return?.();
      await client.connection.close();
      // Let the periodic reader flush the activation/terminal transitions.
      await new Promise((resolve) => setTimeout(resolve, 2_500));
    }, {
      trellis: {
        command: {
          cmd: "env",
          args: [
            ...Object.keys(Deno.env.toObject()).filter((key) =>
              key.startsWith("OTEL_")
            ).flatMap((key) => ["-u", key]),
            "OTEL_METRICS_EXPORTER=otlp",
            `OTEL_EXPORTER_OTLP_ENDPOINT=${endpoint}`,
            "OTEL_METRIC_EXPORT_INTERVAL=500",
            serverBinary(),
            "--config",
            "{config}",
            "all",
          ],
        },
      },
    });
  } finally {
    await collector.shutdown();
  }
  const decoded = bodies.map((body) =>
    new TextDecoder("utf-8", { fatal: false }).decode(body)
  ).join("\n");
  assert(bodies.length > 0, "the server exported no metrics over the wire");
  const live = LIVE_FAMILIES.filter((name) => decoded.includes(name));
  // Every family this ordinary session touches must appear; the durable
  // rejection and retained-cleanup families only appear on their own events.
  for (
    const required of [
      "trellis.live.sessions",
      "trellis.live.ends",
      "trellis.live.handshake.duration",
      "trellis.live.frames",
    ]
  ) {
    assert(
      decoded.includes(required),
      `missing ${required}; exported live families: ${live.join(", ")}`,
    );
  }
});
