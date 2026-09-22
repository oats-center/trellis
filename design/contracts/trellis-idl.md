# Trellis IDL

## Purpose

Trellis IDL is the declarative source language for APIs and deployable
participants. `trellis-idl` parses explicitly listed source files, resolves the
locked package graph, builds the native semantic model, prints canonical source,
checks selected-surface compatibility, and projects generated SDK descriptors.
Rust and TypeScript are generated targets, not authoring languages.

## Package manifest and imports

```toml
[package]
name = "acme-orders"
version = "2.3.1"

[sources]
types = "types.trellis"
orders = "orders.trellis"

[dependencies]
common = { package = "acme-common", version = "^1.4", registry = "ghcr.io/acme/trellis" }

[generate.rust]
output = "trellis"

[generate.typescript]
output = "packages/trellis"
```

Package names are lower-case ASCII identifiers with hyphens. Sources are
explicit regular files beneath the package root after canonical path resolution.
Source and direct-dependency aliases share one namespace. Local dependencies use
`{ package, path }`; published packages freeze identities and never contain
filesystem paths.

Imports are explicit per file:

```trellis
import { Order, OrderId } from types;
import { Telemetry as telemetry } from common;
```

Only same-file declarations and explicit imports are visible. Local source
cycles are allowed; package dependency cycles, wildcard/transitive lookup,
suffix matching, escaping paths, duplicate canonical files, and multiple
versions of one package identity in a resolved graph are errors.

`trellis.lock` format 2 records exact versions, semantic and distribution
digests, direct edges, and acquisition locations. `install` preserves valid
locked choices; `update` deliberately resolves; `generate` uses the exact
lock/cache without upgrading.

## APIs and participants

```trellis
model CreateOrderRequest { customerId: string; }
model CreateOrderResponse { orderId: string; customerId: string; }

api orders@v1 {
  title "Orders";
  description "Order creation.";
  rpc Create { input CreateOrderRequest; output CreateOrderResponse; }
  capabilities { public { allows { rpc Create; } } }
}

service OrdersService { implements orders; }
app OrdersCaller { use orders { rpc Create; } }
```

The authored API lineage token (`orders`) is lowercase alphanumeric and is used
unchanged in IDL references and generated API paths. Generated language symbols
may use PascalCase names such as `Orders`.

API bodies contain title, description, optional human version, named RPC,
Operation, Event, and Feed declarations, API-scoped named errors, and one
capabilities block. Action names are exact unquoted identifiers or dot paths.
Inputs, outputs, progress, events, and error payloads reference top-level types.
There is no `rpc internal`, `EventClass`, per-action version, general transfer
block, participant-local schema, or API State.

Participants are `service`, `device`, `app`, or `agent`. A device may contain at
most one named `app` or `agent`. Only service/device participants implement
APIs. `use Api;` selects nothing; a nonempty use body selects exact
interactions. Operation selection always includes its lifecycle and signals.

Resource headers are `state`, `kv`, `store`, `job`, or `consumer`, optionally
followed by `optional`, then a local name. Bodies require nonempty `title` and
`description`. State is one value and has no TTL. Store is raw bytes. Consumer
authoring has events, concurrency, replay, and retry—never ordering, ack-wait,
max-delivery, or DLQ switches. Job progress, logs, and dead lifecycle are always
available rather than feature flags.

Job resources may declare a positive creation-relative `deadline` and a `retry`
block with positive total `attempts` and exactly `attempts - 1` positive,
ordered `backoff` durations. `attempts 1` requires `backoff []`. Omitting retry
uses five deliveries with `[5s, 30s, 2m, 10m]`; omitting deadline declares no
deadline.

## Types and wire codecs

Top-level declarations are models, enums, named scalar aliases, and API-scoped
errors. Model fields use lower camel case and may be optional with `?`.
Supported composition is named references, `list<T>`, `map<T>`, and `T | null`.
There are no general unions/generics, defaults, raw `json`, duration payload
type, patterns, or arbitrary string formats. `CursorQuery` and `CursorPage<T>`
are the only reserved finite generic forms.

Scalars are `string`, `bool`, `int32`, `uint32`, `int64`, `uint64`, `number`,
`bytes`, `timestamp`, and `ulid`. Constraints are `min_length`, `max_length`,
`min`, `max`, `min_items`, and `max_items`. Durations in resource/retry config
use integer `ms`, `s`, `m`, `h`, or `d`; capacities use `B`, `KiB`, `MiB`, or
`GiB`.

On JSON wires, 64-bit integers are canonical decimal strings, bytes are padded
standard base64, timestamps decode RFC3339 (except leap seconds) and emit canonical UTC
RFC3339, ULIDs are uppercase, and models are open to unknown fields.
Optional absence and nullable `null` are
different. Enums retain an unknown-symbol arm.

## Canonicalization and evidence

Canonical source has presentation-preserving and semantic modes using the same
parser. A canonical compilation unit may include frozen `package` and
`dependency` prelude declarations. Semantic mode removes nonsemantic metadata
and source-layout choices while retaining package/dependency identity,
capability consent text, capability allows, and machine declarations.

The package digest is base64url SHA-256 of semantic canonical UTF-8 source.
Package evidence contains the complete dependency closure exactly once with
presentation canonical source. The server independently resolves it and rejects
missing/extra packages, cycles, edge/version mismatches, or digest mismatches.
Generated Rust constructors and runtime descriptors are compiler output, not
contract-as-code builders.

Published OCI source bundles contain the manifest, listed source, and frozen
resolution metadata—not canonical API JSON. Generated language packages may be
committed when consumers must build without the CLI/cache; never hand-edit them.

## Pagination

A paginated RPC has optional `page: CursorQuery` input and returns
`CursorPage<Row>` directly. `page` is always present; only absent `nextCursor`
means completion. Empty pages may continue. Cursors are opaque, filter/scope
bound continuation positions, not authority or snapshot tokens. Services own
their default and maximum limits and reauthorize each page.
