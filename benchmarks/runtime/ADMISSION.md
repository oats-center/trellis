# Request admission exploration

Status: the bounded-admission policy below is approved and implemented;
sustained calibration, focused lifecycle checks, and live-suite verification are
complete. The original review and measurements remain below as historical
evidence, not current scheduling behavior. The retained tool is the `admission`
lane described in
[README.md](README.md#fixed-arrival-rate-admission-exploration).

**Default selection is not approved or complete.** The user clarified that these
are general-framework safety ceilings, not tuned application capacity. The
current numeric settings remain provisional. Exhaustion must produce
rate-limited actionable warning logs identifying the budget, outcome, and
configuration control; that logging is still outstanding. Passing functional
tests does not approve the numeric defaults.

## Grow-only verifier policy

Current development changes make worker verification the only TypeScript service
request/Transfer path. The opt-in service switch is removed. Optional
`runtime.verificationWorkerQueueAgeMs` and
`runtime.verificationWorkerBacklogDurationMs` configure the thresholds below,
defaulting to 20 and 100 milliseconds. Both accept nonnegative values; zero
removes the corresponding delay. Admission defaults remain unchanged. Full
acceptance is deferred until the Live/event work is complete; this is not a
release or publication claim. Measurements below describe the preceding opt-in
trial, not a new verification of these development changes.

The approved adaptive trial starts **one ordinary verifier plus one reserved
verifier**. All ordinary workers must be occupied, and the oldest queued
ordinary proof must have waited at least **20 ms**, with that condition
persisting for **100 ms**, before one additional ordinary worker starts.
Starting capacity counts toward `runtime.maxVerificationWorkers`, whose
configurable default is **three ordinary workers**. The reserved worker is
additional. Growth is evaluated again after startup completes; workers remain
until shutdown, without shrinking or a cooldown. The thresholds are
experimental, not established capacity defaults.

Admission budgets remain process-wide and unchanged. Only queued,
never-attempted verification moves between ready ordinary workers. Idle capacity
takes waiting work before considering another thread. Failed attempts are
rejected, not retried or sent through inline verification. Ordinary worker
failure stops further growth for that pool, preventing automatic restart loops
while healthy workers remain usable. Request, Transfer, revocation, freshness,
and control-priority semantics remain on the same security implementation.

The runtime lane accepts `--max-verification-workers` and retains the value in
run metadata/reports. Production telemetry retains ready-worker occupancy in
`trellis.auth.worker.active`, startup latency in the `worker.start` phase of
`trellis.auth.verification.duration`, and failures in
`trellis.auth.worker.failures`. Existing queue-delay, proof-phase, admission,
payload-copy, process-memory, cancellation, and refusal measurements still
apply. An idle pool has no growth sampling timer.

Retained real-runtime checks are in `worker_lifecycle_test.ts` with a packaged
Node provider and the native inspector. They verify queued signed requests
finishing after growth to configured maxima 1/2/3, retained capacity, use of
idle ordinary capacity while another worker is occupied, a brief small-message
burst, worker loss, loss during added-worker startup, received context
revocation, and stale-proof rejection. Inspector break-on-start pauses are
resumed separately from deliberate proof-verification pauses. These are
functional scheduling/security checks, **not throughput measurements**.

Final scoped verification passed **nine real worker lifecycle checks, three
real-NATS admission/control/offline checks, two Transfer generation/revocation
checks, and two real-WASM component checks**. The idle-worker regression failed
before the queue-sharing fix and passed afterward: all fresh requests complete
without resuming the occupied verifier. Public SDK and benchmark consumers
type-check, npm packaging builds, `cargo xtask install` and generated Rust
formatting pass. Full framework acceptance was not rerun for this adaptive
change.

### Eta adaptive measurements

Password authentication resolved the SSH-agent blocker without bypassing host
verification or storing the password in scripts. Four isolated measurements used
the real native Rust caller and TypeScript provider, without debugger pauses or
CPU profiling. The SDK and scaling thresholds stayed unchanged between runs.
Handlers waited 50 ms; ordinary admission was explicitly configured to 1,024
requests and 512 MiB, with a 1,024-call generator cap, to measure verification
rather than the provisional admission defaults. These are test budgets, not new
shipping settings.

The native caller now accepts per-phase `--arrival-rates`. Each phase drains its
RPCs and completes its concurrently issued cancellation probe before the next
begins, keeping the same provider and verifier pool. This is a stepped workload,
not an instantaneous rate transition with unfinished RPCs crossing phases. Raw
samples and windows retain phase indexes and start times. The normal report
retains phase outcomes, cancellation latency, and ready-worker count changes
from production metric exports; exported timestamps approximate observation
time, not exact worker creation instants.

| Workload                   | Offered phases                                                 | Completed RPCs | Ordinary workers                                      | Cancellation       |
| -------------------------- | -------------------------------------------------------------- | -------------: | ----------------------------------------------------- | ------------------ |
| 1 KiB values               | 200 → 800 → 200/s; 4,000 calls per phase                       |  12,000/12,000 | One throughout                                        | 3/3; 292–300 ms    |
| 256 KiB values, first run  | 200 → 400 → 200/s; 12,000 calls per phase                      |  36,000/36,000 | 1 → 2 during initial 200/s, then 3 at 400/s; retained | 3/3; 294–324 ms    |
| 256 KiB values, repeat     | Same fresh-service workload                                    |  36,000/36,000 | Same progression                                      | 3/3; 301–313 ms    |
| Eight short 256 KiB bursts | Eight calls per burst, offered over roughly 4 ms, then drained |          64/64 | One throughout                                        | 8/8; 300 ms median |

All **84,064 RPCs** completed with no busy replies, transport errors, timeouts,
or unsent requests; all **17 cancellation probes** settled. The initial small
smoke run (24 RPCs and three probes) is separate from these counts. Large-ramp
RPC p95 was 70–83 ms; small-ramp p95 was 57–65 ms, including the 50 ms handler
wait. The large runs' ordinary verification queue mean was 0.61/0.87 ms, with
observed maxima 74/136 ms. Added workers remained through the return to 200/s
and reached zero only at service shutdown.

**The trigger scales on queue delay, not a universal requests/second
threshold.** In both large runs a second ordinary worker was already observed
about 0.63/0.94 seconds into the initial 200/s phase. A third was observed about
8.65/4.70 seconds into the 400/s phase. These measurements do not establish why
the initial delay arose or prove that two workers are necessary for 200/s
throughput: the historical fixed 1+1 trial also completed 200/s. They
demonstrate that the agreed latency signal actually crosses its threshold in
this workload. Conversely, 1 KiB traffic at 800/s and brief large bursts did not
add workers. The burst rate is not a sustained 2,000/s capacity claim.

Retained cases are `adaptive-large-ramp-1`, `adaptive-large-ramp-2`,
`adaptive-small-ramp-1`, and `adaptive-large-bursts-1` under the isolated Eta
workspace's `results/`, with local copies under
`.verification/results/eta-adaptive-*`. Reports were regenerated with the
retained phase/count reporting after execution. These are two large repeats and
one small/burst case each on a shared host, not deployment sizing guarantees.
The trial remains opt-in; full framework acceptance and shipping enablement are
not claimed by these measurements. Admission defaults are unchanged.

## Live, Transfer frame, and event offloading trial

The development trial moves frame digests, complete frame-proof verification,
and historical event verification into the existing Rust/WASM workers. Service
data uses ordinary pool capacity; clients lazily start one shared ordinary
worker per logical connection. Private keys and digest signing remain with the
existing signer. Wire bytes, ordering, credit, current authority checks, and
publication-time event authorization are preserved. Whole-object integrity
hashing, serialization, and small digest signing still run on their existing
paths: this does not remove every synchronous operation.

### Matched Eta comparison, October 8

Both SDK snapshots used the same generated benchmark contracts, WASM assets,
optimized prebuilt server/CLI, Deno runtime, and workloads. The preserved SDK
baseline already included mandatory adaptive service request verification; this
comparison isolates the subsequent frame/event routing. Worker thresholds and
admission limits were not retuned. Each workload used fresh services,
alternating variant order, and three measured samples. Event delivery used
ephemeral listeners in both versions, with explicit authored subscription needs.
Early durable-listener diagnostics are retained as failures, not capacity
results.

Without foreground RPC probes:

| Workload                                          | Baseline median | Offloaded median |
| ------------------------------------------------- | --------------: | ---------------: |
| Live, 2,000 1 KiB frames, one stream              |          758 ms |         2,058 ms |
| Live, 500 256 KiB frames per stream, four streams |         10.86 s |          12.72 s |
| 500 verified 256 KiB event round trips            |          3.55 s |           7.31 s |
| 8 MiB verified download                           |          303 ms |           622 ms |
| 8 MiB verified upload, persisted object read back |          633 ms |           762 ms |

Cancellation during these Live cohorts completed in both versions, roughly
270–313 ms. These are elapsed workload medians, not bare cryptographic speed.

### Independent foreground RPC schedule

Earlier frame-paced probes were not a fair comparison: slower frame delivery
also reduced their offered RPC rate. Those diagnostics remain retained, but the
decision comparison uses an independent 50 RPC/s schedule during each Live
cohort. At most one probe is outstanding per client. Missed scheduler slots and
offers while a probe is outstanding are counted as not sent. Neither errors nor
unsent probes contribute to successful latency quantiles. Both versions
completed every RPC actually sent in the following comparisons.

| Frame workload        | Baseline stream median | Offloaded stream median | RPC median baseline → offloaded | RPC p95 baseline → offloaded | Unsent / offered baseline → offloaded |
| --------------------- | ---------------------: | ----------------------: | ------------------------------: | ---------------------------: | ------------------------------------: |
| 1 KiB, one stream     |                0.702 s |                 2.400 s |                  5.30 → 5.16 ms |              14.01 → 9.86 ms |                        6/118 → 12/377 |
| 1 KiB, four streams   |                1.821 s |                 2.778 s |                 28.90 → 3.42 ms |              43.89 → 8.53 ms |                       141/267 → 8/398 |
| 256 KiB, one stream   |               11.205 s |                11.681 s |                  4.68 → 5.13 ms |             23.78 → 13.55 ms |                  253/1,684 → 98/1,763 |
| 256 KiB, four streams |               10.541 s |                13.043 s |                  8.03 → 9.26 ms |             13.98 → 16.45 ms |                 124/1,579 → 128/1,946 |

Offered totals differ because streaming durations differ; the schedule remains
50/s, and both sent and unsent counts are reported. Cancellation completed in
all twelve streaming samples per variant, with cohort medians of 271–348 ms. The
small four-stream case demonstrates better foreground responsiveness at lower
streaming throughput. The large four-stream case does not demonstrate a
foreground latency benefit. This does not establish a universal reason to move
all frame traffic into workers.

Sampled whole-process peak PSS across each fixed-rate comparison was:

| Cohort  | Provider baseline → offloaded | Caller baseline → offloaded |
| ------- | ----------------------------: | --------------------------: |
| 1 KiB   |             232.1 → 259.2 MiB |           247.5 → 251.6 MiB |
| 256 KiB |             492.9 → 509.9 MiB |           433.1 → 491.8 MiB |

These include the complete cohort, event traffic, allocator retention, and
runtime work. They are sampled process peaks, not isolated worker allocations or
total-memory estimates from admission counters. Service startup policy is the
same in both snapshots; lazy client startup is included in first-use frame
timing, not independently isolated as a startup-cost benchmark.

### Verification and limits

Final focused verification passed seven real-NATS Live flow/credit,
authorization-reduction, and Transfer generation/revocation tests, plus five
real-WASM/native-proof and reporting transformation tests. Earlier admission and
broker-offline teardown checks also passed. Public SDK/service entrypoints and
benchmark consumers type-check; artifact regeneration, Rust formatting, and
patch whitespace checks passed. The normal source formatter was run; its broad
pre-existing generated-JavaScript layout churn was discarded by regeneration,
retaining only the authored contract and package-evidence changes.

Packaged proof checks authenticate native Live frames, reject tampering, and
preserve historical event authorization in Node, Deno, and insecure-HTTP
Chromium without browser WebCrypto. These packaged checks exercise the verifier
boundary, not a complete packaged browser Live app. The added npm dependency
declaration includes the already-used Zod runtime.

The frame/event path adds payload copies, worker scheduling, and reply waits on
sending and receiving paths. These are known additional steps, not a measured
attribution of the slowdown. Moving crypto off the NATS-processing thread may
improve foreground responsiveness, but slower frame delivery also reduces
competing work. The independent RPC schedule does not remove that confound:
equal-frame-rate comparisons and frame-specific phase profiling are still needed
to separate those effects.

The trial is **not accepted as a performance improvement**. The measurements do
not isolate the cause of the regressions, establish production capacity, or
prove UI responsiveness on target devices. Full framework acceptance remains
deferred until the combined work is settled.

Evidence is retained locally under `.verification/results/` and on Eta under
`/home/abalmos/trellis-admission.2lnzFPry/results/`: successful unprobed
comparisons are `frame-{baseline,offloaded}-{small,large,transfer}-2`, and the
fixed-50/s comparisons are `frame-{baseline,offloaded}-{small,large}-rpc-4`.
Each fixed-rate comparison also has a matching `.source.tar.gz` containing its
exact SDK, benchmark, generated-contract, and WASM sources. Raw samples,
metadata, resource timelines, production metrics, cleanup outcomes, and earlier
failed diagnostic cohorts are retained. Reports recompute quantiles from raw
sent successes; the summary transformation regression first failed against the
old inclusion of unsent probes.

## Implemented policy

Both SDKs share local non-waiting count-and-incoming-byte budgets across the
connected service's endpoints and transport generations. Ordinary RPCs execute
concurrently without ordering guarantees. Built-in Operation get/cancel and Live
controls have separate bounded capacity; application signals use ordinary slots.
Live controls retain protocol order. Operation start reservations end at the
start response, not execution termination.

A separate bounded verification pool authenticates refusals before returning
`trellis.service.busy` without invoking application code. Traffic beyond that
pool is dropped without a reply. Busy refusals are not automatically retried.
Incoming byte accounting excludes NATS buffers and decoded objects. Production
`trellis.service.admission.inflight`, `.bytes`, and `.rejections` expose all
three pools with shared meanings across SDKs. Unary RPC instrumentation may also
count refusal validation; use admission metrics for bounded occupancy.

Real-NATS tests for both provider languages prove concurrent dispatch, shared
RPC/Operation-start capacity, byte refusals, no execution on busy refusal, and
cancellation with ordinary slots occupied. TypeScript additionally proves its
public `stop()` drains accepted concurrent callbacks before closing the logical
connection. Rust has no public graceful-stop entrypoint; its accepted-work drain
uses transport-generation retirement instead. Sustained-load cancellation and
generation replacement also passed existing lifecycle suites (4 TypeScript and
10 Rust cases), including held concurrent RPC callbacks across cutover and
baseline reclamation. These are not sustained-overload cutover tests.

The complete Deno real-NATS suite passed **214 tests across 86 modules** in
split runs. The first invocation hit the runner's 30-minute command limit; its
interrupted module and all remaining modules were run separately. The
mixed-language module initially failed because the verification command omitted
`TRELLIS_TESTKIT_LIVE_BIN`; it passed with the current-source prebuilt
executable. No assertions, test timeouts, or security settings were weakened.
The public Rust live suite passed **23 tests**; its three child-process
entrypoints are ignored in standalone enumeration and exercised through their
parent tests, including the mixed-language test. SDK and runtime library suites
passed **181** and **240** tests respectively. Verification outputs remain under
`.verification/results/`.

## Bounded implementation measurements

### Eta follow-up: small messages

The user authorized measurements on `abalmos@eta.oats` to explore substantially
higher safety ceilings. The isolated home-directory workspace is
`/home/abalmos/trellis-admission.2lnzFPry`. Production server, CLI, and Rust
provider executables were built there in release mode from the transferred
current source. Source manifests, the base revision and overlay, executable
hashes, reports, samples, and process measurements are retained. The host has
Xeon Gold 6426Y CPUs, 32 logical CPUs online, and 128 GiB RAM. Existing host
activity was present; this is not a dedicated-idle-host measurement. Deno was
2.9.4, unlike the earlier laptop runs, so these are not controlled cross-host
speedup comparisons.

The longer-wait workload offered 1,600 requests/s to one provider with a 500 ms
delay and small messages. Three independent 20-second runs offered 96,000
requests per language and count setting. All used a 16 MiB incoming-byte budget
and the same public TypeScript client. Successful counts were:

| Concurrent ceiling | Rust successes | TypeScript successes |
| -----------------: | -------------: | -------------------: |
|                128 |         15,360 |               15,149 |
|                256 |         30,720 |               30,400 |
|                512 |         60,925 |               59,879 |
|              1,024 |         96,000 |               95,999 |

At 1,024, Rust had no busy replies, timeouts, or generator drops; TypeScript had
one busy reply and no timeouts or generator drops. Successful-request p95 was
524–532 ms for Rust and 529–538 ms for TypeScript. All 24 cancellation probes in
this workload completed. A TypeScript run sampled 899 occupied ordinary slots
while retaining about 410 KiB of incoming messages. Its provider process grew
from 160 MiB baseline PSS to 350 MiB sampled maximum; a matching Rust run grew
from 22 MiB to 43 MiB. These are whole-process observations, not per-request
allocation estimates or memory guarantees.

The 3,200/s, 50 ms workload was not a clean capacity result at any tested
ceiling. Across three runs at 1,024, Rust recorded 137,220 successes, 23,277
timeouts, and 31,503 generator drops; TypeScript recorded 142,739 successes,
18,388 timeouts, and 30,873 generator drops. Neither recorded busy replies at
that ceiling. Seven of the 30 cancellation probes across all count settings in
this higher-rate workload timed out. The precise timeout origins are not
attributed; reserved dispatch slots do not establish end-to-end responsiveness
when other stages are overloaded.

These results demonstrate legitimate concurrency far above 32. They do not
select a universal optimum or establish hard overload-latency guarantees.
Large-message follow-up remains incomplete: the original 4,096-outstanding-call
generator exhausted its 4 GiB JavaScript heap. A redundant collection retaining
completed promises was removed from the retained worker; one rerun then
completed, but another still exhausted the heap. A separately recorded rerun
uses a 1,024-outstanding-call generator cap and retains all generator drops.
That cap also crashed. Standard Deno heap snapshots then established a real
TypeScript SDK request-lifecycle leak, rather than merely a generator promise
collection issue. A late snapshot contained 7,208 large `payload` references and
6,922 NATS queued iterators despite a 1,024-call generator bound. The oldest
large payload had a direct strong retaining path through a status iterator's
pending signal, async-generator request, and the SDK request context's `args`
binding. Large strings occupied about 2.68 GB and array-buffer backing stores
another 941 MB in that snapshot.

The previous `session.ts` implementation created a physical NATS status iterator
for every request to catch publish denials, then called its async-generator
`return()` when the request settled. Installed `@nats-io/nats-core` 3.4.0 waits
for a status signal inside that generator; `return()` queues behind the pending
`next()` and cannot wake it. Completed requests therefore remain retained until
a later status event or connection closure. Large payloads make the leak visible
quickly. The service's incoming admission ceiling does not bound these completed
client-side requests.

With user approval, the correction now reuses the existing physical-generation
status monitor with immediately detachable request callbacks. Fixed transports
also share a monitor. Callback registration stays on the exact leased
attachment, including draining generations; logical transport replacement does
not redirect publish-denial handling to another socket.

The real-NATS regression sends 2,048 256 KiB requests, with only 16 calls
outstanding, and checks heap growth while the connection stays open. Temporarily
restoring the previous watcher implementation produced 773 MiB of additional
heap and failed the test. The fixed implementation measured 28 MiB and passed.
This proves release of completed request history, not an exact heap bound for
arbitrary applications. Seven focused live checks and thirteen additional
growth/candidate checks passed (twenty executions, including two runs of the
memory regression). Type-checking, formatting, and diff checks also passed; the
full integration suite has not been rerun after this correction.

Crashed and debugger-instrumented attempts remain retained and are not counted
as complete capacity measurements or replaced silently. Follow-up large-message
results are recorded separately below.

### Eta large-message follow-up after the client leak fix

The six original cases and two additional TypeScript repetitions have finished.
All used 256 KiB values, 500 ms handlers, 800 offered requests/s, a 512 MiB
incoming-message budget, and a 1,024-outstanding-call generator bound. Each
complete case offered 48,000 requests across three windows. These are overload
results, not clean throughput or safety-default calibration:

| Provider / count limit                           | Successful |   Busy | Timed out | Other errors | Generator drops | Cancellation probes completed |
| ------------------------------------------------ | ---------: | -----: | --------: | -----------: | --------------: | ----------------------------: |
| Rust / 128                                       |     10,256 | 12,545 |    13,302 |            0 |          11,897 |                           2/3 |
| Rust / 512                                       |     12,862 |  1,069 |    15,123 |            0 |          18,946 |                           2/3 |
| Rust / 1,024                                     |     13,654 |      0 |    15,831 |            0 |          18,515 |                           2/3 |
| TypeScript / 128 (interrupted after two windows) |      3,541 |  5,229 |     5,165 |        4,228 |          13,837 |                           0/2 |
| TypeScript / 512 (interrupted after two windows) |      5,516 |      0 |     9,226 |            0 |          17,258 |                           0/2 |
| TypeScript / 1,024                               |     11,701 |      0 |    14,647 |           49 |          21,603 |                           0/3 |
| TypeScript / 512 (additional run)                |     11,318 |      0 |    15,085 |            0 |          21,597 |                           0/3 |
| TypeScript / 1,024 (additional run)              |     10,232 |      0 |    15,547 |            2 |          22,219 |                           1/3 |

The original TypeScript-128 run received an Operation `no responders` failure
and stalled in provider shutdown. Its isolated provider was forcibly stopped to
release benchmark cleanup; that intervention is not a successful shutdown
measurement. The retained benchmark now bounds child cleanup, records forced
termination in `cleanup.json`, and keeps failed workloads failed. The additional
runs use that cleanup change and are reported separately, not substituted for
the first attempts.

The client leak regression is fixed and the follow-up runs no longer report
JavaScript heap exhaustion. That does not resolve their transport failures.
Large-message transport timeouts, interruption of Operation control/start, and
generator drops were initially unclassified. The transport and cleanup traces
below now establish specific failure mechanisms, without attributing every
original failed request individually. In particular, the Rust-1,024 run had no
busy replies yet still timed out 15,831 requests. Increasing admission capacity
alone cannot be claimed to solve these failures, and reserved slots do not
establish end-to-end control responsiveness under this workload.

Raw results, source manifests, binary hashes, resource timelines, production
metrics, and logs remain under the isolated Eta workspace's
`results/admission-status-fix` and `results/admission-status-fix-rest`
directories, with local copies in `.verification/eta-status-fix-results` and
`.verification/eta-status-fix-rest-results`. All numeric framework defaults
remain unapproved and unchanged. The clean small-message, long-wait runs support
room for concurrency well above 32, but these large-message failures do not
select an incoming-byte ceiling or a reliable control-latency target.

### Transport and shutdown traces

These are diagnostic reproductions, not additional capacity measurements. They
use the unchanged 800/s, 256 KiB, 500 ms workload, production SDK paths, and
loopback NATS monitoring. Inspector profiling is reported separately because it
changes scheduling. No security checks, broker pending-byte limits, request
timeouts, or framework defaults were relaxed.

**Native-provider failures originate at the caller in the socket-mapped
reproduction.** All 25 `Slow Consumer (Pending Bytes)` closures map to the
TypeScript load generator's TCP ports; none map to the Rust provider. Broker
pending outgoing bytes were approximately 66.9 million at closure, just below
its 64 MiB ceiling. During offered windows the native provider averaged 0.51 CPU
cores, versus 1.20 for the generator. The three-window run retained 12,213
successful requests, 12,631 timeouts, 1,287 other errors, and 21,869 unsent
generator drops. All three cancellation probes completed in this reproduction;
that does not erase failures in earlier runs.

The caller CPU profile from a separate TypeScript-provider reproduction spends
24.96% of sampled time in request encoding, 7.96% in UTF-8 encoding, 4.88% in
NATS buffer packing, and 6.13% in garbage collection. This is a shared
TypeScript caller bottleneck, not evidence that Rust needs more dispatch slots.
The broker closes a socket that cannot consume its outgoing data quickly enough;
reserved service slots cannot recover replies already lost with that socket.

**The TypeScript provider also falls behind.** Socket-mapped profiling recorded
33 slow-consumer closures on provider connections and one on a caller
connection. Its provider profile spends 38.98% of sampled time in synchronous
WASM SHA-256. The recorded stacks run through
`build_authorization_request_proof_input` and `verify_request`, hashing the
actual incoming payload. This verification runs on the same JavaScript thread
that handles NATS messages; concurrent request slots do not make it parallel.
This identifies a concrete expensive path, not a claim that hashing is the only
cost or that an unimplemented optimization will achieve a particular rate.

The independent 100/s comparison completed all 6,000 large-message requests and
all cancellation probes in each SDK, without slow-consumer closures. Those
results distinguish sustained overload from a universally broken transport; they
do not prescribe a throughput target for applications.

**The original `no responders` exception occurred during an Operation start, not
a cancellation call.** Its stack enters `invokeOperation` and sends to the
descriptor's start subject. The SDK received NATS's no-responder status and
reported a transport failure; it was not a busy refusal or an executed handler
failure. The original attempt did not retain socket monitoring, so its precise
connection-loss timeline cannot be reconstructed. Later monitored runs directly
show provider subscriber loss during slow-consumer disconnect/reconnect churn.

**Shutdown has an independent cleanup dependency defect.** The final four
observation-loop reproductions reached service `closed` twice, required forced
cleanup once, and exited once without reaching `closed`. Process exit alone is
therefore not proof that `service.stop()` resolved. In the forced case,
generation cleanup finished disposing all six Operation observation providers,
then blocked awaiting their observation loops. The physical connection was
client ID 350; NATS recorded `Authentication Failure`, zero application
messages, and zero subscriptions on that connection. The cause of its
unsuccessful authentication is not established by this trace.

The source dependency is specific:

- `#createOperationLiveProvider().close()` starts `observeSub.drain()` and then
  awaits `observeDone` before releasing retained authority.
- Installed `@nats-io/nats-core` 3.4.0 `SubscriptionImpl.drain()` sends UNSUB,
  calls protocol `flush()`, and cancels the local subscription only after that
  flush resolves or rejects. `flush()` waits for PONG without its own timeout.
- Generation retirement waits for owner disposal before closing the physical
  socket. Consequently local cleanup depends on a broker round trip or further
  reconnect progress on a connection that has already lost usable transport.

The retained phase logs show `provider` followed by `observation-loop`, without
`authority`, on each affected route. A correct fix must let forced/local
teardown settle without a broker flush, while preserving accepted-handler
draining, ordered Live controls, signed termination ordering, and authority
release. This trace identifies the defect; it does not claim that it is fixed.

The authorized correction now separates planned intake draining from terminal
local cleanup. Terminal cleanup unsubscribes locally and waits for accepted
callbacks and observation loops; planned rollover retains acknowledged draining,
with local subscription closure able to release an outstanding drain wait. The
real-broker regression pauses NATS with `SIGSTOP` before service shutdown, then
requires `service.stop()` to resolve before resuming the broker. It fails on the
previous implementation and passes on the correction. Four focused
admission/memory tests and fourteen transport-lifecycle, Operation-growth, and
candidate-stage tests passed. The full integration suite has not been rerun
after this correction. No network timeout or security configuration was changed.

Evidence is retained locally under
`.verification/eta-native-role-trace-results`,
`.verification/eta-cpu-trace-fixed-results`,
`.verification/eta-provider-profile-results`,
`.verification/eta-stop-owner-trace-results`, and
`.verification/eta-observation-loop-trace-results`, with their corresponding
isolated Eta result directories. Profiles and monitoring records are private
diagnostics, not publication inputs. Production shutdown/owner phase logging and
the benchmark's optional debugger/monitoring controls remain in source.

### Native-caller separation and terminal-cleanup verification

The retained admission lane now supports independent `--client-language` and
`--provider-language` choices. The native generator uses actual generated public
clients, a 3-second RPC timeout, fixed-arrival scheduling, a bounded outstanding
set, exact process CPU windows, and confirmed Operation readiness before each
cancellation probe. Completed calls are reaped continuously; generator drops,
transport errors, and busy refusals remain failed attempts. A native smoke run
passed before measurement. These new runs retain the 1,024-request / 512 MiB
trial settings; neither is an approved default.

Each unprofiled comparison uses 256 KiB values, 500 ms handlers, and three
20-second windows. The results separate the previous TypeScript-caller
bottleneck from provider behavior:

| Caller → provider | Offered/s | Successful | Timeouts | Other errors | Generator drops | Cancel probes | Successful p95 |
| ----------------- | --------: | ---------: | -------: | -----------: | --------------: | ------------: | -------------: |
| Rust → Rust       |       100 |      6,000 |        0 |            0 |               0 |           3/3 |         508 ms |
| Rust → Rust       |       800 |     48,000 |        0 |            0 |               0 |           3/3 |         626 ms |
| Rust → TypeScript |       100 |      6,000 |        0 |            0 |               0 |           3/3 |         517 ms |
| Rust → TypeScript |       800 |     17,854 |   14,172 |        7,481 |           8,493 |           0/3 |       1,297 ms |

No busy refusals occurred in these four cases. The native-to-native 800/s run
had no slow-consumer closures. The native-to-TypeScript 800/s run recorded 44
slow-consumer closures; its unsuccessful result remains unsuccessful. All seven
new runs, including smoke and profiles, finished without forced child cleanup.
This validates the cleanup correction under these overload reproductions, not
every possible lifecycle failure. Larger ceilings do not make a single
TypeScript event loop process unlimited large signed messages.

Separate 200/s profiles isolate the TypeScript caller against a Rust provider
and the TypeScript provider against a Rust caller. Each completed 4,000 requests
and its cancellation probe, without slow-consumer closures. Profiles sample 15
seconds during the offered window and are not capacity measurements:

- Provider: 36.88% idle, 24.87% synchronous WASM SHA-256, 6.56% response
  encoding, and 2.82% garbage collection. SHA-256 is about 39.4% of non-idle
  sampled time.
- Caller: 40.69% idle, 8.47% request encoding, 4.03% garbage collection, 3.68%
  UTF-8 decoding, and 3.11% NATS JSON parsing. Request encoding is about 14.3%
  of non-idle sampled time, rather than a universal fraction of caller cost.

Source inspection confirms provider payload hashing happens synchronously in the
shared Rust/WASM proof verifier. Native WebCrypto hashing is already used for
caller proofs. A provider optimization would need to move hashing off the event
loop without trusting a caller-supplied digest, duplicating protocol logic, or
losing post-await revocation/time checks. That is a design proposal, not an
implemented change. Caller inspection also identifies duplicate UTF-8 encoding
for proof creation and NATS publication as a narrower candidate. Installed
TypeBox 1.2.16 `Value.Encode` performs cloning, codec transforms, defaults,
conversion, cleaning, and validation; bypassing it is not a semantics-preserving
optimization. No security check or codec stage was removed.

Results, exact source hashes, binary hashes, process/transport timelines, and
profiles are retained in Eta's `results/admission-native-caller` and locally in
`.verification/eta-native-caller-results`. `benchmarks/runtime/profile.ts`
retains the normal Deno inspector capture workflow for future investigations.

### WASM versus WebCrypto hashing on Eta

`benchmarks/runtime/hash.ts` compares the existing production
`live_server_proof_digest` WASM entrypoint with the identical proof digest
constructed using native WebCrypto. Both hash the same body, then hash the same
length-prefixed proof framing; output equality is checked before and after each
measurement. This exercises the same SHA-256 implementation seen in request
verification, without adding a test-only hashing export. It includes boundary
copies and framing but excludes signatures, request authorization, and network
traffic. Three alternating one-second batches per backend and size produced
these median body throughputs:

| Body size | WASM MiB/s | WebCrypto MiB/s | WebCrypto / WASM |
| --------- | ---------: | --------------: | ---------------: |
| 1 KiB     |     172.16 |           16.46 |            0.10× |
| 256 KiB   |     287.51 |          320.74 |            1.12× |
| 1 MiB     |     287.12 |          335.50 |            1.17× |

For the relevant 256 KiB case, WebCrypto is modestly faster, not an order of
magnitude faster. Small bodies are substantially slower through sequential
native async submissions. The key difference is event-loop availability: the
synchronous WASM one-second batch delays the 5 ms timer by approximately 996 ms,
while the WebCrypto batch's median maximum delay is 1.27 ms at 256 KiB (1.07 ms
at 1 KiB and 2.06 ms at 1 MiB). The WASM delay is deliberately a sustained-batch
measurement, not a one-second pause per hash or a measurement of SDK scheduling.
At 256 KiB its average complete proof-digest cost is about 0.87 ms, versus 0.78
ms for WebCrypto.

