# Trellis APIs and Participants

## Source of truth

Trellis contracts are native `.trellis` source packages. The compiler resolves
the locked package graph into a native semantic model and generated runtime
descriptors. There is no canonical API/participant JSON authoring or exchange
format, and generated descriptor constructors are not a second authoring API.

Published OCI bundles contain the manifest, explicitly listed source files, and
frozen resolution metadata. Runtime JSON Schema is a validator projection only.

## Identity and evidence

- API identity is `<package>.<ApiName>@v<major>`.
- Participant identity is `<package>.<lexical-path>`.
- Capability identity is `<api-id>::<name>`.
- Resource identity is the owner binding, participant identity, kind, and local
  resource name.

Package version and API metadata are descriptive. Machine meaning is identified
by the semantic package digest. Participant evidence combines that package
digest with its exact lexical participant path; source filenames and generated
symbol spellings are not identity.

Package evidence carries presentation-preserving canonical source for the exact
dependency closure. The server parses and resolves it, verifies every frozen
edge/version/digest, and recomputes semantic digests. Externally supplied
evidence for the reserved `trellis` package must match an explicitly trusted
installed digest.

## APIs, capabilities, and grants

APIs define RPCs, Operations, Events, Feeds, API-scoped errors, and one
capabilities block. A capability groups exact actions and carries human consent
wording. A capability declaration or approval does not itself confer runtime
authority. The server derives and signs the exact selected permission atoms for
the current revision and current policy ceiling.

Capability approval is keyed by qualified capability ID and a consent digest of
its description and consequence. Changing those words requires new approval;
changing a title or the exact actions covered does not, provided newly selected
actions remain within the approved capability and delegation ceiling. Public
actions remain selected exact actions, not anonymous wildcard grants.

`GrantBinding` is the single current authority record. It stores exact grants,
platform privileges, approval mode, approved capability fingerprints, approved
resource commitments, and the delegation ceiling. Capability-mode approval and
exact administrative grants remain distinct. Readiness reports missing required
capabilities/resources separately from temporary provider liveness.

## Participants and selection

Top-level participants are `service`, `device`, `app`, and `agent`. Only a
device may contain one named optional or required `app` or `agent` companion.
That companion is the device's lexical child, not an independently bootstrapped
top-level participant. It is activated only through the parent device flow and
uses a separate user-owned `GrantBinding`, ordinary login session, authorization
context, resource bindings, and connection. Parent and child authority are never
merged.

Companion activation exposes server-computed child consent. The user submits a
separate child `Approval` fenced by the consent decision digest, installed
revision, expected grant revision, and exact eligible capability/resource
selections. Stale or mismatched consent is rejected; successful approval and
device authority are committed before the child login session is created.

`implements Api;` means complete provider implementation. A nonempty
`use Api { ... }` body is the sole interaction-selection location; `use Api;` is
legal and selects nothing. An Operation selection includes its complete
lifecycle and declared signals. There is no whole-API required/optional mode,
action-level optional flag, participant-local schema, or API State declaration.

## Resource lifecycle

State, KV, Store, Job, and Consumer declarations include a nonempty title and
description and may be optional. Approval records kind-specific commitments: KV
history/TTL/capacity, Store TTL/capacity, or enabled status for State, Job, and
Consumer. Desired capacities are hints, not approval and not actual limits.

Physical resources have stable identity after first allocation. Removing a
declaration detaches and revokes access but does not delete data created by this
architecture. A compatible, newly authorized re-add of the same logical identity
reattaches it; rename or kind change creates a new identity. Explicit
administrator `Resources.Destroy` is the normal destructive path.

State is one typed value per resource. KV is typed key/value history. Both use
positive representation versions and direct historical-to-current SDK migrations
for representations written by this architecture; reads and conflict projection
never rewrite stored bytes. Store remains raw object bytes.

## Generation and runtime presentation

Each selected language receives one ordinary generated package with APIs,
participants, types, codecs, and descriptors. Generated packages need neither
the compiler nor CLI at runtime. Clients connect through their generated
participant facade; assignments, routes, exact grants, resources, and signed
contexts are resolved by Trellis rather than supplied as bootstrap assertions.

Operations are durable, restartable, at-least-once caller-visible workflows.
Jobs are service-private queued execution. Durable Consumers use local
per-process concurrency, managed retry, and per-Consumer Events DLQ history.
These are distinct runtime facilities even when they share JetStream mechanics.
