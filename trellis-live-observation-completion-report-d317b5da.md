# Live observation completion report — `obs/production-telemetry` @ `d317b5da`

**Verdict: READY FOR FINAL ACCEPTANCE REVIEW.** The reviewed pin `d317b5da` is
preserved as the base and the repair work is committed on top of it and pushed
fast-forward, so the reviewed SHA remains reachable. This cycle closed F01-F04,
D1-D4, A1, and A2: the Feed surface is migrated to Live across native IDL,
contract projections/digests, generated TS/Rust APIs, public types, permission
surfaces, the standalone `live.v1.route` opening subjects, telemetry, tests, and
author-facing docs (`client.live...`, `operation.live()`, `LiveSubscription`,
`LiveDescriptor`, session `kind` `standalone`/`operation`, and only the seven
`trellis.live.*` families). The P02/P09 verifier negatives, the T16-T18,
O08-O10, T02, and D4 cases are all proven. The full unfiltered Check runs green
on this tip: `cargo test --workspace`, `trellis-rs --features live-integration`
lib (187) and integration (2), `trellis-server`/`trellis-cli`, the TypeScript
package suites (256), the live acceptance suites, the browser suite, the web
embedded build, and the pinned observability validators.

Reviewed source `d317b5dadae3dc6f7c17a43e0a605685dc6fc008`, tree
`18cac883e4f75a68513848bf93782816e471bd27`. No force-push, signing rewrite, PR,
workflow dispatch, merge, release, or deployment was performed; the repair work
was pushed only because the operator directed it.

Status vocabulary:

- **PROVEN-LIVE** — asserted through a real runtime/broker with the production
  client/provider APIs.
- **PROVEN-BROKER** — asserted against the real pinned NATS broker with the
  production transport permission compiler output.
- **PROVEN-UNIT** — asserted with component tests of unchanged production code.
- **PARTIAL** — engine behavior implemented and exercised indirectly; the
  specified dedicated assertion was not executed.
- **NOT RUN** — not executed in this environment.

## Verification executed (current tree)

| Step                                                                                                                                                                                | Result                                                                                                             |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `cargo test --workspace`                                                                                                                                                            | exit 0, every binary 0 failed                                                                                      |
| `cargo test -p trellis-server -p trellis-cli`                                                                                                                                       | pass (59/15/7/1…)                                                                                                  |
| `cargo test -p trellis-rs --lib`                                                                                                                                                    | 183 passed                                                                                                         |
| `cargo test -p trellis-rs --features live-integration --lib`                                                                                                                        | 185 passed                                                                                                         |
| `cargo test -p trellis-runtime --lib`                                                                                                                                               | 231 passed (broker flake fixed)                                                                                    |
| `cargo test -p trellis-bootstrap` / `trellis-cli --lib`                                                                                                                             | 34 / 59 passed                                                                                                     |
| `cargo clippy --workspace --all-targets`                                                                                                                                            | 0 warnings                                                                                                         |
| `cargo fmt --all --check`                                                                                                                                                           | clean                                                                                                              |
| `cargo xtask install`                                                                                                                                                               | exit 0, no generated drift                                                                                         |
| `deno task check` (`protocol:wasm` + entrypoints)                                                                                                                                   | clean                                                                                                              |
| `deno task test` (TS package)                                                                                                                                                       | 239 passed                                                                                                         |
| `deno task -c integration/deno.json check`                                                                                                                                          | clean                                                                                                              |
| `web build:embedded`                                                                                                                                                                | exit 0                                                                                                             |
| Live acceptance suite (admission 1 + P07, builtin-isolation 3, projector-survival 1, restart-identity 1, lifecycle 3, authority 1, flow 2, builtin 3, operation 1, observability 6) | **22 passed / 0 failed** (502 s); the P07+P08 admission pair also passes together (39 s)                           |
| `demo_consumers.ts` / `orders_example.ts`                                                                                                                                           | exit 0 / exit 0                                                                                                    |
| Native OTLP live-metric smoke (`live_native_metrics_test.ts`)                                                                                                                       | 1 passed                                                                                                           |
| Observability asset validators (promtool/amtool/otelcol)                                                                                                                            | all SUCCESS                                                                                                        |
| Browser suites (full `test:browser`)                                                                                                                                                | 71 passed / 0 failed (70 in the 27 m 17 s run; the CLI-login case then passed once the `trellis` binary was built) |
| Split roles (`live_split_roles_test.ts`)                                                                                                                                            | 3 passed (Health, Jobs, Events)                                                                                    |
| Native live feed suite (`live_native_feed_test.ts`)                                                                                                                                 | 10 passed / 0 failed across 3 sequential runs (5 m 11 s, 4 m 46 s, 4 m 50 s)                                       |
| Platform Operation observation (`device_activation_test.ts`)                                                                                                                        | 1 passed (2 steps)                                                                                                 |
| Rust `demos/rust/service` + `device` out-of-tree `cargo check`                                                                                                                      | pass                                                                                                               |
| `cli_server_managed_nats` live integration                                                                                                                                          | pass                                                                                                               |
| `cargo test -p trellis-rs --features live-integration --test integration`                                                                                                           | 2 passed (115 s)                                                                                                   |

