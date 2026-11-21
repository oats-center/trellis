---
title: Workspaces
description: Proposal to make the registry location a package is published from part of its identity, so independent communities can use the same package names without a central authority.
---

# Design: Workspaces

> **Status: proposal awaiting review.** Nothing in this document is
> implemented. It records the decision to use registry-based workspaces and
> works out what that decision changes. The questions it leaves open are at the
> end, each with a recommendation.

## Prerequisites

- [trellis-idl.md](./trellis-idl.md): package manifests, dependencies, locks,
  digests, and published bundles
- [trellis-api-participants.md](./trellis-api-participants.md): how API,
  participant, and capability IDs are formed
- [../auth/service-updates.md](../auth/service-updates.md): how a deployment
  moves to a new build, including the waiting state after a force-replace

## The problem

Every Trellis package has a name, set in the `[package]` table of its
`trellis.toml`. That name is the first part of every ID the package defines. A
package named `krishi` that declares `service SherpaService` defines the
participant ID `krishi.SherpaService`. An API `samples@v1` in the same package
has the ID `krishi.samples@v1`, and its capabilities look like
`krishi.samples.v1:samples.read`. Auth stores deployments, grants, roles, and
remembered consent under these IDs.

A package is published to an OCI registry, at a repository such as
`ghcr.io/krishi-diagnostics/krishi`. Today that location only says where to
download the package. It is not part of any ID.

