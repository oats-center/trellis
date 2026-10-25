# trellis-testkit

Isolated, live Trellis runtimes for Rust integration tests.

`trellis-testkit` runs production `trellis-server` and `nats-server` processes
with a private sandbox, and gives your
test the URLs and identities it needs to exercise your generated service/app
facades against a real server.

It is a **development dependency**. Add it to your test crate:

```toml
[dev-dependencies]
trellis-testkit = "0.100.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## What it needs

- No source checkout or compiler/runtime Cargo dependencies. The default
  downloads the latest Trellis release from GitHub (the newest stable release,
  or the newest prerelease before the first stable one), or the version in
  `TRELLIS_TEST_VERSION`, and checks it against the checksum published with that
  release (currently Linux x86-64). Each run reports the `trellis-server`
  version it tests against. Network is needed on first use; the latest release
  is rechecked hourly and cached archives work offline. Set both
  `TRELLIS_TEST_CLI_BIN` and `TRELLIS_TEST_SERVER_BIN`, or use
  `.native(NativeSource::Path { cli, server })`, for explicit binaries; invalid
  explicit paths never trigger a download.
- A real NATS executable. By default the bare builder uses the verified pinned
  download (network required on first use). To use a prebuilt broker, set
  `TRELLIS_TEST_NATS_BIN`, choose `NatsSource::Path`, or choose
  `NatsSource::PathLookup` for `PATH` discovery.

`TRELLIS_TEST_CACHE_DIR` selects the owned private executable/archive cache;
otherwise the platform cache is used. Only verified artifacts are shared, never
credentials, broker state, databases, or logs. `trellis init config` and
`trellis init admin --password-stdin` perform normal installation/bootstrap;
the administrator password is not passed in process arguments.
Hosts default to two Tokio worker threads, matching the Node testkit;
an explicit `TOKIO_WORKER_THREADS` setting is retained.

Ports are reserved automatically outside Linux's outbound ephemeral range; you
never select them. The kernel enforces each reservation until startup, and
outbound bootstrap connections cannot claim its port after release.

The application listener's extra development origins can be configured with
`TrellisTestRuntime::builder().extra_origin("http://localhost:5174")` (or
`extra_origins(vec![...])`). This does not register an OAuth client or grant
authority. OAuth and portal requests use the separate `oauth_origin()`;
browser clients need ordinary registered callback metadata and user consent.

## Example

```rust,no_run
use trellis_testkit::TrellisTestRuntime;
use my_contract::participants::my_provider::{Participant as Provider, Provider as ProviderApi};
use my_contract::apps::my_caller::{Client as CallerClient, Request as Caller};

# async fn example() -> Result<(), trellis_testkit::TrellisTestError> {
let mut runtime = TrellisTestRuntime::builder().start().await?;

let provider_identity = runtime.register_service::<Provider>("provider").await?;
let caller_identity = runtime.register_client::<Caller>("caller").await?;

let mut service = Provider::connect(provider_identity.connect_options()).await.unwrap();
// Register generated handlers on `ProviderApi::new(&mut service)` here.
let service_task = tokio::spawn(async move { service.run().await });

let connected = trellis_rs::generated::Client::connect_user(caller_identity.connect_options()).await.unwrap();
let caller = CallerClient::from_generated(connected);
// Call your generated RPCs with `caller`.

