---
title: State Patterns
description: Single-value typed State ownership, CAS, and representation evolution.
---

# State Patterns

State is a Trellis-owned single typed value. It is not a map, has no application
key, and has no TTL. Normal requests name only the local resource; ownership
comes from the verified session binding, never request-supplied owner data.

Browser State is owned by `(principalId, browserOrigin, clientId, resourceName)`.
Native user State is owned by `(principalId, clientId, resourceName)` in a distinct
native namespace. Replacing a browser tab, device, login session, or native
proof key does not replace the user's State. Device State remains isolated by
the provisioned device principal. A companion uses its own user OAuth binding,
not the parent's credentials. Services use KV or application storage instead.

The native-IDL State API registers user representations under State write authority.
The SDK registers its declared representation before using the handle; browser
and native apps do not install app participants or upload service IDL. Repeated
registration of the same representation is idempotent. Representation changes
require revision checking, and a representation version cannot silently acquire
a different schema. Registration and value mutations retain the existing State
storage, revision, validation, and migration semantics.

Devices use their accepted installed State declarations rather than user schema
registration. Every device request checks the signed resource-binding digest
against the active instance binding before resolving its declared schema. Devices
in the same deployment share that schema, but retain separate principal-owned
values. Device credentials cannot register or change user State representations.

Generated handles expose get, create, set, replace, and delete. Revisions are
opaque CAS tokens. Create requires no live value, replace requires the exact
revision, set is last-write-wins, and delete may be checked or unconditional.
Conflicts return the authoritative current optional value. An enabled but
unwritten resource returns no value; this differs from an unavailable optional
handle.

Stored values contain generated-codec UTF-8 JSON, a positive u32 representation
version, revision, and timestamps. The server validates current or accepted
historical schemas but never runs application migration code. SDKs register one
direct historical-to-current migration per accepted version. Reads, watches, and
conflict values migrate in memory, preserve original revision/time, and do not
write back or retry CAS automatically.

Generated Rust app accessors register asynchronously with an explicit expected
registration revision and idempotency key, returning the typed handle and
registration response. TypeScript handles register before first use at revision
`0` and expose explicit `register(expectedRevision, idempotencyKey)` for reviewed
representation changes. Registration and stored-value revisions are distinct.
Typed `watch()` wraps the generated State Live descriptor and the shared certified
Live transport. It yields an initial snapshot and subsequent updates; absent values
represent unwritten/deleted State. Watch preserves explicit close and terminal
outcomes and does not silently reopen after transport or authorization loss.

The resource catalog gives State stable logical identity. Each allocation has a
fresh incarnation in its physical key; device values additionally include their
principal-owned namespace. Detach removes
runtime authority without deleting new-version data. A compatible, newly
approved re-add of the same owner/participant/kind/name reattaches it. Rename or
kind change creates a different identity.

Explicit administrative resource deletion is the only allocation-deleting lifecycle and is available
only for a detached catalog resource. State shares one Trellis-owned provider
bucket, so removal verifies ownership of that bucket and purges only the captured
incarnation's user key or device-value prefix. A delayed purge cannot erase a
replacement allocation; it never deletes or purges unrelated State. A foreign provider at a
deterministic name is rejected and is never adopted, purged, or destroyed.

The State RPC handler verifies signed requests through the connection's shared
authority owner and requires the action's State capability. It derives the
namespace from the authenticated binding. A guessed physical identifier or an
administrator role cannot substitute for that ownership check. Device resource
access additionally remains limited to its declared State resources.
