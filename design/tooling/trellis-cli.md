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
trellis add <package> [--version <requirement>]
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
resolution metadata to OCI with immutable version rules.

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
custody and establishes fresh ephemeral runtime authority. `--ca-certificate`
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
trellis clients list|get|put|disable|delete ...
trellis apis list|get|review|accept|force-replace ...
trellis issuers revoke <issuer-key-id> ...
```

Client registration and catalog operations call the generated `Auth.OAuthClients`
and `Auth.Apis` families. `apis review` and `apis accept` take `--source` and an
optional `--participant`, compile the package's evidence the way `svc create`
does, and review (and accept, when compatible) each API the participant
implements. Other JSON request files use generated native-contract DTOs; the CLI
does not author a second policy or contract schema. Mutations retain
expected revisions and idempotency keys. Force replacement requires
`trellis.auth.v1:apis.forceReplace` and explicit breakage/consent acknowledgements.
Client configuration supports exact registered loopback browser callbacks.

Issuer commands manage the online signer. No `infra`, trust-root, certificate,
manifest, trust-floor, offline-signing, or compatibility command exists.

## Deployment Commands

```text
trellis svc <name> create --source <project> ... [--acknowledge-shared-apis]
trellis svc <name> create
trellis svc apply <deployment-id> <project> ... [--acknowledge-shared-apis]
trellis svc get <deployment-id>
trellis svc provision <deployment-id> ...
trellis svc enable|disable|remove ...
trellis svc <deployment-id> updates
trellis svc <deployment-id> approve|reject [<update-id>] [--acknowledge-shared-apis]

trellis dev create ...
trellis dev apply <deployment-id> <project> ...
trellis dev get <deployment-id>
trellis dev provision <deployment-id> ...
trellis dev enable|disable|remove ...
```

Apply compiles the project source in memory, validates its participant kind and
selected participant, installs the participant revision when needed, then calls
the direct deployment mutation. Inspection uses `Auth.Deployments.Get`.

`svc <name> create` without `--source` creates a service claim
(`Auth.ServiceClaims.Create`) with a locally generated identity and prints its
seed; see `design/auth/service-updates.md`. `updates`, `approve`, and `reject`
accept a waiting claim's ID or name as well as a deployment's.

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
consumers and dead letters through the Events API.

## Users And Portals

```text
trellis users ...
trellis roles list|show|create|edit|delete|assign|revoke ...
trellis sign-in show|set [--[no-]local-users] [--[no-]local-sign-up] [--[no-]providers]
trellis sign-in providers list|show|add|edit|disable|remove ...
trellis clients sign-up <client> [--[no-]password] [--[no-]providers]
```

User commands use generated Auth account operations. Role commands use
`Auth.Roles.*`; `--capability` names each capability a role gives, and
`--new-users`/`--no-new-users` sets whether users created afterwards get the
role. `roles show` lists who has the role; `roles assign|revoke <role> <user>`
gives or takes it from one user. `sign-in` turns the installation's sign-in
methods on or off at once (`Auth.SignInSettings`). `sign-in providers` manages
the company identity providers (`Auth.OIDCProviders.*`): `list` and `show` print
each provider's ID, which `users find --provider` takes, and the callback URL to
register with the provider; `add` and `edit` read the client secret from
`--client-secret-file` (`-` for standard input), never the command line, and
`edit` keeps every option left out. `clients sign-up` chooses
whether people may create their own account while signing in to one app. The separately originated
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
the Trellis NATS user, the instance ID and its own provider seed there.
Everything the server writes goes to the kind's data folders, which `[paths]`
overrides; the NATS download goes to the user or system cache for every kind,
so installations share it. `trellis-server nats <user>` runs the `nats` CLI
with the installation's address and CA and that user's creds, written to the
private runtime folder only while it runs; `--print` writes the creds to
standard output instead. A start validates the whole config before it fetches
or starts NATS.

## Utility Commands

```text
trellis keys ...
trellis upgrade [--check] [--prerelease]
trellis version
trellis completion <shell>
```

Key commands generate or derive the requested online issuer, identity, or
session-compatible key material without introducing offline trust artifacts.

## Errors And Output

Human output is concise and actionable. JSON mode emits one documented value to
stdout and sends tracing/progress to stderr. Expected Auth RPC errors retain
their generated result codes, including stale binding/deployment revisions,
invalid participant artifacts, denied administration, missing required
evidence, and unavailable issuer/runtime dependencies.

## Removed Surface

- offline trust infrastructure and `infra` commands;
- authority plans and reconciliation;
- desired/materialized authority inspection;
- identity-grant IDs or contract-digest-scoped grant commands; and
- client-side artifact/digest assertions during bootstrap.
