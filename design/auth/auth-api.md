# Design: Auth API

Status: authoritative public Auth surface after WO-02.

## Source Of Truth

`rust/crates/runtime/contract.trellis` is the native IDL source. Generated Rust
and TypeScript clients and schemas are the public API reference. HTTP-only
browser/bootstrap DTOs remain Rust-owned at their transport boundary.

There are no handwritten desired/materialized-authority, proposal,
reconciliation, identity-grant, trust-root, certificate, manifest, or legacy
device-connect APIs.

## HTTP Surface

| Method | Route                          | Purpose                                                         |
| ------ | ------------------------------ | --------------------------------------------------------------- |
| `GET`  | `/.well-known/trellis`         | Canonical origin and current online issuer metadata             |
| `POST` | `/bootstrap/service`           | Admit a provisioned service and return shared installation data |
| `POST` | `/bootstrap/device`            | Admit an approved device and return shared installation data    |
| `POST` | `/auth/device/enroll`          | Create/resume proof-bound device review                         |
| `POST` | `/auth/requests`               | Start a proof-bound browser flow                                |
| `GET`  | `/auth/requests/{flowId}`      | Read browser-flow progress                                      |
| `POST` | `/auth/requests/{flowId}/bind` | Create a login after approval; no context returned              |
| `POST` | `/auth/context/refresh`        | Revalidate and return current runtime installation data         |
| `POST` | `/auth/logout`                 | Revoke a real user login                                        |
| `POST` | `/auth/password`               | Change password and revoke sibling logins                       |

Browser local-login, registration, OIDC, portal approval/denial, and account
recovery routes remain part of the existing browser machine. They preserve the
portal-binding and OIDC-cookie/CAS controls documented in `auth-protocol.md`.

Native bootstrap request DTOs are strict and accept only identity/proof inputs.
Browser request and bind proofs bind the complete raw body. All HTTP timestamps
and expiries are epoch milliseconds.

## Shared Installation Response

Native bootstrap and context refresh return one shape:

- `assignment`: server-owned deployment and instance identifiers;
- `runtime`: exact package/participant evidence, effective grant, resource
  evidence, signed context, route JWT, inbox, and expiries; and
- `transports`: exact Core NATS and WebSocket endpoints/options.

Bind returns only the minimal login projection. Browser clients commit that
result under their generation fence, then call context refresh.

## RPC Surface

### Participants

- `Auth.Participants.Get`
- `Auth.Participants.Install`

Installation accepts verified source-package evidence and exact participant
lexical path, resolves semantics, rejects reserved Trellis namespaces except
built-ins, and creates an immutable participant revision.

### Grants

- `Auth.Grants.Get`
- `Auth.Grants.List`
- `Auth.Grants.Set`
- `Auth.Grants.Revoke`

Bindings are keyed by identity plus participant. Mutations require
`expectedRevision`, validate atoms against the selected installed revision, and
carry a finite platform-privilege set containing only `Admin`.

### Issuers

- `Auth.Issuers.Revoke`

Issuer installation and rotation occur online during runtime startup from the
configured private issuer seed. No offline root or manifest RPC exists.
Historical contexts remain stored after ordinary rotation.

### Deployments And Provisioning

- `Auth.Deployments.Create`
- `Auth.Deployments.Apply`
- `Auth.Deployments.Get`
- service and device provisioning/lifecycle RPCs;
- device review list/decide operations; and
- resource evidence and portal-policy operations retained by the contract.

Deployment Apply is the direct mutation path. There is no proposal, plan,
approve, reconcile, or materialized-authority RPC family.

### Sessions And Connections

- `Auth.Sessions.Me`
- `Auth.Sessions.List`
- `Auth.Sessions.Revoke`
- `Auth.Connections.List`

`Sessions.Me` returns the logical connection and nullable user login. Durable
session RPCs are login-only; native service/device connections have no session
row.

## Authorization

Administrative RPCs require `PlatformPrivilege::Admin` in the caller's current
participant-scoped `GrantBinding`. This privilege gates the administrative
surface; it does not fabricate participant grants. Mutation authorization and
idempotency replay are checked in the same SQL transaction using the current
principal, login/credential, exact binding revision, expiry, and privilege.

## Events And Post-Commit Work

Participant, grant, principal, login, issuer, resource, device, and deployment
changes emit ordered typed events through the transactional post-commit queue.
Participant installation precedes the corresponding grant-change event.
Superseded contexts are durably revoked before connection kicks are attempted.

Auth's own event publisher uses the ordinary event authorization path and a
durably prepared body/proof tuple. The Events journal retains the context digest rather
than copying authorization state.

## Non-Goals

- compatibility endpoints for the retired authorization model;
- client-authored wire schemas;
- user-facing APIs for raw SQL revisions/timestamps beyond declared DTOs; and
- server-side use of browser-persisted authorization state.
