# Design: Trellis CLI

Status: Revision 7 command and project-tooling model.

## Principles

- Commands expose current runtime operations directly; removed authorization
  models do not survive as aliases or wrappers.
- Machine-readable output uses `--format json` and reserves stdout for JSON.
- Project compilation completes before generated output is replaced.
- Locks and generated packages are exact, deterministic, and never hand-edited.

## Project Commands

```text
trellis add <package> [--version <requirement>] [--alias <alias>]
trellis rm <package>
trellis check
trellis update
trellis install
trellis generate
trellis publish
```

`check` parses, resolves, and validates the local source package without
writing outputs. `add`, `rm`, and `update` resolve the source-package graph and write an exact
`trellis.lock`. `install` recreates language outputs from that lock. `generate`
compiles native Trellis IDL and stages all generated output before atomically
replacing the last-good tree. After an edit to the project's own IDL or
manifest, `generate` (and `--watch`) re-records only the project's own package
entry in `trellis.lock` from local sources; dependencies stay exactly as locked
and change only through `update`. `generate --check` reports a stale lock or
stale output instead of writing either. `publish` pushes the source bundle and frozen
resolution metadata to the package's own repository, `<workspace>/<name>`, with
immutable version rules; there is no `--registry` flag, and the registry's own
access control is the ownership check. A package without a workspace is never
published, and a published package depends only on registry packages.

`add` takes a project directory or a published package's full identity, such
as `trellis add ghcr.io/acme/orders`; `add`, `update`, and `install` pull each
registry dependency from the workspace it names and accept a release only
when it declares that same identity. A dependency's alias is its package
name unless `--alias` names another, and a second package of the same name
from another workspace needs one: `add` refuses to replace a dependency of a
different identity and changes nothing. `rm` takes an alias, a full identity,
or a package name; an exact alias or full identity is preferred, and a package
name that fits several dependencies is refused with their aliases and
identities. A `trellis` dependency the CLI's own
build of the built-in package satisfies is taken from that build, which is the
release published at `ghcr.io/oats-center/trellis`, without downloading it.

Package references are declared in `trellis.toml`. Generated Rust and TypeScript
modules consume exact locked semantic projections; application code does not
author wire schemas.

## Authentication

```text
trellis login <oauth-origin> [--ca-certificate <ca.crt> | --ca-fingerprint <sha256>]
trellis logout
trellis whoami
```

Login uses public OAuth Device Code with explicit capability consent. It prints
and QR-encodes the issuer's link with the code filled in, plus the code to
compare on the page, and waits up to 15 minutes for approval; the
administrative `trellis.*` capabilities are optional and take the Portal's
separate confirmation step. The registered client is `trellis.cli`; no app
participant installation is involved. The native SDK owns durable P-256 custody,
refresh-token rotation, locking and atomic replacement. Each process resumes
custody with `resume_retained`: it reuses the previous run's admission and
runtime key while at least 60 seconds of it remain, so scripted runs make no
sign-in request and stay clear of the issuer's per-address limit, and otherwise
rotates the refresh token for fresh ephemeral runtime authority. A refused
reused admission is followed by one ordinary resume. `issuers revoke` always
resumes on a fresh runtime, because runs that reuse an admission share one
authorization session that the revocation could end. `--ca-certificate`
selects a private installation CA as the only trust anchor for that deployment's
OAuth and broker TLS; login prints its fingerprint for out-of-band comparison
and the selection retains its path.

Without it, a deployment whose CA no system root vouches for is trusted on first
use, like an SSH host key. Generated leaf chains end with their installation CA;
login shows that CA's SHA-256 fingerprint and, once the user accepts it (an
interactive prompt, or `--ca-fingerprint` naming the same value), pins it under
`trellis/trusted-ca/<host>_<port>.pem` in the configuration directory as the only
trust anchor for that origin. JSON output reports the offer as an `untrustedCa`
record on stderr. A later login whose deployment presents a different CA than
its pin is a hard error (`pinnedCaMismatch`); replacing a pin means deleting it
after confirming the new fingerprint with the operator. Logout revokes the
application refresh family and clears the selected native credentials.

## Authorization Administration

