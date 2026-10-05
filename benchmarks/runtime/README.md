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
  broker or bypassing authorization/readiness.
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
