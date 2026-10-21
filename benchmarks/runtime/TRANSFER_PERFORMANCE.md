# Streaming Transfer performance

## Outcome and measurement boundary

The retained changes improve large-object uploads without changing Transfer v1,
authentication, per-frame proofs, credit validation, or durable completion.
**HTTP parity is not achieved.** Rust download throughput does not improve;
TypeScript remains substantially slower than the HTTP lower bound.

Measurements below use the `oauth` baseline `f4e07c2b` and final implementation
`a9fd4bd9`, including `oauth` through `09d83630`. All final scores and verification
use rebuilt RC.3 executables. Earlier framing ablations and the separate native
hashing control predate that metadata/readiness-test merge.

All rates are MiB/s, calculated from median elapsed time: three measured objects
after one warmup, one active transfer, Node 24.19.0, Linux x64, Xeon Gold 6426Y,
loopback NATS TCP, warm storage, ordinary production authentication. Lanes ran
serially without concurrent builds or tests. Observers were disabled for scored
runs. These are local measurements, not WAN or statistical significance claims.
The matched baseline and final size lists are 8/128 MiB; 512 MiB runs are
separate. Finite backend measurements now run in their own process instead of
the provider's startup path, so allocator and broker-cache history are not
identical.

“Committed” means the authenticated Transfer staging commit. “Full upload” also
waits for the Operation to persist the object and read it back for verification.
Source exhaustion is not either completion boundary. HTTP uses unauthenticated
HTTP/1.1 keep-alive and filesystem storage; it is not a security-, write-count-,
or fsync-equivalent implementation of this workflow.

## Repeated 128 MiB results

| Workflow                        | Baseline MiB/s | Final MiB/s | Baseline / final seconds |
| ------------------------------- | -------------: | ----------: | -----------------------: |
| Rust full verified upload       |            9.1 |        37.3 |           14.066 / 3.427 |
| Rust verified download          |           58.7 |        55.5 |            2.182 / 2.308 |
| TypeScript full verified upload |           11.2 |        13.6 |           11.408 / 9.387 |
| TypeScript verified download    |           47.3 |        52.8 |            2.706 / 2.425 |

Rust full upload is **4.10×** the initial rate; TypeScript upload improves
**21.5%**, and TypeScript download **11.6%**. Rust download is approximately 5%
slower in this final set. The previous pre-merge repeat measured 59.9 MiB/s
download and 39.2 MiB/s upload with the same algorithms; the reported final table
does not select those faster results. No Rust download improvement is established.

The staging-commit boundary improves from 11.508 to 1.556 seconds in Rust, and
6.589 to 4.744 seconds in TypeScript. Full completion remains slower because it
includes the service's additional persisted write and verified readback.

| Current isolated/lower-bound path, 128 MiB | Rust MiB/s | TypeScript MiB/s |
| ------------------------------------------ | ---------: | ---------------: |
| Object Store write                         |      354.6 |             50.4 |
| Object Store verified read                 |      234.8 |             83.8 |
| HTTP full verified upload                  |       57.3 |            217.3 |
| HTTP verified download                     |      120.1 |            165.0 |

Storage alone does not explain the remaining full-path gap. The TypeScript
storage-write rate remains approximately unchanged with the retained default
hash backend.

## Largest supported Operation upload

The existing Operation upload limit is 512 MiB. A 1 GiB attempt was rejected by
that limit; the limit was not raised for this benchmark.

| 512 MiB, three samples plus warmup | Full upload seconds / MiB/s | Download seconds / MiB/s | Staging commit seconds |
| ---------------------------------- | --------------------------: | -----------------------: | ---------------------: |
| Rust                               |               12.977 / 39.5 |             8.790 / 58.2 |                  5.841 |
| TypeScript                         |               37.981 / 13.5 |            10.198 / 50.2 |                 19.399 |

All measured payloads, commits, and persisted readbacks succeeded. Neither
implementation collects the object in memory to obtain these results.

## CPU and memory

CPU below is mean **core-seconds per complete workflow**, summed over the
provider, NATS, and Trellis server. It is not CPU utilization: finishing sooner
can raise utilization while reducing total CPU work. The load generator is
excluded from this table and remains separately reported in the raw reports.

| 128 MiB workflow    | Baseline server core-seconds | Final server core-seconds |
| ------------------- | ---------------------------: | ------------------------: |
| Rust upload         |                        16.79 |                      5.61 |
| Rust download       |                         3.55 |                      3.28 |
| TypeScript upload   |                        21.66 |                     18.56 |
| TypeScript download |                         5.12 |                      4.95 |

Observed upload-window peak RSS, MiB:

| Process             | Baseline 128 MiB | Final 128 MiB | Final 512 MiB |
| ------------------- | ---------------: | ------------: | ------------: |
| Rust provider       |            48.78 |         53.18 |         55.67 |
| Rust caller         |            46.49 |         44.70 |         48.00 |
| TypeScript provider |           301.48 |        310.33 |        279.41 |
| TypeScript caller   |           267.32 |        281.75 |        280.42 |

