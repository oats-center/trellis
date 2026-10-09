# Runtime performance

Run from the repository root on Linux. This uses the ordinary production server,
real NATS/JetStream, generated clients, and the existing testkit for isolated
provisioning. Execution does **not** build executables.

```sh
cargo build --release -p trellis-server
cargo xtask install
deno task -c ts/deno.json bench:runtime --output=/tmp/opencode/trellis-perf-baseline
deno task -c ts/deno.json bench:runtime:report /tmp/opencode/trellis-perf-baseline
```

## Matched Live frame and event comparisons

The `frames` lane uses generated TypeScript clients and a real TypeScript
provider. It verifies every Live value and sequence across one or more streams,
cancels a running Operation halfway through delivery, and checks signed event
round trips in bounded batches. Event listeners use the supported ephemeral mode
in both versions; the provider contract explicitly needs `Changed` subscription
permission. This does not measure durable consumer replay.

```sh
deno run -A -c ts/deno.json benchmarks/runtime/run.ts --lane frames --samples 3 --calls 500 --sessions 1,4 --rpc-value-bytes 262144 --max-outstanding 32 --server target/release/trellis-server --cli target/release/trellis --output .verification/frame-candidate
deno run -A -c ts/deno.json benchmarks/runtime/report.ts .verification/frame-candidate .verification/frame-baseline
```

Repeat with 1 KiB values and 2,000 frames per stream. Baseline and candidate
must use byte-identical benchmark/contract/WASM assets and the same prebuilt
server/CLI; only the SDK path under comparison changes. Retain source snapshots,
raw samples, production telemetry, and process resource timelines. Alternate
case order and use fresh services; small repeated cohorts are observations, not
universal performance guarantees. Use workspace-local temp directories and a
private NATS cache when required by the environment.

Add `--live-rpc-probes` for an independent 50 RPC/s foreground schedule during
streaming. At most one foreground RPC is outstanding per client. Missed timer
slots and offers while that RPC is outstanding count as **not sent**, not
zero-latency successes. Reports recompute quantiles from actual sent, successful
samples and retain errors and not-sent counts. `liveRpcProbeRate` in metadata
distinguishes this from earlier diagnostic probes paced by frame delivery.

Use the existing `transfer` lane with `--sizes 1048576,8388608` to compare
verified downloads and uploads whose persisted object is read back. See
`ADMISSION.md` for the measured trial costs and proof limits; worker offloading
is not assumed to improve throughput.

## Fixed-arrival-rate admission exploration

The `admission` lane drives either provider language with the same generated
TypeScript client. It adds a configurable delay to Echo, offers requests at a
fixed rate, and cancels a confirmed running Operation halfway through each
burst. Each cancellation must return a terminal Cancelled snapshot. The lane
uses the configured production admission limits without changing request
timeouts.

```sh
cargo build --release --locked --manifest-path benchmarks/runtime/native/Cargo.toml --target-dir target
deno task -c ts/deno.json bench:runtime --lane admission --provider-language rust --rust-bin target/release/trellis-performance --rpc-delay-ms 50 --rpc-value-bytes 262144 --arrival-rate 200 --calls 64 --max-outstanding 128 --samples 3 --sessions 1 --sizes 64 --idle-seconds 0.1 --output /tmp/opencode/admission-rust
deno task -c ts/deno.json bench:runtime:report /tmp/opencode/admission-rust
```

Use `--provider-language typescript` for the other provider. Compare each
against `--arrival-rate 10 --calls 32 --rpc-value-bytes 0`, then repeat the
200/s run with zero appended bytes. `rpc-value-bytes` counts appended ASCII
characters, not the entire signed wire message. The generator cap is a
measurement safeguard, not a service limit: retain and report any generator
drops separately from request failures. A run with request failures exits
unsuccessfully while preserving its samples and process timeline; reporting
those samples must not turn it green.

`metrics.jsonl` retains ordinary production OTLP HTTP/protobuf exports, decoded
with the same upstream schema used by live coverage. The lane requests a 100 ms
export interval and disables trace export consistently for both providers. The
existing `trellis.rpc.server.inflight` metric counts dispatched unary RPCs, not
the client subscription backlog; sampled maxima are lower bounds. Reports also
retain provider/client/broker/runtime process memory during load. Whole-process
PSS is not a queue-byte estimate or a memory limit. The native provider enables
the SDK's existing `telemetry-otlp` feature; no test-only metric hooks are used.

