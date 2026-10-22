# Streaming Transfer performance

All measurements: Linux x64, Xeon Gold 6426Y, Node 24.19.0, loopback NATS,
production authentication, three samples after one warmup unless noted, run
serially from frozen source snapshots with prebuilt release binaries (each run
records its revision, overlay diff and binary hashes). Rates are MiB/s from
median elapsed time. "Server CPU" sums provider, NATS and Trellis server for
Trellis rows and HTTP provider plus NATS for HTTP rows.

The work follows `TRANSFER_PLAN` phases: matched baselines (1), native
TypeScript hashing (2), direct `stream;` delivery (3), and frame and window
tuning (4).

## Outcome

| 128 MiB, MiB/s               | Baseline TS | Now TS | Baseline Rust | Now Rust |
| ---------------------------- | ----------: | -----: | ------------: | -------: |
| Upload, staged copy+verify   |        12.9 |   30.0 |          41.9 |     39.4 |
| Upload to handler, stream    |           — |   81.8 |             — |    171.0 |
| Upload, stream into store    |           — |   46.8 |             — |    128.0 |
| Download from store          |        49.9 |   97.6 |          62.9 |     50.1 |
| Download, generated stream   |           — |  145.1 |             — |    316.5 |
| HTTP stream upload (TLS off) |           — |  674.3 |             — |    838.6 |

"Now" is the 16 MiB-window build on loopback with 256 KiB TypeScript frames
(the 1 MiB TypeScript frame change was measured separately, below). HTTP parity
is not achieved; the gap is now Trellis endpoint CPU, not storage hashing,
chunking or windows.

## Phase 1: matched baselines

Results directories: `p0-ts-baseline`, `p0-rust-baseline` (unmodified harness on
this host), `p1-ts`, `p1-rust`, `p1-ts-tls`, `p1-rust-tls`, `p1-*-chunk262144`,
`p1-*-chunk1048576`, `p1-ts-512`, `p1-rust-512`.

### The Rust HTTP figure was a 4 KiB chunking artefact

The previous Rust HTTP upload rate (57.3 MiB/s) came from `ReaderStream::new`
emitting 4 KiB chunks on the client and download server, plus an unbuffered
`tokio::fs` write per chunk on the upload server. With one declared chunk
policy in both languages:

| 128 MiB, MiB/s     | Rust before | Rust 64 KiB | Rust 256 KiB | Rust 1 MiB | TS 64 KiB | TS 256 KiB | TS 1 MiB |
| ------------------ | ----------: | ----------: | -----------: | ---------: | --------: | ---------: | -------: |
| HTTP upload, raw   |        62.5 |       176.9 |        281.4 |      288.4 |     279.0 |      297.9 |    358.9 |
| HTTP download, raw |       140.9 |       841.7 |        817.9 |      950.6 |     384.3 |      542.8 |    512.6 |

Raw upload writes a local file and re-reads it to hash, as before. At matched
256 KiB to 1 MiB chunks the languages are within a plausible range of each
other; the remaining Rust upload gap is the `tokio::fs` blocking-pool hop per
write. Rust is faster everywhere else.

### Three workload categories, matched outcomes

Plaintext, 128 MiB, MiB/s (Phase 1 code):

| Category      | Workload                              |  Rust | TypeScript |
| ------------- | ------------------------------------- | ----: | ---------: |
| stream→stream | HTTP upload, hash and discard         | 837.4 |      624.7 |
| stream→stream | HTTP download, generated              | 845.6 |      569.5 |
| stream→stream | raw Core NATS, best window            | 811.3 |      446.2 |
| stream→store  | HTTP upload into the provider's store | 147.3 |       47.6 |
| stream→store  | Trellis staged upload (commit only)   |  79.8 |       26.1 |
| stream→store  | Trellis download from the store       |  54.5 |       53.1 |
| stream→store  | direct Object Store write             | 471.1 |       50.7 |
| complete      | Trellis upload, copy and verify       |  36.6 |       13.2 |

Mutual TLS with per-request signatures costs HTTP 10 to 30 percent in the
stream modes and almost nothing once storage dominates.

### Protocol byte overhead is negligible

From the new production counters, cumulative over each run:

| Run        | Upload overhead | Download overhead |
| ---------- | --------------: | ----------------: |
| TypeScript |          0.231% |            0.202% |
| Rust       |          0.062% |            0.120% |

TypeScript frames are 256 KiB, Rust frames about 1 MiB; the header block
(descriptor, proof, authority) is the same size, so smaller frames pay more.
Byte overhead is not where Transfer loses time.

## Phase 2: native hashing in TypeScript

`tools/perf/frame_digest.ts` times each hashing pass on this host:

