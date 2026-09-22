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
trellis update
trellis install
trellis generate
trellis publish
```

`add`, `rm`, and `update` resolve the source-package graph and write an exact
`trellis.lock`. `install` recreates language outputs from that lock. `generate`
compiles native Trellis IDL and stages all generated output before atomically
replacing the last-good tree. `publish` pushes the source bundle and frozen
resolution metadata to OCI with immutable version rules.

Package references are declared in `trellis.toml`. Generated Rust and TypeScript
modules consume exact locked semantic projections; application code does not
author wire schemas.

## Authentication

```text
trellis login [--server <url>]
trellis logout
trellis whoami
```

Login uses the browser request/bind flow, then context refresh. Local CLI state
retains the session signing seed and nullable login metadata required for
continuity; authorization contexts, route JWTs, transports, grants, and issuer
keys are process-memory-only. Logout revokes the real login and clears local
credentials.

The CLI uses the installed `trellis-app.cli@v1` participant. First-admin setup
creates its Admin `GrantBinding` atomically with the Console binding.

## Authorization Administration

```text
trellis identity grants get <identity-id> <participant-id>
trellis identity grants list [--identity <identity-id>]
trellis identity grants set <identity-id> <participant-id> ...
trellis identity grants revoke <identity-id> <participant-id> ...

trellis participants get <participant-id> [--revision <revision>]
trellis participants install <project-or-artifact> ...

trellis issuers list
trellis issuers rotate ...
trellis issuers revoke <issuer-key-id> ...
```

Grant commands call `Auth.Grants.*` and require explicit expected revisions for
mutations. They operate on one identity/participant binding and may assign only
declared atoms and finite platform privileges. Participant installation compiles
verified source-package semantics and calls `Auth.Participants.Install`; it does
not grant authority as a side effect.

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

## Users And Portals

```text
trellis users ...
trellis portals ...
```

These commands retain current account, user identity, portal route, login
setting, and portal grant-override operations. Portal grant overrides select
concrete atoms from an installed participant revision and do not create a second
authority model.

## Utility Commands

```text
trellis init ...
trellis keys ...
trellis upgrade ...
trellis version
trellis completion <shell>
```

Initialization creates ordinary secure directories only when absent and rejects
unsafe paths. Key commands generate or derive the requested online issuer,
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