The evidence does not support blaming a huge WASM speed penalty. Both paths pay
real SHA-256 work; native asynchronous hashing chiefly frees the JavaScript
thread to receive messages. It remains unproven whether integrating that change
would eliminate provider slow-consumer closures, and a blanket switch would hurt
small-message hashing in this sequential benchmark. No SDK hashing path,
security check, or framework default was changed. Raw results are retained in
Eta's `results/hash-comparison.json` and locally in
`.verification/results/eta-hash-comparison.json`.

### Worker feasibility investigation (proposal, not SDK implementation)

The existing WASM runs in a dedicated worker without changing cryptography.
Installed Node 24.19.0 and Deno 2.8.3 probes verified identical proof digests
for 1 KiB, 256 KiB, and 1 MiB bodies and buffer ownership. A real Chromium page
at non-loopback HTTP `http://192.168.1.135:18941` also ran the production WASM
in a module worker. Both page and worker reported `isSecureContext: false` and
no `crypto.subtle`; the worker reported `crossOriginIsolated: false`. Its 1,000
proof digests over a 256 KiB body took 813 ms while 162 parent timer ticks ran,
with maximum observed delay 0.10 ms. This proves insecure-origin worker/WASM
feasibility in that browser, not packaged SDK compatibility. The isolated
browser and HTTP probe server were closed afterward.

