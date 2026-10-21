# Design: Auth API

The public Auth surface: the dedicated OAuth listener's HTTP routes, native
admission, and the Auth contract's RPC families. See
[trellis-auth.md](trellis-auth.md) for the authoritative core model and
[auth-protocol.md](auth-protocol.md) for proofs, custody, and admission rules.

## Source Of Truth

`crates/builtin-contracts/src/builtin_source/auth.trellis` is the native Auth
source. Generated Rust and TypeScript clients and schemas are the public RPC
reference. The OAuth listener's routes are defined by the router in
`crates/runtime/src/platform/auth/oauth/http.rs`; their HTTP-only DTOs remain
Rust-owned at that transport boundary.

There are no handwritten desired/materialized-authority, proposal,
reconciliation, identity-grant, trust-root, certificate, manifest, or legacy
device-connect APIs, and no HTTP bootstrap or context-refresh endpoints.

## HTTP Surface

All routes are served only by the dedicated OAuth listener (`auth.oauth`).
Console and application routers are never mounted there.

| Method | Route                                     | Purpose                                                                                         |
| ------ | ----------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `GET`  | `/.well-known/oauth-authorization-server` | Public OAuth metadata plus the Trellis installation, issuer, and NATS endpoint extension        |
| `GET`  | `/.well-known/trellis/issuer-chain`       | Public authenticated issuer rotation chain                                                      |
| `GET`  | `/authorize`                              | Authorization Code with PKCE for browser and native clients with callbacks                      |
| `POST` | `/token`                                  | DPoP-bound `authorization_code`, `refresh_token`, and Device Code grants with Trellis admission |
| `POST` | `/device_authorization`                   | Start an RFC 8628 Device Code authorization bound to the client's DPoP key                      |
| `POST` | `/revoke`                                 | RFC 7009 token revocation                                                                       |
| `POST` | `/auth/device/enroll`                     | Create or resume a proof-bound device review                                                    |
| `GET`  | `/login`, `/consent`, `/device`           | Portal pages for sign-in, capability consent, and Device Code entry                             |
| `GET`  | `/device/complete`                        | Portal page shown after a Device Code decision                                                  |
| `GET`  | `/portal-assets/{path}`                   | Embedded portal assets                                                                          |
| `POST` | `/portal/session`                         | Create a portal session cookie and CSRF token                                                   |
| `POST` | `/portal/login`                           | Local username/password sign-in                                                                 |
| `POST` | `/portal/logout`                          | End the portal login                                                                            |
| `GET`  | `/portal/providers`                       | List enabled upstream OIDC providers                                                            |
| `GET`  | `/portal/oidc/{provider}/begin`           | Start upstream OIDC with server-owned state, nonce, and PKCE                                    |
| `GET`  | `/portal/oidc/callback`                   | Verify the upstream OIDC callback                                                               |
| `GET`  | `/portal/authorization/{transaction}`     | Read a pending authorization's consent view                                                     |
| `POST` | `/portal/approve`                         | Apply a fresh, transaction-bound consent decision                                               |
| `POST` | `/portal/device`                          | Submit a Device Code user code for consent                                                      |
| `GET`  | `/account/{kind}`                         | One-use account action page (`reset` for password reset, `link` for identity linking)           |
| `POST` | `/portal/account/{kind}`                  | Complete an Origin-checked, CSRF-protected account action                                       |
| `GET`  | `/profile/avatars/{token}`                | Expiring, sanitized profile image reference                                                     |

Discovery and the issuer chain allow credential-free cross-origin GETs. Token,
Device Code, enrollment, and revocation endpoints allow CORS only from validated
client callback origins. Cookie-authenticated portal routes never enable CORS.
Request admission budgets are described in `trellis-auth.md`. All HTTP
timestamps and expiries are epoch milliseconds.

## Native Admission

There is no HTTP bootstrap. Services and devices present a signed
`trellis.provisioned-admission.v1` credential in NATS CONNECT, proving their
provisioned identity key and fresh runtime session key; Auth Callout resolves
deployment, instance, participant, accepted implementation, and resources from
its own records. Users and applications obtain a DPoP-bound OAuth token whose
`trellis.oauth-admission.v1` extension authorizes CONNECT for the same runtime
key. Connected participants retrieve and renew their own signed authority over
NATS through `Auth.Authority.*` and `Auth.Admission.Renew`, never over HTTP.

## RPC Surface

The Auth contract groups its RPCs into these families. Generated clients carry
the exact request, response, and error types.

| Family                | RPCs                                                                                                                                                                             |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Own authority         | `Authority.Resolve`, `Authority.Renew`, `Authority.Status`, `Admission.Renew`                                                                                                    |
| APIs and capabilities | `Apis.List/Get/Review/Accept/ForceReplace`, `Capabilities.List`                                                                                                                  |
| Capability grants     | `CapabilityGrants.Grant/List/Revoke`, `Delegations.List/Review/Revoke`                                                                                                           |
| Roles and privileges  | `Roles.*`, `RoleAssignments.*`, `PlatformPrivileges.*`, `PlatformDelegations.*`                                                                                                  |
| Principals            | `Principals.EffectiveAccess`                                                                                                                                                     |
| OAuth clients         | `OAuthClients.List/Get/Put/Disable/Delete`                                                                                                                                       |
| Upstream identity     | `OIDCProviders.*`, `OIDCRoleMappings.*`                                                                                                                                          |
| Issuers               | `Issuers.Rotate`, `Issuers.ResolveChain`, `Issuers.Revoke`                                                                                                                       |
| Deployments           | `Deployments.Create/Apply/Get/List/Disable/Enable/Remove`                                                                                                                        |
| Service instances     | `ServiceInstances.Provision/List/Disable/Enable/Remove`                                                                                                                          |
| Devices               | `Devices.Provision/List/Disable/Enable/Remove`, `Devices.Reviews.*`, `DeviceCompanions.Attach/Revoke`                                                                            |
| Resources             | `Resources.List/Get/Remove`, `ResourceBindings.Query`                                                                                                                            |
| Sessions              | `Sessions.Me/List/Revoke/Logout`, `AuthorizationSessions.List/Revoke`                                                                                                            |
| Connections           | `Connections.List`, `Connections.Kick`                                                                                                                                           |
| Users                 | `Users.Create/Get/List/Resolve/Update`, `Users.Profile.Update`, `Users.Password.Change`, `Users.PasswordReset.Create`, `Users.IdentityLink.Create`, `UserIdentities.List/Unlink` |

