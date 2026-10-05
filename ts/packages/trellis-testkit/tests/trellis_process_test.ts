import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import {
  parseTrellisBootstrapUrl,
  startTrellisProcess,
} from "../src/trellis_process.ts";

Deno.test("parseTrellisBootstrapUrl reads structured and fallback log lines", () => {
  assertEquals(
    parseTrellisBootstrapUrl(
      JSON.stringify({ bootstrapUrl: "http://127.0.0.1:8000/bootstrap" }),
    ),
    "http://127.0.0.1:8000/bootstrap",
  );
  assertEquals(
    parseTrellisBootstrapUrl(JSON.stringify({
      fields: { bootstrapUrl: "http://127.0.0.1:8002/bootstrap" },
    })),
    "http://127.0.0.1:8002/bootstrap",
  );
  assertEquals(
    parseTrellisBootstrapUrl(
      "ready TRELLIS_ADMIN_BOOTSTRAP_URL=http://127.0.0.1:8001/bootstrap",
    ),
    "http://127.0.0.1:8001/bootstrap",
  );
  assertEquals(parseTrellisBootstrapUrl("ready"), undefined);
});

Deno.test("startTrellisProcess reports early exit with output tails", async () => {
  const configPath = "/tmp/trellis-test-config.json";

  const error = await assertRejects(
    () =>
      startTrellisProcess({
        trellisUrl: "http://127.0.0.1:9",
        configPath,
        command: {
          cmd: Deno.execPath(),
          args: [
            "eval",
            "console.log('TRELLIS_CONFIG=' + Deno.env.get('TRELLIS_CONFIG')); console.log('TRELLIS_ADMIN_BOOTSTRAP_URL=http://127.0.0.1:9000/bootstrap'); console.error('NO_COLOR=' + Deno.env.get('NO_COLOR')); console.error('TOKIO_WORKER_THREADS=' + Deno.env.get('TOKIO_WORKER_THREADS')); Deno.exit(23);",
          ],
        },
        startupTimeoutMs: 10_000,
        shutdownTimeoutMs: 1_000,
      }),
    Error,
    "Trellis process exited before readiness (exit code 23)",
  );

  assertStringIncludes(error.message, "http://127.0.0.1:9/readyz");
  assertStringIncludes(
    error.message,
    "TRELLIS_ADMIN_BOOTSTRAP_URL=http://127.0.0.1:9000/bootstrap",
  );
});