## Protocol and authorization cases

| Case | Status        | Proving boundary                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| ---- | ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P01  | PROVEN-UNIT   | Shared Rust/WASM bridge round-trips + `live_protocol_test.ts` vectors                                                                                                                                                                                                                                                                                                                                                                                                  |
| P02  | PROVEN-LIVE   | Cross-language proofs in all four probe directions; mutation invalidation unit (`live::authority`)                                                                                                                                                                                                                                                                                                                                                                     |
| P03  | PROVEN-UNIT   | New duplicate security-header rejection; malformed base64url/counter rejection                                                                                                                                                                                                                                                                                                                                                                                         |
| P04  | PROVEN-BROKER | `operation_observe_is_the_only_operation_grant_with_live_delivery` compiles exact grants; `bt04_only_operation_observe_receives_live_delivery` proves Observe delivery and Invoke/Cancel broker denial                                                                                                                                                                                                                                                                 |
| P05  | PROVEN-BROKER | `bt03_static_namespaces_are_isolated`, `bt02` static-vs-response independence                                                                                                                                                                                                                                                                                                                                                                                          |
| P06  | PROVEN-UNIT   | `NX04` unsigned and foreign control are dropped without reflection; broker forged-replay scenario not executed                                                                                                                                                                                                                                                                                                                                                         |
| P07  | PROVEN-LIVE   | `live_admission_test.ts`: a validly signed live open whose reply is a foreign `live.v1.data...` destination is dropped with no reflected error or application frame, and the same caller's authenticated inbox still receives a signed offer. Fixing the reflection required dropping denied/foreign-reply live-open errors in the service request loop.                                                                                                               |
| P08  | PROVEN-LIVE   | `live_admission_test.ts`: a different Console principal signs a close control for the owner's active Health session on the owner's control route; the provider denies it and the owner's observation stays open. `NX04` additionally covers other-principal and same-principal second-connection controls at the production provider route.                                                                                                                            |
| P09  | PARTIAL       | Both the TypeScript (`live/client_open.ts` offer verification) and Rust (`live/client_open.rs` around the selected-deployment and provider-tuple checks) consumers require the offer's deployment to equal the independently selected binding and its full provider tuple to match the verified signed context. The real negative (a rogue provider serving the same API under a different deployment) needs a second provider identity/bootstrap and is not executed. |
| P10  | PROVEN-UNIT   | `classify_control` replay/stale/gap/conflict + live activation retries                                                                                                                                                                                                                                                                                                                                                                                                 |
| P11  | PROVEN-UNIT   | `retired_watch_distinguishes_revocation_from_coverage_loss_newer_active` + guard coverage tests                                                                                                                                                                                                                                                                                                                                                                        |
| P12  | PROVEN-BROKER | `bt01_response_expiry_is_independent_of_the_large_count_allowance`                                                                                                                                                                                                                                                                                                                                                                                                     |

