import init, {
  live_server_proof_digest,
} from "../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm.js";

const url = new URL(
  "../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bg.wasm",
  import.meta.url,
);
const node = globalThis.process?.versions?.node;
const bytes = node
  ? (await import("node:fs/promises")).readFile(url)
  : globalThis.Deno
  ? Deno.readFile(url)
  : (await fetch(url)).arrayBuffer();
await init({ module_or_path: await bytes });
const port = node
  ? (await import("node:worker_threads")).parentPort
  : globalThis;
const post = (value) => port.postMessage(value);
const receive = (data) => {
  try {
    const body = data.body instanceof ArrayBuffer
      ? new Uint8Array(data.body)
      : data.body;
    const before = performance.now();
    let digest;
    for (let i = 0; i < data.count; i++) {
      digest = live_server_proof_digest(
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "benchmark.hash",
        body,
      );
    }
    post({
      digest,
      elapsedMs: performance.now() - before,
      secure: globalThis.isSecureContext,
      subtle: !!globalThis.crypto?.subtle,
      isolated: globalThis.crossOriginIsolated,
    });
  } catch (error) {
    post({ error: String(error) });
  }
};
if (node) port.on("message", receive);
else globalThis.onmessage = (event) => receive(event.data);
post({
  ready: true,
  subtle: !!globalThis.crypto?.subtle,
  secure: globalThis.isSecureContext,
});
