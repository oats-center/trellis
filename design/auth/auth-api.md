# Design: Auth API

Status: authoritative public Auth surface after WO-02.

## Source Of Truth

`crates/runtime/contract.trellis` is the native IDL source. Generated Rust and
TypeScript clients and schemas are the public API reference. HTTP-only
browser/bootstrap DTOs remain Rust-owned at their transport boundary.

There are no handwritten desired/materialized-authority, proposal,
reconciliation, identity-grant, trust-root, certificate, manifest, or legacy
device-connect APIs.

## HTTP Surface

| Method | Route                                         | Purpose                                                                                                     |
| ------ | --------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `GET`  | `/auth/keys/{keyId}`                          | Current issuer signing key by key ID                                                                        |
| `POST` | `/bootstrap/service`                          | Admit a provisioned service and return shared installation data                                             |
| `POST` | `/bootstrap/device`                           | Admit an approved device and return shared installation data                                                |
| `POST` | `/auth/device/enroll`                         | Create/resume proof-bound device review                                                                     |
| `POST` | `/auth/requests`                              | Validate a proof-bound request and return a signed intent and portal URL; no authentication attempt created |
| `POST` | `/auth/intents/view`                          | Verify intent and read current public login choices or discover an existing transaction                     |
| `POST` | `/auth/transactions`                          | Start an independently portal-bound authentication attempt from a verified intent                           |
| `GET`  | `/auth/transactions/{transactionId}`          | Read authentication-attempt progress                                                                        |
| `GET`  | `/auth/transactions/{transactionId}/portal`   | Read bound portal progress and current consent                                                              |
| `POST` | `/auth/transactions/{transactionId}/approval` | Apply a fresh transaction-bound consent decision                                                            |
| `POST` | `/auth/transactions/{transactionId}/bind`     | Return the key-bound, idempotent login result and intent ID; no context returned                            |
| `POST` | `/auth/context/refresh`                       | Revalidate and return current runtime installation data                                                     |
| `POST` | `/auth/logout`                                | Revoke a real user login                                                                                    |
| `POST` | `/auth/password`                              | Change password and revoke sibling logins                                                                   |

Browser sign-in separates a reusable signed request from a bounded server-owned
authentication transaction. Rendering the portal verifies the intent without
starting an attempt; starting authentication creates the transaction. Local
login and registration carry `transactionId`; OIDC, consent, and completion
belong to that transaction. Account-flow continuations use
`browserTransactionId`; account and device review flows retain their own IDs and
lifetimes. Portal-binding and OIDC-cookie/CAS controls are documented in
`auth-protocol.md`.

Native bootstrap request DTOs are strict and accept only identity/proof inputs.
Browser request and bind proofs bind the complete raw body. All HTTP timestamps
and expiries are epoch milliseconds.

## Shared Installation Response

Native bootstrap and context refresh return one shape:

- `assignment`: server-owned deployment and instance identifiers;
- `runtime`: exact package/participant evidence, effective grant, resource
  evidence, signed context, route JWT, inbox, and expiries; and
- `transports`: exact Core NATS and WebSocket endpoints/options.

Bind returns the minimal login projection and its `intentId`. Browser clients
validate the intent, participant, and initiating-key bindings, commit that
result under their generation fence, then call context refresh. Exact supported
replay of a completed transaction returns the same semantic login result.

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

An exact binding stores the approved desired permission snapshot, not just the
permissions whose resources are already available. A new binding can initiate
resource provisioning; context issuance still requires the applicable resource
evidence and readiness. Resource permissions remain bounded by the exact
restrictions and delegation ceiling: approving a resource must not expand a
read-only snapshot into write access.

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

### User Management

`Auth.Users.Update` updates profile fields and account state with
`expectedVersion`. Optional `username` renames an existing local login: the
credential and local identity subject change atomically, while the principal ID
and password remain unchanged. Usernames are normalized, uniqueness is enforced,
and a conflicting username commits none of the accompanying profile changes.
Accounts without a local login establish one through an account flow, not a
profile update.

`Auth.Users.PasswordReset.Create` accepts a target `userId` and returns a
time-limited, one-use account-flow link. Creating the link does not itself
change the password. Completing it changes the target account's credential and
revokes its existing user sessions.

`Auth.UserIdentities.List`, `Auth.UserIdentities.Unlink`, and
`Auth.Users.IdentityLink.Create` accept optional `userId`. Omission targets the
caller; targeting another user requires platform administration. Unlinking still
enforces the last-sign-in-method and protected-administrator checks. An
identity-link flow is bound to its selected target user, not the administrator
who generated the link.

`Auth.Connections.List` supports `principalId` to inspect only the selected
principal's physical connections. User-session revocation and connection kicks
remain separate actions.

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
durably prepared body/proof tuple. The Events journal retains the context digest
rather than copying authorization state.

## Non-Goals

- compatibility endpoints for the retired authorization model;
- client-authored wire schemas;
- user-facing APIs for raw SQL revisions/timestamps beyond declared DTOs; and
- server-side use of browser-persisted authorization state.