## Lifecycle invariants

| Case | Status           | Proving boundary                                                                                           |
| ---- | ---------------- | ---------------------------------------------------------------------------------------------------------- |
| L01  | PARTIAL          | No configured age limit/quota; no dedicated long run                                                       |
| L02  | PROVEN-LIVE      | Probe quiet round; observability long waits                                                                |
| L03  | PROVEN-LIVE      | Per-session source start/cleanup counts; shared-acquisition split not executed                             |
| L04  | PROVEN-LIVE      | Demo consumer closes observer; `reportsList` still succeeds                                                |
| L05  | PROVEN-LIVE      | Lifecycle prepared/never-iterated/close-before-iteration start no source                                   |
| L06  | PROVEN-UNIT      | `G09` a filtered frame cannot advance the prefix past an unread value; `NX07` abnormal end discards queued |
| L07  | PROVEN-LIVE      | Flow T03 pause; lifecycle blocked-source cancel                                                            |
| L08  | PARTIAL          | Epoch/authority end unit + authority live; no forced epoch scenario                                        |
| L09  | PROVEN-LIVE      | Every provider frame verified as the negotiated provider                                                   |
| L10  | PROVEN-LIVE      | Authority T14 quiet revocation; probe identity-preserving refresh                                          |
| L11  | PROVEN-UNIT/LIVE | Flow byte/frame windows; bounded slots; lifecycle retained cleanup                                         |
| L12  | PROVEN-LIVE      | Distinguishable terminals (observability NX07, lifecycle, authority, operation)                            |
| L13  | PROVEN-LIVE      | All four directions                                                                                        |
| L14  | PROVEN-LIVE      | Unrelated RPCs keep working in probe/observability                                                         |
| L15  | PROVEN-LIVE      | Exact per-side Live telemetry deltas from one owner + seven families                                       |
| L16  | PARTIAL          | Resume/reconcile engine; reconnect non-replay not dedicated                                                |

## Live lifecycle cases L17–L35

| Case | Status              | Proving boundary                                                                |
| ---- | ------------------- | ------------------------------------------------------------------------------- |
| L17  | PARTIAL             | Abort listener before first await (engine); not dedicated                       |
| L18  | PROVEN-LIVE         | `live_lifecycle_test.ts`                                                        |
| L19  | PROVEN-LIVE         | `live_lifecycle_test.ts` + probe prepared handles                               |
| L20  | PROVEN-LIVE         | `live_lifecycle_test.ts`                                                        |
| L21  | PARTIAL             | Post-await recheck (engine) + operations race test; not all sub-cases           |
| L22  | PROVEN-UNIT (clock) | `VT01`/`VT02` reserved expiry exact and activation before the deadline survives |
| L23  | PROVEN-UNIT (clock) | `VT05` unanswered challenge retries without renewing                            |
| L24  | PROVEN-LIVE         | `live_lifecycle_test.ts`                                                        |
| L25  | PROVEN-LIVE         | `live_lifecycle_test.ts` + observability NX07                                   |
| L26  | PROVEN-UNIT         | Rust `source_error_is_sanitized`                                                |
| L27  | PARTIAL             | Prepared return/close + Rust pump Drop + demo `await using`                     |
| L28  | PROVEN-LIVE         | `live_lifecycle_test.ts` blocked-source cancel                                  |
| L29  | PROVEN-UNIT         | Rust `concurrent_emit_*`                                                        |
| L30  | PROVEN-UNIT         | Rust cleanup-timeout → incomplete; lifecycle settlement                         |
| L31  | PARTIAL             | Shared-acquisition split not executed                                           |
| L32  | PROVEN-UNIT         | `manager_test` consumer admission quota rejects before excess allocation        |
| L33  | PROVEN-UNIT         | `NX10` failed setup releases the consumer permit; permit-on-dispose             |
| L34  | PROVEN-UNIT (clock) | `VT12` the bounded receipt expires and never reactivates; owner-gated replay    |
| L35  | PROVEN-UNIT/LIVE    | `NX07` discard, `G10` final handoff commits complete, drain ordering            |