```text
trellis clients list|get|add|edit|put|disable|enable|delete ...
trellis apis list|get|review|accept|force-replace ...
trellis issuers rotate <current-key-id>
trellis issuers revoke <issuer-key-id>
```

`issuers rotate` calls `Auth.Issuers.Rotate` with the named key as the expected
current key and prints when Trellis starts signing with the new key
(`activatesAt`), and `issuers revoke` calls `Auth.Issuers.Revoke`. Auth's
refusals print as they come: an unknown key is "no issuer key ...", the current
signing key is refused with the advice to rotate first, or, while a new key is
announced, with the time Trellis switches to it, a key revoked once is "already
revoked", and a rotation from a key that is no longer current names the one
that is.

Client registration and catalog operations call the generated `Auth.OAuthClients`
and `Auth.Apis` families. `clients add [<client>]` registers an app and
`clients edit <client>` changes one; both take every setting as a flag (`--kind
browser|native`, `--title`, `--description`, `--[no-]development`, repeated
`--redirect-uri` and `--allow-without-asking`, `--[no-]sign-up-password`,
`--[no-]sign-up-providers`, `--source`/`--app`) and ask for the rest. `edit`
asks each question starting from the current value, and without a terminal
keeps every setting left out. Redirect URIs are checked as they are typed
against Auth's rules: HTTPS, or plain HTTP on a loopback host for a development
or native app; Auth checks again on save. Both save with `OAuthClients.Put` at
the current revision, then `OAuthClients.SignUp.Update` when sign-up changed and
`Config.Apps.Declare` for `--source`. `clients put <file>` (`-` for standard
input) stays the scripting path: it sends one `AuthOAuthClientPutRequest` JSON
file unchanged. A Put of an existing app keeps its enabled state, so saving a
disabled app's settings by `edit`, `put`, or the Console leaves it disabled;
only `clients enable <client> --expected-revision <n>` (`OAuthClients.Enable`)
turns it back on. A new app starts enabled. `edit` follows the group's
verb-first shape (`roles edit`, `users edit`), not the target-first `svc <id>`
shape.

`apis review` and `apis accept` take `--source` and an optional
`--participant`, compile the package's evidence the way `svc create` does, and
review (and accept, when compatible) each API the participant implements.
`apis force-replace --source` reviews the same way and replaces each accepted
API whose definition changed, after showing its review and getting
`--acknowledge-breakage` and `--acknowledge-consent-impact` (asked for on a
terminal). `apis force-replace <file>` sends one `AuthApiForceReplaceRequest`
JSON file instead, for role and mapping edits. JSON request files use
generated native-contract DTOs; the CLI does not author a second policy or
contract schema. Mutations retain expected revisions and idempotency keys.
Force replacement requires `trellis.auth.v1:apis.forceReplace` and explicit
breakage/consent acknowledgements.

Issuer commands manage the online signer. No `infra`, trust-root, certificate,
manifest, trust-floor, offline-signing, or compatibility command exists.

### Questions for missing input

Commands that need several values ask for each one their flags leave out:
`clients add|edit`, `apis force-replace --source`, `roles create`,
`users create`, `users find`, `sign-in providers add`, and the participant
choice of every `--source` and `--package` command when the package has several. A question
lists the choices when they are a known set, starts from the default or current
value, and checks the answer as it is typed; a write that asked anything shows
a summary and asks to go ahead. Questions and summaries go to stderr, so
`--format json` keeps stdout for the result. A command given every flag asks
nothing. Without a terminal on stdin and stderr, or with the global
`--no-input`, nothing is asked: missing required input fails before any write,
naming every missing flag. The prompt layer is `crates/cli/src/prompt.rs`.

## Deployment Commands

```text
trellis svc <name> create --source <project> | --package <workspace>/<name>@<version|sha256:digest>
                          [--participant <id>] [--acknowledge-shared-apis]
trellis svc <name> create [--public-key <key>]
trellis svc <id> apply --source <project> | --package <release> ...
trellis svc <id> show
trellis svc <deployment-id> provision [--public-key <key>] [--new]
trellis svc enable|disable|remove ...
trellis svc <deployment-id> updates
trellis svc <deployment-id> approve|reject [<update-id>] [--acknowledge-shared-apis]

trellis dev <name> create --source <project> | --package <release> ...
trellis dev <id> apply --source <project> | --package <release> ...
trellis dev <id> show
trellis dev <id> provision ...
trellis dev enable|disable|remove ...

trellis registries list
trellis registries set <name> --prefix <workspace> [--username <user> [--secret-file <path|->]]
trellis registries remove <name>
```

