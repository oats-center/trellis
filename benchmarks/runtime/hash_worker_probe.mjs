import { readFile, writeFile } from "node:fs/promises";
import init, {
  live_server_proof_digest,
} from "../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm.js";
await init({
  module_or_path: await readFile(
    new URL(
      "../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bg.wasm",
      import.meta.url,
    ),
  ),
});
const node = !globalThis.Deno;
const WorkerType = node ? (await import("node:worker_threads")).Worker : Worker;
const worker = new WorkerType(new URL("./hash_worker.mjs", import.meta.url), {
  type: "module",
});
let resolve;
let reject;
const receive = (message) => {
  if (message.error) reject(new Error(message.error));
  else resolve(message);
};
if (node) {
  worker.on("message", receive);
  worker.on("error", (error) => reject(error));
} else {
  worker.onmessage = (event) => receive(event.data);
  worker.onerror = (event) => reject(new Error(event.message));
}
const response = () =>
  new Promise((yes, no) => {
    const timeout = setTimeout(
      () => no(new Error("Worker response timed out")),
      10000,
    );
    resolve = (value) => {
      clearTimeout(timeout);
      yes(value);
    };
    reject = (error) => {
      clearTimeout(timeout);
      no(error);
    };
  });
const ready = await response();
const rows = [];
try {
  for (const size of [1024, 262144, 1048576]) {
    for (const mode of ["clone", "transfer"]) {
      const body = new Uint8Array(size).fill(120);
      const expected = live_server_proof_digest(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "benchmark.hash",
        body,
      );
      const pending = response();
      const delays = [];
      let tick = performance.now() + 5;
      const timer = setInterval(() => {
        const time = performance.now();
        delays.push(Math.max(0, time - tick));
        tick = time + 5;
      }, 5);
      const before = performance.now();
      worker.postMessage({
        body,
        count: size === 1024 ? 100000 : size === 262144 ? 1000 : 250,
      }, mode === "transfer" ? [body.buffer] : []);
      const value = await pending;
      clearInterval(timer);
      if (value.digest !== expected) throw new Error("Worker digest mismatch");
      rows.push({
        size,
        mode,
        roundTripMs: performance.now() - before,
        workerMs: value.elapsedMs,
        parentBytesAfter: body.byteLength,
        maxTimerDelayMs: Math.max(0, ...delays),
        timerSamples: delays.length,
      });
    }
    const body = new Uint8Array(size).fill(120);
    const expected = live_server_proof_digest(
      "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
      "benchmark.hash",
      body,
    );
    for (
      const mode of [
        "single-request-clone",
        "single-request-copy-transfer",
        "single-request-buffer-clone",
        "single-request-buffer-copy-transfer",
      ]
    ) {
      const started = performance.now();
      let workerMs = 0;
      for (let attempt = 0; attempt < 100; attempt++) {
        const pending = response();
        const transfer = mode.endsWith("copy-transfer");
        const copy = transfer ? body.slice() : body;
        worker.postMessage(
          { body: mode.includes("buffer-") ? copy.buffer : copy, count: 1 },
          transfer ? [copy.buffer] : [],
        );
        const value = await pending;
        if (value.digest !== expected || body.byteLength !== size) {
          throw new Error("Single-request worker digest or ownership mismatch");
        }
        workerMs += value.elapsedMs;
      }
      rows.push({
        size,
        mode,
        attempts: 100,
        averageRoundTripMs: (performance.now() - started) / 100,
        averageWorkerMs: workerMs / 100,
      });
    }
  }
} finally {
  await worker.terminate();
}
const result = {
  runtime: node ? process.version : Deno.version.deno,
  ready,
  rows,
};
console.log(JSON.stringify(result, null, 2));
await writeFile(process.argv[2], JSON.stringify(result, null, 2));