Per-request messaging matters. On the local host, 100 sequential 256 KiB
requests averaged 16.60 ms with typed-array messages in Deno but 0.82 ms when
copying exact bytes and transferring an ArrayBuffer. WASM work in the latter
case averaged 0.67 ms. Corresponding Node measurements were 0.89 and 0.88 ms,
with 0.67–0.69 ms in WASM. An ArrayBuffer clone also avoided most of the
measured Deno typed-array overhead (1.04 ms). These are single local probes, not
throughput guarantees or attribution of Deno internals. Copy-and-transfer leaves
the original bytes intact; transferring the original detaches bytes still needed
for parsing and application dispatch. Worker-boundary copies remain real costs,
and one worker does not by itself increase hashing capacity.

Source-grounded integration constraints:

- `provider_cache.ts:#verified` creates retained live/historical WASM context
  handles; `#disposeResources` frees them after entry leases end. Handles cannot
  move between WASM instances. Worker-owned handles must be created from the
  same signed evidence, cached rather than recreated per request, and released
  with bounded lifetime.
- `#verifyRequest` holds the exact cache-entry lease across verification and
  rechecks epoch, coverage, identity, and revocation afterward. Preserve those
  main-thread checks after worker completion. The worker is not an alternative
  authorization or revocation source.
- Rust `authorization.rs:verify_request` checks context time and proof `iat`
  before expensive hashing. Queueing can make submission-time policy stale.
  Check fresh time at execution and again before dispatch, keeping the rules in
  Rust rather than translating them to JS. Existing WASM `assert_current` checks
  context eligibility but does not alone prove request `iat` is fresh on return.
