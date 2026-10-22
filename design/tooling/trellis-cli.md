# Design: Trellis CLI

Status: authoritative command and project-tooling model after WO-02.

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
replacing the last-good tree. `publish` pushes the source bundle and frozen
resolution metadata to OCI with immutable version rules.

Package references are declared in `trellis.toml`. Generated Rust and TypeScript
modules consume exact locked semantic projections; application code does not
author wire schemas.

## Authentication

```text
trellis login <oauth-origin> [--ca-certificate <ca.crt>]
trellis logout
trellis whoami
```

Login uses public OAuth Device Code with explicit capability and optional finite
administrative privilege consent. The registered client is `trellis.cli`; no app
participant installation is involved. The native SDK owns durable P-256 custody,
refresh-token rotation, locking and atomic replacement. Each process resumes
custody and establishes fresh ephemeral runtime authority. `--ca-certificate`
selects a private installation CA as the only trust anchor for that deployment's
OAuth and broker TLS; login prints its fingerprint for out-of-band comparison
and the selection retains its path. Logout revokes the
application refresh family and clears the selected native credentials.

## Authorization Administration

```text
trellis clients list|get|put|disable|delete ...
trellis apis list|get|review|accept|force-replace ...
trellis issuers revoke <issuer-key-id> ...
```

Client registration and catalog operations call the generated `Auth.OAuthClients`
and `Auth.Apis` families. JSON request files use generated native-contract DTOs;
the CLI does not author a second policy or contract schema. Mutations retain
expected revisions and idempotency keys. Force replacement requires the finite
`apis.forceReplace` privilege and explicit breakage/consent acknowledgements.
Client configuration supports exact registered loopback browser callbacks.

Issuer commands manage the online signer. No `infra`, trust-root, certificate,
manifest, trust-floor, offline-signing, or compatibility command exists.

## Deployment Commands

```text
trellis svc create ...
trellis svc apply <deployment-id> <project> ...
trellis svc get <deployment-id>
trellis svc provision <deployment-id> ...
trellis svc enable|disable|remove ...

trellis dev create ...
trellis dev apply <deployment-id> <project> ...
trellis dev get <deployment-id>
trellis dev provision <deployment-id> ...
trellis dev enable|disable|remove ...
```

Apply compiles the project source in memory, validates its participant kind and
selected participant, installs the participant revision when needed, then calls
the direct deployment mutation. Inspection uses `Auth.Deployments.Get`.

There are no authority show/plan/propose/approve/reject/reconcile commands.
Deployment changes and participant grant changes are distinct explicit
operations.

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
```

User commands use generated Auth account operations. The separately originated
OAuth portal owns interactive login, device-code confirmation and consent;
authenticated administration remains on NATS.

Fresh administrator initialization uses `trellis init admin --config <path>`.
`--username` defaults to `admin`; `--password` or `--password-stdin` sets initial
credentials. With neither, the command returns a one-time password-setup URL on
the isolated OAuth origin. Runtime startup offers corresponding
`--admin-username`, `--admin-password`, and `--admin-password-stdin` options and
uses the same atomic initialization. Existing administrator credentials are never
overwritten by these options. `--reset-admin` is explicit local recovery using
the existing password-reset flow, not administrative authority recreation.

## Utility Commands

```text
trellis init ...
trellis tls renew --bundle <dir> [--ca-dir <dir>]
trellis tls status --config <config.toml>
trellis keys ...
trellis upgrade ...
trellis version
trellis completion <shell>
```

Initialization creates ordinary secure directories only when absent and rejects
unsafe paths. `trellis init config` selects listener security with `--tls
generated|supplied|loopback-development` (generated by default),
`--trusted-proxy`, and `--nats-websocket-behind-proxy`, validates that every
endpoint can be protected before writing, and writes a generated CA's private
key to `<out>-tls-ca/` or `--tls-ca-dir`, outside the bundle. It always warns
with the CA fingerprint when it generated one. `trellis tls renew` reissues the
bundle's leaves from that CA with unchanged names; `trellis tls status` reports
the CA fingerprint and expiry and repeats the warning. Key commands generate or derive the requested online issuer,
identity, or session-compatible key material without introducing offline trust
artifacts.

## Errors And Output

Human output is concise and actionable. JSON mode emits one documented value to
stdout and sends tracing/progress to stderr. Expected Auth RPC errors retain
their generated result codes, including stale binding/deployment revisions,
invalid participant artifacts, denied Admin privilege, missing required
evidence, and unavailable issuer/runtime dependencies.

## Removed Surface

- offline trust infrastructure and `infra` commands;
- authority proposals/plans and reconciliation;
- desired/materialized authority inspection;
- identity-grant IDs or contract-digest-scoped grant commands; and
- client-side artifact/digest assertions during bootstrap.
