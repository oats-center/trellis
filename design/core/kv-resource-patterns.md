---
title: KV Resource Patterns
description: Typed KV history, commitments, CAS, and representation evolution.
---

# KV Resource Patterns

KV is participant-owned typed keyed storage. Service resources belong to the
deployment; device resources belong to the device principal. Applications use
generated handles from the connected runtime rather than raw bucket names or
binding payloads.

Declarations include title, description, value type, history, TTL, optional
capacity desires, and optional status. Desired capacities are planning hints
only: they do not grant approval, gate readiness, configure the provider, or
become reported actual limits. Approved hard commitments control provisioning.
History must be supported by the backend (currently 1..64). Capacity absence
means no finite committed application limit, not zero.

Handles support create, current value/entry reads, put, replace, checked or
unconditional delete, bounded ordered history, and watch. Revisions are explicit
opaque CAS tokens. History and watch include tombstones and preserve backend
revision/time.

Every value uses the `TRKV` envelope, format byte 1, big-endian positive u32
representation version, and generated-codec UTF-8 JSON. There is no raw JSON
fallback. For accepted representations written by this architecture, SDKs run a
direct historical-to-current migration and validate the result. Migration is per
revision, fallible, and never writes back. Corrupt or unsupported data fails
visibly rather than appearing absent or deleted.

Resource identity is stable for owner/participant/kind/name. Approved increases
to hard history or TTL retention can reconcile the owned provider in place.
Reductions that could discard retained data fail rather than mutate, destroy, or
recreate storage.

Omission performs Detach: it removes runtime authority while preserving the
allocation and its data. A compatible approved re-add reattaches it. A provider
resource found at the deterministic name is accepted only when its ownership
marker proves that it belongs to the same catalog resource; foreign resources
are rejected and are never adopted, purged, or destroyed. Explicit Destroy is
available only for a detached catalog resource and deletes an allocation only
after that ownership proof succeeds.

Trellis-owned runtime streams follow the same non-destructive startup rule. The
runtime creates a missing stream, accepts an exact existing configuration, and
may apply only safe subject or retention-limit expansions. Startup rejects any
existing stream whose identity, storage, retention, discard policy, sources, or
limits are incompatible or would reduce retained data.