service_task.abort();
runtime.shutdown().await?;
# Ok(())
# }
```

Your application owns the clients and services it connects; stop those
transports before calling [`TrellisTestRuntime::shutdown`]. The runtime owns
only its own administration connection and production server/broker processes.
Cancellation and drop kill and reap owned children before sandbox cleanup.

For persistence acceptance, `runtime.restart().await?` stops and starts the
ordinary runtime against the same data and still-running NATS broker. It does
not reseed identities or credentials. Close application clients first, then
reconnect them after restart and verify their stored values through public APIs.

## Browser and portal-driven tests

### Distinct ordinary users

```rust,no_run
# use trellis_testkit::TrellisTestRuntime;
# use my_contract::apps::my_caller::Request;
# use trellis_rs::generated::AppRequestDescriptor;
# async fn users() -> Result<(), trellis_testkit::TrellisTestError> {
let mut runtime = TrellisTestRuntime::builder().start().await?;
let alice = runtime.create_user("Alice").await?;
let bob = runtime.create_user("Bob").await?;
for user in [&alice, &bob] {
    runtime.grant_user_capabilities(user, Request::REQUIRED_CAPABILITIES).await?;
}
let app = runtime.register_native_application::<Request>("shared app").await?;
let a1 = runtime.login_native_application_as(&app, &alice).await?;
let b1 = runtime.login_native_application_as(&app, &bob).await?;
let a2 = runtime.login_native_application_as(&app, &alice).await?;
// Each identity supplies connect_options() once to the generated client.
# runtime.shutdown().await?;
# Ok(())
# }
```

`TestRegisteredNativeClient<P>` retains the exact generated request descriptor;
all logins reuse its client ID but have separate credential scopes and DPoP keys.
`TestUserIdentity` exposes its label, username, and real principal ID, never its
password. Handles belong to their runtime. Explicit-user login never grants or
restores capabilities. `register_client_as::<P>` composes registration and user
login; `register_client::<P>` preserves the seeded-administrator convenience.

### Seeded administrator

The runtime exposes the isolated administrator credentials it bootstrapped:
`TrellisTestRuntime::admin_username()` and `admin_password()`. Use them to drive
a real browser or portal login against `oauth_origin()`. The administrator is
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

Browser authorization uses registered callback metadata; an `extra_origin`
entry does not implicitly trust or register an OAuth client.

## Generated service and application descriptors

`register_service` reviews and accepts implemented and consumed APIs (required
and optional selections) from generated package evidence before creating a
deployment. Consumers can be registered before their providers. It provisions its immutable public identity through the
ordinary generated Auth RPCs. `register_client` takes an `AppRequestDescriptor`,
registers a native OAuth client, and completes Device Code, portal login, and
explicit user consent. OAuth custody is exclusive: `connect_options()` consumes
the client identity. Neither flow uses participant-bound browser bind or portal
overrides. There is no separate participant installation endpoint.

Source that ships only a TypeScript package needs a Rust projection. Generate
one from the same contract — set `[generate.rust].output` in the contract's
`trellis.toml` and run `trellis generate` — then depend on the generated
`participants::<name>` module and pass its `Participant` to
`register_service::<...>`, or use `apps::<name>::Request` with
`register_client::<...>`. Do not hand-author the
descriptor or its package evidence.

## What it does not do

- It does not embed the server or publish server/compiler implementation crates.
- It does not simulate authorization. `register_client` runs real native OAuth
  and portal consent; denied calls are denied by the real server.
- Browser application requests are rejected by the native caller helper. Drive
  their ordinary browser authorization flow against `oauth_origin()` instead.
- It does not cover device activation or the full administrator surface in this
  release. `Device` participants are rejected by `register_service` /
  `register_client`.
- It does not promise cleanup after machine power loss or an uncatchable parent
  termination. Explicit `shutdown`, `Drop`, cancellation, and panic cleanup are
  the tested guarantees.

## Isolation and retention

Each runtime creates a fresh `trellis-testkit-<id>` sandbox with its own home,
config, data, credentials, broker state, PID files, and logs, and binds only
loopback addresses. It never mutates the calling process's environment.
Retention is `OnFailure` by default (keep the sandbox after a failure); `Always`
and `Never` are available through
`TrellisTestRuntime::builder().retention(...)`.

Only verified native executables and the archive cache are shared:
`TRELLIS_CACHE_DIR` selects it, otherwise the production downloader uses the
platform cache directory. A warmed cache permits subsequent runtimes to start
without network access. Keep any override outside runtime sandboxes so sandbox
removal does not delete it.

Call `TrellisTestRuntime::shutdown` to remove a sandbox according to the
retention policy. An unexpected nonzero or signal exit, even before shutdown
starts, fails shutdown and retains `OnFailure` diagnostics. Startup failures
already reported by readiness are not reported again during cleanup.
`Drop` and process termination are best-effort: a sandbox can
survive if the process exits or is killed before cleanup finishes, and
`OnFailure` intentionally keeps failed-run sandboxes as evidence. Reclaim
leftovers with `trellis_testkit::remove_retained_workdirs(parent, older_than)`,
which only removes sandboxes carrying this harness's ownership marker.

## Supported platforms

Linux on the existing x86_64 and aarch64 release targets. The crate compiles on
other platforms where practical and fails at `start` with a clear
`UnsupportedPlatform` error rather than a link error.
