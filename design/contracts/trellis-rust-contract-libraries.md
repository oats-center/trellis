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

Generated participant facades reference one shared package semantic-evidence
projection and retain their exact lexical participant path. Cursor traversal and
pagination errors live in the common SDK runtime; generated methods remain typed
adapters. Service/device connection paths submit identity/proof inputs;
the server resolves assignments, approvals, exact grants, routes, and resources.
Generated device options do not accept a reusable session seed.

Primitive wire wrappers, nullable values, and cursor-page structures live in
the transport-independent protocol library. Generated packages re-export those exact types rather
than defining per-package copies; authored IDL models retain their own nominal
types. Their serialization remains unchanged. Generated Rust ABI 2 requires this
shared support; regenerate packages with the matching CLI and SDK instead of
adding compatibility definitions. This is a Rust support-ABI change, not a wire
protocol version change.

Rust generation can bind an exact native package to a compiled model provider.
The binding matches package identity, authored version, and semantic digest,
including locked dependency identities. Matching packages re-export the provider's
nominal models instead of emitting copies; other packages remain independently
generated. Generated constants verify the actual provider's package metadata at
compilation. The CLI offers the independently
versioned `trellis-models` bundle through this generic binding mechanism;
neither package names alone nor matching API majors select shared models.
Generated packages declare their model dependency directly, while retaining the
normal compatible SDK dependency range. SDK implementation updates do not require
regeneration when execution ABI and selected models remain compatible. The SDK
can use a newer model snapshot without making an older generated client adopt it.

Generated implementations are disposable build artifacts, not committed source.
The SDK keeps authored Cargo manifests and entrypoints. `cargo xtask install`
prepares the model bundle and SDK runtime descriptors from the shared native
IDL inputs into ignored, package-relative directories before compilation.
Published model and SDK archives include those frozen sources and have no
compiler or generator build dependencies. A repository checkout therefore needs
preparation, while an archive/registry consumer needs only ordinary Cargo; a bare
Git dependency is not a prepared SDK distribution. Private runtime API wrappers
may still generate into `OUT_DIR`; they are not published SDK dependencies.
Project-local SDKs are generated before compilation through `trellis install`.
Package generated code when distributing a consumer, rather than tracking it in
Git. Formatting checks apply to authored code, not generated implementations.
Use current Rustdoc/generated source for exact participant-specific methods.

Generation rejects collisions across the complete normalized Client method
namespace, including API accessors, resource accessors, and runtime methods,
before replacing last-good output. Model field and enum value names are checked
the same way, including against the generated `extra` field that preserves
unknown wire fields and the `Unknown` variant that preserves unknown enum
values. Device State accessors use an SDK-owned Watch
descriptor generated from native State IDL; a State-only device does not need an
application dependency on the built-in API package.

## Installed Availability

Generated optional-action adapters consult one advisory projection of the
verified whole-capability authority and its exact accepted signed catalogs.
Capability identity generations, consent revisions, API generations, and
accepted revisions remain significant; generated names do not confer authority.
Dispatch still performs the authorization owner's live signed-catalog check.

Authenticated native resource bindings retain their physical names and handle
generations across transport suspension and readiness. Readiness changes only
usability; an authenticated replacement that changes or removes a binding retires
the previous physical handle. State availability follows supported user/device
ownership, with the State provider validating the declared resource at dispatch,
not fabricated participant-resource permission atoms.
