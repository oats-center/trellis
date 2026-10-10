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

Generation targets are selected explicitly: a configured `[generate.rust]` or
`[generate.typescript]` emits that package even when the project root has no
matching toolchain file, so an app authored in one language can emit a
projection in the other (for example a TypeScript app participant that needs a
Rust crate for a Rust test harness). With no target configured, a single
detected language defaults to `trellis`; multiple targets require explicit
`output` paths.

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
  capabilities {
    capability create {
      title "Create orders";
      description "Create an order for a customer.";
      consequence "New orders enter the customer's workflow.";
      consent_revision 1;
      allows { rpc Create; }
    }
  }
}

service OrdersService { implements orders; }
app OrdersCaller { use orders { required capability create; } }
```

The authored API lineage token (`orders`) is lowercase alphanumeric and is used
unchanged in IDL references and generated API paths. Generated language symbols
may use PascalCase names such as `Orders`.

API bodies contain title, description, optional human version, named RPC,
Operation, Event, and Live declarations, API-scoped named errors, and one
capabilities block. Action names are exact unquoted identifiers or dot paths.
Inputs, outputs, progress, events, and error payloads reference top-level types.
There is no `rpc internal`, `EventClass`, per-action version, general transfer
block, participant-local schema, or API State.

Deployable participants are `service` and `device`. The `app` and `agent`
declarations describe browser/native application requests, not installable
principals. A device may contain one named `app` or `agent` companion request.
Only service/device participants implement APIs.

`use Api;` selects nothing. A use body selects whole capabilities:
`required capability read;` is required; `capability export;` is optional.
Duplicate requests normalize deterministically, and required wins over optional.
Unknown authored capability references are compile errors. Generated callable
surfaces are the union of the selected capabilities' actions; unrelated
capabilities do not become requirements merely because they overlap those
actions. Operation selections include the operation lifecycle and signals.

Administrative Auth RPCs also have ordinary, narrowly scoped capabilities.
Selecting one exposes its generated caller methods but does not grant platform
privileges: the required finite administrative privilege is an additional
runtime condition. Own-authority and own-session maintenance are fixed session
rights, not ordinary capability requests. There is no IDL privilege-request
syntax.

Every authored capability has an explicit positive integer `consent_revision`;
descriptor projections must expose it as `consentRevision`. Authors change this
when delegated meaning changes; prose edits do not increment it automatically.
Compatibility checking rejects a decrease for an existing capability identity;
Auth must enforce the same rule when accepting a force replacement. API
generation, accepted revision, and action/membership introduction revisions are
Auth-owned acceptance metadata, never authored counters.

Resource headers are `state`, `kv`, `store`, `job`, or `consumer`, optionally
followed by `optional`, then a local name. Bodies require nonempty `title` and
`description`. State is one value and has no TTL. Store is raw bytes. Consumer
authoring has events, concurrency, replay, and retry—never ordering, ack-wait,
max-delivery, or DLQ switches. Job progress, logs, and dead lifecycle are always
available rather than feature flags.

Application requests may declare local typed State codecs, but cannot own KV,
Store, Job, or Consumer resources. A State declaration grants no implicit
Get/Put/Delete authority: the application must request the appropriate State
capabilities explicitly. Deployable resources remain principal-owned.

Built-in capability selections follow the same rule. State `read`, `write`, and
`delete` select Get, Put, and Delete independently. A Jobs `query` selection
exposes Query, not job inspection, cancellation, or dead-letter replay. Health
`statusChanges` selects event consumption, not publication; provider publication
and Consumer delivery reporting are runtime responsibilities rather than caller
capabilities.

A participant declaring a Consumer requires a direct dependency on the `trellis`
source package containing `trellis.events@v1`. The author chooses and locks that
package version; the compiler rejects a missing dependency or a selected package
without the Events API before runtime binding resolution.

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
standard base64, timestamps decode RFC3339 (except leap seconds) and emit
canonical UTC RFC3339, ULIDs are uppercase, and models are open to unknown
fields. Optional absence and nullable `null` are different. Enums retain an
unknown-symbol arm.

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