This focused lane is opt-in rather than appended to every repeated performance
suite. See [ADMISSION.md](ADMISSION.md) for source findings, measured evidence,
limitations, and the proposed policy decisions.

Use `--request-limit 16`, `32`, or `64` and `--request-byte-limit` to calibrate
the ordinary shared service budget. Both limits are recorded in run metadata.
TypeScript providers always use worker verification. This starts one ordinary
verifier and one reserved verifier, prioritizing controls over refusal work.
Sustained ordinary queue age grows the pool to `--max-verification-workers`
(three ordinary workers by default, plus the reserved worker), without
shrinking. Use the same native caller
(`--client-language rust --rust-bin <prebuilt executable>`) for worker-ceiling
comparisons. Worker use and caller language are recorded in metadata; these
settings do not imply improved throughput. See the retained trial results and
proof limits in [ADMISSION.md](ADMISSION.md). For sustained rather than
short-burst load, use `--calls 4000 --samples 1
--arrival-rate 200`; each
value-size case offers work for 20 seconds. Refusals remain failed attempts,
never implicit retries or successful samples. The production
`trellis.service.admission.*` metrics report shared occupied slots, retained
incoming bytes, and refusals, separately from unary dispatch metrics and
whole-process memory.

With the native admission caller, `--samples 3 --arrival-rates 200,400,200`
offers successive rates through the same service and verifier pool. `--calls`
applies to each phase; rates must provide one value per sample. Warmups use the
first rate. Each phase drains outstanding calls and finishes its cancellation
probe before the next begins, so this is a stepped workload, not an
instantaneous rate transition with pending calls crossing phases. Raw samples
retain `phaseIndex`, windows retain offered rates and start times, and the
report shows per-phase outcomes and cancellation latency.

For client-memory diagnosis, `--inspect-client` enables Deno's standard debugger
on `127.0.0.1:9229` for the TypeScript workload process only. For provider CPU
or memory diagnosis, `--inspect-provider` enables the same debugger for
TypeScript providers on `127.0.0.1:9230` plus their zero-based index. Neither
option enables an inspector on a native Rust executable. Keep diagnostic runs
separate from capacity measurements: debugger pauses and heap snapshots change
scheduling. Heap snapshots may contain session credentials and request contents;
retain them privately, not in published reports.

`--cpu-profile-provider` uses Deno's native profiler and writes main-thread and
worker-isolate `.cpuprofile` files under the run's `provider-cpu-<index>/`
directory, not a system temporary directory. These cover the provider lifetime,
unlike the offered-window inspector profiles. Production
`trellis.auth.verification.duration` phase metrics and the admission report
separate scheduler wait, dispatch copying, worker execution, complete WASM
verification, boundary/scheduling overhead, and successful post-verification
checks. `worker.execute` contains `worker.verify`; never sum the two.

`python3 benchmarks/runtime/admission_profile.py <isolated-eta-workspace>
<new-result-directory-name> [rates...]`
repeats inline/worker comparisons sequentially using already-built native
clients. The workspace must contain `source/`, `target/release/` executables,
`tmp/`, and the existing isolated caches. The default rates are 200 and 400/s;
the generator cap remains 1,024 requests. Use this as diagnostic tooling, not a
production-default calibration. It retains source hashes, failures, CPU
profiles, and ordinary production OTLP exports.

Capture an offered-window CPU profile with
`deno run -A -c ts/deno.json benchmarks/runtime/profile.ts <output> 9229 client 15`
or `<output> 9230 provider 15` in another terminal on the same machine.

For native admission callers, add `--client-language rust --rust-bin <binary>`
to the admission lane. Provider language is independent: select
`--provider-language rust` for Rust-to-Rust, or `typescript` to isolate the
TypeScript provider from caller costs. Native callers retain every refused,
timed-out, and unsent call, use the same 3-second RPC timeout as the TypeScript
admission caller, and confirm Operation readiness before cancellation probes.
Both generators run in one process; neither configuration alone proves that the
generator is not the bottleneck.