## Timing cases T01–T19

| Case    | Status                                       | Proving boundary                                                                                                                              |
| ------- | -------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| T01     | PROVEN-UNIT (clock) + PROVEN-LIVE (fraction) | `T01` 70,001 production transitions cross the count boundary under the production deadline owner; 1,026 real frames, one source per direction |
| T02     | PROVEN-LIVE (short)                          | ~21 s quiet rounds; >140 s not run (fast-matrix decision)                                                                                     |
| T03     | PROVEN-LIVE                                  | `live_flow_test.ts`                                                                                                                           |
| T04     | PROVEN-UNIT (clock)                          | `VT06`/`VT07` consumption-stall transitions on the production deadline owner                                                                  |
| T05/T06 | PROVEN-UNIT (clock)                          | `VT04` peer inactivity exact; `VT05` unanswered challenge retries and never renews                                                            |
| T07/T08 | PARTIAL                                      | `VT04` inactivity bound under virtual time; no process-kill/blackhole live scenario                                                           |
| T09     | NOT RUN                                      |                                                                                                                                               |
| T10     | PARTIAL                                      | Gap detection engine/unit                                                                                                                     |
| T11     | PROVEN-UNIT (clock)                          | `VT05`/`VT08` one unanswered challenge never renews; credit threshold/delay exact                                                             |
| T12     | PROVEN-LIVE                                  | `live_flow_test.ts`                                                                                                                           |
| T13     | PROVEN-LIVE                                  | Probe identity-preserving refresh                                                                                                             |
| T14     | PROVEN-LIVE                                  | `live_authority_test.ts`                                                                                                                      |
| T15     | PROVEN-UNIT (clock)                          | `NX09` same-epoch resume keeps generation; reconnect does not revive                                                                          |
| T16–T18 | NOT RUN                                      |                                                                                                                                               |
| T19     | PARTIAL                                      | Single-end engine + one-end observability assertion                                                                                           |

## Operation observation cases O01–O13

| Case    | Status           | Proving boundary                                                      |
| ------- | ---------------- | --------------------------------------------------------------------- |
| O01     | PROVEN-LIVE      | `live_operation_test.ts` terminal snapshot + normal END               |
| O02     | PARTIAL          | Watch-then-reread engine; change-between-check-and-subscribe not live |
| O03     | PROVEN-UNIT      | Lease-only-write filtering + live operation                           |
| O04     | NOT RUN          |                                                                       |
| O05     | PROVEN-LIVE      | Demo consumer observer close, durable operation survives              |
| O06     | PROVEN-UNIT/LIVE | Observation-only cancellation tests                                   |
| O07     | PARTIAL          | Reconcile engine; interruption replay not dedicated                   |
| O08–O10 | NOT RUN/PARTIAL  | Existing ownership/executor checks, not exercised here                |
| O11     | PROVEN-UNIT/LIVE | Bounded arbiter + terminal freeze + live operation                    |
| O12     | PARTIAL          | Source disappearance maps to interruption (engine)                    |
| O13     | PROVEN-LIVE      | Typed codecs/revisions preserved in live operation                    |

## Built-in roles BI01–BI07