So package names are global. Two communities that never coordinated can both
publish a package named `krishi`, one at `ghcr.io/krishi-diagnostics` and one at
`ghcr.io/qlever-llc`. Trellis treats them as the same package. When one depends
on the other, `trellis-idl` stops with `package cannot depend on itself`, because
both manifests say `name = "krishi"`. If both were installed in one Trellis,
their IDs would collide in Auth.

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
changes its identity. [Moving a package](#moving-a-package) describes the
explicit path for that.

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

### How IDs are written

IDs keep their current shape. A workspace-qualified ID puts the workspace and
a `/` in front of today's ID. For Krishi's `krishi` package (the API `samples` and the group `lab` are
examples):

| What        | Short ID                         | Full ID                                                     |
| ----------- | -------------------------------- | ----------------------------------------------------------- |
| Package     | `krishi`                         | `ghcr.io/krishi-diagnostics/krishi`                         |
| Participant | `krishi.SherpaService`           | `ghcr.io/krishi-diagnostics/krishi.SherpaService`           |
| API         | `krishi.samples@v1`              | `ghcr.io/krishi-diagnostics/krishi.samples@v1`              |
| Capability  | `krishi.samples.v1:samples.read` | `ghcr.io/krishi-diagnostics/krishi.samples.v1:samples.read` |
| Group       | `krishi.samples.v1:lab`          | `ghcr.io/krishi-diagnostics/krishi.samples.v1:lab`          |

The **short ID** is the full ID without its workspace. It is what every ID
looks like today.

A full ID splits without ambiguity. Everything before the last `/` is the
workspace. Package names contain no `/` or `.`, so the package name runs from
there to the first `.`. A capability's name follows the last `:`, which still
works for a registry host with a port. An ID with no `/` has no workspace.

The longest API ID the protocol accepts (`validate_api_id` in
`crates/protocol/src/identifiers.rs`) grows from 128 bytes to 255 bytes, to
make room for the workspace.

### Where each form appears

| Place                                      | Form                                           |
| ------------------------------------------ | ---------------------------------------------- |
| `.trellis` source                          | Neither. Source names imports and local names. |
| `trellis.toml`                             | Workspace and name, as separate fields         |
| Generated symbols and module names         | Short ID (see [Effects](#effects))             |
| Generated descriptors, the wire, JWTs      | Full ID                                        |
| Auth's storage, audit records, logs        | Full ID                                        |
| CLI and Console text output                | Short ID when it is unique in the installation |
| CLI `--format json`                        | Full ID                                        |
| CLI input (grants, roles, `--participant`) | Either; a short ID must be unique              |

IDL source never spells an ID, so it does not change. A file imports the
API `samples` from a dependency alias and selects `capability samples.read`;
the compiler turns that into the full ID.

The CLI and Console show the short ID until a second installed package has the
same short ID. Then both are shown in full, and a short ID typed on the command
line is refused with the full IDs it could mean. Logs always carry the full
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

A registry dependency names its workspace with `workspace`. That replaces
today's `registry` field, the `[registries]` table, and `default-registry`. The
dependency's full address is in the manifest, where a reader can see it. A path
dependency stays `{ package, path }`; its identity comes from its own
`trellis.toml`, so switching it to a registry dependency later changes nothing.

A dependency is the same package only when workspace and name both match. The
self-dependency check compares full identities, so the manifest above is
valid. In imports the two `krishi` packages are told apart by their aliases,
as any two dependencies are today.

`trellis.lock` moves to format 3, which keys each package by its full identity
and records the workspace it was pulled from. `trellis update` writes it.

## Unpublished packages

A package that declares no workspace has no workspace. Its IDs are short IDs,
exactly as today: `krishi.SherpaService`. It can be a path dependency, it can be
installed from source with `trellis svc create --source` or the Console's Add
service form, and it can be used in tests. It cannot be published, and a
published package cannot depend on it.

Names without a workspace are one shared space with no owner, which is how
every name works today. Two of them can still collide, and Auth reports the
collision as it does now. That is fine for development and tests, which
usually run against a fresh installation.

A package that will be published should declare its workspace from the start.
Its IDs are then final before the first release, and installing it from source
in production gives it the same identity a later published release will have.
Adding a workspace to a package that is already installed is a move.

## The built-in trellis package

The `trellis` package ships inside `trellis-server`. Its
authority is the server binary, not a registry, so it has no workspace. Its IDs
stay as they are: `trellis.auth@v1`, `trellis.auth.v1:users.edit`. These are
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
over directly carries a claim instead, described last.

**Publish.** `trellis publish` pushes to `<workspace>/<name>` from the
manifest; there is no `--registry` flag. The registry refuses the push unless
the publisher's credentials allow writes there. That refusal is the ownership
check. A package without a workspace cannot be published.

**Pull into Auth.** `Auth.Packages.Pull` accepts a release only when the
workspace it declares is the location Auth pulled it from, or a registry the
administrator added as a mirror of that workspace (see
[Mirrors and private copies](#mirrors-and-private-copies)). A release at
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
service form, and service self-updates hand Auth package files directly. No
registry is involved, so the workspace in those files is a claim. The
administrator who installs from source vouches for it, as they vouch for the
code. Auth records where each package came from (pulled from a named registry,
or supplied by a named user) and the Console shows supplied packages that
claim a workspace as not checked against their registry. A service's own
update must name its deployment's participant by full ID, so a service cannot
change its workspace by submitting an update. A changed workspace only arrives
through an administrator's move.

**What ownership does not cover.** Trellis trusts the registry's access
control. If an organization loses its registry path and someone else takes it,
the new holder can publish under that workspace. Existing locks and
installations still pin exact digests, so they keep the releases they already
have.

## Mirrors and private copies

A mirror holds byte-for-byte copies of another location's releases, for
example a registry inside a network without internet access. Copying a release
with ordinary OCI tools keeps its manifest digest, and the frozen resolution
pins dependencies by that digest.

A mirror does not get its own workspace. Its packages keep the identity of the
workspace they came from. An administrator adds the mirror to Auth and says
which workspace it mirrors:

```sh
trellis registries set lab-mirror --prefix registry.lab.internal/krishi \
  --mirror-of ghcr.io/krishi-diagnostics
```

Auth then accepts `ghcr.io/krishi-diagnostics` packages from either location.
The administrator's statement is the trust decision, made for their own
installation.

A private copy that changes anything is a fork. A fork is a different package
and is published under the forker's own workspace, with new IDs. Publishing a
modified copy under the original workspace's identity at another location
fails the pull check.

## Moving a package

### When it happens

A package moves when its identity changes: it moves to another registry, its
organization is renamed, or a package without a workspace gains one. Every
existing installation on Trellis 0.100.0-rc.52 or earlier needs the last
kind of move (see [Migration](#migration)).

A renamed package is the same case with a different name. The move below
handles both.

### The move

A move is an administrator action, applied together with the first release
from the new identity:

```sh
trellis svc <deployment-id> apply \
  --package ghcr.io/krishi-diagnostics/krishi@2.1.0 --move
```

The Console offers the same move when an administrator applies a release whose
package identity differs from the one the deployment runs. The review lists
everything the move changes before the administrator confirms.

A package moves as a whole, because its APIs and participants share its
identity. In one transaction, Auth:

1. Compiles the new release and installs it, as a new revision under the new
   participant ID, for every deployment of a participant in the old package.
2. Accepts the release's APIs under their new IDs. Each one is checked against
   the API it replaces exactly as a new revision is checked today, so a move
   cannot slip in an incompatible change.
3. Rewrites the old IDs to the new ones in Auth's current policy: deployments,
   deployment capability selections, roles, direct grants, identity-provider
   mappings, remembered consent, OAuth grants, and any other current-policy row
   that names a moved ID. Capability identity generations and consent
   revisions carry over unchanged.
4. Keeps each deployment's resources, with their data and physical storage.
5. Writes one audit record naming the old identity, the new one, and the
   administrator.
6. Reissues the sessions the change affects, as it does after a role change.

Auth refuses the move if anything is already installed or accepted under the
new identity. A move back is another move.

### What happens to grants and consent

Users and administrators keep everything they granted. A role that held
`krishi.samples.v1:samples.read` now holds
`ghcr.io/krishi-diagnostics/krishi.samples.v1:samples.read` at the same
generation. A user who approved an app's request for it does not see a consent
page again, because the consent revision carries over and the `grants` text is
the same. If the new release changes a capability's `grants` text, the
ordinary rule applies and users approve that capability again.

### What does not move

- **History.** Installed participant revisions, accepted package evidence, and
  API verification snapshots are immutable in Auth's database. They keep the
  old IDs and stay readable as history.
- **Old builds.** A running build has the old IDs compiled into it. Its
  instances wait the way instances wait after a force-replace (step 7 in
  [service updates](../auth/service-updates.md#flow)): their sessions hold
  only `serviceUpdates.submit` until a build from the new identity connects.
- **Consumers in other packages.** A service or app in another package that
  uses a moved API was compiled against the old API ID. It keeps its rewritten
  selections, so its next build, made against the new workspace, asks for
  nothing new and applies without approval. Until then its calls to the moved
  API fail.
- **Event history and work in flight.** Events published before the move are
  stored under subjects derived from the old API ID. Consumers subscribe to
  the new subjects and do not see them. Operations started before the move
  belong to the old ID. A move belongs in a quiet period.

The rebuild of consumers is the real cost of a move. The alternative, keeping
old IDs as aliases for new ones, adds dual-read paths to admission, routing,
and grants. This proposal leaves it out.

## Effects

### NATS subjects

Event subjects already carry the full API ID, base64url-encoded:
`events.v1.<token>.<Event>`. They take the workspace with no format change. For
`ghcr.io/krishi-diagnostics/krishi.samples@v1` the token is
`Z2hjci5pby9rcmlzaGktZGlhZ25vc3RpY3Mva3Jpc2hpLnNhbXBsZXNAdjE`.

RPC and Operation subjects do not carry the package at all. They are built
from the API's short name and the action, as in `rpc.v1.samples.Get` (see
`derive_rpc_subject` in `crates/protocol/src/subjects.rs`). Two APIs named
`samples@v1` in different packages share that subject and, through it, one NATS
queue group, so callers' requests are split between two unrelated services.
This already happens today between packages with different names. Workspaces
mean nothing on the wire until it is fixed.

This proposal changes RPC and Operation subjects to carry the API identity
token the way events do: `rpc.v1.<token>.<Action>` and
`operations.v1.<token>.<Action>`. Live routes already use the token. The
built-in APIs follow the same rule, including the hand-written subjects in the
runtime such as `rpc.v1.jobs.History`. Auth stores each accepted action's
route (`auth_api_actions.route_json`), so those stored routes are derived again
(see [Migration](#migration)).

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
built from the short ID instead. The TypeScript generator today names an API
by its short name and falls back to the whole API ID when two APIs share a name
(`api_modules` in `crates/codegen-ts/src/lib.rs`). The fallback becomes the
short ID, then the full ID only when the short IDs also collide. A Krishi
package that uses both `krishi` packages' `samples` APIs gets
`GhcrIoKrishiDiagnosticsKrishiSamplesV1`, which is long and also rare.

### Resources

A deployment's resources are identified today by a digest that includes the
participant ID (`crates/runtime/src/platform/auth/resources.rs`). A move would
give every resource a new ID and empty storage. Under this proposal a
deployment's resources belong to the deployment: Auth finds a deployment's
existing resource by deployment, kind, and local name, and keeps its stored ID
and allocation across a move. A user-owned resource records the app request's
participant ID, and the move rewrites that column.

### Package resolution in Auth

`Auth.Packages.Resolve` satisfies a dependency by full identity and version.
A dependency without a workspace matches only packages without one or the
built-in `trellis`.

## Migration

Trellis is pre-release, so this is a clean break with no compatibility path
for old formats. Two things still need care, because installations hold data
people care about: user accounts, grants, and the data in each deployment's
resources.

**What stays valid.** Every package installed today has no workspace. Its IDs
are short IDs, which remain valid as the IDs of workspace-less packages. If the
canonical source prints nothing for an absent workspace, which this proposal
requires, their semantic digests do not change either. Auth's stored
deployments, grants, consent, and evidence therefore need no rewrite on
upgrade, and the database schema does not change.

**What breaks.**

- RPC and Operation subjects change. Every service, device, and app has to be
  regenerated and rebuilt with the new SDK. Builds from rc.52 cannot reach the
  new release's APIs.
- Auth derives every accepted action's stored route again on the first start
  of the new release, from the evidence it retains. This rewrites derived data
  only.
- `trellis.lock` format 2, the `registry` dependency field, `[registries]`, and
  `default-registry` are no longer accepted. `trellis update` writes format 3
  after the manifest names workspaces.
- Releases published before workspaces declare none, so Auth will not pull
  them. Packages are published again with a workspace.

**What an installation such as Krishi's (on rc.52) does.** Every build has to
be rebuilt anyway, so the cheapest path moves the packages in the same window:

1. Upgrade the `trellis` CLI. Add `workspace = "ghcr.io/krishi-diagnostics"`
   to each package Krishi publishes, and name the workspace of each registry
   dependency, such as `ghcr.io/qlever-llc` for the other `krishi`. Run
   `trellis update`, then build and publish each package. The `qlever-llc`
   community does the same for its packages; Krishi's `krishi` can then
   depend on theirs.
2. Upgrade `trellis-server`. Auth derives the stored routes again on its first
   start. From here until step 4, rc.52 builds cannot call the new APIs.
3. For each deployment, run `trellis svc <id> apply --package
ghcr.io/krishi-diagnostics/<name>@<version> --move`. Grants, consent, and
   resource data carry over.
4. Roll out the new builds. Consumers and apps in other packages are rebuilt
   against the new workspaces, and their updates apply without approval.

An installation can also upgrade without moving. Its packages keep working
without a workspace once they are rebuilt with the new SDK, and they can move
later.

## Out of scope

- Signing releases or verifying publishers by key. The registry's access
  control is the only ownership proof.
- Mirrors for the CLI. `trellis add` and `update` pull from the workspace
  itself.
- Following a move automatically in other installations.
- Aliases that keep old IDs working after a move.

## Open questions

1. **Should Auth refuse supplied evidence that claims a workspace, when an
   administrator installs from source?** Refusing would force a publish before
   any production install. Recommendation: accept it, record the origin, and
   show the package as not checked against its registry in the Console and in
   `trellis svc <id> show`.

2. **Should the old registry location carry a machine-readable "moved to"
   record, so installations learn about a move?** Whoever controls the old path
   could publish one there, so it would need no new authority. It would not
   help when the old location is already gone, which is a common reason to
   move. Recommendation: not now. Revisit if a widely used public package has
   to move.

3. **Should Auth refuse packages without a workspace in production
   installations?** That would make collisions impossible there, but every
   existing installation runs such packages today. Recommendation: allow them,
   and label them "no workspace" in the Console and CLI.

4. **Should the CLI show short IDs only while they are unique, or always show
   full IDs?** Always showing full IDs is simpler to explain but makes every
   grant and participant line long. Recommendation: short while unique, full
   when two installed packages share a short ID, and full in JSON output.

5. **Should the RPC and Operation subject change ship in its own release,
   ahead of workspaces?** It fixes an existing collision between packages that
   use the same API name, so it stands on its own. But it forces every service
   to be rebuilt, and so does a move; two releases would mean two rebuilds for
   an installation that moves. Recommendation: ship both in one release, so
   each installation rebuilds once.
