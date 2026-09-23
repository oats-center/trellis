# OBS-02 collected evidence

This document records the executable evidence behind the **collected metric**,
**collected trace**, and **executed boundary** categories in
[`disposition.md`](./disposition.md). Evidence is sanitized: no credentials,
proof payloads, user identifiers, or bulk historical series are included.

## Collected Export

[`collected-2bfabca3.json`](./collected-2bfabca3.json) is the accessible,
sanitized manifest for source `2bfabca31eb09a6dbc5d096a6b7e670f5e063e8e` on top
of `rs` `061f11fe8ef67cbda9dab608a49f13e53be214bb`. It contains actual
Prometheus sample values and exported trace/span/parent/link IDs, not a list of
source owners. `maxSeriesValue` is the maximum of the scraped series at that
instant, **not** a total over all label combinations or a benchmark. A zero
DLQ/Consumer gauge is a real observation, not proof of a transition; the live
DLQ replay case separately yielded two `trellis.dlq.transitions` events.

[`collected-5f9db489.json`](./collected-5f9db489.json) is a separate capture
from source `5f9db489d22ba1f4d0f7ee8e43cedb24d5bad317` including the TypeScript
Operation, Job worker, and KV owners plus the corrected `events_dlq` component.
It retains selected per-owner numeric samples, not aggregate counts across
unrelated process instances. Its trace IDs show retry and completion linked to
the **same exported** TypeScript RPC producer, both Rust and TypeScript
Operation links to exported creators, and matched caller/server parent IDs in
both language directions. The old manifest retains its historical source SHA.
Each numeric row was checked against a Prometheus scrape taken immediately after
its named scenario; expired process series make a single end-of-campaign scrape
incomplete. On the repeated TypeScript run, one local Operation completed and
the other lost its fence while the active gauge returned to zero; the manifest
records that observed mix, not a fixed outcome expectation.

[`collected-obs02-owners.json`](./collected-obs02-owners.json) is the
source-pinned manifest for this candidate (`94caa7ee`, the same `rs`
`aae0b66dd43990924395c4483d4ac5c904c67645`). It adds the TypeScript owners that
the previous two captures could not claim: the service connection
`usable`→terminal transition case with `coverage_lost`, the outer request and
event verifier outcomes, Live `active`/`ends` for normal and early client close,
and the real-NATS connection and refresh cases. It also records the new
attempt-context trace evidence: `trellis.job.attempt.start`/`finish` spans in
distinct per-attempt traces whose finish links target the same exported
producer, the downstream RPC as a child of its own attempt's start span, and the
receiver as a child of the caller's client span. The checker additionally
rejects any lifetime `trellis.job.attempt` span, so the old long-lived-span
evidence cannot reappear unnoticed. The `rustDelivery` rows come from the
`integration/fixtures/runtime` Events binary after it started initializing
process telemetry as `events-rust`; they show the durable Consumer recording
actual `ok`/`retry`/`exhausted` processing outcomes and `ack`/`nak` Event
dispositions at the language owner, plus the new short
`trellis.event.attempt.start` context whose producer link is added only after
proof verification.

The native and browser export flows used a case-owned `otelcol-contrib:0.160.0`
at `127.0.0.1:4318`; its Prometheus endpoint was `127.0.0.1:9464`, with traces
forwarded to a second case-owned Collector's file exporter at `127.0.0.1:4322`.
The two capture-only configurations are
[`capture-native.yaml`](./capture-native.yaml) and
[`capture-traces.yaml`](./capture-traces.yaml); the production
`deploy/observability/` configurations are unchanged. No raw payloads,
credentials, principal identifiers, request IDs, dynamic subjects, or
high-cardinality metric labels are retained in the manifest.

Reproduce from the repository root with real NATS, a built server, and cached
Chromium. Keep raw traces private and run the capture exporter as the host user:

```sh
set -e
CAPTURE_DIR=$(mktemp -d)
trap 'podman rm -f trellis-obs-native trellis-obs-traces >/dev/null 2>&1 || :; rm -r -- "$CAPTURE_DIR"' EXIT
podman run -d --rm --name trellis-obs-traces --network host \
  --userns=keep-id --user "$(id -u):$(id -g)" \
  -v "$PWD/docs/observability/capture-traces.yaml:/config.yaml:ro,z" \
  -v "$CAPTURE_DIR:/capture:z" \
  docker.io/otel/opentelemetry-collector-contrib:0.160.0 --config /config.yaml
podman run -d --rm --name trellis-obs-native --network host \
  -v "$PWD/docs/observability/capture-native.yaml:/config.yaml:ro,z" \
  docker.io/otel/opentelemetry-collector-contrib:0.160.0 --config /config.yaml
```