- Admission permits must cover pending/active verification and copied payload
  memory needs separate accounting. Mailboxes must not introduce unbounded work
  or defeat reserved control/refusal capacity. A single FIFO mailbox of ordinary
  requests puts cancellation behind them; scheduling or reserved execution needs
  a measured design before implementation.
- Worker failure must fail verification closed, settle pending requests and
  release resources, without handler execution or automatic retries. Shutdown
  must not wait forever for a worker response.
- No existing JS worker abstraction was found. Browser/Deno module workers and
  Node `worker_threads` need small adapters. The npm build compiles explicit
  entrypoint dependency graphs: a worker referenced only by `new URL(...)`
  requires explicit build inclusion and WASM asset verification. Browser
  bundling and CSP `worker-src` need real packaged-consumer checks.

Recommended design direction: offload full request proof verification using the
same Rust/WASM, keep NATS and authoritative cache/lifecycle fencing on the main
thread, and use exact ArrayBuffer payload copies. Do not split cryptography into
JS or accept externally supplied hashes. Worker count, scheduling, enablement,
and any small-message inline threshold remain undecided. No SDK worker path or
framework default was implemented. The reusable probes are
`benchmarks/runtime/hash_worker.mjs` and `hash_worker_probe.mjs`; raw Node,
Deno, and insecure-browser results are in
`.verification/results/*worker-feasibility.json`. These probes exercise hashing,
not end-to-end worker-based authorization or revocation races.

