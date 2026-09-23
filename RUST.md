# Rust

Rust crates for the Trellis platform.

The public Cargo runtime package is `trellis`; projects author APIs and
participants in Trellis IDL and generate Rust code with the `trellis` CLI.
Low-level crates in this workspace support the platform implementation,
generators, CLIs, and tests; they are not the stable package surface that normal
Rust services and apps should author against. Internal workspace crates are
marked `publish = false`; runtime implementation for public authoring lives
behind modules of the public `trellis` facade.

**Crates in this repository:**

| Crate                     | Purpose                                                          |
| ------------------------- | ---------------------------------------------------------------- |
| `trellis-auth`            | Unpublished compatibility/test package for auth helpers          |
| `trellis-auth-adapters`   | Unpublished compatibility/test package for auth adapters         |
| `trellis`                 | Curated public Rust facade for clients and services              |
| `trellis-cli`             | Operator CLI crate for the `trellis` binary                      |
| `trellis-client`          | Unpublished compatibility package for `trellis_rs::client`       |
| `trellis-codegen-rust`    | Internal Rust SDK code generation                                |
| `trellis-codegen-ts`      | Internal TypeScript SDK code generation                          |
| `trellis-idl`             | Native Trellis IDL compiler                                      |
| `trellis-core-bootstrap`  | Internal bootstrap helpers for infrastructure state              |
| `trellis-local-bootstrap` | Internal local Trellis/NATS bootstrap bundle generation          |
| `trellis-service`         | Unpublished compatibility/test package for `trellis_rs::service` |
| `trellis-events-runtime`  | Internal built-in Events runtime                                 |
| `trellis-jobs-runtime`    | Internal built-in Jobs admin runtime                             |

See `../design/tooling/trellis-cli.md` and
`../design/contracts/trellis-rust-contract-libraries.md`.

Rust SDK crates are generated as disposable build output rather than tracked
workspace crates.

Generated SDK and participant crates include a package-local `TRELLIS.md` for AI
agents. Those files summarize the contract id, kind, crate/package name, owned
RPC/event/feed/operation descriptors, facade methods, and used dependency
surfaces. Use them together with the raw docs index:

- https://raw.githubusercontent.com/oats-center/trellis/main/docs/static/llms.txt
- https://raw.githubusercontent.com/oats-center/trellis/main/docs/static/llms-full.txt

Current Rust service code should prefer descriptor and facade APIs:
`trellis_client.call::<RpcDescriptor>(...)`,
`trellis_client.publish::<EventDescriptor>(...)`,
`trellis_client.subscribe::<EventDescriptor>()`,
`trellis_client.feed::<FeedDescriptor>(input)`,
`trellis_client.operation::<Operation>().start(...)`, generated client wrappers
such as `.rpc().group().method(...)`, and service registration through
`handle().rpc().group().method(handler)` where generated.

Prepared event support includes `PreparedTrellisEvent`,
`prepare_event::<Descriptor>(...)`, `publish_prepared`, and
`dispatch_outbox_once`. Durable stores include `OutboxStore`, `InboxStore`,
`SqliteOutboxStore`, `SqliteInboxStore`, `PostgresOutboxStore`, and
`PostgresInboxStore`.

Rust client event subscriptions are live/ephemeral by default. Service-level
durable event processing is a contract/resource concern: declare
`eventConsumers` in the canonical service manifest so Trellis provisions the
consumer binding and grants exact bound JetStream subjects. Rust service code
should not create arbitrary durable event consumers for contract event
processing.

Run `cargo xtask install` from the repository root to install every locked API
dependency and regenerate project-local artifacts in dependency order. If you
are doing a normal Rust build from the repo, prefer `cargo xtask build`, which
runs installation first. Rust client-library integration coverage lives in the
public `trellis` facade crate and runs with:

```sh
cargo test --manifest-path Cargo.toml -p trellis-rs --features live-integration --test integration -- --nocapture
```

That Rust suite is a peer of the TypeScript/Deno suite
(`deno task -c ts/deno.json test:integration`). Both use native test discovery
and real runtime processes; see [the setup commands](../integration/README.md).