This is **not a measured general memory reduction**. Provider RSS does not grow
with the additional 384 MiB of payload in these runs. NATS cache/GC behavior and
process warmup materially affect whole-process peaks; those figures are not the
SDK's retained-payload accounting or a bound on broker memory.

## Why these changes were retained

- **Rust Object Store upload:** at most eight 128 KiB chunk acknowledgements are
  outstanding (1 MiB). Metadata publishes only after every chunk ACK succeeds.
  async-nats 0.50 exposes a sequential `ObjectStore::put`, so the bounded writer
  uses its public JetStream publishing APIs and native Object Store format.
  A single-ACK control using the same buffers took 13.78 seconds for the full
  128 MiB upload; pipelining took 8.58 seconds before subsequent optimizations.
  Backend write rate moved from approximately 110 to 354 MiB/s. Source failure,
  cancellation, chunk rejection, overwrite, and official Object Store reads
  exercise the real broker and preserve the ambiguous metadata-commit boundary.
- **Read management:** TypeScript no longer issues consumer INFO on every
  healthy finite batch. Failure reconciliation and verified recovery cursors
  remain. Rust retains finite byte-capped reads. Overlapping pulls with separate
  inboxes were rejected because their batches could not safely be interpreted
  as contiguous; the same-inbox SDK-stream experiment was also removed after a
  reproducible backend stall. No retries or longer timeouts conceal that stall.
- **Frame formation:** Rust coalesces only immediately ready reads; TypeScript
  uses one shared byte-sized Web Stream queue and caps ready frames at 256 KiB.
  Neither waits for a frame to fill or uses a flush timer. Short reads, mutable
  Node buffers, delayed sources, cancellation, and source errors are covered.
- **Authorization reuse:** an unchanged retained authority context is reused,
  but each DATA/control proof and current authority/transport fence is still
  checked. The six-sample finite-reader control shortened full native upload by
  another 11%. No cached verification may outlive its authority.
- **Idle Live renewal:** 512 MiB runs exposed a real correctness bug, not a
  throughput outlier. An idle TypeScript route could sign its first offer using
  an expired retained guard after ordinary authority renewal. The opening path
  now reconciles before signing, as Rust already does. The real short-lifetime
  regression failed before this fix and passed afterward.

## Live responsiveness and frame quantum

The same provider serves simultaneous 128 MiB upload/download work while a
separate caller opens 30 Live sessions. Dispatch latency is measured from actual
dispatch to the first application frame. The offered-load duration additionally
includes time queued behind the harness's one-outstanding-probe bound. P95 below
uses nearest rank, not interpolation.

| Matched TypeScript frame-policy control | Full upload seconds | Mixed transfer seconds | Live dispatch p50 / p95 ms |
| --------------------------------------- | ------------------: | ---------------------: | -------------------------: |
| No ready coalescing                     |              11.161 |                 13.856 |              144.1 / 197.1 |
| Near-1 MiB ready frames                 |               8.787 |                 11.158 |              174.2 / 226.8 |
| 512 KiB ready frames                    |               9.171 |                 11.581 |              131.6 / 171.6 |
| **256 KiB, retained**                   |           **9.408** |             **11.848** |          **120.3 / 153.8** |

The retained policy trades some peak bulk rate for the better observed Live
tail. The final independent run recorded loaded dispatch p50/p95 of
124.2/156.0 ms at 128 MiB and 128.6/160.8 ms at 512 MiB. This does not mean there
is no backlog: offered-load p95 was 2.196/2.380 seconds respectively; median
scheduler backlog was 1.063/1.192 seconds. Raw samples retain both quantities.
These loaded-Live measurements cover the TypeScript provider; native loaded-Live
latency has not been benchmarked by this lane.

Small-object improvement is not established. The earlier `4c5ad66f` 64 KiB
TypeScript download was 68 ms, but full upload was 206 ms versus 162 ms in the
pre-framing control.
Three samples and different preparation/cache history do not isolate that
difference to frame formation. Do not advertise a small-upload latency win.

## Remaining limits and separately reviewed follow-up

### Separate frame and wait diagnostics

The final `ts-rc3-diagnostic` and `rust-rc3-diagnostic` runs are instrumented,
single-sample runs, not throughput scores. TypeScript upload emitted 1,022
256 KiB frames, two 192 KiB frames, and two 64 KiB frames across its workflows;
download emitted 935 256 KiB and 178 128 KiB frames. Rust upload emitted 128
1,044,480-byte frames and one 512 KiB frame. Native download retained immediately
available short frames, from 4 KiB through the negotiated 1,044,480-byte maximum.
Neither observer overflowed its frame-size histogram.

Production client credit-wait histograms recorded 321 waits / 6.103 seconds
aggregate / 55.7 ms maximum for TypeScript, and 53 / 0.978 seconds / 44.9 ms for
Rust. Provider storage-refill waits totaled 2.475 seconds and 1.786 seconds
respectively, including already-ready deliveries. These aggregate different
workflow mixes and must not be compared as per-object or uninstrumented times.

