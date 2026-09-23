---
title: Type System and Error Patterns
description: Native IDL types, codecs, compatibility, and Result-style errors.
---

# Type System and Error Patterns

Native Trellis IDL is authoritative. Models are open objects with lower-camel
fields; enums preserve unknown symbols; named scalars carry validated
constraints. Supported composition is references, lists, string-keyed maps, and
nullable `T | null`. Optional absence differs from null. There is no raw JSON,
general union/generic, default, pattern, or arbitrary string format.

On JSON wires, int64/uint64 are canonical decimal strings, bytes are padded
base64, timestamps decode RFC3339 (except leap seconds) and emit canonical UTC
RFC3339, ULIDs are uppercase, and numbers must be finite. Administrative JSON
documents use bytes with documented UTF-8 JSON media semantics rather than an
untyped JSON contract value.

Compatibility is semantic, selected-surface, and direction-aware: providers
accept all caller inputs, while callers decode all promised outputs/events/
updates/errors. Open output models permit extra fields. Versions are metadata,
not admission authority.

Expected domain/RPC failures use generated Result-style variants. Transport,
authorization, codec, unknown remote, and domain errors remain distinguishable.
Do not erase them with unchecked casts or thrown exceptions inside public
library boundaries.

Cursor pagination uses only the finite built-ins `CursorQuery` and
`CursorPage<T>`. Cursors are opaque; absence of next cursor is the sole end
signal. Lazy SDK iteration stops after one typed error.