### Opt-in two-worker verification trial

The approved trial is implemented behind TypeScript service
`runtime.verificationWorkers: true`, also selectable through benchmark
`--verification-workers`. Defaults are unchanged. One ordinary worker and one
reserved worker run complete request and Transfer proof verification using the
same Rust/WASM implementation. Controls precede queued refusal work; existing
Live control ordering is preserved. Pending work remains bounded by the admitted
pools and scheduler ceilings. Only executing work gets a copied, transferred
ArrayBuffer; a fresh Uint8Array copy preserves exact slice boundaries, including
Node Buffer inputs. Main-thread coverage/entry/revocation checks and Rust-owned
context/proof freshness checks run after successful worker verification.

Production `trellis.auth.worker.pending` and `trellis.auth.worker.payload_bytes`
metrics expose pending attempts and copied executing payload bytes by lane.
Neither is total memory. Worker errors fail closed without inline fallback or
retry. Worker-owned context handles are released when the authoritative cache
releases the corresponding entry; shutdown terminates the workers.

Eta comparisons used the same optimized native Rust caller against TypeScript,
256 KiB appended values, 500 ms handlers, 1,024 ordinary slots, 512 MiB incoming
budget, and a 1,024-call generator cap. Each case offered three 20-second bursts
at the stated rate; these are repeated bursts in one runtime, not independent
host samples. Two sequential matrices tested 100/800 and then 200/400 per
second. Raw evidence is retained in `.verification/eta-worker-trial-results/`
and `.verification/eta-worker-intermediate-results/`.

| Offered/s | Inline successful/offered  | Workers successful/offered | Worker busy | Worker timeouts | Worker unsent |
| --------- | -------------------------- | -------------------------- | ----------- | --------------- | ------------- |
| 100       | 6,000/6,000                | 6,000/6,000                | 0           | 0               | 0             |
| 200       | 12,000/12,000              | 12,000/12,000              | 0           | 0               | 0             |
| 400       | 24,000/24,000              | 3,909/24,000               | 5,354       | 14,201          | 536           |
| 800       | 7,962/32,000 (interrupted) | 2,235/48,000               | 6,153       | 19,800          | 19,812        |

The inline 800/s run ended before its third burst: among its 32,000 offered
attempts, 12,671 failed as service unavailable, 7,005 timed out, and 4,362 were
unsent. Its two recorded cancellation attempts failed; an Operation-start
failure interrupted the remaining run. It is not a full three-burst comparison.
In worker mode all three cancellation probes completed at every rate. At 800/s
they took 281–294 ms; at 400/s they took 288–396 ms. Unsent work is generator
backpressure, not service refusal. Busy replies are explicit non-execution;
timeouts are not counted as successful or explained away.

**Result:** this topology preserved control responsiveness in these overload
runs but reduced ordinary throughput. Moving hashing off the receiving thread
does not establish a throughput gain. The bottleneck inside the worker path has
not been isolated; no additional workers, inline threshold, or default change
was introduced to hide the outcome.

Verification: 59 Rust protocol tests, worker component interoperability/tamper/
freshness/context-release/bounded-scheduling/control-priority checks, six real
NATS admission/reduction/revocation tests, and two real Transfer lifecycle tests
passed. A native signed proof also verified through the built npm package in
Node and a Vite-built Chromium consumer on non-loopback insecure HTTP, with
`isSecureContext: false` and no `crypto.subtle`. Node Buffer slice verification
failed with the earlier slice/backing-buffer copy and passes with an exact copy.
These initial checks were focused, not full acceptance. At that stage,
revocation precisely interleaved inside worker execution and unexpected worker
death had not been demonstrated end-to-end; those proofs covered real revocation
lifecycle, post-wait source fencing, freshness at the protocol boundary, and
pending-work rejection on pool closure. CSP-restricted deployment policies and
other browser bundlers remain unverified.

### Worker trial final vetting

The trial remains opt-in with two ordinary verifiers and one reserved verifier.
The service default was temporarily enabled for the broad live check, then
restored; this is not approval of a shipping worker count or default.

The broad real-NATS run completed with 215 passing tests and one failed mixed
Rust/TypeScript test. Its Rust child rejected the shared NATS cache's unsafe
permissions before starting a runtime. With the supported private-cache setting,
that unchanged module passed, including both real Rust child runtimes. These
results cover all 216 tests across the original run and corrected setup; they
are not a claim that the original full command was green. No assertions,
timeouts, or security checks were weakened.

Public Rust live acceptance passed 23 tests; three child entrypoints are invoked
by their parent or TypeScript tests, not independent skipped coverage. Rust
protocol and SDK library suites passed 59 and 181 tests. The wider runtime
library run passed 239 of 240 tests: the three-node KV conformance test failed
writing connection presence with `no stream found for given subject`. The same
prebuilt executable passed that test in isolation without changes. That run did
not establish its cause or a green full Rust runtime suite; the follow-up below
records the investigation rather than treating the isolated pass as a fix.

