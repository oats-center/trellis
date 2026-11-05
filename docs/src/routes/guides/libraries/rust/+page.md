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
run `trellis install` and `trellis generate`. The generated crate takes the
package's name, so a package named `acme-orders` with `output = "trellis"` is
used as:

```toml
[dependencies]
acme-orders = { path = "trellis" }
trellis-rs = "0.100.0"
tokio = { version = "1", features = ["full"] }
```

Use the `trellis-rs` version the generated `trellis/Cargo.toml` names. Each
participant becomes a module under `participants` (services and devices) or
`apps` (apps and agents), and each API becomes a module under `apis`. Module
names join the package, the participant or API, and its major version:
`participants::acme_orders_orders_service`, `apps::acme_orders_orders_caller`,
`apis::acme_orders_orders_v1`. Check the generated crate for the exact names.

The generated package embeds exact participant/package evidence. Callers never
supply participant IDs, semantic digests, deployments, instances, grants,
resources, or transport endpoints as bootstrap assertions.

## Connect a service

A service first discovers the installation, which gives it the broker addresses
and the installation's signing trust, then connects with the identity seed that
`trellis svc <deployment> provision` printed. The examples use this contract,
packaged as `acme-orders`:

```trellis
model CreateOrderRequest { customerId: string; }
model CreateOrderResponse { orderId: string; customerId: string; }
model GetOrderRequest { orderId: string; }

api orders@v1 {
  title "Orders";
  description "Orders for customers.";
  rpc Create { input CreateOrderRequest; output CreateOrderResponse; }
  rpc Get { input GetOrderRequest; output CreateOrderResponse; }
  capabilities {
    capability orders.create {
      title "Create orders";
      description "Create and read orders.";
      grants "The caller may create and read customer orders.";
      allows { rpc Create; rpc Get; }
    }
  }
}

service OrdersService { implements orders; }
agent OrdersCaller { use orders { required capability orders.create; } }
device InspectionDevice { use orders { required capability orders.create; } }
```

```rust
use acme_orders::participants::acme_orders_orders_service::{Participant, Provider};
use acme_orders::types::CreateOrderResponse;
use trellis_rs::auth::{discover_accepting_certificate, CertificateAcceptance};
use trellis_rs::service::ServiceConnectOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let trellis_url = std::env::var("TRELLIS_URL")?;
    let identity_seed = std::env::var("TRELLIS_IDENTITY_SEED")?;
    let server = discover_accepting_certificate(
        &trellis_url,
        &CertificateAcceptance::for_program("orders-service"),
    )
    .await?;
    let mut service = Participant::connect(
        ServiceConnectOptions::new(server.nats_servers, server.trust, &identity_seed)
            .with_name("orders-service")
            .with_timeout_ms(10_000),
    )
    .await?;

    Provider::new(&mut service)
        .acme_orders_orders_v1()
        .register_create(|_context, input| async move {
            Ok(CreateOrderResponse {
                order_id: ulid::Ulid::new().to_string(),
                customer_id: input.customer_id,
                extra: Default::default(),
            })
        });
    service.run().await?;
    Ok(())
}
```

`TRELLIS_URL` is the Trellis address, such as `http://localhost:3000` for a
development installation. A certificate the platform trust store vouches for
needs nothing. A development installation's own certificate is accepted by its
fingerprint: set `TRELLIS_CERTIFICATE_FINGERPRINT`, or
`TRELLIS_ACCEPT_SELF_SIGNED=1` to accept it on first contact. If you have the
CA file, `discover_native_oauth(&trellis_url, &[ca_path])` trusts it directly.

Register every handler before `service.run()`, which runs the service until it
stops or fails. Every generated model has an `extra` map for fields it doesn't
know, so construct it with `extra: Default::default()`.

NATS CONNECT carries a signed provisioned admission that proves the identity
and a fresh session key. Auth Callout resolves the deployment, installed
participant, and resources, then returns signed session authority and exact
transport permissions, which the runtime verifies. Authorization state remains
in process memory and is refreshed proactively as SDK-owned maintenance.
Renewing unchanged transport authority promotes the new session authority in
place without replacing a healthy socket. Newly approved transport authority is
adopted automatically, with no manual refresh API or upgrade notification. There
is no authorization-context file.

## Features

`trellis-rs` enables `native-oauth` and `device` by default. Discovery
(`discover_accepting_certificate` and `discover_native_oauth`) is part of
`native-oauth`, so services keep it. A service can drop `device` (device
enrollment and activation):

