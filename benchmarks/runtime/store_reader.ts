/** Manual 8 MiB Rust Store-reader audit through the ordinary download runtime. */
import { assert, assertEquals } from "@std/assert";
import { TransferGrantSchema } from "@oatscenter/trellis";
import { Value } from "typebox/value";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../../ts/integration/_support/runtime.ts";

const [fixture, destination, latencyMs] = Deno.args;
if (!fixture || !destination || latencyMs === undefined) {
  throw new Error(
    "Usage: store_reader.ts <transfer_generation binary> <result.json> <configured RTT ms>",
  );
}
const binaryHash = Array.from(
  new Uint8Array(
    await crypto.subtle.digest(
      "SHA-256",
      await Deno.readFile(fixture),
    ),
  ),
  (byte) => byte.toString(16).padStart(2, "0"),
).join("");
const samples: number[] = [];
await withTrellisRuntime(async (runtime) => {
  const identity = await runtime.registerService({
    name: "rust-reader-audit",
    contract: participants.Provider.participant,
  });
  const caller = await runtime.connectClient({
    name: "rust-reader-audit-caller",
    contract: participants.Caller.participant,
  });
  const child = new Deno.Command(fixture, {
    env: {
      TRELLIS_URL: runtime.trellisUrl,
      TRELLIS_IDENTITY_SEED: identity.seed,
    },
    stdout: "piped",
    stderr: "inherit",
  }).spawn();
  let output = "";
  let ended = false;
  const reading = (async () => {
    for await (
      const text of child.stdout.pipeThrough(new TextDecoderStream())
    ) output += text;
  })();
  void reading.catch(() => {});
  const status = child.status.then((value) => {
    ended = true;
    return value;
  });
  try {
    await runtime.waitFor(() => {
      assert(!ended, `provider exited before readiness: ${output}`);
      return output.includes("transfer provider ready");
    }, { timeoutMs: 60_000 });
    for (let index = 0; index < 23; index++) {
      const started = performance.now();
      const response = await caller.download({ value: "reader-audit" })
        .orThrow();
      const grant = Value.Parse(TransferGrantSchema, response.transfer);
      assert(grant.direction === "receive");
      const bytes = await caller.transfer(grant).bytes().orThrow();
      const elapsed = performance.now() - started;
      assertEquals(bytes.length, 8 * 1024 * 1024);
      for (let offset = 0; offset < bytes.length; offset++) {
        if (bytes[offset] !== offset % 251) {
          throw new Error(`download byte ${offset} differs`);
        }
      }
      if (index >= 3) samples.push(elapsed);
    }
  } finally {
    await caller.connection.close();
    if (!ended) child.kill("SIGTERM");
    await status;
    await reading;
  }
});
const sorted = samples.toSorted((a, b) => a - b);
const report = {
  fixture,
  binarySha256: binaryHash,
  configuredRttMs: Number(latencyMs),
  sizeBytes: 8 * 1024 * 1024,
  warmup: 3,
  samplesMs: samples,
  p50Ms: (sorted[9] + sorted[10]) / 2,
  p95Ms: sorted[18],
  errors: 0,
};
await Deno.writeTextFile(destination, JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify(report));
