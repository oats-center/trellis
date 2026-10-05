# trellis-testkit

Isolated, live Trellis runtimes for Rust integration tests.

`trellis-testkit` links and runs the real production Trellis runtime in-process,
with a private sandbox and a real managed `nats-server` child, and gives your
test the URLs and identities it needs to exercise your generated service/app
facades against a real server.

It is a **development dependency**. Add it to your test crate:

```toml
[dev-dependencies]
trellis-testkit = "0.100.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## What it needs

- No Trellis executables or source checkout. Cargo resolves the linked runtime
  with the testkit; implementation dependencies are distribution-only crates,
  not supported service-author APIs.
- A real NATS executable. By default the bare builder uses the verified pinned
  download (network required on first use). To use a prebuilt broker, set
  `TRELLIS_TEST_NATS_BIN`, choose `NatsSource::Path`, or choose
  `NatsSource::PathLookup` for `PATH` discovery.

Ports are chosen automatically from kernel-assigned loopback reservations; you
never select them.

A runtime accepts only its own loopback origin by default. If a browser app or
operator console is served from a different development origin, allow it with
`TrellisTestRuntime::builder().extra_origin("http://localhost:5174")` (or
`extra_origins(vec![...])`). Each origin is added to the runtime's accepted
request origins and insecure-origin allow-list, so that app can complete a
portal login against the harness runtime.

## Example

```rust,no_run
use trellis_testkit::TrellisTestRuntime;
use my_contract::participants::my_provider::{Participant as Provider, Provider as ProviderApi};
use my_contract::participants::my_caller::{Client as CallerClient, Participant as Caller};

# async fn example() -> Result<(), trellis_testkit::TrellisTestError> {
let mut runtime = TrellisTestRuntime::builder().start().await?;

let provider_identity = runtime.register_service::<Provider>("provider").await?;
let caller_identity = runtime.register_client::<Caller>("caller").await?;

let mut service = Provider::connect(provider_identity.connect_options()).await.unwrap();
// Register generated handlers on `ProviderApi::new(&mut service)` here.
let service_task = tokio::spawn(async move { service.run().await });

let caller = CallerClient::connect(caller_identity.connect_options()).await.unwrap();
// Call your generated RPCs with `caller`.

service_task.abort();
runtime.shutdown().await?;
# Ok(())
# }
```

Your application owns the clients and services it connects; stop those
transports before calling [`TrellisTestRuntime::shutdown`]. The runtime owns
only its own administration connection, production runtime task, and NATS
process.

## Browser and portal-driven tests

The runtime exposes the isolated administrator credentials it bootstrapped:
`TrellisTestRuntime::admin_username()` and `admin_password()`. Use them to drive
a real browser or portal login against `trellis_url()`. The administrator is
already seeded, so this is normal login rather than first-run setup. Both values
are sandbox-only secrets; never log them or include them in uploaded evidence.

You can also pin them so the test knows them up front:

```rust,no_run
# use trellis_testkit::TrellisTestRuntime;
# async fn example() -> Result<(), trellis_testkit::TrellisTestError> {
let mut runtime = TrellisTestRuntime::builder()
    .admin_username("trellis-testkit-admin")
    .admin_password("a-known-test-password")
    .start()
    .await?;
# runtime.shutdown().await?;
# Ok(())
# }
```

If a browser app or operator console is served from a different origin, allow it
with `extra_origin` so its portal login is accepted.

## Participants without a generated Rust package

`register_service`/`register_client` install a participant through its generated
Rust `ParticipantDescriptor`. A participant that ships only a TypeScript package
(for example a first-party operator app) needs a small Rust projection. Generate
one from the same contract — set `[generate.rust].output` in the contract's
`trellis.toml` and run `trellis generate` — then depend on the generated
`participants::<name>` module and pass its `Participant` to
`register_client::<...>` or `register_service::<...>`. Do not hand-author the
descriptor or its package evidence.

## What it does not do

- It does not start a separate `trellis-server` process. The runtime is the same
  production library the server uses, resolved and compiled by Cargo.
- It does not simulate authorization. `register_client` runs a real
  participant-bound local login; denied calls are denied by the real server.
- It does not cover device activation or the full administrator surface in this
  release. `Device` participants are rejected by `register_service` /
  `register_client`.
- It does not promise cleanup after machine power loss or an uncatchable parent
  termination. Explicit `shutdown`, `Drop`, cancellation, and panic cleanup are
  the tested guarantees.

## Isolation and retention

Each runtime creates a fresh `trellis-testkit-<id>` sandbox with its own home,
config, data, cache, and logs, and binds only loopback addresses. It never
mutates the calling process's environment. Retention is `OnFailure` by default
(keep the sandbox after a failure); `Always` and `Never` are available through
`TrellisTestRuntime::builder().retention(...)`.

Call `TrellisTestRuntime::shutdown` to remove a sandbox according to the
retention policy. `Drop` and process termination are best-effort: a sandbox can
survive if the process exits or is killed before cleanup finishes, and
`OnFailure` intentionally keeps failed-run sandboxes as evidence. Reclaim
leftovers with `trellis_testkit::remove_retained_workdirs(parent, older_than)`,
which only removes sandboxes carrying this harness's ownership marker.

## Supported platforms

Linux on the existing x86_64 and aarch64 release targets. The crate compiles on
other platforms where practical and fails at `start` with a clear
`UnsupportedPlatform` error rather than a link error.
