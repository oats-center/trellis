# Trellis Demos

This directory contains the Field Ops demo across separate browser UI,
TypeScript participants, and Rust participants. The layout keeps each demo as
close to out-of-tree development as possible while remaining in this repository.

- `demos/app`: the shared Svelte Field Inspection Desk browser app, with its own
  Deno config.
- `demos/ts/service`: the TypeScript Field Ops service.
- `demos/ts/device`: the TypeScript activated field-device TUI.
- `demos/ts/shared`: sample data and helpers for the TypeScript participants.
- `demos/ts`: the Deno workspace for TypeScript demo participants.
- `demos/rust`: independent Rust Field Ops service and field-device projects.

Every participant is authored in `contract.trellis`. Run `trellis update` for
registry dependencies and `trellis generate` in each project root. Each project
generates one ordinary package at its configured destination, normally
`trellis/`, with `apis` and `participants` namespaces. Dependencies remain in
the global API cache; regenerate generated packages rather than editing them.

## Browser App

The browser app is intentionally separate from both the service and device
participants. It consumes generated demo service SDKs and can be used with the
TypeScript or Rust service/device implementations.

```sh
deno task -c demos/app/deno.json check
deno task -c demos/app/deno.json dev
```

## TypeScript Demo

The TypeScript demo is the full end-to-end runtime path today. It includes:

- service deployment apply/provision flow through the `trellis` CLI
- service-bootstrap authority updates, including the local-development path
  where a service can upload its manifest and wait for authority acceptance
  instead of requiring every dependent contract to already be active
- activated device approval and reconnect flow
- browser app sign-in and SDK calls
- operations, operation progress, cancel, events, state, send transfers, receive
  transfer previews, and private jobs behind public operations

See `demos/ts/README.md` for the complete walkthrough.

## Rust Demo

Rust demo tasks:

```sh
trellis update --root demos/rust/service
trellis generate --root demos/rust/service
trellis update --root demos/rust/device
trellis generate --root demos/rust/device
cargo check --manifest-path demos/rust/service/Cargo.toml
cargo check --manifest-path demos/rust/device/Cargo.toml
```

The Rust service mounts generated `demo.service@v1` RPC and operation handlers
and can run either through authenticated service bootstrap or the raw local NATS
developer loop. The Rust device can run offline, with user/session credentials,
or through a demo-local activated-device persistence flow; online actions use
the generated participant `fieldOps` and state facades, including send/receive
transfer helpers. In authenticated service mode, site summaries use the resolved
service-owned `siteSummaries` KV bucket and evidence bytes use the resolved
service-owned `uploads` object store.

Remaining Rust gaps are narrower than the TypeScript path: live activated-device
authenticated smoke coverage, live verification of worker-host job consumption,
and reusable public device persistence ergonomics beyond the demo-local file.

See `demos/rust/README.md` for Rust-specific setup, supported modes, and current
limitations.

## Consumer Acceptance

CI runs `trellis generate` in all five participant projects, `trellis update` in
the dependent device/app projects, the TypeScript/app checks above, and the two
Rust checks. Local path dependencies in `trellis.toml` and package overrides in
the checked-in Deno configuration select current repository sources without
rewriting the demos.

Focused runtime tests live in `ts/integration/` with a separate small IDL
fixture. Outbox commit/rollback atomicity is tested against real SQLite in the
core SDK. The Field Ops walkthrough publishes events directly and is not a
persisted outbox example.