The passive observer matched 6,144 TypeScript and 6,152 Rust storage chunk ACKs,
with means of 0.052/0.551 ms and maxima of 12.5/27.1 ms. Matched fetch-first-response
means were 2.422/2.972 ms. These are broker-observed extra-copy timings, not SDK
enqueue-to-ACK or disk-fsync latency. The bounded correlation collector reported
4,097/6,153 evictions and 4,095 unresolved entries in each run: its timing
histograms describe matched observations, **not an exhaustive accounting of
every request**. Production credit/refill histograms are independent of that
passive correlation limitation. Consumer INFO totals were eight for TypeScript
and zero for Rust; setup/open requests are included, not just steady state.

### Existing fixed windows

Transfer v1 still caps outstanding DATA at 4 MiB and 16 frames. A 64 KiB producer
can therefore use only 1 MiB of its byte credit. With DATA fixed at 64 KiB, the
raw-NATS control raises only the frame ceiling from 16 to 64: 128 MiB throughput
improves from 333.5 to 372.9 MiB/s; at 512 MiB, from 340.6 to 380.0 MiB/s.
This is a raw ceiling without Transfer proofs or storage, not an end-to-end
prediction. At the larger raw frame size, 128 MiB reaches 447.7 MiB/s.

These loopback results do not establish the 4 MiB byte ceiling as the primary
current bottleneck. On a delayed path it can become one: the theoretical ceiling
is window bytes divided by credit-return delay, or 100 MiB/s at 40 ms and
40 MiB/s at 100 ms. Actual WAN measurements are still required.

**Proposed v2, not implemented in this change:**

1. Negotiate immutable per-transfer receive byte and frame budgets in the
   authenticated opening and signed grant. Keep the NATS payload limit separate
   from the aggregate in-flight budget. Do not add caller chunk-size knobs.
2. Reserve receive capacity before issuing credit, from one process-wide byte
   budget and a bounded frame/metadata budget. Charge all active streams, not
   each stream against an independently unlimited allowance. Release reservations
   on every terminal path. The granted byte limit must be supported by enough
   frame slots at the implementation's ready-frame quantum.
3. Keep current defaults until measured. Evaluate candidate ceilings of 64 MiB
   per transfer and 256 MiB per process, alongside the current 4 MiB control,
   against actual memory and fairness. Those candidates are not selected
   production defaults. Admission must queue or reject when aggregate capacity
   is exhausted; it must not silently overcommit.
4. Preserve cumulative sequence/credit validation, exact frame proofs,
   authorization renewal/revocation, cancellation, and durable commit. Keep
   bounded DATA scheduling so more credit does not starve Live or other streams.
   Start with negotiated fixed budgets; mid-transfer adaptation is unnecessary
   until its benefit is measured.
5. Use a distinct protocol format and version-specific validation. Advertise
   supported versions in the signed opening before executing the workflow; use
   v1 only when explicitly supported. Do not widen v1 validators or retry a
   potentially executed Operation to discover protocol support.
6. Gate selection on isolated real-NATS delayed-network tests, concurrent bulk
   flows, Live opening tails, paused readers, cancellation, authority changes,
   and process-level memory. Test credit exhaustion and budget admission, not
   configuration literals. No host-wide network changes are needed for this PR.

### Existing native SHA-256 option

A separate same-512-KiB-frame control enables the installed NATS Object Store
SDK's native Node SHA-256 backend. Backend write time drops from 2.529 to
0.926 seconds; full verified upload from 9.171 to 5.470 seconds. This is a
measured opportunity, **not part of the shipped figures or implementation**.
Its selector is process-wide and can affect application-owned NATS clients.
Prefer explicit host ownership or an upstream per-store hash factory over
silently changing a third-party global when importing Trellis. Frame proofs and
digest algorithms must remain unchanged.

## Reproduction and artifacts

Use the prepared production executables and commands in [README.md](README.md).
The scored directories under the ignored `results/` directory are
`rust-baseline-01`, `ts-baseline-02`, `rust-rc3-verified-final`,
`ts-rc3-verified-final`, `rust-rc3-512MiB-verified-final`, and
`ts-rc3-512MiB-verified-final`. Each records source
revision/diff, executable hashes, environment, raw samples, phase boundaries,
per-process CPU windows, RSS/PSS, and the generated report. Retain failed and
diagnostic runs separately; never replace failed samples with successful ones.

For example, after building the native benchmark with the repository-local
Cargo patches already in its manifest:

```sh
node --experimental-transform-types tools/perf/transfer.ts \
  --output=benchmarks/runtime/results/ts-reproduce \
  --samples=3 --warmups=1 --calls=10 --sizes=8388608,134217728

node --experimental-transform-types benchmarks/runtime/run.ts \
  --lane=rust --rust-bin=target/release/trellis-performance \
  --output=benchmarks/runtime/results/rust-reproduce \
  --samples=3 --warmups=1 --calls=10 --sizes=8388608,134217728 \
  --sessions=1 --idle-seconds=1
```

Run the 512 MiB case separately with `--sizes=536870912`. Use a separate
`--diagnostics` run for passive frame/request distributions and production wait
histograms; do not compare its instrumented throughput with unobserved scores.
