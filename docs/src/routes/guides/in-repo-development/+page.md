---
title: "In-repo development"
description: "Run the Trellis server, Console and NATS from a source checkout while you change them."
---

This is for working on Trellis itself. The server runs from your checkout the
same way an installed one does in [Start Trellis](/guides/starting-trellis):
it creates an installation on first start and runs NATS for you.

## What you need

- Git
- [Node](https://nodejs.org) 24 and npm 11+
- a Rust toolchain with the `wasm32-unknown-unknown` target
  (`rustup target add wasm32-unknown-unknown`)

## Prepare the checkout

```sh
git clone https://github.com/oats-center/trellis.git
cd trellis
npm ci
cargo xtask install
npm run browser:embedded
```

## Run the server

```sh
cargo run -p trellis-server
```

The first run asks a few questions to set up an installation in
`~/.config/trellis` and, if `nats-server` isn't on your `PATH`, offers to
download NATS. `cargo run -p trellis-server -- --init --nats-download` skips
the questions and creates a localhost development installation over plain
HTTP.
If that folder already holds an installation from a released Trellis, keep
them apart: `cargo run -p trellis-server -- init --out ~/trellis-dev` once,
then `cargo run -p trellis-server -- --config ~/trellis-dev/config.toml`.

Once it's up, Console is at `http://localhost:3000/console`. To check on it:

```sh
curl http://localhost:3000/healthz
curl http://localhost:3000/readyz
curl http://127.0.0.1:8222/healthz
```

## While you work

- Runtime changes under `crates/runtime/`: stop the server and `cargo run` it
  again.
- Console or sign-in changes under `web/` or `portal/`: run
  `npm run browser:embedded`, then restart the server. It serves the copies
  compiled into it, never the source folders.
- Contract changes: run `cargo xtask install`.
- CLI changes: `cargo run -p trellis-cli -- <command>`, or
  `cargo install --path crates/cli` to replace your installed `trellis`.
- Type checks: `npm run check:sdk` and `npm run check:integration`.
- Tests: `npm test` builds once and runs everything the Pipeline checks, in
  parallel (`npm test -- --all` adds packaging and images). `scripts/check.sh <job>`
  runs one job.
