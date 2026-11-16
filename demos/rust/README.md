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

Create the service deployment, make an identity for the service, and register
its public key as an instance:

```sh
trellis svc rust-field-ops create --source demos/rust/service --participant demo-rust-service.Service
trellis keys new
trellis svc rust-field-ops provision --public-key <public-key>
```

`trellis keys new` prints the seed and the public key; the seed stays with the
service. Run with authenticated service bootstrap:

```sh
cargo run --manifest-path demos/rust/service/Cargo.toml -- \
  --trellis-url http://localhost:3000 \
  --seed <seed>
```

When a later build asks for more access, the service waits and prints a review
link. Approve it in the Console under **Admin → Services**, or with
`trellis svc rust-field-ops approve`.

The service mounts generated RPC and operation handlers. Authenticated mode uses
the resolved `siteSummaries` KV bucket and `uploads` object store.

## Device

Run the wizard with offline sample data:

```sh
cargo run --manifest-path demos/rust/device/Cargo.toml
```

Run as a provisioned device, after
`trellis dev rust-field-device create --source demos/rust/device --participant demo-device.Device`
and `trellis --format json dev rust-field-device provision`, which prints the
`rootSecret` once:

```sh
cargo run --manifest-path demos/rust/device/Cargo.toml -- \
  --device \
  --trellis-url http://localhost:3000 \
  --device-root-secret <root-secret>
```

`dev provision` returns no one-use provisioning secret today
(`provisioningSecret: null`), so leave out `--provisioning-secret`.

The generated participant facade provides the `fieldOps` RPC, operation, event,
transfer, and state APIs used by the device.
