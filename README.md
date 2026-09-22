# Trellis

Trellis is a contract-driven platform for building distributed services over
NATS JetStream. Contract definitions live with the code that owns them. Build
and release tooling derives canonical JSON artifacts, SDKs, authorization
scopes, and runtime wiring from those contract sources.

## Repository layout

```
conformance/    Shared TypeScript/Rust test vectors (canonical JSON, auth proofs)
demos/          Shared demo app plus TypeScript and Rust service/device examples
docs/           Trellis documentation site (SvelteKit static site, published to GitHub Pages)
ts/             TypeScript packages, services, and apps (Deno workspace)
rust/           Rust crates (public facades plus internal CLI, codegen, and runtime support)
generated/      Derived manifests and SDKs when generated locally (usually absent from a clean checkout)
deploy/         Deployment assets, including quadlets and NATS templates
design/         Trellis design docs
```

See `/guides/write-a-service/contract-artifacts` for regeneration details. See
`/guides/releasing-trellis` for repository testing, versioning, and release
checklists.

## Key concepts

- **Contracts** - native `.trellis` source packages with explicit source files
  and locked dependencies. Published bundles contain source and frozen
  resolution metadata, not API JSON artifacts.
- **Auth** - two-layer model: NATS transport auth plus Trellis session-key
  proofs with contract-gated approval. See `design/auth/trellis-auth.md`.
- **Jobs** - JetStream-backed job lifecycle with retry, progress tracking, and
  dead-letter handling. See `design/jobs/trellis-jobs.md`.
- **Operations** - caller-visible asynchronous workflows with durable state and
  watch semantics. See `design/operations/trellis-operations.md`.
- **CLI** - public `trellis` operator/runtime and package-manager CLI. See
  `design/tooling/trellis-cli.md`.
- **Patterns** - top-level architecture boundaries and communication patterns.
  See `design/core/trellis-patterns.md`.

## Getting started

See the [Trellis docs](docs/) to get started.

Trellis service deployments need persistent writable storage at
`/var/lib/trellis` by default. The control-plane SQLite database defaults to
`/var/lib/trellis/trellis.sqlite` and can be moved with `storage.dbPath` in the
Trellis service config.

Trellis requires `nats-server` 2.10.0 or newer. Jobs rely on JetStream source
subject transforms and the filtered consumer create API permission model. When
`nats.jetstream.replicas` is omitted from the Trellis service config, the
runtime probes NATS JetStream topology and uses `3` only when at least three
current JetStream metadata peers are visible; otherwise it falls back to `1`.
Set the value explicitly to pin a deployment to a known replica count.

Current TypeScript runtime entrypoints:

- `TrellisClient.connect(...)` for browser and client runtimes
- `TrellisService.connect(...)` for services
- `TrellisDevice.connect(...)` for activated devices

Install locked API dependencies and regenerate project-local artifacts with:

- `cargo xtask install`
- `cd ts && deno task install`
- `cargo xtask build`
- `cargo xtask release check-versions`
- `cargo xtask release prepare --tag v0.9.0-rc.1`

Each contract project owns `trellis.toml`, commits `trellis.lock`, and consumes
its private generated SDK through the configured TypeScript or Rust output.
`cargo xtask build` installs the fixed repository project DAG before building
the Rust workspace. Live client-library integration coverage is language-owned.
Run these peer suites when you need that coverage:

```sh
deno task -c ts/deno.json test:integration
cargo test --manifest-path rust/Cargo.toml -p trellis-rs --features live-integration --test integration -- --nocapture
```

Both suites use ordinary Rust and Deno discovery. Build the server and CLI once
and pass their paths as documented in
[integration/README.md](integration/README.md).

## Design documents

The Trellis design docs live in [design/](design/). Start with
`design/README.md` for the topic index.
