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
types. Generated packages depend only on `trellis-rs` among Trellis crates:
`trellis-rs` re-exports the `trellis-protocol` it links as
`trellis_rs::protocol`, and generated code names protocol types through that
path. A `trellis-rs` that links a path, git, or alternate-registry
`trellis-protocol` with no workspace `[patch]` therefore gives the generated
package that exact crate, never a second copy whose types differ. Their serialization remains unchanged. Generated Rust ABI 2 requires this
shared support; regenerate packages with the matching CLI and SDK instead of
adding compatibility definitions. This is a Rust support-ABI change, not a wire
protocol version change.

Rust generation can bind an exact native package to a compiled model provider.
The binding matches package identity and semantic digest. The digest covers
locked dependency identities and excludes the package's own version, so a
provider that carries the same snapshot under another version provides the same
models. Matching packages re-export the provider's nominal models instead of
emitting copies; other packages remain independently generated. Generated
constants check at compilation that the provider Cargo actually built has the
same identity and digest. Neither package names alone nor matching API majors
select shared models.

The CLI offers the independently versioned `trellis-models` bundle through this
binding mechanism. The provider must be the `trellis-models` that `trellis-rs`
links, because a second copy would make the SDK's models different Rust types
from the ones `trellis-rs` exposes. Generated packages therefore never name
`trellis-models` themselves: `trellis-rs` re-exports the crate it links as
`trellis_rs::models`, and reused models come through that path. Whatever source
Cargo resolved for `trellis-rs`'s model dependency (crates.io, an alternate
registry, git, or a sibling path with no workspace `[patch]`), the SDK gets that
exact crate. The CLI takes that crate from the
nearest `Cargo.lock` above the Rust output (the one `trellis-rs` links there),
including its exact source, and reads its binding from that source's own
`generated/__types.rs`. Generation stays offline and downloads nothing; it
finds each kind of source as the lock identifies it:

- A path source, which the lock does not record, comes from
  `cargo pkgid --offline --locked`. When Cargo cannot load the graph yet
  (before the SDK being generated exists, for instance), the path is unknown.
- A git source is the checkout Cargo made of the locked commit under
  `$CARGO_HOME/git/checkouts`, accepted only when its `HEAD` is that commit.
- A registry source is the unpacked copy under `$CARGO_HOME/registry/src`
  whose `.crate` archive in the matching `registry/cache` directory has the
  SHA-256 checksum the lock records. Another same-version copy, from another
  registry or with other contents, is never used.

The CLI's version from crates.io uses the binding the CLI embeds; the same
version from any other source is read like any other version. With no lock, or
no `trellis-models` in it, the generated SDK brings in the CLI's version of
`trellis-rs` and with it that release's models, so the embedded binding applies. A locked source that is not on disk or does not
verify cannot be compared, and its models are emitted inline.
`trellis generate -v` (and `trellis install -v`) logs the lock, the resolved
version, and where its binding was read, then, for each package, whether its
models are reused or inlined and which provider's digest differed.

Because reused models follow `trellis-rs`, a later `trellis-models` release that
Cargo selects for it still compiles when its digest is unchanged. Generated
packages retain the normal compatible SDK dependency range. SDK implementation
updates do not require regeneration when execution ABI and selected models
remain compatible. When `trellis-rs` moves to a model snapshot with another
digest, the generated package's compile-time model check fails and the package
is regenerated; it does not keep an older snapshot as a second, distinct copy.

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

Module names come from short IDs, the ID without its workspace, so
`ghcr.io/acme/orders.billing@v1` is `apis::orders_billing_v1`. Packages, APIs,
participants, and apps whose short IDs agree are named by full ID instead, and
names that still agree after punctuation is dropped take `_` and an eight-digit
hash of the full ID (`trellis_idl::generated_names`, shared with the
TypeScript generator). Every other module keeps its readable name.

Generation rejects collisions across the complete normalized Client method
namespace, including API accessors, resource accessors, and runtime methods,
before replacing last-good output. A service's Provider methods are checked the
same way, so a KV, store, or consumer named `config` collides with the settings
accessor a service with settings gets, as a State named `config` does on a
device Client. Settings whose names normalize to one Rust field, such as
`apiURL` and `apiUrl`, are rejected too. Model field and enum value names are checked
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

Availability reports each selected optional capability and group, and each
member of a selected optional group that allows a selected action. A member is
available when the authority holds it and its selected actions are allowed; a
group, which an authority carries only as its members, when all of its selected
actions are. TypeScript keys and generated Rust fields follow the same rule.

Authenticated native resource bindings retain their physical names and handle
generations across transport suspension and readiness. Readiness changes only
usability; an authenticated replacement that changes or removes a binding retires
the previous physical handle. State availability follows supported user/device
ownership, with the State provider validating the declared resource at dispatch,
not fabricated participant-resource permission atoms.
