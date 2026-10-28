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
Rust crate for a Rust test harness). Explicit configuration selects only the named
outputs, even if another toolchain file exists. With no target configured, a single
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
      grants "New orders enter the customer's workflow.";
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

Transfers are member flags, not a block. An RPC's `download;` lets its response
carry a receive grant. An Operation's `upload;` accepts caller bytes that the
runtime durably stages before the handler reads them, so a recovered execution
can read them again. An Operation's `stream;` accepts caller bytes that go
directly to its running handler: nothing is staged, and the bytes cannot be
replayed, so an interrupted `stream` Operation fails and its caller starts a
new one. `upload` and `stream` are mutually exclusive; both use the same caller
helpers and wire protocol. See
[Files Transfer Patterns](../core/files-transfer-patterns.md).

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

Capabilities are named `area.action`, such as `sessions.read` or
`users.manage`; the ID is the API's prefix and that name,
`trellis.auth.v1:sessions.read`. Administrative Auth RPCs are allowed by
administrative capabilities, declared with the `administrative;` marker. Only
the built-in `trellis` package may use it, and Auth refuses any other API in
the `trellis.` namespace. Own-authority and own-session maintenance are fixed
session rights, not ordinary capability requests.

Every authored capability has a `title`, a `description`, and `grants`: what
holding it lets the holder do, shown when a user consents. A user approves that
`grants` statement, and authors keep it honest. Auth owns each capability's
consent revision and advances it when an accepted API changes the `grants`
text; a user's approval then no longer covers the capability until they approve
again. Other compatible changes, including adding actions to `allows`, reach
approved apps within the same API version; a change that breaks what users
agreed to belongs in a new API major version. Authors declare no revision
counter. API generation, accepted revision, consent revisions, and
action/membership introduction revisions are Auth-owned acceptance metadata.

A group is a developer-defined bundle of an API's capabilities. It is granted,
assigned to roles, mapped from identity providers, requested by participants,
and consented to exactly like a capability, and it stands for its members:

```trellis
capabilities {
  capability orders.read { ... }
  capability orders.cancel { ... }
  group support {
    title "Support";
    description "Look up and cancel customer orders.";
    grants "Can see and cancel any order.";
    capability orders.read;
    capability orders.cancel;
  }
}
service Desk { use orders { required group support; } }
```

A group's ID is the API prefix and its one-word name,
`acme.orders.v1:support`; `public` is reserved for the public capability. It
needs a title, description, grants, and at least one member, and lists only
capabilities of the same API. Groups are part of the API's meaning, so they are in both canonical
forms and change its digest. Whoever holds a group through a grant, role, or
mapping holds its current members, so a membership change reaches them at once.
Approval of a group covers its `grants` text, exactly as for a capability: an
approved app gets the group's current members, including new ones, until the
group's `grants` text changes and the user must approve again. A member's own
`grants` change does not reopen an approved group. A group containing an administrative capability is
administrative.

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
`observeStatusChanges` selects event consumption, not publication; provider publication
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
There are no general unions/generics, defaults, duration payload type, patterns,
or arbitrary string formats; a UUID or email is a `string`. `CursorQuery` and `CursorPage<T>`
are the only reserved finite generic forms.

Scalars are `string`, `bool`, `int32`, `uint32`, `int64`, `uint64`, `number`,
`bytes`, `timestamp`, `ulid`, and `json`. `json` is any JSON value (object,
array, string, number, bool, or null) carried unchecked: its schema accepts
anything, it takes no constraints, and it cannot be an event parameter or job
key. An optional `json` field distinguishes absent from a present `null`. Constraints are `min_length`, `max_length`,
`min`, `max`, `min_items`, and `max_items`. Constraints apply only through a
package-level named type alias. Both SDKs enforce them, so handlers never see an
out-of-bounds input and a violation is `ValidationError`: generated TypeScript
codecs check declared limits when decoding and encoding (a provider rejects the
request before its handler runs, and a caller refuses to send it), and Rust
providers check RPC and operation inputs against the contract's JSON Schema.
String lengths count Unicode code points. Durations in resource/retry config
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
Successful compilations may be reused from a bounded process-local cache only
for identical complete evidence, including presentation source and dependencies;
a claimed semantic digest alone is not a cache key. This reuses immutable
compiler output, never policy, resource eligibility, or authorization decisions.
Generated Rust constructors and runtime descriptors are compiler output, not
contract-as-code builders.

Published OCI source bundles contain the manifest, listed source, and frozen
resolution metadata—not canonical API JSON. Generated implementations are
disposable build/release artifacts, not committed source; run generation before
building local consumers and package generated code when distributing them.

## Pagination

A paginated RPC has optional `page: CursorQuery` input and returns
`CursorPage<Row>` directly. `page` is always present; only absent `nextCursor`
means completion. Empty pages may continue. Cursors are opaque, filter/scope
bound continuation positions, not authority or snapshot tokens. Services own
their default and maximum limits and reauthorize each page.
