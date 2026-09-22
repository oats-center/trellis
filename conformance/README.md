# Shared Test Vectors

This directory contains language-neutral test vectors that are consumed by both
the TypeScript and Rust implementations.

Use this directory for values that should stay byte-for-byte identical across
runtimes:

- `canonical-json/` - canonical serialization and digest vectors
- `session-proof/` - session-key proof and domain-signature vectors
- `authorization-context/` - signed authorization trust chains, contexts, and
  request/event-proof v1 vectors
- `transfer-v1-vectors.json` - transfer framing and digest vectors

Authorization-context vectors are executable data rather than a case-name
catalog. Every case names one public protocol operation, starts from the pinned
canonical complete chain, applies documented target-aware RFC 6902 mutations,
optionally re-signs with deterministic conformance seeds, and pins success
output or a stable error code plus RFC 6901 path. Implementations must execute
every case; counting names is not conformance.

Do not put package-local snapshot tests here. If a fixture is only used by one
implementation, keep it next to that implementation's tests instead.