Vetting fixed a worker-startup error that escaped Deno's parent error handling:
denied asset loading failed before the fix and now returns a connection failure
without inline fallback. Source review also corrected routing of new requests
away from failed ordinary workers; affected attempts are rejected, never retried
or moved to another worker. Individual-worker loss was not deterministically
proved in that pass: a real-browser debugger attempt was inconclusive, and no
synthetic worker-failure hook was added.

Packaged Node, Deno, and non-loopback insecure-HTTP Chromium consumers verified
native signed proofs, concurrent proofs, tamper rejection, and context release
and reinstallation. Component checks also passed after the scheduler correction.
Exact revocation during a worker call was then source-reviewed rather than a
deterministic live interleaving: dispatch rechecks the current cache entry,
coverage, revocation, context digest, and Rust-owned request/context freshness.
Restrictive CSP, other browser bundlers, and architectures not exercised here
remain unverified.

Five local startup samples measured median three-worker startup at 291 ms in
Node and 141 ms in Deno, with approximately 68 MiB and 56 MiB of additional idle
RSS respectively. These are local cost measurements, not universal deployment
requirements. All verification temporary storage, caches, and outputs used
workspace/home-directory disk, not `/tmp`.

### Worker lifecycle proof-gap follow-up

`worker_lifecycle_test.ts` now retains three real-NATS/public-client checks
using the installed npm SDK and Node 24.19.0's native debugger. The worker is
paused after actual successful WASM verification, before returning the result to
the service. Its signed issue time and context digest are read from that real
exchange; verified contexts and SDK state are not injected or modified.

- Terminating the actual ordinary worker rejects its held request without a
  handler-entry KV write. Two subsequent requests succeed through the remaining
  worker. The failed request is not silently moved or retried.
- Waiting beyond the signed proof's allowed age rejects a previously successful
  verification result without running the handler. This is **request-age
  expiry**, not a claim that the context itself expired during that live run.
- Revoking the real caller session through the versioned admin API, then waiting
  for the service cache to receive the **exact proof context's** revocation,
  rejects the held request without running the handler. Merely awaiting the
  admin acknowledgment was insufficient to establish this interleaving.

All three checks passed. Handler non-execution is observed through the real
service's persisted marker, not an in-memory counter. Native protocol coverage
also checks expiry of the previously verified context through the same
post-worker freshness function; that context-expiry case uses the crypto
boundary rather than a separate live worker interleaving. These held runs are
correctness diagnostics, not additional performance measurements.

### Clustered Rust KV follow-up

The clustered repository fixture began work before the new NATS cluster was
usable. Protocol traces captured stream placement and leader election before the
assigned node received the caller's system-account reply subscription. Checking
only metadata quorum or application route counts did not prevent this. A further
reproduction returned error 10005 (`peer offline`) during promotion to three
replicas: NATS 2.14.4 placement requires peer configuration/statistics in
addition to connected routes and a current metadata quorum.

The fixture now waits for both the application route pool and dedicated system
routes, and establishes a real three-replica put/get before repository setup.
Only the readiness probe retries explicit placement-unavailable error 10005;
repository operations and assertions are not retried. After intentional leader
termination, readiness includes a successful leader read, using a short timeout
on the readiness client only. Production repository behavior, storage semantics,
and request timeouts are unchanged.

Verification passed in **20 consecutive fresh clusters**, followed by the full
runtime library suite: **240 passed, zero failures**. The protocol suite also
passed **59/59**, including rejection of a previously verified context after its
expiry. These results resolve the observed fixture-startup failures; they do not
claim that production KV operations succeed during arbitrary outages.

### Worker verification profiling pass

Eta diagnostic runs kept the approved one-ordinary/one-reserved topology, native
Rust caller, 256 KiB appended values, 500 ms handlers, 1,024 admitted ordinary
requests, 512 MiB incoming budget, and 1,024 outstanding caller cap. Each case
offered one 20-second burst. The initial matrix compared 200 and 400/s; a second
matrix repeated 200/s, and a third repeated 400/s with Deno's native per-isolate
CPU profiler. Inspector main-thread profiles cover 15 seconds of the offered
window. Native profiles cover the whole provider lifetime, including startup and
settling. Profiling changes scheduling, so these are diagnostic measurements,
not new capacity/default recommendations.

All temporary directories, caches, and profile outputs were disk-backed under
the local `.verification/` and Eta home-directory workspace; no `/tmp` writes
were used. Source manifests accompany each matrix. Eta's embedded inline WASM
and worker-loaded WASM were byte-identical (SHA-256
`5bf5b5da431f0795fd13c589ba417599b3ef96de5c64042d0e8ebc3c075f6232`).

Ordinary-lane means from final exported cumulative histograms:

| Phase                                     | Workers 200/s | 200/s repeat | Workers 400/s | 400/s native-profile repeat |
| ----------------------------------------- | ------------: | -----------: | ------------: | --------------------------: |
| Scheduler queue                           |      19.48 ms |     39.93 ms |   3,162.22 ms |                 3,096.39 ms |
| Dispatch preparation/copy                 |      0.076 ms |     0.074 ms |      0.154 ms |                    0.191 ms |
| Worker-local processing                   |      2.901 ms |     3.240 ms |      2.684 ms |                    2.601 ms |
| Complete WASM proof call (included above) |      2.888 ms |     3.226 ms |      2.674 ms |                    2.590 ms |
| Boundary and scheduling residual          |      1.334 ms |     1.283 ms |      1.479 ms |                    1.581 ms |
| Successful post-verification checks       |      0.040 ms |     0.043 ms |      0.027 ms |                    0.028 ms |

Inline complete verification averaged 1.55/1.62 ms at 200/s and 1.17/1.15 ms at
400/s. Inline postchecks averaged approximately 0.001 ms. Worker queue/copy/
execution counts were 4,002 at 200/s, 5,489 in the first 400/s case, and 5,530
in the native-profile repeat. The extra two requests are setup/Operation-start
verification. Queue samples describe dispatched verification only, not all
offered traffic or attempts dropped before scheduling. Cumulative exports are
selected once per process/metric lifetime, not summed across export intervals.

**CPU attribution:** in the native-profile 400/s worker run, software SHA-256
consumed about 80% of the ordinary verifier's non-idle sampled time; Ed25519
field arithmetic was the next visible cost. The ordinary verifier was idle for
about 47% of its whole-lifetime samples despite substantial queued ordinary work
during overload. This fits the serial send/result/next-send scheduling path but
does not prove every idle sample is handoff delay: startup and settling are
included. The main thread was about 44% idle; response encoding was its largest
named CPU function at about 23% of non-idle samples. Main-thread offered-window
WASM share fell from about 48% non-idle inline to 3–5% in worker mode. The
worker execution timer includes time descheduled by the OS and is not a CPU-time
counter. The boundary residual includes serialization, delivery and both
event-loop schedules; it is not pure IPC latency.

**Conclusions:** payload copying, worker JSON/context preparation, and final
freshness checks are not the dominant measured stages. Complete WASM
verification dominates worker-local processing, with meaningful serialized
handoff cost as well. Verification ran slower in the worker than inline in these
runs even with identical WASM; its lower-level cause has not been isolated. Do
not attribute that difference to a different security implementation or claim
that adding workers has been proven to fix it. More ordinary workers or bounded
pipelining are follow-up experiments requiring an explicit design decision;
neither was introduced during this profile pass.

All inline cases completed every offered request; both worker 200/s cases did
too. At 400/s the first worker case completed 1,718/8,000, with 1,802 busy
replies, 4,301 timeouts, and 179 unsent. The native-profile repeat completed
1,731/8,000, with 1,765 busy replies, 4,326 timeouts, and 178 unsent. The
recorded cancellation probe completed in every case, including both overloaded
cases (296 and 285 ms). No assertions, security settings, limits, worker counts,
or retry policy changed.

Production phase metrics remain in the SDK and are rendered by the ordinary
admission report. Retained tooling is `benchmarks/runtime/admission_profile.py`,
`profile.ts`, and `run.ts --cpu-profile-provider`. Raw evidence lives in
`.verification/eta-worker-profile-results/`,
`.verification/eta-worker-profile-repeat-results/`, and
`.verification/eta-worker-profile-native-results/`. The component/native-proof
check and focused real-NATS admission checks were rerun after instrumentation;
full acceptance was not rerun.

### Two ordinary verifiers: approved follow-up trial

