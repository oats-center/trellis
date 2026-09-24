# trellis-test

Isolated, live Trellis runtimes for Rust integration tests.

`trellis-test` starts normal production Trellis executables — the released
`trellis` CLI and `trellis-server` — out of process, in a private sandbox, and
gives your test the URLs and identities it needs to exercise your generated
service/app facades against a real server.

It is a **development dependency**. Add it to your test crate:

```toml
[dev-dependencies]
trellis-test = "0.100.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## What it needs

- A matching `trellis` CLI and `trellis-server` executable. Provide them with
  `TrellisTestRuntime::builder().cli_binary(...).server_binary(...)`, or through
  the `TRELLIS_TEST_CLI_BIN` / `TRELLIS_TEST_SERVER_BIN` environment variables.
  Versions must match the `trellis-test` version exactly (build metadata is
  ignored).
- A NATS executable. By default it is resolved from `PATH`; set
  `TRELLIS_TEST_NATS_BIN`, choose `NatsSource::Path`, or explicitly request the
  server's verified pinned download with `NatsSource::DownloadPinned`.

Ports are chosen automatically from kernel-assigned loopback reservations; you
never select them.

## Example

```rust,no_run
use trellis_test::TrellisTestRuntime;
use my_contract::participants::my_provider::{Participant as Provider, Provider as ProviderApi};
use my_contract::participants::my_caller::{Client as CallerClient, Participant as Caller};

# async fn example() -> Result<(), trellis_test::TrellisTestError> {
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
transports before calling [`TrellisTestRuntime::shutdown`]. The runtime owns only
its own administration connection and the server/NATS processes.

## What it does not do

- It does not embed a Trellis server, and it does not compile Trellis from
  source. It has no dependency on private Trellis implementation crates.
- It does not simulate authorization. `register_client` runs a real
  participant-bound local login; denied calls are denied by the real server.
- It does not cover device activation or the full administrator surface in this
  release. `Device` participants are rejected by `register_service` /
  `register_client`.
- It does not promise cleanup after machine power loss or an uncatchable parent
  termination. Explicit `shutdown`, `Drop`, cancellation, and panic cleanup are
  the tested guarantees. On Linux a parent-death signal is applied to the owned
  server.

## Isolation and retention

Each runtime creates a fresh `trellis-test-<id>` sandbox with its own home,
config, data, cache, and logs, and binds only loopback addresses. It never
mutates the calling process's environment. Retention is `OnFailure` by default
(keep the sandbox after a failure); `Always` and `Never` are available through
`TrellisTestRuntime::builder().retention(...)`.

## Supported platforms

Linux and macOS on the existing x86_64/aarch64 release targets. Unsupported
platforms fail at `start` with a clear error rather than a link error.