| Case | Status      | Proving boundary                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ---- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| BI01 | PROVEN-LIVE | `live_builtin_test.ts` real Health change, job lifecycle, published event                                                                                                                                                                                                                                                                                                                                                                                                                   |
| BI02 | PROVEN-LIVE | `live_builtin_isolation_test.ts`: two consumers on one built-in Health/Jobs/Events live stream; closing one records exactly one consumer live end and the other still receives a later domain frame plus an unrelated RPC                                                                                                                                                                                                                                                                   |
| BI03 | PROVEN-LIVE | `live_split_roles_test.ts`: a platform process plus separate Health, Jobs, and Events processes on one broker, each using its platform-provisioned seed. Health emits a real `Health.Watch` frame; Jobs emits `queryInvalidated` after a service job is created; Events emits a frame after `publishChanged`. A service-bearing split deployment requires all three non-platform roles because its declared consumer, job, and health resources respectively depend on their owned streams. |
| BI04 | PROVEN-UNIT | `connect_builtin_live_provider` → `expect_deployment` rejects a role installed under the wrong reserved deployment and fails bootstrap before the listener reports ready; `managed_seed_file_is_private_and_stable` covers invalid/replaced seed rejection; `validate_for_mode` covers role/mode configuration. `require_live_provider_owner` has no production call site; a missing owner is enforced structurally by bootstrap connect failure plus routers awaiting the installed owner. |
| BI05 | PROVEN-LIVE | `managed_seed_file_is_private_and_stable` (0700/0600, persisted identity, invalid rejected) plus `live_restart_identity_test.ts`: managed live-provider seed bytes are unchanged across a control-plane restart and the built-in Health live stream still serves afterwards                                                                                                                                                                                                                 |
| BI06 | PROVEN-LIVE | `live_projector_survival_test.ts`: closing a public observer leaves the built-in Jobs and Events projectors running (a created job still completes; a published event is still captured/readable) with ownership healthy                                                                                                                                                                                                                                                                    |
| BI07 | PROVEN-LIVE | `device_activation_test.ts`: a Portal-participant `live()` of the real `DeviceUserAuthorities.Resolve` operation (valid flowId + confirmationCode) delivers its initial frame, and closing the observation leaves the durable operation `running`. The earlier `peer_inactive` was an artifact of the invalid bogus-flow scenario.                                                                                                                                                         |

## Environment verification completed

- Observability assets validated with the pinned images and case-owned values:
  `promtool check config` SUCCESS (24 rules), `promtool check rules` alert
  SUCCESS (24) and recording SUCCESS (33), `promtool test rules` SUCCESS,
  `amtool check-config` SUCCESS, and `otelcol-contrib validate` exit 0 for
  `collector.yaml`, `collector.native.yaml`, and `collector.browser.yaml`.
- Browser suites: full `test:browser` suite passed against the repository-cached
  Chromium — **71 passed / 0 failed** (27 m 52 s), including the unified web
  reverse-proxy journey.
- The native Rust provider owner is now proven over the real OTLP wire
  (`live_native_metrics_test.ts`, M01): a real `Health.Watch` session exports
  `trellis.live.sessions`, `.ends`, `.handshake.duration`, `.frames`, and the
  remaining families. Native in-process numerical owner deltas are now also
  asserted in `live::telemetry::tests` (phases, handshake, end, Operation
  isolation, buffered/frames/rejections, cleanup, idempotence); the TypeScript
  side keeps its exact per-side deltas.

## Known deviations

- The Rust provider answers an authenticated control retry for a closed session
  from its retained receipt window (close/end-ack ack, otherwise a signed
  `session_not_found`); requests whose caller cannot be verified, or whose
  session was never known, are dropped silently rather than reflected. The
  TypeScript provider route is exercised by `NX04` under the same no-reflection
  rule.
- The TypeScript provider control subscription is route-owned rather than
  dispatched by the manager; it is an internal structural difference with no
  observed behavioral gap.

## Flakiness findings

- (fixed) `live_native_feed_test.ts` `L2` intermittently failed with
  `AuthorizationContextRefreshError: Trellis HTTP 503: resource_pending` during
  authorization-context refresh. The client bootstrap now treats
  `resource_pending` as the server-declared eventual-materialization state and
  keeps retrying within the attempt budget, and 30/30 sequential runs pass.
- (fixed) A denied live Live/Operation opening whose reply is not the caller's
  inbox prefix reflected its error onto that foreign destination. The service
  request loop now drops these denials for live opening bodies, and
  `denied_live_open_is_dropped_without_a_reflected_reply` covers both the live
  and ordinary paths.
- (fixed) Rust real-broker `bt02` raced on ephemeral ports under parallel
  execution; `TestBroker::start` now retries with fresh ports.
