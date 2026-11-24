# Trellis

Trellis is a contract-driven platform for building distributed services over
NATS JetStream. Contract definitions live with the code that owns them. Build
and release tooling derives canonical JSON artifacts, SDKs, authorization
scopes, and runtime wiring from those contract sources.

Trellis 2.0 was a joint effort between the OATS Center and The Qlever Company,
LLC from the beginning. Qlever made major contributions and developed Trellis
into production systems used by several companies. After incubation at Qlever,
Trellis 2.0 moved to public stewardship under the OATS Center. The Trellis 2.0
initiative begins releasing on the `0.100.x` line; earlier versions belong to
the TypeScript-based Trellis lineage.

## Repository layout

```
Cargo.toml      Rust workspace: public libraries, CLI, server, and tooling
crates/         Rust platform crates and public facades
xtask/          Rust repository build, generation, and release tooling
integration/    Shared TypeScript/Rust test vectors and cross-language fixtures
demos/          Shared demo app plus TypeScript and Rust service/device examples
docs/           Trellis documentation site (Sveltepress, published to GitHub Pages)
ts/             TypeScript client libraries, adapters, and integration tests
web/            Embedded Console, served under /console
portal/         Embedded sign-in and consent portal
scripts/        Test and release runners shared by `npm test` and the Pipeline
generated/      Derived manifests and SDKs when generated locally (usually absent from a clean checkout)
deploy/         Deployment assets: NATS templates and observability
design/         Trellis design docs
```

See `/guides/authoring-contracts` for the native IDL workflow and
`/guides/write-a-service/contract-artifacts` for regeneration details.

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

[Install Trellis](/guides/install-trellis) covers the `trellis` and
`trellis-server` binaries. To try it, [run a development
Trellis](/guides/running-trellis/development): `trellis-server --dev` creates
an installation on first start and runs NATS for you. For a real
deployment, run it [from the binaries](/guides/running-trellis/binaries) or
[with Podman](/guides/running-trellis/podman), with your own `nats-server`
(the guides use 2.15).

A `--system` installation keeps its config in `/etc/trellis` and its data in
`/var/lib/trellis`, where the platform, jobs, health and events subsystems
each get their own SQLite database. A `[paths]` section in `config.toml` moves
those folders; [Where the files go](/guides/running-trellis/binaries) lists
them, and its Other settings section covers a NATS cluster.
[Keep Trellis running](/guides/running-trellis/upkeep) covers upgrades,
backups, stream copies and `[leases] replicas`.

`/healthz` answers with the server's version as soon as it is up. `/readyz`
returns `503` until startup has finished and `200` after, and goes back to
`503` while the server stops or while another server runs Auth.

TypeScript runtime entrypoints, from `@oatscenter/trellis`:

- `TrellisClient.connect(...)` for browser and client runtimes
- `TrellisService.connect(...)` for services
- `TrellisDevice.connect(...)` for activated devices

## Working in this repository

You need Node 24 to 26 (CI uses 26) with npm 11 or newer, and a Rust
toolchain with the `wasm32-unknown-unknown` target. Prepare a checkout with:

```sh
npm ci
cargo xtask install
npm run browser:embedded
```

`cargo xtask install` generates the built-in contract bindings and each
repository project's SDK, and rebuilds the TypeScript SDK's protocol WASM.
Each contract project owns `trellis.toml`, commits `trellis.lock`, and
consumes its generated SDK through the configured TypeScript or Rust output.
Generated implementations are ignored build artifacts, not committed source.
`cargo xtask build [cargo args]` runs the install, builds the embedded Console
and Portal, and then runs `cargo build`. Repeated builds preserve unchanged
built-in bindings and reuse intact browser bundles when the inputs match.

[In-repo development](/guides/in-repo-development) covers running the server
from source. `npm test` runs the Pipeline's check jobs on this host through
`scripts/check.sh`; `npm test -- --all` adds the packaging and image jobs.
The live integration suites run through their own runners, which build or
download what they need:

```sh
scripts/run-rust-tests.sh
npm run test:integration
```

[integration/README.md](integration/README.md) describes both, and
[Releasing Trellis](/guides/releasing-trellis) covers testing policy,
versioning and release checklists.

## Design documents

The Trellis design docs live in [design/](design/). Start with
`design/README.md` for the topic index.