| Path                                   | MiB/s |
| -------------------------------------- | ----: |
| `js-sha256` (Object Store SDK default) |    68 |
| `@noble/hashes`                        |   205 |
| WASM `transferFrameDigest`             |   285 |
| `node:crypto`                          | 1,530 |

A TypeScript staged upload took five pure-JavaScript passes on the provider
(object digest, Object Store write, staged read verification twice, store
reader EOF check) besides the WASM frame digest. Phase 2 moves all of them to
`node:crypto` and selects the Object Store SDK's native backend.

| TypeScript, 128 MiB                   | Phase 1 | Phase 2 |
| ------------------------------------- | ------: | ------: |
| Upload, copy and verify, MiB/s        |    13.2 |    30.0 |
| Staged upload, MiB/s                  |    26.1 |    53.2 |
| Store-backed download, MiB/s          |    53.1 |    89.8 |
| Direct Object Store write, MiB/s      |    50.7 |   147.9 |
| Direct Object Store read, MiB/s       |   132.8 |   283.2 |
| HTTP upload into the store, MiB/s     |    47.6 |   134.1 |
| Upload server CPU, core-s/GiB         |   142.7 |    92.0 |
| Live dispatch under transfer, p50/p95 | 131/164 |  76/106 |
| Concurrent 128 MiB up+down, s         |    12.5 |     5.5 |

The WASM frame digest is now the slowest remaining pass at about 3.5 ms per
1 MiB frame. Live dispatch improved without changing the frame size, so the
earlier Live tail was synchronous per-frame hashing, not frame size.

## Phase 3: direct streaming

Results: `p3-ts`, `p3-rust`, `p3rerun-ts-512`, `p3-rust-512`. `p3-ts-512`
overlapped a `cargo check` and is retained but marked contaminated; the rerun
replaces it in these tables.

`stream;` Operations hand the caller's bytes to a running handler.
`download-stream` serves generated bytes from a provider reader with declared
metadata. Both compare directly with HTTP `stream` mode and raw Core NATS.

| 128 MiB, MiB/s (server CPU s/GiB)         |   TypeScript |         Rust |
| ----------------------------------------- | -----------: | -----------: |
| Trellis upload, stream to handler         |  68.4 (31.2) |  81.9 (14.7) |
| Trellis download, generated stream        | 125.7 (15.8) | 134.2 (10.5) |
| HTTP upload, stream                       |  674.3 (2.7) |  838.6 (1.2) |
| HTTP download, stream                     |  562.2 (0.7) | 1087.4 (0.5) |
| raw Core NATS, best window                |        416.7 |        905.1 |
| Trellis upload, stream into service store |  50.2 (48.5) |  61.4 (27.8) |
| Trellis upload, staged commit only        |  52.8 (46.7) |  75.1 (21.0) |
| Trellis upload, staged then copy+verify   |  29.0 (90.1) |  36.8 (46.9) |

A handler that writes the stream once into its own store now beats the staged
copy-and-verify workflow by 1.7× in both languages, and matches the staged
commit alone in TypeScript. Staged uploads did not regress against Phase 2.

The remaining gap is the Trellis endpoint itself, not the broker, window or
storage: on identical loopback NATS with the same 4 MiB window, the raw
transport carries 5 to 11 times more, and Trellis spends roughly 10 to 30
times the server CPU per GiB of HTTP. In Rust the provider runs at about one
full core while streaming; that is several milliseconds of CPU per 1 MiB
frame, far more than its two SHA-256 passes and one signature check.

## Phase 4: frame quantum and windows

Results: `q262144`, `q524288`, `q1048576`, `rust-loaded` (frame quantum and
the first Rust loaded-Live run), and `w{4,16,64}-d{0,10}-{ts,rust}` (window
variants; `d10` runs inside a private network namespace with `netem` adding
10 ms to every loopback packet, a 20 ms round trip). Window variants are
production builds that differ only in the protocol's default constants; the
snapshot `.benchmark-overlay.diff` records each change.

### Frame quantum

Bound chosen in advance: loaded Live and RPC p95 within 25% of 256 KiB frames.

| TypeScript, 128 MiB         | 256 KiB | 512 KiB | 1 MiB |
| --------------------------- | ------: | ------: | ----: |
| Live p95 under transfer, ms |     110 |     115 |   134 |
| RPC p95 under transfer, ms  |      41 |      44 |    50 |
| Staged upload, MiB/s        |    50.8 |    57.7 |  66.7 |
| Stream upload, MiB/s        |    69.8 |    82.3 |  89.1 |

