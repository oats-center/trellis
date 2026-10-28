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
trellis clients sign-up <client> [--[no-]password] [--[no-]providers]
```

User commands use generated Auth account operations. Role commands use
`Auth.Roles.*`; `--capability` names each capability a role gives, and
`--new-users`/`--no-new-users` sets whether users created afterwards get the
role. `roles show` lists who has the role; `roles assign|revoke <role> <user>`
gives or takes it from one user. `sign-in` turns the installation's sign-in
methods on or off at once (`Auth.SignInSettings`), and `clients sign-up` chooses
whether people may create their own account while signing in to one app. The separately originated
OAuth portal owns interactive login, device-code confirmation and consent;
authenticated administration remains on NATS.

Fresh administrator initialization uses `trellis-server init admin --config <path>`.
`--username` defaults to `admin`; `--password` or `--password-stdin` sets initial
credentials. With neither, the command returns a one-time password-setup URL on
the isolated OAuth origin. Runtime startup offers corresponding
`--admin-username`, `--admin-password`, and `--admin-password-stdin` options and
uses the same atomic initialization. Existing administrator credentials are never
overwritten by these options. `--reset-admin` is explicit local recovery using
the existing password-reset flow, not administrative authority recreation.

## Host Setup Is `trellis-server`

`trellis` talks to a Trellis over the network and carries no server runtime.
Setting up the host that runs one belongs to the server binary, which already
holds the runtime and storage code:

```text
trellis-server [--init] [--nats-download | --local-nats=<path>] ...
trellis-server init [--out <dir>] [--external-nats] ...
trellis-server init admin [--config <config.toml>] [--system] ...
trellis-server tls renew --bundle <dir> [--ca-dir <dir>]
trellis-server tls status [--config <config.toml>]
trellis-server upgrade check|install [--prerelease]
```

These accept `--format json` for scripts. Initialization creates ordinary secure directories only when absent and rejects
unsafe paths. `trellis-server init` selects listener security with `--tls
generated|supplied|loopback-development` (generated by default),
`--trusted-proxy`, and `--nats-websocket-behind-proxy` or
`--nats-websocket relay|direct` (relay, the default with generated TLS unless a
WebSocket URL or external proxy is given, has the runtime serve `/nats` on the
Console origin from the broker's plaintext WebSocket listener), validates that every
endpoint can be protected before writing, and writes a generated CA's private
key to `<out>-tls-ca/` or `--tls-ca-dir`, outside the bundle. It always warns
with the CA fingerprint when it generated one. `trellis-server tls renew` reissues the
bundle's leaves from that CA with unchanged names; `trellis-server tls status` reports
the CA fingerprint and expiry and repeats the warning.

A bundle is the installation. `trellis-server init --out <dir>` writes `[paths]`
that put data, state, cache, runtime files, and logs in folders inside the
bundle, so servers started from different bundles on one host never share
state. Without `--out`, `init` writes the bundle where the server looks by
default (`~/.config/trellis`, or `/etc/trellis` with `--system`) and omits
`[paths]`. The server itself has no bundle-relative default: an omitted root
falls back to the user (XDG) or `--system` (FHS) profile.

A first plain `trellis-server` in the user profile, with no `--config`, offers
to create that default localhost bundle; `--init` creates it without asking,
and with no terminal the server stops with the flag to use. The bundle decides
who runs NATS: `[nats] managed = true`, which `init` writes unless given
`--external-nats`, makes the server start and stop the broker described by
`nats/nats.conf`, using `nats-server` from `PATH` or, once the operator agrees
or passes `--nats-download`, the pinned checksum-verified release. Without
`managed`, the server connects to `[nats] servers` and never owns the broker.
A start validates the whole config before it fetches or starts NATS.

## Utility Commands

```text
trellis keys ...
trellis upgrade ...
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