Then:

```sh
export TRELLIS_TEST_SERVER_BIN="$PWD/rust/target/debug/trellis-server"
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
export OTEL_TRACES_SAMPLER=always_on
export OTEL_METRIC_EXPORT_INTERVAL=1000
export OTEL_BSP_SCHEDULE_DELAY=100
export TRELLIS_OBS_CAPTURE_ENDPOINT=http://127.0.0.1:4318

deno test -A -c ts/integration/deno.json ts/integration/observability_test.ts \
  --filter 'collector enabled'
deno test -A -c ts/integration/deno.json ts/integration/observability_test.ts \
  --filter 'TS service connection observes'
deno test -A -c ts/integration/deno.json ts/integration/observability_test.ts \
  --filter 'TS Live active'
deno test -A -c ts/integration/deno.json ts/integration/runtime_test.ts \
  --filter 'surviving replica reclaims an expired operation lease'
deno test -A -c ts/integration/deno.json ts/integration/kv_telemetry_test.ts \
  --filter 'one bounded storage'
OTEL_SDK_DISABLED=true deno test -A -c ts/integration/deno.json \
  ts/integration/kv_telemetry_test.ts --filter 'telemetry disabled'
deno test -A -c ts/integration/deno.json ts/integration/events_test.ts \
  --filter 'rust durable delivery leases, DLQ replay, and crash recovery'
TRELLIS_OBS_BROWSER_CAPTURE_ENDPOINT=http://127.0.0.1:4318 \
  deno test -A -c ts/browser/deno.json ts/browser/console_login.browser_test.ts \
  --filter 'browser admin bootstrap'
curl --fail http://127.0.0.1:9464/metrics
jq -s -f docs/observability/verify-capture.jq "$CAPTURE_DIR/traces.json"
```

The `jq` command fails unless the exported span IDs prove both caller
directions, the Rust and TypeScript Operation links target exported creators,
retry/completion Job attempts link to the same exported RPC producer, each
attempt's downstream RPC is a child of its own attempt-start span, and no
lifetime `trellis.job.attempt` span exists. It also rejects any exported span
attribute key denoting a credential, token, cookie, proof, payload, body,
authorization header, or raw NATS subject. It checks attribute **names**, not
the semantic safety of arbitrary values; inspect the local capture before
publishing any further data. It prints booleans only, and the shell exits on
failure while its trap removes the raw capture.

The enabled integration test executes real authenticated generated Jobs to
terminal completion, including a retrying Job created by an RPC handler and a
keyed lease, a published and **consumed** Event, RPCs in both language
directions, and Operations completed by TypeScript and Rust providers. The
separate Operation owner-failover case records an interrupted predecessor and a
completed successor with one balanced active gauge, without crediting the old
owner for the successor's persisted terminal result. The KV integration uses a
real binding and observes exactly one read/not-found and CAS/conflict total per
public call; its disabled-mode case runs in a fresh Deno process. The browser
test bootstraps the first administrator and loads the Events page in a real
Chromium document. Its same-origin `/otel` route forwards requests to the
capture Collector without changing the business transport. Exported spans prove
browser-to-Rust and Rust-to-TypeScript parentage by matching `traceId` and
`parentSpanId` to the caller's actual `spanId`, including the Events query after
browser navigation. Persisted Operation executions link to exported creator
spans; retrying Job attempts link to the same exported RPC producer, each in its
own attempt context. No raw exported request payload is attached.

The deterministic owner proof lives with each production owner rather than in
the capture pipeline: `ts/packages/trellis/connection_test.ts` drives the real
`observeTrellisConnection` handle through `connecting`→`usable`→`suspended`→
`resumed`→terminal for both `service` and `device`, asserts a same-context
`refreshed` transition leaves the active count at one, and asserts final cleanup
clears every previously represented aggregate to zero.
`authorization_context_test.ts` wraps the real `verifyLocalAuthorization`
boundary for a valid request, a valid Event, an invalid proof (with telemetry
disabled and installed), and a missing cache, checking the bounded
`request:*`/`event:*` outcomes; it also drives the provider cache's own/peer
coverage gauge through unavailable and restored states.