The next authorized experiment changed only the opt-in scheduler to two ordinary
workers plus the unchanged reserved worker. Ordinary requests select the less
occupied ordinary worker; admission counts remain shared across both workers,
and context release visits all three workers. No new queue, admission budget,
retry, security rule, or shipping default was introduced. The exact trial source
hashes are retained in the matrix source manifest.

Eta repeated the same sequential diagnostic cases (Rust caller, 256 KiB values,
500 ms handlers, 1,024 admitted requests, 512 MiB incoming budget, 1,024 caller
cap, 20-second burst) with both offered-window and native per-isolate CPU
profiling enabled. Home-directory disk-backed temporary storage and caches were
used throughout; no `/tmp` writes were used.

| Case                       | Completed/offered | Busy/timeouts/unsent | Cancellation | Mean ordinary queue wait |
| -------------------------- | ----------------: | -------------------- | -----------: | -----------------------: |
| Inline 200/s               |       4,000/4,000 | 0/0/0                |       284 ms |           Not applicable |
| Two ordinary workers 200/s |       4,000/4,000 | 0/0/0                |       313 ms |                  2.42 ms |
| Inline 400/s               |       8,000/8,000 | 0/0/0                |       388 ms |           Not applicable |
| Two ordinary workers 400/s |       8,000/8,000 | 0/0/0                |       285 ms |                 36.50 ms |

At 400/s, ordinary worker verification averaged 2.605 ms, boundary/scheduling
1.455 ms, dispatch preparation/copy 0.053 ms, and successful final checks 0.020
ms (8,002 verification samples including setup). Inline verification averaged
1.145 ms. Two workers therefore removed the observed 400/s backlog without
making each WASM call faster. The preceding one-ordinary-worker profiles
completed 1,718 and 1,731 of 8,000, with mean dispatch queue waits of 3.16 and
3.10 seconds. These are separate sequential runs, not a statistical capacity
study or proof of a production sizing rule. Cancellation has one sample per
case; the values are not latency guarantees. The cause of slower worker-local
verification remains unisolated. Higher rates were not tested in this follow-up.

The opt-in trial now uses two ordinary workers plus one reserved worker; it is
still off by default. Verification passed the component/native-signature check
(including context release/reinstallation across concurrent requests and shared
capacity), eight real-NATS admission/revocation/Transfer tests, packaged Node
proof verification, service/benchmark typing, formatting and diff checks.
Browser packaging and full acceptance were not rerun for this topology-only
change. Raw evidence is retained in
`.verification/eta-two-ordinary-profile-results/`.

### Original laptop trials

The original laptop trial used **32 ordinary dispatches / 16 MiB incoming
bytes**, with 8 control dispatches / 1 MiB and 4 refusal-verification dispatches
/ 1 MiB. These are not approved framework defaults. At 200/s with 50 ms
handlers, 16 slots occasionally refused large-message bursts; 32 avoided those
refusals in both SDKs. 64 allowed more retained work without improving the
below-capacity case. That result does not establish a safety-only framework
default or a production capacity or memory guarantee. Control and verification
settings were functionally tested but not independently optimized in a parameter
sweep.

Final isolated runs used the optimized Rust provider, ordinary development
server/CLI executables, real NATS, a public TypeScript client, and no concurrent
builds or acceptance runs. Each offered window lasted 20 seconds; the generator
limited outstanding calls to 128. A confirmed running Operation was cancelled
halfway through each window. Every cancellation completed; one probe per case is
not a control-latency distribution. Raw reports, OTLP/resource samples,
executable hashes, source diff, and attempts are retained in
`.verification/results/admission-isolated/` in the verification worktree. Failed
setup/quota runs and overlapping-workload runs are excluded from these
conclusions. Earlier unoptimized Rust runs are not production capacity evidence.

At 32 slots and 200/s, all four small/large TypeScript/Rust cases completed all
4,000 requests with no refusal, timeout, or generator drop. Echo p95 was 54.55
ms (TypeScript small), 71.53 ms (TypeScript 256 KiB), 54.42 ms (Rust small), and
68.09 ms (Rust 256 KiB). Cancellation took 256–277 ms.

At 800/s each case offered 16,000 requests. Busy replies, transport timeouts,
and requests never sent are distinct; do not collapse them into server
throughput. Echo latency describes successful RPCs only.

| Provider   | Slots | Value bytes | Submitted | Busy replies | Timeouts | Generator drops | Echo p95 ms | Cancellation ms |
| ---------- | ----: | ----------: | --------: | -----------: | -------: | --------------: | ----------: | --------------: |
| Rust       |    16 |           0 |     16000 |         9816 |        0 |               0 |       55.38 |          259.82 |
| Rust       |    16 |      262144 |     10152 |         5103 |      631 |            5848 |      231.52 |          325.82 |
| Rust       |    32 |           0 |     16000 |         3803 |        0 |               0 |       56.02 |          261.83 |
| Rust       |    32 |      262144 |     10989 |         2356 |      258 |            5011 |      263.21 |          416.70 |
| Rust       |    64 |           0 |     16000 |            0 |        0 |               0 |       55.48 |          257.87 |
| Rust       |    64 |      262144 |     10467 |          124 |       86 |            5533 |      310.38 |          619.42 |
| TypeScript |    32 |           0 |     16000 |         3837 |        0 |               0 |       56.16 |          275.23 |
| TypeScript |    32 |      262144 |      9974 |         1293 |        0 |            6026 |      346.23 |          937.99 |

Timeouts do not establish where each message was lost. The policy intentionally
drops traffic when refusal verification cannot reserve capacity, and NATS
client/broker buffers remain separate. Production admission metrics establish
bounded dispatch, not a hard end-to-end control-latency or process-memory
ceiling. The generator's large-message heap is separate from provider
incoming-byte accounting. Runtime teardown exceeded the testkit's existing
5-second bound and was killed after measurements; these reports do not prove
whole-runtime graceful shutdown, and its cause is not attributed by this
experiment.

Reproduce with the retained `admission` lane, existing matching server/CLI
binaries, `--provider-language rust`, an optimized `--rust-bin`,
`--arrival-rate 800 --calls 16000 --samples 1 --warmups 0 --rpc-delay-ms 50`,
`--rpc-value-bytes 262144 --request-limit 32 --request-byte-limit 16777216`, and
a fresh `--output`. Use workspace-local `TMPDIR`, `TMP`, `TEMP`, and
`TRELLIS_CACHE_DIR`; no writes to `/tmp` are permitted in this worktree.

## Original mechanisms before this change