```toml
[dependencies]
trellis-rs = { version = "0.100.0", default-features = false, features = ["native-oauth"] }
```

The default build includes prepared events, common inbox/outbox interfaces, and
memory stores, but no SQL database drivers. Enable `event-store-sqlite` for
`SqliteInboxStore`/`SqliteOutboxStore` or `event-store-postgres` for
`PostgresInboxStore`/`PostgresOutboxStore`. These also expose the corresponding
database-specific `EventStoreError` variants. Services using only RPCs, Live,
events, or runtime resources need neither.

SDK HTTP connections and optional `telemetry-otlp` exporters preserve an
application-installed Rustls provider. Otherwise they use ring locally without
changing the process-wide provider, with platform certificate verification.

## Connect an app or agent

Native apps and agents sign the user in with OAuth Device Code: discover the
installation, open durable credential custody for a registered native client,
show the user the link and code, and connect with the resulting session.
`client_id` is a native client registered with `trellis clients`.

```rust
use acme_orders::apps::acme_orders_orders_caller::{Client, Request};
use acme_orders::types::GetOrderRequest;
use std::path::PathBuf;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_rs::generated::AppRequestDescriptor as _;

let server = discover_native_oauth("https://trellis.example.com", &[]).await?;
let scope = NativeCredentialScope {
    issuer: server.issuer.clone(),
    instance_id: server.trust.instance_id.clone(),
    account: server.trust.account.clone(),
    client_id: "orders-agent".into(),
    credential_handle: ulid::Ulid::new(),
};
let login = NativeOAuthClient::open(PathBuf::from("orders-agent-login"), scope, server.trust.clone())
    .await?;
let device = login
    .start_device(&serde_json::json!([{
        "type": "trellis_capabilities",
        "required": Request::REQUIRED_CAPABILITIES,
        "optional": Request::OPTIONAL_CAPABILITIES,
    }]))
    .await?;
// The complete URI already carries the code; the page shows it to compare.
let link = device.verification_uri_complete().unwrap_or(device.verification_uri());
println!("Open {link} and check the code is {}", device.user_code());
let session = device.complete().await?;

let client = Client::from_generated(
    trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
        server.nats_servers,
        30_000,
        session,
    ))
    .await?,
);
```

Keep `scope` (it holds no secret) to sign in again later: after a restart,
`NativeOAuthClient::open` with the same scope and then `resume()` rotates the
retained credential for a fresh runtime key instead of asking the user again.
The connection presents the signed OAuth admission in CONNECT and then
retrieves its signed authority. Do not assemble route credentials, NATS
permissions, or semantic evidence manually.

## Connect a device

Activated devices retain only their durable identity material:

```rust
use acme_orders::participants::acme_orders_inspection_device::{Client, Participant};
use trellis_rs::auth::discover_native_oauth;
use trellis_rs::client::DeviceConnectOptions;

let trellis_url = "https://trellis.example.com";
let server = discover_native_oauth(trellis_url, &[]).await?;
let device = Client::connect(
    DeviceConnectOptions::<Participant>::new(
        trellis_url,
        &identity_seed_base64url,
        server.nats_servers,
        server.trust,
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

A connected client has one accessor per API it uses, and one method per RPC:

```rust
let order = client
    .acme_orders_orders_v1()
    .get(&GetOrderRequest {
        order_id: "ord_123".into(),
        extra: Default::default(),
    })
    .await?;
```

A device calls the same way through `device.acme_orders_orders_v1()`, and a
service through `Provider::new(&mut service).client()`. Generated declared
errors are typed variants. Unknown remote and transport failures remain
distinct call-error variants.

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
[`trellis-testkit` Rustdoc](https://docs.rs/trellis-testkit).

The harness downloads the latest Trellis release (the newest stable release, or
the newest prerelease before the first stable one) or `TRELLIS_TEST_VERSION`,
verified against the checksum published with it, plus a pinned NATS archive. It
reports the `trellis-server` version it tests against. Set `TRELLIS_TEST_SERVER_BIN`, or select `NativeSource::Path { server }`, to
use explicit binaries without download fallback. Only verified artifact
cache is shared (`TRELLIS_TEST_CACHE_DIR`, or the platform cache directory); a warmed
cache works offline. Credentials, broker state, databases, PID files, and logs
remain sandbox-local. Server-only restart retains the same running broker;
shutdown and cancellation reap owned children before sandbox cleanup.
