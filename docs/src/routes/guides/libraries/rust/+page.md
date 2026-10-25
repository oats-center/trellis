---
title: "Rust libraries"
description: "Use Trellis from Rust services, CLIs, devices, and generated Cargo participant facades."
---

The Rust libraries give services, apps, CLIs, devices, and agents a typed async
interface to Trellis. Native generation emits one ordinary crate with:

- `apis::<api>` types, schemas, descriptors, and adapters; and
- `participants::<participant>` facades exposing only selected surfaces.

Verify exact generated names and signatures in the generated crate and the
[Rust API reference](/api).

## Generated package

Set `[package]` identity and `[generate.rust].output` in `trellis.toml`, then
run `trellis generate`.

```toml
[dependencies]
orders-trellis = { path = "trellis" }
```

The generated package embeds exact participant/package evidence. Callers never
supply participant IDs, semantic digests, deployments, instances, grants,
resources, or transport endpoints as bootstrap assertions.

## Connect a service

The default `trellis-rs` build includes prepared events, common inbox/outbox
interfaces, and memory stores, but no SQL database drivers. Enable
`event-store-sqlite` for `SqliteInboxStore`/`SqliteOutboxStore` or
`event-store-postgres` for `PostgresInboxStore`/`PostgresOutboxStore`:

```toml
[dependencies]
trellis-rs = { version = "0.100.0", features = ["event-store-sqlite"] }
```

These additive features also expose the corresponding database-specific
`EventStoreError` variants. Services using only RPCs, Live, events, or runtime
resources do not need either feature.

Two default features cover connections a service does not make:
`native-oauth` (signing in as a user, `UserConnectOptions`, and a device's
companion session) and `device` (device enrollment and activation, which
implies `native-oauth`). A service that connects with its provisioned identity
can turn them off and skip the HTTPS client and P-256 dependencies:

```toml
[dependencies]
trellis-rs = { version = "0.100.0", default-features = false }
```

SDK HTTP connections and optional `telemetry-otlp` exporters preserve an
application-installed Rustls provider. Otherwise they use ring locally without
changing the process-wide provider, with platform certificate verification.

```rust
use orders_trellis::participants::acme_orders_service::{
    connect, ServiceConnectOptions,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity_seed = std::env::var("ORDERS_SERVICE_IDENTITY_SEED")?;
    let mut service = connect(
        ServiceConnectOptions::new("http://localhost:3000", &identity_seed)
            .with_name("orders-service")
            .with_timeout_ms(10_000),
    )
    .await?;

    register_handlers(&mut service);
    service.run().await?;
    Ok(())
}
```

NATS CONNECT carries a signed provisioned admission that proves the identity
and a fresh session key; there is no HTTP bootstrap. Auth Callout resolves the
deployment, installed participant, and resources, then returns signed session
authority and exact transport permissions, which the runtime verifies.
Authorization state remains in process memory and is refreshed proactively as
SDK-owned maintenance. Renewing unchanged transport authority promotes the new
session authority in place without replacing a healthy socket. Newly approved transport authority is
adopted automatically, with no manual refresh API or upgrade notification. There
is no authorization-context file.

## Connect an app or agent

Native apps and agents authorize the user with OAuth Device Code. Discover the
installation, open durable credential custody, and complete the user-facing code
to obtain a `NativeOAuthSession`; a generated user participant wraps the public
`UserConnectOptions` boundary:

```rust
use trellis_rs::auth::{discover_native_oauth, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;

let server = discover_native_oauth("https://auth.example.com").await?;
let client = NativeOAuthClient::open(custody_dir, scope, server.trust.clone()).await?;
let device = client.start_device(&authorization_details).await?;
// The complete URI already carries the code; the page shows it to compare.
let link = device.verification_uri_complete().unwrap_or(device.verification_uri());
println!("Open {link} and check the code is {}", device.user_code());
let session = device.complete().await?;
let options = UserConnectOptions::new(server.nats_servers, 30_000, session);
```

After a restart, `NativeOAuthClient::resume()` rotates the retained credential
for a fresh runtime key instead of repeating consent. The connection presents
the signed OAuth admission in CONNECT and then retrieves its signed authority.
Do not assemble route credentials, NATS permissions, or semantic evidence
manually.

## Connect a device

Activated devices retain only their durable identity material. Generated device
facades use the public `DeviceConnectOptions` shape:

```rust
use orders_trellis::participants::inspection_device::{
    connect, DeviceConnectOptions,
};

let connected = connect(
    DeviceConnectOptions::new(
        "http://127.0.0.1:8080",
        identity_seed_base64url,
    )
    .with_timeout_ms(10_000),
)
.await?;
```

Use `DeviceActivationOptions` with `check_device_activation` or
`wait_for_device_activation` before connecting when review is required. The
helper reuses one attempt identity/session key while polling. Approved devices
connect with a signed provisioned admission in NATS CONNECT; deployment and
instance IDs are resolved server-side.

## Call RPCs

Dependency aliases are the first facade boundary:

```rust
let order = connected
    .client()
    .orders()
    .orders_get(&orders_trellis::apis::acme_orders::OrdersGetRequest {
        order_id: "ord_123".into(),
    })
    .await?;
```

Generated declared errors are typed variants. Unknown remote and transport
failures remain distinct call-error variants.