- **Rust:**
  [`run_nats_request_loop_until`](../../crates/trellis/src/service/request_loop.rs#L1126)
  takes messages from the subscription streams, decodes them, captures the
  receiving generation, and adds a future to `FuturesUnordered` without a count
  or byte limit (lines 1137–1172). These are concurrently polled futures, not a
  spawned Tokio task per request. RPC and Operation control subjects are both
  registered by the
  [runtime facade](../../crates/trellis/src/service/runtime_facade.rs#L1364) and
  passed through provider ingress. There is no reserved control execution
  capacity in this request loop.
- **TypeScript unary RPC:**
  [`#runRpcIntake`](../../ts/packages/trellis/session.ts#L4650) awaits
  processing and sends the reply before consuming the next message. This is one
  request at a time **per route and receiving generation**, not one request for
  the whole service. Excess messages stay in the NATS subscription iterator. An
  execution semaphore after dequeue would not bound that backlog.
- **TypeScript Operations:** control, start, and reconciliation pumps use
  [`createOperationIntakeQueue`](../../ts/packages/trellis/service/runtime/core.ts#L423).
  Its backing array has no count or byte limit. The
  [control pump](../../ts/packages/trellis/service/runtime/core.ts#L2252) holds
  the receiving generation's lease while queueing the delivery. Separate
  subscriptions therefore do not imply reserved or bounded execution capacity.
  Live also has its own protocol-specific flow control; that is not a general
  request-admission budget.
- **Installed clients:** `@nats-io/nats-core` **3.4.0** appends iterator
  messages to an array (`lib/queued_iterator.js`, lines 44–67). Its
  slow-consumer notification does not impose a message/byte cap. `async-nats`
  **0.50.0** defaults to **65,536 messages per subscription** (`src/options.rs`,
  line 113). Its full-channel path drops the message and emits `SlowConsumer`
  (`src/lib.rs`, lines 756–774); it is not a caller-visible overload reply or a
  byte budget. Trellis does not override that subscription capacity.
- **Broker limits:** maximum payload and connection pending bytes protect the
  protocol/socket boundary, not application handler concurrency or the memory
  already buffered in the client. Raising buffers is not admission control.

The upstream
[NATS slow-consumer guidance](https://docs.nats.io/learn/resilient-clients/slow-consumers)
agrees with these installed client mechanisms. The installed sources, rather
than newer online API descriptions, are authoritative for the versions above.

### Retirement and shutdown constraints

Rust retirement drains subscriptions, consumes already-delivered messages, and
waits for accepted sibling futures even after one fails (request loop lines
1180–1206). TypeScript
[RPC intake installation](../../ts/packages/trellis/session.ts#L4629) separately
tracks subscription drain and completion of buffered processing. Both preserve
the receiving generation through handling/reply; queued Operation deliveries
also retain their generation lease.

A proposed limit must not cancel accepted work to reclaim capacity, move replies
to a different generation, or interpret a subscription protocol flush as handler
completion. Budgets must be shared across active and retiring generations of the
same service, rather than reset on replacement. Transport revocation and its
fail-closed behavior remain authoritative.

## Measurements

Base: `1eac779b9422c935243b59a75ac6183b909af490`, with the retained benchmark
overlay. Six sequential runs on the same Linux host (Intel Core Ultra 9 185H),
with real NATS/JetStream and ordinary production server/CLI builds. The native
provider used an unoptimized development build with debug information disabled;
these are mechanism comparisons, **not production capacity estimates**. Cgroup
CPU and memory maxima were unlimited.

Each case uses one provider, one generated TypeScript client, a 50 ms async Echo
handler delay, three trials, and no excluded warmups. Baseline: 32 requests per
trial at 10/s. Load: 64 per trial at 200/s, then wait for the client attempts to
finish. The large case appends 262,144 ASCII characters to the value; this is
not the complete signed message size. Echo verifies the returned value.

Before each burst, the client starts an Operation and observes its
input-specific running progress. Halfway through the offered burst, it requests
cancellation and requires a terminal `cancelled` snapshot. Cancellation latency
includes that terminal result, not just dispatch of the command. All **18
cancellation probes** succeeded. The load-generator cap was 128; **no generator
drops** occurred.

| Provider / case         | RPC attempts / errors | Successful RPC median ms | Successful RPC p95 ms | Cancel median ms | Sampled maximum dispatched RPCs | Provider idle → maximum sampled PSS MiB |
| ----------------------- | --------------------: | -----------------------: | --------------------: | ---------------: | ------------------------------: | --------------------------------------: |
| TypeScript / baseline   |                96 / 0 |                   53.830 |                55.771 |          262.558 |                               1 |                         163.69 → 174.24 |
| TypeScript / small load |               192 / 0 |                 1521.336 |              2849.009 |          262.129 |                               1 |                         161.80 → 177.01 |
| TypeScript / large load |               192 / 8 |                 1549.508 |              2852.792 |          265.326 |                               1 |                         161.09 → 297.94 |
| Rust / baseline         |                96 / 0 |                   59.329 |                60.670 |          270.059 |                               1 |                           35.83 → 39.51 |
| Rust / small load       |               192 / 0 |                  123.538 |               177.981 |          484.669 |                              13 |                           36.55 → 40.00 |
| Rust / large load       |               192 / 0 |                  692.990 |               799.108 |         1084.039 |                              23 |                           36.10 → 81.41 |

All eight failed calls retain `trellis.request.failed` with a nested NATS
`TimeoutError: timeout`; durations were 3001.6–3003.6 ms. Neither the ordinary
3-second request timeout nor assertions were changed. The large TypeScript run
and its report both exit **1** while retaining evidence. The other five runs and
their reports exit **0**. This is observed overload, not a green full acceptance
suite or the earlier integration-fixture startup failure.

### Interpretation and proof limits

- TypeScript's serialized 50 ms handler can process approximately 20 requests/s
  before protocol overhead; offering 200/s builds a backlog. The large run
  demonstrates real response deadlines expiring. The benchmark does not identify
  the individual CPU costs responsible for its additional overhead.
- Rust's successful burst does not prove a capacity bound: the intake source is
  still unbounded. More simultaneously dispatched RPCs and higher cancellation
  latency are observed. The experiment does not isolate the exact contribution
  of parsing, signing, scheduling, broker work, or persistence to cancellation
  latency, and does not prove that a separate connection is needed.
- Large values increase whole-process memory in both providers. PSS includes
  allocator/runtime caches, parsed values, replies, and buffers. It is **not** a
  measurement of queue bytes, per-request allocation, or a leak.
- `trellis.rpc.server.inflight` counts dispatched unary RPCs, not all retained
  frames or requests awaiting authentication. Exports request a 100 ms interval;
  observed maxima are lower bounds. Raw route labels are preserved: Rust uses
  the qualified route; TypeScript uses the method name.
- These are short fixed-arrival bursts, not sustained capacity tests. Successful
  latency percentiles exclude failed calls, whose count and full errors remain
  visible. There are too few cancellation trials for tail-latency claims. No
  hard control-latency guarantee, Live-control flood behavior, slow
  signal-handler isolation, or shutdown/replacement under this load was
  experimentally proved.

### Retained evidence and instrumentation

Raw runs are at
`/tmp/opencode/admission-main-{typescript,rust}-{baseline,small,large}`. Each
contains metadata, the tracked source overlay, server/CLI hashes, native
provider hash when applicable, complete samples, resource windows/timeline,
production OTLP exports, and Markdown/Bencher reports. Adjacent `.runner.log`
and `.report.log` files retain the runner output and report verdict. This
repository report preserves the measured conclusions even when local artifacts
are cleaned.

The reusable lane, collector, memory/metric reporting, and slow/cancellable
handlers remain in `benchmarks/runtime`. The collector decodes real exports with
the existing upstream OTLP protobuf schema; it does not inspect exporter-private
objects. The native fixture initializes the normal telemetry guard and enables
the SDK's existing `telemetry-otlp` feature. No production metric family or
test-only runtime hook was invented. Both providers disable trace export for
these matched runs; production metrics remain enabled.

## Proposed direction — approval required

1. **Bound admission by both retained request count and wire-message bytes**, at
   the earliest safe intake boundary, before creating unbounded futures or
   parsing/cloning every accepted body. Retain the charge until handling and its
   reply finish, including failure/unwind paths. Count and byte limits address
   different workloads; the byte charge must not be described as exact heap use.
2. **Keep execution concurrency distinct from admission.** Do not wait on an
   execution semaphore while allowing another unbounded subscription/array
   backlog. Prefer no new waiting queue unless a bounded burst queue has a real
   requirement. TypeScript's existing route serialization is a separate behavior
   decision, not something to silently replace with concurrent handlers.
3. **Reserve bounded capacity for protocol control**, including cancellation and
   Live lifecycle traffic, independently of ordinary RPC/Operation starts.
   Reservation does not by itself promise latency against CPU-bound application
   code. Keep existing Live flow-control and generation ownership intact.
4. **Define honest caller-visible overload behavior.** Only issue an overload
   reply after ordinary authentication, authorization, and exact reply binding;
   never reflect a response to an unverified target. Bounded verification or
   transport overflow may still require dropping unverified excess frames, so a
   classified overload reply cannot be promised for every arrival. Do not add
   automatic SDK retries for possibly side-effecting requests.
5. **Put budget observability in the production catalog, in both SDKs.** If the
   policy is approved, add admitted-request count, charged wire bytes, and
   rejected-arrival count with bounded kind/reason labels. Add queue wait/depth
   only if a real bounded queue exists. Keep the existing RPC in-flight metric's
   meaning; do not relabel it as intake backlog or use request/generation IDs as
   metric dimensions. This lane should consume those ordinary exports.

Use existing first-party intake/generation ownership and standard primitives:
Tokio permits/RAII in Rust; synchronous budget reservation and
`finally`-released ownership in TypeScript. JavaScript's existing NATS callback
API avoids creating an additional iterator backlog at that boundary. Do not add
a queueing framework or a generic scheduler library for this change. Native
subscription count limits remain a transport backstop, not a replacement for the
service byte budget.

Before implementation, settle service-wide versus route execution fairness,
TypeScript concurrency semantics, any bounded waiting allowance, overload wire
classification/hard-drop behavior, and initial count/byte/control defaults. This
experiment justifies investigating the mechanism, not selecting those values.

Verification of an approved implementation must use ordinary runtimes and public
clients: sustained slow/small and large-value overload; actual refusal and
subsequent recovery; cancellation and Live control while saturated; permit/byte
release after real failure; and accepted-request completion during generation
replacement and shutdown. Include caller timeout followed by observable handler
completion rather than assuming timeout cancelled the handler. No fake
transport, synthetic runtime failure hooks, longer timeouts, or weakened
authorization.