Compare the current production WASM SHA-256 proof-digest path with WebCrypto:
`deno run -A -c ts/deno.json benchmarks/runtime/hash.ts <results.json>`. This
checks identical protocol digests at 1 KiB, 256 KiB, and 1 MiB, then runs three
alternating one-second batches per backend and size. It includes two hashes,
framing, and backend-boundary copies, but no signature verification. Timer
delays measure blocking during sustained batches, not single-request latency or
a production cancellation guarantee. The WebCrypto framing in this benchmark is
measurement-only and is not an alternative SDK verifier.

Probe the existing WASM in a reusable worker with
`deno run -A benchmarks/runtime/hash_worker_probe.mjs <results.json>` or
`node benchmarks/runtime/hash_worker_probe.mjs <results.json>`. It verifies
equal digests and buffer ownership, measures parent timers during worker
batches, and compares per-request typed-array and ArrayBuffer messages.
Copy-and-transfer preserves the original payload. This is a feasibility probe,
not an SDK authorization worker or acceptance of revocation, expiry, packaging,
or overload.

For transport diagnosis, `--nats-diagnostics` records the isolated broker's
ordinary `/varz` and `/connz?state=any` monitoring responses in `nats.jsonl`,
plus owned-process TCP ports in the resource timeline. This distinguishes broker
slow-consumer closures from SDK admission refusals and maps affected sockets to
their process. Monitoring is loopback-only; keep these diagnostic runs separate
from capacity comparisons because polling adds work. `--keep-workdir` preserves
runtime state for further inspection. Service shutdown logs identify the phase
being awaited; `cleanup.json` records any benchmark child requiring forced
termination instead of treating that intervention as successful shutdown.

## Complete repeated suite

Build producer artifacts once, before measuring. The suite executes prebuilt
server/CLI/Rust binaries and a prebundled browser workload; it only generates
and type-checks the requested native IDL contract variants during preparation.

```sh
cargo xtask install
cargo build --release -p trellis-server -p trellis-cli
cargo build --release --locked --manifest-path benchmarks/runtime/native/Cargo.toml --target-dir target
deno task -c ts/deno.json bench:runtime:browser:build
deno task -c ts/deno.json bench:runtime:suite --server=target/release/trellis-server --cli=target/release/trellis --rust-bin=target/release/trellis-performance --browser-bundle=benchmarks/runtime/browser.bundle.js --output=/tmp/opencode/trellis-perf-suite
```

Requires Linux, Python 3, `nats-server`, and installed `agent-browser` with
Chromium. No browser download or executable compilation occurs during measured
workloads. Every suite run gets an isolated runtime; measured cases run
**sequentially**, with deterministic order rotation across independent repeats.

Defaults: three repeats, seven samples, 1,000 RPCs, 64 KiB/1 MiB/8 MiB objects,
1/10/100/500 held sessions, 1/4 provider replicas, and 1/32/128 selected RPCs.
Override `--repeats`, `--samples`, `--calls`, `--sizes`, `--sessions`,
`--provider-counts`, `--actions`, and `--idle-seconds` explicitly. Larger curves
are opt-in; neither skips nor retries conceal failing points.

The top-level `report.md` links each detailed Markdown report and retains
between-run median ranges. `suite-summary.json`, `suite-runs.json`, raw
requests, resource samples, metadata, and logs are retained. Failed runs are
visible and never contribute a clean score. To compare a second suite with the
same host and configuration, add `--compare=/path/to/previous-suite`. To
regenerate or compare existing reports without rerunning workloads:

```sh
deno task -c ts/deno.json bench:runtime:suite --output=/tmp/opencode/trellis-perf-suite --report-only --compare=/path/to/previous-suite
```

### Focused lanes

```sh
deno task -c ts/deno.json bench:runtime --lane=rust --rust-bin=target/release/trellis-performance --output=/tmp/opencode/native-perf
deno task -c ts/deno.json bench:runtime --lane=browser --browser-bundle=benchmarks/runtime/browser.bundle.js --output=/tmp/opencode/browser-perf
deno task -c ts/deno.json bench:runtime --lane=lifecycle --output=/tmp/opencode/lifecycle-perf
deno task -c ts/deno.json bench:runtime --providers=4 --output=/tmp/opencode/provider-perf
```