### Issuers

Fresh initialization provisions the initial private issuer seed. Authorized
rotation generates and durably stores successor custody locally before committing
the signed trust transition and idempotent result. Startup reads the committed
current key; it never rotates or recreates missing keys. No private key crosses
an RPC, and no offline root or manifest RPC exists. Historical contexts remain
stored after ordinary rotation; explicit issuer revocation remains separate.

### Deployments And Provisioning

Deployment Apply is the direct mutation path. There is no proposal, plan,
approve, reconcile, or materialized-authority RPC family.

### Resource administration

`Resources.List/Get` expose all catalog allocations, including detached resources,
with digest resource IDs, explicit `user`/`deployment` owner kind and owner ID,
participant, local name, kind, revision, and lifecycle state. List supports direct
owner filters without joining only active deployments. The finite
`principals.manage` privilege is required in addition to the route capability.

Declaration omission and deployment Remove detach resources and remove binding
authority while retaining physical data. Disable preserves allocations and binding
commitments but retires sessions and denies admission while disabled; Enable reuses
those retained bindings without reapplying the contract. No lifecycle path automatically destroys
an allocation. `Resources.Remove` is an explicit permanent deletion choice for a
detached owned KV at its expected revision. Its typed idempotent result fences a
durable manual intent before the ownership-checked broker deletion. The same
manual request can retry after failure; no startup cleaner or automatic omission
job consumes that intent. Reattachment uses a compatible retained allocation;
retention reductions fail rather than purge or recreate data.

### Sessions And Connections

`Sessions.Me` returns the logical connection and nullable user login. Durable
session RPCs are login-only; native service/device connections have no session
row. `Connections.List` supports `principalId` to inspect only the selected
principal's physical connections. User-session revocation and connection kicks
remain separate actions.

### User Management

`Auth.Users.Update` checks account lifecycle `expectedRevision`. Profile fields
are optional: a lifecycle-only update leaves the profile untouched. An intentional
profile edit must also supply `expectedProfileRevision`; its independent CAS is
checked in the same transaction, so a stale combined update changes neither the
account lifecycle nor its profile. Owner edits continue through
`Auth.Users.Profile.Update` with the self account's profile revision. Creation
does not provide a zero-revision bypass for updating existing profiles.

`Auth.Users.PasswordReset.Create` accepts a target `userId` and returns a
five-minute, one-use token. Open `/account/reset?token=...` on the
isolated OAuth origin to complete it with an Origin-checked, CSRF-protected
native form. Creating the token does not change the password. Completion checks
credential CAS, the target's protected-administrator status, and the requester's
still-active logical authorization session and current accepted
`principals.manage` privilege. It atomically changes the credential, consumes
the action, and revokes existing logins, OAuth grants, and refresh families.
Bootstrap administrators cannot use this RPC reset path; their existing local
recovery path remains authoritative. Replay responses keep the same bearer token
under existing OAuth sealing custody; replaying creation never makes a consumed
action usable again.

`Auth.UserIdentities.List`, `Auth.UserIdentities.Unlink`, and
`Auth.Users.IdentityLink.Create` target the current user in the current generated
contract. Identities have persisted ULIDs. Unlinking enforces owner isolation,
last-sign-in-method and protected-administrator checks. Link creation returns a
five-minute isolated portal URL. The portal requires the exact initiating user to
be signed in, then starts fresh upstream authentication with server-owned state,
nonce, and PKCE through the ordinary verified OIDC callback. Completion rechecks
the portal session and provider revision/state, rejects identities owned by
another user, and atomically inserts the identity and consumes the account action.

## Authorization

Administrative RPCs require the finite platform privilege named by the route
(for example `principals.manage`, `clients.manage`, or `privileges.manage`) in
the caller's current accepted authority, in addition to the route capability.
Privileges gate the administrative surface; they do not fabricate participant
grants. Mutation authorization and idempotency replay are checked in the same
SQL transaction using the current principal, login/credential, exact authority
revision, expiry, and privilege.

## Events And Post-Commit Work

Authorization, issuer, resource, device, and deployment changes emit ordered
typed events through the transactional post-commit queue. Superseded contexts
are durably revoked before connection kicks are attempted.

Attachment admission, closure, and confirmed kicks enqueue their declared
`Connections.Opened`, `Connections.Closed`, and `Connections.Kicked`
events in the transition's SQL transaction. A confirmed kick is
recorded separately from disconnect observation, so either callback order emits
one closure and one kick event. Outbox retries retain the event ID, occurrence
time, subject, descriptor, and generated payload.

Auth's event publisher uses its ordinary admitted provider and signs each
publication with current authority; it does not retain an expired proof.
Publication failure retains durable work. Publication starts after provider
admission, independently of the enforcement worker needed to establish that
admission. The Events journal retains the context digest rather than copying
authorization state.

## Non-Goals

- compatibility endpoints for the retired authorization model;
- client-authored wire schemas;
- user-facing APIs for raw SQL revisions/timestamps beyond declared DTOs; and
- server-side use of browser-persisted authorization state.