1 MiB stays inside the bound, so TypeScript now fills frames to the negotiated
limit like Rust. Rust's first loaded-Live run, at 1 MiB frames, measured Live
p95 139 ms and RPC p95 62 ms under transfer (31 and 17 ms idle); no change.
Each figure is one cohort of 30 probes.

### Windows

128 MiB, MiB/s. Defaults now 64 frames / 16 MiB.

| Workload        | Lang | w4 0 ms | w16 0 ms | w64 0 ms | w4 10 ms | w16 10 ms | w64 10 ms |
| --------------- | ---- | ------: | -------: | -------: | -------: | --------: | --------: |
| stream upload   | Rust |    81.4 |    171.0 |    194.5 |  failed¹ |   failed¹ |   failed¹ |
| stream download | Rust |   125.9 |    316.5 |    304.7 |  failed¹ |   failed¹ |   failed¹ |
| staged upload   | Rust |    67.6 |     85.1 |    101.6 |  failed¹ |   failed¹ |   failed¹ |
| stream upload   | TS   |    69.0 |     81.8 |     86.7 |     40.1 |      62.5 |      67.5 |
| stream download | TS   |   119.5 |    145.1 |  failed² |     54.9 |     108.4 |     105.2 |
| staged upload   | TS   |    51.9 |     48.0 |  failed² |      5.4 |       5.4 |       5.4 |

¹ The Rust lane's native Device Code login timed out under 10 ms delay before
any transfer ran; this is a harness limit, not a transfer result.
² With 64 MiB windows on loopback the TypeScript provider became unreachable
mid-run (`Trellis could not reach the transfer provider`). 64 MiB is therefore
not a safe default.

16 MiB captures most of the gain everywhere it was measured. Staged and
store-backed TypeScript uploads under delay sit at 5.4 MiB/s regardless of
window: the Object Store write, not Transfer, bounds them on a delayed path.

### Admission

Each process admits receive windows from one 256 MiB budget (16 default
windows). The live admission test fills the budget with held windows, is
refused with `transfer receive capacity exhausted`, and is admitted again once
a transfer ends.

## What each feature costs

Per GiB on loopback, from the Phase 3 stream lanes (4 MiB windows):

| Path                         | Server CPU s/GiB, TS | Rust |
| ---------------------------- | -------------------: | ---: |
| HTTP stream upload           |                  2.7 |  1.2 |
| Trellis stream upload        |                 31.2 | 14.7 |
| Trellis stream into store    |                 48.5 | 27.8 |
| Trellis staged + copy+verify |                 90.1 | 46.9 |

Bytes on the wire cost 0.06–0.23% (Phase 1). Storage roughly doubles the CPU
of a stream; staging plus a handler copy doubles it again. The largest
remaining cost is the Transfer endpoint itself: the Rust provider runs at
about one core while streaming, several milliseconds of CPU per 1 MiB frame,
well above its two SHA-256 passes and one signature. It has not been profiled
yet; that is the next investigation (`perf record` on the provider during
`upload-stream`).

## Remaining limits and follow-ups

- **Release decision (D1).** Phase 4 changed Transfer v1 in place: windows
  above the old fixed values and the activation's receive-window fields. This
  assumes v1 is refined before, or explicitly unstable at, 0.100.0.
- **Unknown-digest streams.** Receive grants still declare size and digest up
  front, so generated content of unknown digest cannot be streamed; that needs
  a grant change.
- **One-pass frame proofs.** Proposed in `design/core/files-transfer-patterns.md`,
  not implemented.
- **Rust under delay.** The native login path needs a longer budget before
  delayed Rust lanes can run.
- **Host notes.** Idle leftover test processes from another worktree were
  present during these runs and used negligible CPU. Samples are three per
  workload after one warmup; differences under about 10% are within noise.

## Reproduction

```sh
cargo xtask build --release -p trellis-server -p trellis-cli
cargo build --release --locked --manifest-path benchmarks/runtime/native/Cargo.toml --target-dir target
node --experimental-transform-types tools/perf/transfer.ts --output=benchmarks/runtime/results/ts \
  --samples=3 --warmups=1 --calls=10 --sizes=8388608,134217728 [--http-tls] [--http-chunk=262144]
node --experimental-transform-types benchmarks/runtime/run.ts --lane=rust \
  --rust-bin=target/release/trellis-performance --output=benchmarks/runtime/results/rust \
  --samples=3 --warmups=1 --calls=10 --sizes=8388608,134217728 --sessions=1 --idle-seconds=1
node --experimental-transform-types tools/perf/frame_digest.ts
```

Delayed runs wrap the same command in
`unshare --user --net --map-root-user sh -c 'ip link set lo up && tc qdisc add dev lo root netem delay 10ms limit 100000 && exec "$@"' _`,
which affects only that private namespace.