Default `observability_test.ts` runs separately with isolated server OTEL
environment variables: no endpoint, `OTEL_SDK_DISABLED=true`, traces-only (an
explicit trace endpoint, metrics disabled), and an unavailable Collector. Each
case reaches real terminal Job and Event-consumer boundaries. The traces-only
case observes `/v1/traces` but no `/v1/metrics`; disabled mode observes neither.
A telemetry export failure never selects a business result.

### Monitoring configuration validation

```sh
# Collector configuration (three reference configurations)
podman run --rm -e TRELLIS_CLUSTER=validation -e TRELLIS_ENVIRONMENT=validation \
  -e TRELLIS_TRACE_BACKEND=http://127.0.0.1:4318 \
  -v "$PWD/deploy/observability:/cfg:ro,z" \
  docker.io/otel/opentelemetry-collector-contrib:0.160.0 \
  validate --config /cfg/collector.yaml            # also collector.native.yaml, collector.browser.yaml

# Prometheus configuration, recording rules, and rule tests
podman run --rm --entrypoint /bin/promtool \
  -v "$PWD/deploy/observability:/cfg:ro,z" docker.io/prom/prometheus:v3.13.1 \
  check config /cfg/prometheus.yml
podman run --rm --entrypoint /bin/promtool \
  -v "$PWD/deploy/observability:/cfg:ro,z" docker.io/prom/prometheus:v3.13.1 \
  check rules /cfg/recording-rules.yml /cfg/alert-rules.yml
podman run --rm --entrypoint /bin/promtool \
  -v "$PWD/deploy/observability:/cfg:ro,z" docker.io/prom/prometheus:v3.13.1 \
  test rules /cfg/rule-tests.yml

# Alertmanager configuration
podman run --rm --entrypoint /bin/amtool \
  -v "$PWD/deploy/observability:/cfg:ro,z" docker.io/prom/alertmanager:v0.28.1 \
  check-config /cfg/alertmanager.example.yml
```

All three Collector configurations validate without error; Prometheus config,
recording rules, and rule tests report SUCCESS; Alertmanager reports one route
and one receiver.

## Executed-boundary evidence

Focused tests exercise the real owner and assert its observation without a live
Collector:

- `crates/trellis/src/service/request_loop.rs` —
  `dispatch_outcomes_classify_business_results_not_envelopes`,
  `dispatch_error_envelope_never_records_ok`,
  `feed_streams_never_acquire_a_unary_sample` (real dispatcher plus a collecting
  metric reader).
- `crates/trellis/src/telemetry/instruments.rs` — source
  registration/removal and later-source export through a collecting reader.
- `crates/trellis/src/telemetry/export.rs` —
  `repeated_flushes_coalesce_into_one_operation`.
- `crates/trellis/src/telemetry/propagation.rs` — explicit-root extraction,
  duplicate/oversized rejection, propagator validation.
- `crates/runtime/src/telemetry/snapshots.rs` — disabled metrics start no
  sampler task and stop aborts a running sampler promptly.
- `crates/trellis/src/service/operations.rs` — the diagnostic trace carrier
  is excluded from the invocation identity digest.
- `ts/packages/trellis/connection_test.ts` — process-local connection and
  coverage sources survive the first collection, export live counts, and clear
  to zero after the last owner disposes, through a collecting metric reader.
- `ts/packages/trellis/auth/authorization_context_test.ts` — the provider
  cache's own/peer coverage gauge reflects a retained own installation, a
  resolved peer, connection-generation suspension, and same-generation
  restoration through a collecting reader.
- `ts/integration/observability_test.ts` — the capture-enabled cases exercise a
  real service connection through usable, suspended, and terminal states, a real
  Live across normal and early client cancellation with balanced active counts,
  and the outer TypeScript verifier's bounded outcomes.

## Candidate

Each collected manifest identifies the source SHA that emitted its samples;
documentation-only follow-ups and the exact-candidate Check are identified in
the independent-review handoff. The `59` catalog families remain fixed; only
those with captured numeric points or trace IDs in either manifest are claimed
as collected for the stated language owner. Other owners are distinguished as
source-inspected, exercised at a focused test boundary, or **not implemented**
in `disposition.md`, never inferred from another language's sample.
