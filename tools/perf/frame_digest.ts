/**
 * Per-frame hashing cost on the TypeScript Transfer path. Times every SHA-256
 * pass a frame takes: the WASM compact frame digest (request proof input), the
 * portable `@noble/hashes` object hash, the Object Store SDK's default
 * `js-sha256`, and native `node:crypto`. Single-threaded, warm, in-process.
 *
 *   node --experimental-transform-types tools/perf/frame_digest.ts
 */
import { createHash } from "node:crypto";
import { sha256 as noble } from "@noble/hashes/sha256";
import { sha256 as jsSha256 } from "js-sha256";
import {
  transferFrameDigest,
  transferGenerateId,
} from "../../ts/packages/trellis/auth/protocol_wasm.ts";

const totalBytes = 256 * 1024 * 1024;
const transferId = transferGenerateId();
const rows: string[] = [
  "| Frame | Path | MiB/s | µs / frame |",
  "|---:|---|---:|---:|",
];
for (const frameBytes of [256 * 1024, 1024 * 1024]) {
  const frame = Uint8Array.from({ length: frameBytes }, (_, i) => i % 251);
  const frames = totalBytes / frameBytes;
  const paths: Array<[string, () => void]> = [
    [
      "WASM transferFrameDigest",
      () => {
        for (let seq = 1; seq <= frames; seq++)
          transferFrameDigest(
            {
              transferId,
              direction: "send",
              sequence: String(seq),
              kind: "data",
            },
            frame,
          );
      },
    ],
    [
      "@noble/hashes incremental",
      () => {
        const hash = noble.create();
        for (let i = 0; i < frames; i++) hash.update(frame);
        hash.digest();
      },
    ],
    [
      "js-sha256 incremental (Object Store default)",
      () => {
        const hash = jsSha256.create();
        for (let i = 0; i < frames; i++) hash.update(frame);
        hash.digest();
      },
    ],
    [
      "node:crypto incremental",
      () => {
        const hash = createHash("sha256");
        for (let i = 0; i < frames; i++) hash.update(frame);
        hash.digest();
      },
    ],
  ];
  for (const [name, run] of paths) {
    run(); // warm
    const samples: number[] = [];
    for (let trial = 0; trial < 3; trial++) {
      const started = performance.now();
      run();
      samples.push(performance.now() - started);
    }
    const ms = samples.sort((a, b) => a - b)[1];
    rows.push(
      `| ${frameBytes / 1024} KiB | ${name} | ${(totalBytes / 1048576 / (ms / 1000)).toFixed(0)} | ${((1000 * ms) / frames).toFixed(0)} |`,
    );
  }
}
console.log(rows.join("\n"));