A build comes from one of two places. `--source` compiles the project
directory in the CLI from its `trellis.lock`. `--package` names a published
release (`trellis publish`) by its full identity and a version or OCI manifest
digest, such as `ghcr.io/acme/orders@1.2.0`; the CLI calls
`Auth.Packages.Pull`, and Auth pulls the release from that workspace,
accepts it only when it declares that identity, checks every digest, pulls each
package of the release's frozen resolution from its own workspace by its exact
manifest digest (the built-in `trellis` package is the server's own), and
compiles the release from exactly those packages without resolving again. Auth
signs in to a workspace with the registry an administrator added for it
(`Auth.PackageRegistries.*`, edited with `trellis registries` or in the
Console, whose prefix is the workspace), and pulls anonymously when there is
none. Both paths then send the same package evidence to
`Auth.Deployments.Create` or `Apply`, which recompile it. The participant is
chosen as for every `--source` command; `apply` defaults to the deployment's
own participant. Inspection uses `Auth.Deployments.Get`.

The CLI pulls nothing itself for `--package`, so the registry credentials an
administrator stores in Auth serve the CLI and the Console alike, and a
service is deployed from the same bytes whichever client asked. A private
registry's secret is sealed with the storage key and never returned;
`registries set --username` reads it from `--secret-file`, standard input, or a
terminal question. `trellis add`, `update`, and `publish` use the operator's
Docker credentials for the workspace's registry host.

`svc <name> create` without `--source` or `--package` creates a service claim
(`Auth.ServiceClaims.Create`) for the owner's `--public-key`, or for a locally
generated identity whose seed it prints once; see
`design/auth/service-updates.md`. `svc <id> provision --public-key <key>`
registers the owner's key as an instance, or selects the instance that already
has it, and prints no seed. `updates`, `approve`, and `reject`
accept a waiting claim's ID or name as well as a deployment's.

`svc <id> enrollment issue|list|edit|revoke` manages the deployment's
enrollment secrets (`Auth.EnrollmentSecrets.*`). `issue` prints the secret
alone on standard output, so it pipes into a secret store, and reports on
standard error; with `--format json` the secret is the response's `text`.
`edit` reads the secret's current limits and replaces only the flags given.
`revoke --retire-instances` reports how many instances were retired and how
many of their connections were still being closed when the revocation
committed. `svc <id> instance-policy show|set` and `instance-policy show|set`
read and change `Auth.InstancePolicy` at the deployment and installation
level; see `design/auth/instance-enrollment.md`.

`svc <id> instances` hides retired and removed instances unless `--all` is
given, and shows each instance's origin. It joins `Auth.ServiceInstances.List` with
`Health.Inspect` (the `cli` agent selects `health.inspect`) and adds a
`status` to each instance: `state` (`offline`, `starting`,
`waitingForApproval`, `waitingForSettings`, `serving`, `stopping`), `build`
(`digest`, `version`, `current`, and a short `label`), `connectedAt`,
`lastHeartbeat`, `health`, and while waiting `reviewUrl` or `missingSettings`.
`health` is `{ status, failingChecks }`: the heartbeat's effective status
(`healthy`, `degraded`, `unhealthy`, `offline`) and each check not `ok`
(`degraded` or `failed`) with its `name`, `status`, and the `summary` and
`error` it reported. A disconnected
instance is `offline` with no checks; a connected one that has not reported
on this connection has no `health`; a connected one whose heartbeats stopped
keeps the state it last reported and has `offline` health with no checks. The
text table shows state, health, build, connected and last heartbeat, and below
it prints one line per failing check (`<instance ID>: check <name> <status>:
<error or summary>`, the status colored like other states on a terminal), a
`<instance ID>: heartbeats stopped; last heartbeat <time>` line for a
connected instance whose heartbeats stopped, and what a waiting instance
needs. The Console's service page shows the same health and failing
checks. The states follow `design/auth/service-updates.md`.

`svc updates` lists the builds a service submitted for itself
(`Auth.ServiceUpdates.List`) with what each asks for and gives up;
`approve` and `reject` decide the pending one, or the named one
(`Auth.ServiceUpdates.Decide`). Approving an update that would answer an API
another deployment serves requires `--acknowledge-shared-apis`; without it the
CLI explains that requests would be split between the services and stops. See `design/auth/service-updates.md`. There are
no authority show/plan/reconcile commands.

Provisioning returns one-use service/device material once. The CLI does not
persist returned private identity material unless the caller explicitly directs
output to a secure destination.

## Resources And Events

```text
trellis resources list|inspect|destroy ...
trellis events query|inspect ...
trellis events consumers query|inspect ...
trellis events dead-letters query|inspect|replay|dismiss ...
```

Resource commands use `Auth.Resources.*`. `destroy` accepts only a detached
resource and requires exact confirmation; no lifecycle command destroys data
implicitly. Event commands read the Events journal and administer durable
consumers and dead letters through the Events API. `events consumers query`
and `inspect` show, beside each consumer's pending and awaiting-ack counts, a
Failed count: the failed deliveries of that consumer resource still awaiting a
decision, which `Events.Consumers.Query` and `Events.Consumers.Inspect` report
as `deadLetters` from the Events projection. The Console's Events page reads
the same count for each service, so it is right even when the failed
deliveries are not on the page it has loaded.

## Users And Portals

```text
trellis users list|show|find|create|edit|delete ...
trellis users create [--username <name>] [--name <name>] [--email <address>] [--provider <id> --subject <sub>]
trellis users show|edit|delete <user>
trellis users link|unlink <user> --provider <id> --subject <sub>
trellis roles list|create ...
trellis roles show|edit|delete <role>
trellis roles assign|revoke <role> --user <user>
trellis sign-in show|set [--[no-]local-users] [--[no-]local-sign-up] [--password-min-length <8-128>]
trellis sign-in providers list|show|add|edit|enable|disable|remove ...
trellis clients sign-up <client> [--[no-]password] [--[no-]providers]
```

A `<user>` is a username, an email address or a user ID, and a `<role>` is a
role's name (any case) or its ID. A name that fits more than one user or role
is refused with their IDs, and the command changes nothing; give an ID then.
The CLI resolves names with `Auth.Users.List` and `Auth.Roles.List` and sends
IDs to Auth. User commands use generated Auth account operations. `users create
--provider --subject` links that OIDC sign-in in the creating call and prints
no password setup link. Otherwise it prints the one-time setup link,
`/account/reset?token=...` on the Trellis origin, and says when password
sign-in is off, since the password chosen there signs in only while it is on;
the Console's New user page shows the same link and offers to turn password
sign-in on. `users link|unlink` read the account's current revision
and call `Auth.Users.Identities.Link|Unlink`. Naming either `--provider` or
`--subject` makes the other required input, asked for on a terminal (the
provider from a list) and reported as missing without one. Role commands use
`Auth.Roles.*`; `--capability` names each capability a role gives, and
`--new-users`/`--no-new-users` sets whether users created afterwards get the
role. `roles show` lists who has the role; `roles assign|revoke <role> --user <user>`
gives or takes it from one user, and `roles assign --revision <n>` renews an
expired assignment at the revision Auth names when it refuses a new one.
`sign-in` turns username-and-password sign-in and self sign-up on or off and sets the minimum password length, at once
(`Auth.SignInSettings`). OIDC sign-in is on while any provider is.
`sign-in providers` manages the OIDC providers (`Auth.OIDCProviders.*`):
`enable` and `disable` turn one on and off, and `list` and `show` print
each provider's ID, which `users find --provider` takes, and the callback URL to
register with the provider; `add` and `edit` read the client secret from
`--client-secret-file` (`-` for standard input), never the command line (`add`
asks for it without echo when it asks for its other values), and
`edit` keeps every option left out. `--certificate-fingerprint <sha256>` pins
the issuer of a provider's certificate when nothing on the server vouches for
it; `edit --no-certificate-fingerprint` removes the pin, even while the
provider still needs it, after which its sign-ins fail until it is pinned
again. `--authorization-param NAME=VALUE`, repeatable, adds a parameter to
every authorization request sent to the provider, such as Auth0's
`organization=<org id>`; `edit` replaces the list when any are given, and
`--clear-authorization-params` empties it. `clients sign-up` chooses whether people may create their own account while signing in to one app. The separately originated
OAuth portal owns interactive login, device-code confirmation and consent;
authenticated administration remains on NATS.

Fresh administrator initialization uses `trellis-server admin [--config <path>]`.
`--username` defaults to `admin`; `--password` or `--password-stdin` sets initial
credentials. With neither, the command returns a one-time password-setup URL on
the Trellis origin. Runtime startup offers corresponding
`--admin-username`, `--admin-password`, and `--admin-password-stdin` options and
uses the same atomic initialization. Existing administrator credentials are never
overwritten by these options. `--reset-admin` is explicit local recovery using
the existing password-reset flow, not administrative authority recreation.

## Host Setup Is `trellis-server`

`trellis` talks to a Trellis over the network and carries no server runtime.
Setting up the host that runs one belongs to the server binary, which already
holds the runtime and storage code:

```text
trellis-server [--config <path> | --user | --system] [--dev [--host <name>]] ... [<mode>]
trellis-server check [--config <path> | --user | --system] [--dev] ... [<mode>]
trellis-server admin [--config <path> | --user | --system] [--username <name>] [--password-stdin]
trellis-server config example
trellis-server tls issue --out <dir> --host <name>...
trellis-server tls status [--config <path> | --user | --system]
trellis-server nats new --out <dir> [--servers <n>] [--hosts <name,...>] [--listen <ip>] ...
trellis-server nats <system|trellis|auth> [--config <path> | --user | --system] [--print] [<nats args>...]
trellis-server backup <dir> [--config <path> | --user | --system]
trellis-server restore <dir> [--config <path> | --user | --system] [--force]
trellis-server upgrade [--check] [--prerelease]
```

An installation is one of three kinds. The user's lives in `~/.config/trellis`
with its data in the XDG folders; the system's in `/etc/trellis` with its data
in the FHS folders (Linux only); a folder installation holds both, its data in
`var/`. `--user` is the default, and `--system` for root on Linux. Every
command names a folder installation with `--config`, given its folder or its
`config.toml`; naming one together with `--user` or `--system` is an error.
`<mode>` is `all` (the default), `platform`, `jobs`, `health` or `events`.
Options go after the subcommand, and each subcommand takes only its own. Those
that report a result accept `--format json` for scripts.

A person writes `config.toml`; `config example` prints a commented one. There
is no setup command. The first start of a platform role creates what is
missing in `secrets.toml` beside it (owner-only, replaced atomically): the
sign-in and authorization signing keys, the auth-callout keys, the instance ID,
and Trellis's two NATS accounts with their users and sentinel. A start without
`config.toml` stops and names `config example` and `--dev`.

NATS belongs to an operator. `[nats] system_credentials` names the system
account user's creds and `[nats] operator_signing_key` an operator signing key
seed. With both, every platform start signs the `AUTH` account (with the auth
callout) and the `TRELLIS` account with that key and sends them through
`$SYS.REQ.CLAIMS.UPDATE`, so the servers need a `full` resolver. When the
servers refuse the update but already admit Trellis's own user, the start
logs a warning and continues; enforcement that needs the system user stays
pending. `trellis-server admin` creates the keys and accounts without sending
them, so it runs before NATS is up. Every role reads the system user from its
file on start. The sentinel user's JWT is published in the OAuth metadata as
`trellis.natsSentinel`; every client presents it so NATS hands the connection
to the auth callout. Servers carry no `default_sentinel`.

`nats new` creates an operator with a system account and a signing key for
Trellis, and writes `nats-<n>.conf` for each of `--servers` servers (default
3; with more than one, a cluster with routes), `system.creds`, and
`trellis-operator.nk`. Without `--hosts` the servers share `--listen` and take
consecutive ports; with `--hosts` each listens on the same ports on its own host
and routes to the others by name. `--tls-certificate` and `--tls-private-key`
give native NATS and the WebSocket TLS; without them both are plaintext. It
prints the matching `[nats]` section.

`[http.transport] mode` selects listener security: `tls` (supplied files,
reloaded on change), `proxy` (plain HTTP; `trusted_proxies` optionally limits
peers to those networks, and without it every peer is served and the client is
the last `X-Forwarded-For` entry), `generated`, or `loopback_development`.
`generated` makes a name-constrained CA and a leaf for the public and Console
origins in the data folder's `tls/`, reissues the leaf from the same CA within
30 days of expiry (checked every 12 hours by the running server), makes a new
CA when the names change, and warns with the CA fingerprint in the log, the
Console and the Portal. `tls status` reports the CA fingerprint and expiry.
`tls issue` issues one leaf for the given hosts from a new name-constrained CA
whose key it discards, writing `ca.crt`, `server.crt` and `server.key`; the
testkits and the image smoke use it to run the production layout.

`--dev` is the only way the server runs NATS itself. Without `--config` it uses
`<user data>/trellis/dev`, and when that holds no installation it creates one:
`config.toml` (loopback HTTP on port 3000, or with `--host` generated TLS on all
interfaces with only the WebSocket offered to clients), `nats.conf` with a new
operator, `nats/system.creds`, `nats/operator.nk`, and `[nats] managed = true`.
`managed` makes the server start and stop the broker `nats.conf` describes,
using `nats-server` from `PATH` or, once the operator agrees or passes
`--nats-download`, the pinned checksum-verified release; the Trellis port
relays WebSocket upgrades to its plaintext loopback listener. A start of a
`managed` installation without `--dev` is refused. `admin` never starts NATS.

The runtime reads keys only from `secrets.toml`, refusing it when it is a link
or anyone else could read it. A role started apart from the platform needs only
the Trellis NATS user, the instance ID and its own provider seed there. The
instance ID is how it confirms the broker is its installation's before writing
to it (see Runtime Storage Identity in `design/core/trellis-patterns.md`). Its
seed signs its request that Auth prepare its built-in provider, which the
platform checks against its own copy, so the platform's `secrets.toml` keeps
every role's seed (see split roles in `design/auth/trellis-auth.md`).
Everything the server writes goes to the kind's data folders, which `[paths]`
overrides; the NATS download goes to the user or system cache for every kind,
so installations share it. `trellis-server nats <user>` runs the `nats` CLI
with the installation's address and CA and that user's creds, written to the
private runtime folder only while it runs; `--print` writes the creds to
standard output instead. A start validates the whole config before it fetches
or starts NATS.

### Recovery

The default installation is one Trellis server and one NATS server with one
copy of each stream. Its state is the SQLite databases in the data folder and
JetStream on the broker. A Trellis or NATS process restart loses nothing; a
power loss can lose JetStream writes within its `sync_interval` (two minutes
by default); losing the disk or corrupting a database or the JetStream store
needs a restore. Leases choose which process runs a role and move no data. A
standby takes a role over only with the same broker and, for Auth, the same
platform database, `.issuer-keys` folder and `secrets.toml`, shared or
restored; it never recovers a dead server's local disk. Replicated storage is
not part of the default deployment.

`backup` writes a new or empty owner-only folder from a running or stopped
installation whose NATS is reachable: `config.toml`, `secrets.toml`,
`nats.conf`, every file the config names (inside the installation folder by
relative path, outside it by absolute path), a copy of each subsystem database
made with SQLite's online backup in one step, the issuer-key folder, the data
folder's `tls/`, and a JetStream snapshot (`$JS.API.STREAM.SNAPSHOT`, with
consumers) of every stream in the Trellis account except the lease bucket.
`manifest.json` comes last, with the format, versions, instance ID, JetStream
identity, times, and each file's SHA-256 and mode; a folder without it is not a
backup. Databases are copied before streams, so every stream entry a database
refers to is in the snapshot. A snapshot can hold a projector consumer
position later than its projection's copy, so loading the streams deletes the
durable `jobs-projector-*`, `health-projector-*` and `events-projector-*`
consumers; each projection then replays its stream, and projecting an event
again is a no-op by stream sequence. The broker's own server config is not
part of a backup. Before snapshotting, `backup` checks the broker's identity
record as a start does: it must name the configuration's instance ID and,
when the platform database was copied, that database's JetStream identity. A
broker with no record, or one bound to another installation or database,
fails the backup before its manifest is written.

