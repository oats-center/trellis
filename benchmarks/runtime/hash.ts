// Compare the existing production WASM proof-digest path with WebCrypto.
// The WebCrypto implementation below is measurement-only, not SDK verification.
import { initializeProtocolWasm } from "../../ts/packages/trellis/auth/protocol_wasm.ts";
import { live_server_proof_digest } from "../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm.js";
import { z } from "zod";

const output = z.string().parse(Deno.args[0]);
await initializeProtocolWasm();
const encoder = new TextEncoder();
const context = new Uint8Array(32);
const contextEncoded = btoa(String.fromCharCode(...context)).replace(/=+$/, "");
const subject = "benchmark.hash";
const rows = [];

for (const size of [1024, 262144, 1048576]) {
  const body = new Uint8Array(size).fill(120);
  // Exact protocol framing from build_live_server_proof_input: domain,
  // decoded context digest, subject, and SHA-256(body), each length-prefixed.
  const components = [
    encoder.encode("trellis.live-server-proof.v1"),
    context,
    encoder.encode(subject),
  ];
  const prefixBytes = components.reduce(
    (sum, value) => sum + 4 + value.length,
    0,
  );
  async function webCrypto(): Promise<string> {
    const bodyHash = new Uint8Array(
      await crypto.subtle.digest("SHA-256", body),
    );
    const framed = new Uint8Array(prefixBytes + 4 + bodyHash.length);
    const view = new DataView(framed.buffer);
    let offset = 0;
    for (const part of [...components, bodyHash]) {
      view.setUint32(offset, part.length);
      framed.set(part, offset + 4);
      offset += 4 + part.length;
    }
    const digest = new Uint8Array(
      await crypto.subtle.digest("SHA-256", framed),
    );
    return btoa(String.fromCharCode(...digest)).replace(/\+/g, "-").replace(
      /\//g,
      "_",
    ).replace(/=+$/, "");
  }
  const wasm = () => live_server_proof_digest(contextEncoded, subject, body);
  const expected = wasm();
  if (await webCrypto() !== expected) throw new Error("Digest mismatch");
  for (let warmup = 0; warmup < 100; warmup++) {
    wasm();
    await webCrypto();
  }
  for (let repeat = 0; repeat < 3; repeat++) {
    for (
      const backend of repeat % 2
        ? ["webcrypto", "wasm"]
        : ["wasm", "webcrypto"]
    ) {
      const delays: number[] = [];
      let tick = performance.now() + 5;
      const timer = setInterval(() => {
        const time = performance.now();
        delays.push(Math.max(0, time - tick));
        tick = time + 5;
      }, 5);
      const before = performance.now();
      let count = 0;
      let actual = "";
      while (performance.now() - before < 1000) {
        actual = backend === "wasm" ? wasm() : await webCrypto();
        count++;
      }
      const elapsed = performance.now() - before;
      await new Promise((resolve) => setTimeout(resolve, 10));
      clearInterval(timer);
      if (actual !== expected) {
        throw new Error("Digest mismatch after measurement");
      }
      delays.sort((a, b) => a - b);
      rows.push({
        backend,
        size,
        repeat,
        count,
        elapsedMs: elapsed,
        bodyMiBPerSecond: count * size / 1024 / 1024 / (elapsed / 1000),
        timerSamples: delays.length,
        maxTimerDelayMs: delays.at(-1),
        p95TimerDelayMs:
          delays[Math.min(delays.length - 1, Math.floor(delays.length * .95))],
      });
      console.log(JSON.stringify(rows.at(-1)));
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
}
await Deno.writeTextFile(
  output,
  JSON.stringify(
    {
      host: Deno.hostname(),
      deno: Deno.version,
      boundary:
        "two SHA-256 digests plus protocol framing; WASM includes JS/WASM copies; WebCrypto includes native submission/copies; no signature verification",
      timer:
        "5 ms timer; one-second sustained sequential batches; wasm blocks timers intentionally, WebCrypto awaits native operations",
      rows,
    },
    null,
    2,
  ),
);
