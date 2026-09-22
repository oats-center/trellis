---
title: State Patterns
description: Single-value typed State ownership, CAS, and representation evolution.
---

# State Patterns

State is a participant-declared, Trellis-owned single typed value. It is not a
map, has no application key, and has no TTL. Normal requests name only the local
resource; owner and participant scope come from the verified context and current
binding.

User app/agent State is owned by `(userId, participantId, resourceName)`. A
device companion therefore shares its State for the same user and child
participant across physical devices, while outer device State remains isolated
by device principal. Services use KV or application storage instead of State.

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

The resource catalog gives State stable physical identity. Detach removes
runtime authority without deleting new-version data. A compatible, newly
approved re-add of the same owner/participant/kind/name reattaches it. Rename or
kind change creates a different identity.

Explicit Destroy is the only normal data-deleting lifecycle and is available
only for a detached catalog resource. State shares one Trellis-owned provider
bucket, so Destroy verifies ownership of that bucket and purges only the owned
State key; it never deletes or purges unrelated State. A foreign provider at a
deterministic name is rejected and is never adopted, purged, or destroyed.

The State RPC handler verifies message proof/context and exact bound-resource
authority. API-wide State permission, guessed physical identifiers, or an
administrator role alone cannot substitute for the exact resource atom.