`restore` verifies every checksum before writing. It refuses a target folder
holding another installation, and an outside file that exists with other
contents, unless `--force`. The target's ID is the one its start would use:
`[auth] instance_id` from its `config.toml`, else the one in `secrets.toml`; a
`config.toml` that can't be loaded is refused too. It writes the files,
removes any SQLite journal beside a database it replaces, replaces the
issuer-key folder, and stages the stream snapshots with the manifest in the
data folder's `restore/`. The next start of a role including the platform
loads each staged stream (`$JS.API.STREAM.RESTORE`) after pushing the NATS
accounts and before the runtime starts, refusing a broker that already has
one; it removes each snapshot once NATS has the stream. An interrupted restore
is run again: `restore` over a folder that still has a staged restore keeps
the names of that restore's streams in `restore/unfinished.json`, and the next
start, when NATS holds no stream outside that list, deletes those streams
before loading the snapshots. That covers a start stopped, or an answer lost,
after NATS took a stream but before its snapshot was removed. `restore` reads
the list whether or not a manifest is staged, and replaces it with one rename
before clearing the rest of `restore/`; the manifest is written last. A
`restore` that stopped while copying snapshots therefore leaves the list for
the next one, even when that one restores a backup with other stream names. The JetStream
identity marker is one of the restored streams, so the platform's binding
check passes only when the database and streams came from the same
installation.