- Rust hosts generated Echo and transfer handlers and compares them with
  Axum/reqwest. First-process readiness measures native session **resume**;
  password login is prepared outside that timing. Both Rust upload paths read
  back persisted bytes and return their hash before completion.
- Chromium loads the actual generated SDK, logs in through the real portal
  protocol, reconnects a saved session over WebSocket, and proves the first Echo
  result. Document TTFB, navigation-to-first-authorized-response, and
  transferred bytes are reported. This is SDK/page delivery, **not Console
  rendering**. Document, bundle, and WASM delivery deliberately use `no-store`.
- Lifecycle uses a supported 120-second context window; metadata records the
  lifetime, lead, jitter, minimum-lifetime, and clock-skew settings. Real 10 Hz
  RPC traffic crosses two **persisted context issuances for the exact session**.
  Observation time is not issuance latency. TCP endpoint outages exercise real
  disconnection/reconnect followed by a verified RPC, without stopping the
  broker or bypassing authorization/readiness. The outage affects providers as
  well as the caller: recovery waits for both sides' connection readiness before
  issuing that RPC. Recovery duration includes provider readiness; the raw
  provider connection files retain reconnect counts.
- Provider scaling uses multiple processes with the same provisioned identity
  and resources: real replicas, not unrelated deployments. CPU sums across
  replicas use the workload's request count once, not once per process.
- Contract scaling authors native IDL, runs normal `trellis update/generate`,
  compiles generated consumers, provides the full RPC surface, and measures
  fresh login/resume through a real generated Echo as the selected permission
  surface grows. It is not a fabricated evidence-size or shape-only benchmark.

### CI tracking

`.github/workflows/performance.yml` is explicitly dispatched with a successful
**Check run ID**, pins that run's exact commit, and consumes its already-built
server/CLI and generated/WASM artifacts. A producer builds native/browser
workloads once; the execution job does not rebuild. Check artifacts currently
expire after one day, so start performance measurement while they still exist.
Markdown appears in the job summary; the full report tree and raw evidence are
retained as a 30-day artifact. Nothing is published externally, and no automatic
noise-sensitive percentage threshold replaces functional failure checks.

For a smoke run:

```sh
deno task -c ts/deno.json bench:runtime --samples=2 --calls=20 --sizes=65536,1048576 --sessions=1,3 --idle-seconds=1 --output=/tmp/opencode/trellis-perf-smoke
```

For fixed-arrival-rate RPC load, add
`--arrival-rate=1000
--max-outstanding=128`. The default is sequential
closed-loop requests. Fixed-rate latency starts at the offered time, includes
scheduler delay, and records drops when the outstanding-request limit is
reached. Drops fail the run. Do not compare different arrival-rate
configurations as if they measure identical workloads.

## Workloads

- Fresh password login through first authorized RPC, first-process SDK
  initialization, session resume, and logout.
- Equivalent TypeScript Echo work over Trellis and HTTP, live first-frame
  delivery, ephemeral events, and durable event handling acknowledged by a reply
  event.
- State write/read and provider KV write/read through RPC.
- Operation progress/completion/cancellation, job-backed operations, ten
  competing same-key jobs, and a real object-dependent job retry after an
  upload.
- Verified binary downloads and persisted uploads at configurable sizes.
- Idle live-session counts, post-disconnect observation, per-process CPU,
  RSS/PSS, threads, and file descriptors. Provider, Trellis, NATS, HTTP
  provider, and load generator remain separately attributable.

Each workload verifies returned values, terminal states, or complete-body
hashes. Failures remain in raw samples and cause a nonzero exit. Progress
readiness accepts the current live snapshot as well as new progress events. No
fake runtime, authorization bypass, synthetic failure injection, or silent
skips.

## Results and comparisons

