# Generated Rust Libraries

## Purpose

Trellis IDL source packages are the only contract-authoring input. The compiler
resolves a native semantic graph and renders one ordinary Rust crate. Generated
code is a projection, not a second contract model or builder surface.

## Generation

`[generate.rust].output` names the output location. The crate uses the source
package name/version and contains `apis`, `participants`, generated types,
codecs, descriptors, and runtime facade support. It declares the published
Trellis runtime dependency matching the generator. Ordinary Cargo overrides own
local development substitutions.

Published OCI bundles and the global cache hold source plus frozen resolution
metadata. Generated crates contain no canonical API/participant JSON artifact
tree. Runtime descriptors are an internal support ABI and are not accepted as
handwritten authority evidence.

Generated participant facades embed package semantic evidence and exact lexical
participant path. Service/device connection paths submit identity/proof inputs;
the server resolves assignments, approvals, exact grants, routes, and resources.
Generated device options do not accept a reusable session seed.

Commit generated output when consumers must build without the CLI/cache.
Regenerate rather than editing it, and use current Rustdoc/generated source for
exact participant-specific methods.