## Utility Commands

```text
trellis keys ...
trellis upgrade [--check] [--prerelease]
trellis version
trellis completion <shell>
```

`trellis keys new` makes an Ed25519 identity offline for whoever will run a
service: it prints a seed, which the service keeps, and the public key, which
the administrator registers with `svc <id> provision --public-key` or a claim's
`svc <name> create --public-key`. Only the owner ever holds the seed. `--seed`
derives the public key of an existing seed. Without `--public-key`, `provision`
and claim `create` generate the identity and print its seed once.

## Errors And Output

Human output is concise and actionable, and every command uses the shared
layout in `crates/cli/src/output.rs`:

- stdout carries what the command returns: a table, a record, or a secret.
  stderr carries status lines (`✓` for a change, `•` for a request that changed
  nothing, `!` for a warning), dim hints, `Next:` commands, and prompts, so
  piping stdout keeps only data.
- A list is an aligned table under an uppercase header on a terminal, truncated
  to its width, with times relative to now. Piped, it is one tab-separated line
  per item with raw values and no header. An empty list prints a sentence on
  stderr saying so, with the command that would add the first item when there
  is an obvious one.
- A record is a bold title, aligned `Label  value` fields with empty values
  left out, and titled sections for lists and nested values. State words are
  colored by meaning: green for enabled or done, yellow for waiting, red for
  off or failed.
- A secret the CLI generates (an instance seed, a device root secret, a setup
  code, a key seed) prints once with a warning that Trellis does not keep it.
- Color appears on terminals unless `NO_COLOR` is set or `TERM=dumb`, even
  when `CI` is set; `CLICOLOR_FORCE=1` keeps it when piped or on a dumb
  terminal.
- `svc|dev <name> remove` asks for the name before removing; `-f` skips the
  question, and without a terminal on stdin the command refuses and names `-f`.

JSON mode emits one documented value to stdout, pretty-printed (and colored on a
terminal), silences status lines and hints, and sends tracing/progress to
stderr. JSON is the scripting interface; the human layout may change. Errors
render through miette on stderr in both modes, with a `help:` line naming the
command or flag that resolves them where one exists. A declared contract error
shows its message. Expected Auth RPC errors retain
their generated result codes, including stale binding/deployment revisions,
invalid participant artifacts, denied administration, missing required
evidence, and unavailable issuer/runtime dependencies.

## Removed Surface

- offline trust infrastructure and `infra` commands;
- authority plans and reconciliation;
- desired/materialized authority inspection;
- identity-grant IDs or contract-digest-scoped grant commands; and
- client-side artifact/digest assertions during bootstrap.