## Services and resources

Generated service facades expose handler registration plus typed `kv`, `store`,
`jobs`, State, transfer, and Consumer surfaces declared by the participant.
Resources come from the connected runtime. Service code does not import the core
SDK for bootstrap, call `Trellis.Bindings.Get`, construct raw resource handles,
or pass binding payloads into constructors.

An `upload;` Operation handler reads committed bytes with
`operation.open_staged_upload()`. A `stream;` Operation handler starts at once and
takes the caller's live bytes with `operation.take_upload_stream()`, an
`AsyncRead` returned once. It reaches end of file only after Trellis verified the
caller's size and digest and recorded the delivery; a failed transfer is a read
error. A stream cannot be replayed, so an interrupted Operation fails and the
caller starts a new one.

```rust
let mut stream = operation.take_upload_stream().expect("stream Operation");
tokio::io::copy(&mut stream, &mut destination).await?;
let delivered = operation.upload().await?; // verified size and digest
```

Serve generated bytes from any reader whose size and SHA-256 digest you declare:
plan with `context.plan_stream_download_transfer(transfer_id, expires_at, info)`
and start it with `context.handle().spawn_stream_download_endpoint(plan, reader)`.
A reader that ends with different bytes fails the transfer.

Generated cursor-paginated RPCs add lazy `*_pages` and `*_items` fallible
streams. Missing `next_cursor` is the only end signal; general pagination is
keyset-based, not a transaction snapshot.

Optional resource accessors return `Option`. An enabled but unwritten State
resource is still present and `get` returns `None`. State/KV historical
representation handlers migrate values in memory without write-on-read.

App State accessors are asynchronous: `let (state, registration) =
client.saved_resource("0", idempotency_key).await?;`. Accepted historical versions
add a generated `&Migrations` argument. The accessor registers the native-IDL
projected schemas before returning the typed handle. Use `"0"` initially; an
identical registration returns its current revision. Changing the representation
requires an explicitly supplied current registration revision. Handle
`RegistrationConflict` rather than retrying or overwriting automatically; existing
representation versions cannot acquire different schemas. Registration revisions
are distinct from value revisions returned by `get`, `create`, `set`, and `replace`.

`let mut observation = state.watch().await?;` opens the generated certified Live
State observation. `observation.next().await` returns
`Option<Result<Option<StateValue<T>>, StateReadError>>`: the outer `None` ends the
observation, while an inner `None` is an unwritten or deleted value. The initial
snapshot and later updates use the same in-memory migrations as reads and CAS
conflicts. Call `observation.close().await?` for bounded cleanup and
`observation.closed().await` for the Live terminal outcome; dropping also closes.
Watch does not silently reopen after transport or authorization loss.

Generated providers expose `submit_<job>` and `register_<job>` methods. Job
handlers return `Result<JobResult, JobProcessError<E>>`; keyed capacity waits do
not consume retries, while `JobProcessError::retryable(error)` does. See
[Jobs: Rust](/guides/using-jobs-rust).

## Proof and authorization behavior

Requests and events carry context digest, matching redundant session key, and a
message-specific proof. Provider runtimes resolve and cache immutable signed
contexts, recheck explicit revocation, and authorize the exact selected action.

## Automatic transport lifecycle

Keep the generated facade and its typed resource handles across transport
changes. The SDK readies a successor before selecting it for fresh work;
accepted work on a safe predecessor is not restarted merely because of the
handoff. Old connections close when their retained work releases them.
Revocation, reduced authority, physical failure, and hard authorization
deadlines can interrupt affected work.

Still-bound KV and Store handles and retained job references acquire transport
per exchange. Removed resource bindings remain invalid. Jobs and durable event
intake preserve bounded capacity across generations, and acknowledgements and
attempt outcomes use the delivery's receiving connection. Raw NATS access is a
physical connection, not a substitute for these logical, lifecycle-aware
handles.

Fresh application work cannot proceed when own revocation coverage is
unavailable. Bounded acquisitions wait within their budget; immediate
acquisitions report unavailability. Guarded registry maintenance can restore
that coverage on an already admitted safe socket and wake waiting calls. Watch
migration is make-before-break and preserves cached authority and existing
leases; confirmed revocation remains fail-closed.

## Test against a real runtime

For live integration tests, use the published `trellis-testkit` crate. It is a
development dependency that owns production server and NATS processes in a
private sandbox, selects loopback ports automatically, and returns
`connect_options()` values for your generated service and caller participants.
It needs no Trellis source checkout and no private crate patches. See
[Test Trellis services](/guides/testing-trellis-services) and the
[`trellis-testkit` Rustdoc](/api/rust/trellis_testkit/index.html).

The harness acquires exact-version CLI/server and pinned NATS archives with
SHA-256 verification. Set both `TRELLIS_TEST_CLI_BIN` and
`TRELLIS_TEST_SERVER_BIN`, or select `NativeSource::Path { cli, server }`, to
use explicit binaries without download fallback. Only verified artifact
cache is shared (`TRELLIS_TEST_CACHE_DIR`, or the platform cache directory); a warmed
cache works offline. Credentials, broker state, databases, PID files, and logs
remain sandbox-local. Server-only restart retains the same running broker;
shutdown and cancellation reap owned children before sandbox cleanup.