`samples.json` retains individual timings and errors, plus exact workload
resource windows. `resources.jsonl` retains the process timeline.
`metadata.json` records revision, server binary hash, effective Argon2
parameters, host/CPU/cgroup limits, load settings, topology, and telemetry
configuration. `source.diff` captures tracked edits; untracked benchmark source
must be retained with the run until committed. `report.md` and `bencher.json`
are generated by `bench:runtime:report`.

Compare matched runs after changing the checkout and rebuilding the server:

```sh
deno task -c ts/deno.json bench:runtime:report /tmp/opencode/trellis-perf-candidate /tmp/opencode/trellis-perf-baseline
```

Comparison rejects differing host, runtime, hashing parameters, resource limits,
or workload configuration. No automatic regression threshold is enabled. Run
several independent repetitions on an otherwise idle host before treating a
difference as a regression.

## Interpretation and current limits

HTTP is a **plaintext baseline: no TLS, no authentication, and no
authorization**. It is a lower bound, not a security-equivalent stack. Each
comparison uses the same language on both ends (Deno or Rust), loopback,
identity encoding, and no caching. HTTP file storage and Trellis JetStream
storage have different durability semantics. Transfer timings include complete
integrity verification; first payload byte and setup are reported separately for
downloads. Image-sized objects use binary payloads; image decoding and rendering
are not measured.

CPU counters have kernel-tick resolution: short windows can report zero despite
real work. PSS/RSS describe whole processes, not bytes allocated per session.
Post-disconnect retains durable login sessions and allocator caches; it is not a
leak test. Session counts are measured after earlier workloads, not independent
fresh-process cohorts. Idle waits are explicit measurement windows, not
readiness sleeps. Small cohorts omit tail percentiles rather than inventing
precision.

Rust covers RPC and transfers rather than duplicating every TypeScript workload.
Refresh/outage and resource curves currently use native TypeScript connections;
browser covers fresh/resumed WebSocket startup. WAN/mobile hardware, browser
refresh/recovery, high-concurrency capacity limits, additional storage
contention, and image decoding/rendering remain unmeasured. Native process
resource timelines are separated by role, but its exact endpoint CPU window
spans the mixed native workload; do not attribute that whole-window value to
Trellis or HTTP alone.

`--server` selects a prebuilt server and `--keep-workdir` preserves
infrastructure state for diagnosis. Preserved workdirs contain credentials and
must not be published. Setup always registers contract/resource evidence and
consent policy before timing; backend and OS caches are warm. Workload logs and
telemetry can affect results, so keep telemetry settings matched across runs.

## Verifier worker lifecycle checks

`worker_lifecycle_test.ts` uses real public clients, persisted handler-entry
markers, and Node's native debugger to hold a successfully verified request
before returning its worker result. It terminates that actual worker, lets a
proof become stale, or waits for the exact context's revocation to reach the
service. No SDK fault hooks or injected authorization state are used. It also
proves growth to configured ceilings 1/2/3, retained capacity, a brief
small-message burst without growth, and loss during added-worker startup without
repeated restart attempts or rejection of healthy-worker work. An idle verifier
must finish queued requests even while another ordinary worker is paused;
waiting work must not remain trapped in a worker-local queue.

Use the producer-built server/CLI and private NATS cache settings from live
integration. `TRELLIS_WORKER_CONSUMER_DIR` must contain a real installed npm SDK
with its dependencies and the generated runtime fixture package under
`generated/`; a symlink back to the uninstalled SDK output is insufficient. The
test stages its public provider script into that consumer directory.

```sh
export TMPDIR="$PWD/.verification/tmp" TMP="$PWD/.verification/tmp" TEMP="$PWD/.verification/tmp"
export TRELLIS_WORKER_CONSUMER_DIR="$PWD/.verification/worker-consumer"
export TRELLIS_WORKER_NODE_BIN="$(node -p 'process.execPath')"
deno test -A -c ts/deno.json benchmarks/runtime/worker_lifecycle_test.ts
```

These checks ran with Node 24.19.0. They depend on debugger support and
generated script locations, not a product-facing failure-injection API;
source-layout changes may require adjusting breakpoints. A missing or incorrect
breakpoint fails the check rather than claiming the interleaving occurred. These
held runs are correctness diagnostics, not performance measurements.
