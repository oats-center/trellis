---
title: "Generate SDKs and artifacts"
description: "Compile native IDL into canonical artifacts and private language SDKs."
---

# Install source packages and generate SDKs

```sh
trellis install
trellis generate
```

`trellis install` reproduces `trellis.lock`. Use `trellis update` only when
intentionally resolving dependency changes.

`trellis.toml` defines `[package]`, explicitly lists `[sources]`, declares
`[dependencies]`, and configures language output. Local dependencies use
`{ package, path }`; registry dependencies resolve to exact source bundles in
the global content-addressed OCI cache.

Each language output is one ordinary package containing APIs, participants,
types, codecs, and descriptors. Its version equals the source package version.
There is no project-local dependency installation tree, canonical API or
participant JSON directory, generator build script, or startup generation step.

Published Trellis packages contain the manifest, listed `.trellis` files, and
frozen resolution metadata. Generated descriptors are runtime support ABI, not
a contract builder or exchange artifact.

Commit native IDL, manifests, and dependency locks, not generated implementations.
Run `trellis install` before compiling local consumers. Distributions can package
generated code so downstream consumers do not need the CLI. Trellis-owned Rust
libraries instead generate built-in bindings in Cargo `OUT_DIR` during compilation.
Never hand-edit generated files or enforce their formatting as maintained source.
