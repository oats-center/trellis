# Rust Demo

This directory contains independent Rust Field Ops Trellis projects. The service
and device each own a `contract.trellis`, `trellis.toml`, Cargo crate, and one
ordinary generated package under `trellis/`.

## Generate And Check

From the repository root:

```sh
trellis generate --root demos/rust/service
trellis update --root demos/rust/device
cargo check --manifest-path demos/rust/service/Cargo.toml
cargo check --manifest-path demos/rust/device/Cargo.toml
```

Use `trellis generate --watch --root <project>` while editing IDL. Everything
under `trellis/` is generated package output and must not be hand-edited. It has
`apis` and `participants` modules and a published runtime dependency, not
separate SDK crates or repository-relative runtime paths. A local runtime can be
selected through ordinary Cargo overrides supplied by the development/test
invocation.

## Service

Print the generated participant identity:

```sh
cargo run --manifest-path demos/rust/service/Cargo.toml -- --contract
```

Run with authenticated service bootstrap after creating and provisioning the
service deployment:

```sh
cargo run --manifest-path demos/rust/service/Cargo.toml -- \
  --trellis-url http://localhost:3000 \
  --seed <instance-seed>
```

The service mounts generated RPC and operation handlers. Authenticated mode uses
the resolved `siteSummaries` KV bucket and `uploads` object store.

## Device

Run the wizard with offline sample data:

```sh
cargo run --manifest-path demos/rust/device/Cargo.toml
```

Run as a provisioned device:

```sh
cargo run --manifest-path demos/rust/device/Cargo.toml -- \
  --device \
  --trellis-url http://localhost:3000 \
  --device-root-secret <root-secret>
```

For a device whose identity has not yet been enrolled, also pass the one-use
`--provisioning-secret <provisioning-secret>` argument returned by provisioning.
Omit it when reconnecting an already enrolled, ready device.

The generated participant facade provides the `fieldOps` RPC, operation, event,
transfer, and state APIs used by the device.
