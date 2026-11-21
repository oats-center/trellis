---
title: Workspaces
description: The registry location a package is published from is part of its identity, so independent communities can use the same package names without a central authority.
---

# Design: Workspaces

> **Status: implemented.** This document describes registry-based workspaces
> as built. The decisions taken during review are listed under
> [Decisions](#decisions), and what the implementation settled beyond them
> under [As built](#as-built).

## Prerequisites

- [trellis-idl.md](./trellis-idl.md): package manifests, dependencies, locks,
  digests, and published bundles
- [trellis-api-participants.md](./trellis-api-participants.md): how API,
  participant, and capability IDs are formed
- [../auth/service-updates.md](../auth/service-updates.md): how a deployment
  moves to a new build

## The problem

Every Trellis package has a name, set in the `[package]` table of its
`trellis.toml`. That name is the first part of every ID the package defines. A
package named `krishi` that declares `service SherpaService` defines the
participant ID `krishi.SherpaService`. An API `samples@v1` in the same package
has the ID `krishi.samples@v1`, and its capabilities look like
`krishi.samples.v1:samples.read`. Auth stores deployments, grants, roles, and
remembered consent under these IDs.

A package is published to an OCI registry, at a repository such as
`ghcr.io/krishi-diagnostics/krishi`. Before workspaces that location only said
where to download the package, and was not part of any ID.

Before workspaces, package names were global. Two communities that never
coordinated could both publish a package named `krishi`, one at
`ghcr.io/krishi-diagnostics` and one at `ghcr.io/qlever-llc`, and Trellis
treated them as the same package. When one depended on the other, `trellis-idl`
stopped with `package cannot depend on itself`, because both manifests said
`name = "krishi"`. If both were installed in one Trellis, their IDs collided in
Auth.

There is no registry of Trellis package names, and there should not be one.
Communities need a way to own their names without asking anyone.

## The idea

A **workspace** is the registry location a package is published from: a
registry host followed by a repository path, such as
`ghcr.io/krishi-diagnostics`. A package's repository is its workspace followed by
its name, `ghcr.io/krishi-diagnostics/krishi`, which is how Auth's
`Packages.Pull` already builds the address it pulls from.

The workspace becomes part of the package's identity. Whoever can push to
`ghcr.io/krishi-diagnostics` owns every package name under it. The registry
already enforces who may push there, so Trellis needs no authority of its own.

The cost, accepted with the decision: moving a package to another registry
changes its identity. [Changing a workspace](#changing-a-workspace) says what
that means.

## Identity

### Package identity

A package's full identity is `<workspace>/<name>`:

```text
ghcr.io/krishi-diagnostics/krishi
ghcr.io/qlever-llc/krishi
```

These are two different packages. A graph may contain both.

A workspace is written in one canonical form, so that two spellings of one
location never become two identities:

- registry host in lower case, followed by at least one path segment
  (`ghcr.io/krishi-diagnostics`, `localhost:5000/dev`)
- the host keeps an explicit port when it has one; there is no URL scheme,
  trailing slash, tag, or digest
- no implicit registry: Docker Hub is written `docker.io/<org>`
- at most 128 bytes
- the single word `localhost` is the one workspace without a path segment;
  it is reserved for packages that declare no workspace (see
  [Unpublished packages](#unpublished-packages))

### How IDs are written

IDs keep the shape they had. A workspace-qualified ID puts the workspace and
a `/` in front of the short ID. For Krishi's `krishi` package (the API `samples` and the group `lab` are
examples):

| What        | Short ID                         | Full ID                                                     |
| ----------- | -------------------------------- | ----------------------------------------------------------- |
| Package     | `krishi`                         | `ghcr.io/krishi-diagnostics/krishi`                         |
| Participant | `krishi.SherpaService`           | `ghcr.io/krishi-diagnostics/krishi.SherpaService`           |
| API         | `krishi.samples@v1`              | `ghcr.io/krishi-diagnostics/krishi.samples@v1`              |
| Capability  | `krishi.samples.v1:samples.read` | `ghcr.io/krishi-diagnostics/krishi.samples.v1:samples.read` |
| Group       | `krishi.samples.v1:lab`          | `ghcr.io/krishi-diagnostics/krishi.samples.v1:lab`          |

The **short ID** is the full ID without its workspace. It is what every ID
looked like before workspaces.

A full ID splits without ambiguity. Everything before the last `/` is the
workspace. Package names contain no `/` or `.`, so the package name runs from
there to the first `.`. A capability's name follows the last `:`, which still
works for a registry host with a port. Only the built-in `trellis` package has
IDs with no `/`.

The longest API ID the protocol accepts (`validate_api_id` in
`crates/protocol/src/identifiers.rs`) is 255 bytes, which makes room for the
workspace; it was 128 before.

### Where each form appears

| Place                                      | Form                                           |
| ------------------------------------------ | ---------------------------------------------- |
| `.trellis` source                          | Neither. Source names imports and local names. |
| `trellis.toml`                             | Workspace and name, as separate fields         |
| Generated symbols and module names         | Short ID (see [Effects](#effects))             |
| Generated descriptors, the wire, JWTs      | Full ID                                        |
| Auth's storage, audit records, logs        | Full ID                                        |
| CLI and Console human-readable output      | Short ID while it is unique, else full ID      |
| CLI `--format json` (what scripts read)    | Full ID                                        |
| CLI input (grants, roles, `--participant`) | Either; a short ID must be unique              |

IDL source never spells an ID, so it does not change. A file imports the
API `samples` from a dependency alias and selects `capability samples.read`;
the compiler turns that into the full ID.

The CLI and Console show the short ID until a second installed package has the
same short ID. Then both are shown in full, and a short ID typed on the command
line is refused with the full IDs it could mean. `--format json` output
always carries the full ID, so a script's parsing does not change when a
second package with the same short ID is installed. Logs always carry the full
ID, because a log line is often read without the installation at hand.

## Declaring a workspace

The workspace is declared in `trellis.toml`, next to the name:

```toml
[package]
name = "krishi"
workspace = "ghcr.io/krishi-diagnostics"
version = "2.1.0"

[sources]
sherpa = "sherpa.trellis"

[dependencies]
trellis = { package = "trellis", version = "0.100.0-rc.53" }
qlever = { package = "krishi", workspace = "ghcr.io/qlever-llc", version = "^1.0" }
```

The source has to carry the workspace, because IDs are computed at compile
time. The package's semantic digest covers its identity, so it covers the
workspace too. Publishing cannot add the workspace afterwards without changing
every digest the author has already tested against.

A registry dependency names its workspace with `workspace`. That replaced the
`registry` field, the `[registries]` table, and `default-registry`. The
dependency's full address is in the manifest, where a reader can see it. A path
dependency stays `{ package, path }`; its identity comes from its own
`trellis.toml`, so switching it to a registry dependency later changes nothing.

A dependency is the same package only when workspace and name both match. The
self-dependency check compares full identities, so the manifest above is
valid. In imports the two `krishi` packages are told apart by their aliases,
as any two dependencies are.

`trellis add` names a dependency after its package, so the second `krishi`
needs `--alias`. Without one, `add` refuses to replace a dependency whose
identity differs, comparing the identity a path dependency's own
`trellis.toml` declares, and leaves the manifest and lock as they were.
`trellis rm` takes an alias, a full identity, or a package name. An exact
alias or full identity wins; a package name that fits several dependencies is
refused with each candidate's alias and full identity.

`trellis.lock` moves to format 3, which keys each package by its full identity
and records the workspace it was pulled from. `trellis update` writes it.

## Unpublished packages

A package that declares no `workspace` gets the workspace `localhost`. This
follows container tooling: Docker fills in Docker Hub for an image name without
a registry, and Podman names locally built images `localhost/<name>`. Every
package then has the same ID shape. Krishi's service without a declared
workspace is `localhost/krishi.SherpaService`, and its short ID is still
`krishi.SherpaService`.

A `localhost` package can be a path dependency, it can be installed from
source with `trellis svc create --source` or the Console's Add service form,
and it can be used in tests. It cannot be published, and a published package
cannot depend on it. `localhost` names no shared location, so a release
published under it would claim a name nobody owns. A developer who runs a
registry on their own machine gives the package a real workspace such as
`localhost:5000/dev`, which is an ordinary workspace and publishes normally.

Every `localhost` package shares one unowned namespace. Two of them with the
same name collide, and Auth reports the collision as it did before. That is
fine for development and tests, which usually run against a fresh
installation.

A package that will be published should declare its workspace from the start.
Its IDs are then final before the first release, and installing it from source
in production gives it the same identity a later published release will have.
Adding a workspace later is a [workspace change](#changing-a-workspace).

## The built-in trellis package

The `trellis` package ships inside `trellis-server`. Its
authority is the server binary, not a registry, so it has no workspace, not
even `localhost`. It is the one exception to the common ID shape. Its IDs stay
as they are: `trellis.auth@v1`, `trellis.auth.v1:users.edit`. These are
the IDs that appear most often in grants and code, and they stay short.

A dependency on `trellis` names no workspace. The CLI fetches it from where the
Trellis project publishes it, `ghcr.io/oats-center/trellis` (the prefix in
`crates/builtin-contracts/trellis.toml`). That location is a download source
and never part of the identity.

Two reservations protect it:

- Short IDs starting with `trellis.` belong to the built-in package, as Auth
  enforces now.
- The package name `trellis` is reserved in every workspace. A package
  `ghcr.io/someone/trellis` would show short IDs such as `trellis.auth@v1`
  in the Console and on consent pages, where they would pass for the built-in
  APIs. The compiler and Auth refuse it.

Auth keeps its current check that supplied evidence for the built-in package
matches a digest the server trusts.

## Ownership checks

Ownership is checked wherever a package comes from a registry. A package handed
over directly is trusted as written, described last.

**Publish.** `trellis publish` pushes to `<workspace>/<name>` from the
manifest; there is no `--registry` flag. The registry refuses the push unless
the publisher's credentials allow writes there. That refusal is the ownership
check. A `localhost` package cannot be published.

**Pull into Auth.** `Auth.Packages.Pull` always pulls a package from its own
workspace, and accepts a release only when the workspace it declares is the
location Auth pulled it from. A release at
`ghcr.io/someone/krishi` that declares `ghcr.io/krishi-diagnostics` is refused.
Each package in the release's frozen resolution is pulled from its own
recorded workspace, by its manifest digest, and must declare that workspace
too. Auth already pulls dependencies from their recorded location; the new part
is the comparison. `trellis svc create|apply --package` takes the full
identity, as in `ghcr.io/krishi-diagnostics/krishi@2.1.0`, and Auth signs in
with the credentials of the registry the administrator added for that
workspace, or pulls anonymously when there is none.

**Dependency resolution in the CLI.** `trellis add`, `update`, and `install`
pull from the workspace the dependency names and apply the same comparison to
everything they download.

**Evidence supplied without a registry.** Source installs, the Console's Add
service form, and service self-updates hand Auth package files directly. Auth
accepts the workspace in those files as written. This is not a security
boundary. The workspace in a package's source is a name, used for
compatibility and to keep IDs apart, and registry push permission is what
protects published names. An administrator who installs from source already
trusts the code, and trusting the name in it adds nothing new. Auth records
nothing extra about these packages and shows no separate status for them.

**What ownership does not cover.** Trellis trusts the registry's access
control. If an organization loses its registry path and someone else takes it,
the new holder can publish under that workspace. Existing locks and
installations still pin exact digests, so they keep the releases they already
have.

## Private copies

A private copy that changes anything is a fork. It is a different package and
is published under the forker's own workspace, with new IDs. An unmodified copy
at another location, such as a mirror inside a network without internet access,
is not supported in this release.

## Changing a workspace

Changing a package's workspace means editing `workspace` in its `trellis.toml`
and publishing again. There is no move command. The workspace is part of the
identity, so the result is a new package with new participant, API, and
capability IDs. Renaming a package is the same case.

A new identity needs a new deployment. `Deployments.Apply` looks up the
deployment's current participant ID in the incoming package evidence
(`crates/runtime/src/platform/auth/rpc_deployments.rs`), and a build under a
new workspace has a different participant ID, so it cannot be applied to an
existing deployment. Force-replace does not help either, because it replaces
incompatible APIs and keeps the participant. The administrator creates a new
deployment for the new package identity and removes the old deployment when
it is no longer needed. Grants and roles that named the old IDs do not carry
over: the administrator grants the new capabilities, and users consent again
under the new IDs. Consumers in other packages are rebuilt against the new
workspace before they can call it. The new deployment's resources are new and
start empty; data the old deployment held moves only through the service
itself, if the service offers a way to export and import it.

## Effects

### NATS subjects

Event subjects already carry the full API ID, base64url-encoded:
`events.v1.<token>.<Event>`. They take the workspace with no format change. For
`ghcr.io/krishi-diagnostics/krishi.samples@v1` the token is
`Z2hjci5pby9rcmlzaGktZGlhZ25vc3RpY3Mva3Jpc2hpLnNhbXBsZXNAdjE`.

RPC and Operation subjects used not to carry the package at all. They were
built from the API's short name and the action, as in `rpc.v1.samples.Get`, so
two APIs named `samples@v1` in different packages shared that subject and,
through it, one NATS queue group, and callers' requests were split between two
unrelated services. That happened between packages with different names too.

RPC and Operation subjects now carry the API identity token the way events and
live routes do: `rpc.v1.<token>.<Action>` and
`operations.v1.<token>.<Action>` (`derive_rpc_subject` and
`derive_operation_subject` in `crates/protocol/src/subjects.rs`). The built-in
APIs follow the same rule; the runtime's and SDKs' hand-written subjects, such
as Jobs `History`, are derived from the API ID or taken from generated
descriptors. Auth stores each accepted action's route
(`auth_api_actions.route_json`) as derived, and a fresh installation derives
them again (see [Migration](#migration)).

Only the canonical start subject of an Operation gains the API identity. Once
an Operation starts, its reference routes later status and control calls by
API ID and owning deployment ID, and that owner route stays as it is. When two
deployments serve the same API, a reference restored by the caller still
reaches the deployment that owns the work.

### Permissions

NATS permissions are subjects, so they change with the subjects. Capability IDs
in grants carry the workspace. The administrative marker stays limited to the
built-in package. Sharing an API between deployments still needs the
administrator's acknowledgement, and now only applies to two deployments of
the same API identity.

### Generated SDKs

Generated client methods and handler names come from API and action names, so
`client.samplesGet` stays `client.samplesGet`. Descriptors carry full IDs.

Generated module and export names are built from IDs, and full IDs would make
them unreadable (`ghcr_io_krishi_diagnostics_krishi_sherpa_service`). They are
built from the short ID instead. The TypeScript generator names an API by its
name and, when two APIs in the graph share a name, falls back to the short ID,
then to the full ID only when the short IDs also collide (`api_modules` in
`crates/codegen-ts/src/lib.rs`). The Rust generator names modules by short ID
and falls back the same way. A Krishi package that uses both `krishi`
packages' `samples` APIs gets `GhcrIoKrishiDiagnosticsKrishiSamplesV1`, which
is long and also rare.

Different full IDs can still produce the same generated name, because the
generator splits on punctuation: `ghcr.io/acme-lab` and `ghcr.io/acme/lab`
both become `GhcrIoAcmeLab`. When generated names collide, the generator adds
`_` and the first eight hex digits of the SHA-256 of each full identity, to
the colliding names only (`trellis_idl::generated_names`, shared by both
generators). Every other name stays readable. A participant or app that uses
or serves two APIs of the same name takes its flat method names, such as
`client.samplesGet`, from those generated names instead.

The generator checks the flat methods themselves, not only the authored
action names. A qualified action and a different action can still give the
same method: `GhcrIoAlphaKrishiOrdersV1.Get` and an RPC named
`GhcrIoAlphaKrishiOrdersV1Get` are both `ghcrIoAlphaKrishiOrdersV1Get`. Each
action whose method is shared and is not yet qualified then takes its API's
name as well, here `shopGhcrIoAlphaKrishiOrdersV1Get`, until every method is
distinct (`facade_action_names` in `crates/codegen-ts/src/lib.rs`).
Generation fails with the colliding actions named if no further qualification
helps.

### Evidence

Generated clients hand Auth a self-contained evidence bundle, and Auth compiles
it again offline. Today the evidence compiler (`compile_evidence` in
`crates/idl/src/lib.rs`) finds packages by name, checks each against its
canonical prelude by name, and follows dependency edges by the package name in
the prelude. With two `krishi` packages in one graph, that lookup is ambiguous.

The evidence carries full identities, workspace plus name, on the evidence
root, on each package, and on each dependency edge. The import aliases stay as
they are in source. The offline compiler resolves every package and edge by
full identity, so Auth compiles the same graph the author did.

### Package resolution in Auth

`Auth.Packages.Resolve` satisfies a dependency by full identity and version.
A dependency without a workspace matches only `localhost` packages or the
built-in `trellis`.

## Migration

Trellis is pre-release, so this is a clean break with no compatibility path
for old formats or old IDs. Upgrading means starting a fresh installation.

**Every ID changed once.** Each installed package either declares a workspace
or becomes a `localhost` package, so every ID outside the built-in package
gains a workspace: `krishi.SherpaService` becomes
`localhost/krishi.SherpaService` or
`ghcr.io/krishi-diagnostics/krishi.SherpaService`. Semantic digests change with
them. Stored IDs are not rewritten, and there is no migration code. Following
the project's pre-release rule, the Auth schema's baseline migration is edited
in place.

**Every store starts empty.** A fresh Auth database alone is not enough,
because other stores refer to what it held. Recorded events refer to
authorization contexts in `platform.sqlite`, and recreated deployments get new
IDs, so resource data would not reattach to them. The fresh installation
starts without:

- `platform.sqlite` (Auth: deployments, grants, roles, consent, package
  evidence, authorization contexts)
- `events.sqlite`, `jobs.sqlite`, and `health.sqlite`
- the JetStream streams (`trellis`, `JOBS`, `JOBS_WORK`, `JOBS_ADVISORIES`,
  `TRELLIS_HEALTH`, `trellis_consumer_dlq`, `trellis_consumer_replays`)
- the KV buckets, object stores, and streams that hold service resource data
- earlier `trellis-server backup` archives, which restore the old IDs and
  cannot be used with the new release

Before upgrading, an operator exports what they want to keep: each service's
own data through that service's export, if it has one, and any records the
operator needs for reference, such as the event history, audit records, and the
list of deployments, grants, and roles. Anything wanted back is imported again
through the services themselves after the new deployments exist.

**What else breaks.**

- RPC and Operation subjects change. Every service, device, and app has to be
  regenerated and rebuilt with the new SDK. Builds from rc.52 cannot reach the
  new release's APIs.
- `trellis.lock` format 2, the `registry` dependency field, `[registries]`, and
  `default-registry` are no longer accepted. `trellis update` writes format 3
  after the manifest names workspaces.
- The OCI config of a published package names it by full identity (`id`), so
  releases published before workspaces, the built-in `trellis` package's
  included, cannot be read.
- Releases published before workspaces declare none, so Auth will not pull
  them. Packages are published again with a workspace.

**What an installation such as Krishi's (on rc.52) does.**

1. Upgrade the `trellis` CLI. Add `workspace = "ghcr.io/krishi-diagnostics"`
   to each package Krishi publishes, and name the workspace of each registry
   dependency, such as `ghcr.io/qlever-llc` for the other `krishi`. Run
   `trellis update`, then build and publish each package. The `qlever-llc`
   community does the same for its packages; Krishi's `krishi` can then
   depend on theirs.
2. On krishi-dev, export the service data and records worth keeping, as listed
   above. Stop `trellis-server`, remove the installation's data (the SQLite
   databases and the JetStream storage), and start the new `trellis-server` as
   a fresh installation.
3. Create each deployment with `trellis svc create --package
ghcr.io/krishi-diagnostics/<name>@<version>`, grant roles, and roll out the
   new builds. Import the exported service data through the services. Users
   consent again on first use.

## Decisions

These were open during review and are now settled.

1. **Supplied evidence that claims a workspace is accepted.** Source installs,
   the Console form, and service updates are trusted as written. The workspace
   is naming, not a security boundary; registry push permission protects
   published names. Auth records no origin and shows no "not checked" status.
2. **There is no move, and a workspace change needs a new deployment.**
   Changing a workspace is editing `workspace` and republishing, which produces
   a new package identity. `Deployments.Apply` cannot apply it to an existing
   deployment, so the administrator creates a new deployment, removes the old
   one when it is no longer needed, grants again, and users consent again.
   Consumers rebuild. Resources are not re-identified to keep data across the
   change. There are no "moved to" records, no move command, and no policy
   rewrite.
3. **Packages without a declared workspace get `localhost`.** Every package
   except the built-in `trellis` has the same ID shape. `localhost` packages
   cannot be published at all, because `localhost` names no shared location. A
   local registry uses a real workspace such as `localhost:5000/dev`.
4. **Short IDs in human-readable output, full IDs for machines.** The CLI and
   Console show the short ID while it is unique and the full ID when two
   installed packages share it. `--format json`, the wire, Auth storage, and
   logs always use the full ID.
5. **The subject change ships with workspaces.** RPC and Operation subjects
   carry the API identity token in the same release, so each installation
   rebuilds once.
6. **No data is carried across the ID change.** Every installation running
   0.100 today is a development or test installation; Krishi's production
   still runs 0.11 and moves to 0.100 later. So the release starts from a
   fresh installation, as the pre-release rule says, with no migration code
   that rewrites stored IDs. Krishi's production cutover from 0.11 should
   happen after this release, so it isn't done twice.
7. **The clean reset is a fresh installation.** Every store that depends on
   Auth starts empty with it: the SQLite databases, the JetStream streams, the
   resource buckets and stores, and old backups. Operators export first and
   import again through the services.
8. **Evidence carries full identities.** The evidence root, each package, and
   each dependency edge name workspace and name, and the offline compiler
   resolves by full identity. Source keeps local import aliases.
9. **Operations keep their owner route.** Only the canonical start subject
   gains the API identity. A started Operation's reference routes by API ID
   and owning deployment ID.
10. **Generated names stay readable and never collide.** Colliding names, and
    only those, get a short deterministic suffix derived from a hash of the
    full identity.
11. **No mirrors in this release.** Auth and the CLI always pull from the
    workspace's own registry. A modified copy is a fork with its own
    workspace.

## Verification

- Compile a `krishi` package that imports the other `krishi` (different
  workspaces), install its generated evidence through Auth, and check that
  both APIs resolve to their intended full identities.
- Start an Operation with two deployments serving the same API, restore the
  returned reference, and check that a later status or control request
  reaches the owning deployment.
- Compile a consumer that calls two APIs whose generated names collide, such
  as ones from `ghcr.io/acme-lab` and `ghcr.io/acme/lab`, and check that it
  builds and each call reaches its own API.

## As built

The implementation settled these points the decisions left open.

- **Where identity lives.** `[package] workspace` sits beside `name`. A
  package that declares none is `localhost/<name>`; writing
  `workspace = "localhost"` is refused, so there is one spelling. A
  dependency is `{ package, path }` or `{ package, workspace, version }`, and
  only `trellis` is a registry dependency without a workspace. Workspaces are
  validated by `trellis_protocol::validate_workspace`: a lower-case host that
  has a `.` or a port, or is `localhost`, at least one OCI path segment, at
  most 128 bytes.
- **Lock and evidence fields.** Lock format 3 keys packages and dependency
  edges by `id` (the full identity) and records a registry package's
  `source.workspace`; a lock whose recorded workspace is not the package's own
  is invalid, so there are no mirrors. Evidence keeps its fields
  (`rootPackage`, `packages[].name`, and each canonical `dependency` line) and
  puts the full identity in each. A published package's OCI config names it as
  `id`. JSON Schema definitions are keyed by full type identity, so `$ref`
  pointers escape `/` as `~1`.
- **The built-in package from the build.** A `trellis` dependency that the
  CLI's own build of the built-in package satisfies is locked as the release
  at `ghcr.io/oats-center/trellis` and taken from that build, byte for byte the
  release the Pipeline publishes, without downloading it. `Packages.Pull`
  compiles a release against the built-in package the server ships, and the
  compiled digests must still equal the ones the release was published with.
- **Resolution in Auth.** A registry dependency names its identity. A path
  dependency or a manifest-less import names only a package: it is the one
  supplied package of that name, and otherwise the `localhost` package or the
  built-in `trellis`.
- **Registries.** An administrator's registry is a name, a prefix that is a
  workspace, and optional credentials. `Packages.Pull` takes the package's full
  identity and signs in with the registry whose prefix is that workspace, or
  pulls anonymously; several registries naming one workspace are refused
  rather than chosen between. A prefix is checked against the registry host
  as a workspace spells it, so Docker Hub's is `docker.io/<org>`; the
  transport's endpoint for that host, `index.docker.io`, is used only to
  connect.
- **Short IDs in the CLI.** Listing tables (`svc list`, `apis list`,
  `resources list`) show each ID's short form on a terminal while no other row
  shares it. Detail views, piped tab-separated output, and `--format json`
  keep full IDs. `--participant` and role and app capability flags take a
  short ID that names exactly one candidate; the capability flags look the
  short ID up in Auth's capability catalog.
- **Short IDs in the Console.** Pages label IDs by short form while none of the
  IDs on the page share it (`web/src/lib/console/ids.ts`): the services list,
  the APIs page's package groups, and the Add service form. A published
  package is looked up by its full identity, with no registry to choose.

## Out of scope

- Signing releases or verifying publishers by key. The registry's access
  control is the only ownership proof.
- Mirrors and unmodified copies at another location, in Auth and in the CLI.
  Both pull from the workspace's own registry.
- Aliases or rewrites that keep old IDs working after a workspace change.
