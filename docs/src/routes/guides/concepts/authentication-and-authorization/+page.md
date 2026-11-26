---
title: "Authentication and authorization"
description: "Principals, session keys, browser login, portals, approvals, and capabilities."
---

Trellis separates authentication, participant-scoped grants, signed
authorization contexts, and transport permissions. The Rust platform runtime
owns these boundaries. Services and clients never read Auth storage or derive
NATS subjects themselves.

## Principals

| Principal | Authentication                                                |
| --------- | ------------------------------------------------------------- |
| User      | Browser or CLI login established through an Auth portal       |
| Service   | Immutable provisioned Ed25519 identity for a service instance |
| Device    | Provisioned/activated Ed25519 identity for a device instance  |

OIDC identities and local passwords are sign-in methods for a Trellis user.
They are not separate authorization containers. Durable Auth sessions represent
user logins only; services and devices authenticate without a login row.

Signing out of the Portal doesn't sign the person out of their OIDC provider.
So the next OIDC sign-in from that browser sends the provider a `prompt`, set
per provider by its "Prompt after sign-out" setting in the Console, or
`trellis sign-in providers add|edit --prompt-after-sign-out`:

- `login`, the default, sends `prompt=login`: the provider asks for
  credentials again instead of quietly reusing its session. Not every provider
  supports it; Google documents that it doesn't.
- `select-account` sends `prompt=select_account`: the provider shows an
  account chooser. Picking an account doesn't prove the person signed in
  again.
- `consent` sends `prompt=consent`: the person approves this Trellis again,
  which also isn't proof of a fresh sign-in.
- `none` sends nothing, so the provider may sign the person straight back in.

This applies after any sign-out from an app, including the Console's "Sign in
again" after a failed sign-in, even when the Portal login had already ended. A
Portal login that just expires doesn't count. After the sign-in succeeds, later
sign-ins use the provider's session as usual. If the provider refuses the
prompt, the sign-in page names the setting to change, and the next try with
that provider from that browser goes without the prompt. Choosing another
provider from that page still sends that provider's prompt.

Immediate `Auth.ServiceInstances.Provision` and `Auth.Devices.Provision` require
`identityPublicKey`: generate the Ed25519 identity key on the client and submit
only its public key. Keep the private seed client-side; Trellis never generates,
stores, or returns it. A missing public key returns validation code
`identity_public_key_required` without creating a principal or instance, and the
failed request does not consume its idempotency key. The current response has
`provisioningSecret: null`; one-time activation-token provisioning is not yet
implemented. CONNECT proves possession of the registered immutable identity key.

## Account actions

`Auth.Users.PasswordReset.Create` returns a one-use token for a non-bootstrap
user that lasts 24 hours. Open `/account/reset?token=...` on the Trellis origin and
submit the native password form. Completion requires that the requester still
holds `accounts.manage`, not that they are still signed in, rejects stale
credentials, and atomically changes the password and signs out existing
login/grant families.
Bootstrap administrators use the existing local recovery path instead.

`Auth.Users.IdentityLink.Providers` lists the providers the current user can
link, by ID and name. `Auth.Users.IdentityLink.Create` returns a five-minute
portal authorization URL for the current user. Sign in as that same user, then continue through the
identity provider's fresh OIDC authentication. Provider revision/state, nonce,
PKCE, and current portal-user continuity are checked before the identity is linked.
An identity belonging to another user cannot be reassigned. Creation retries
return the original response; they do not renew or revive consumed actions.

## Linking someone else's OIDC sign-in

An administrator with `trellis.auth.v1:accounts.manage` can link an OIDC
sign-in to another person's account without that person signing in:
`Auth.Users.Identities.Link { userId, identity: { providerId, subject },
expectedRevision, idempotencyKey }`, where `subject` is the provider's exact
`sub` and `expectedRevision` is `user.account.principal.revision`. The provider
must be enabled and the account enabled. The result is the account and its
linked sign-ins, as `Users.Find` returns them. Linking a sign-in the account
already has changes nothing and succeeds whatever revision it names, so a
script can run twice. A sign-in another account has fails with the Auth error
code `identity_linked_to_another_user`. `Auth.Users.Create` takes the same
sign-ins as `identities` and links them in the transaction that creates the
account, so one taken sign-in leaves no account behind.

`Auth.Users.Identities.Unlink` takes the same input and removes the sign-in,
ending the sign-ins made with it. It is refused when the sign-in is the
account's only way in, the same rule people meet when they disconnect one
themselves. Neither call reaches the default administrator's account.

On the command line:

```sh
trellis users create --username ada --provider <provider id> --subject 'auth0|1234'
trellis users create --username ada --id 01JD8Z3XQ5V7H2N4K6M8P0R2T4 --provider <provider id> --subject 'auth0|1234'
trellis users link ada --provider <provider id> --subject 'auth0|1234'
trellis users unlink ada --provider <provider id> --subject 'auth0|1234'
```

Commands that take a user accept a username, an email address or a user ID,
and role commands take a role's name or ID (`trellis roles assign Auditors
--user ada`). A name that fits several users or roles is refused with their
IDs. Something shaped like an ID is looked up only as an ID, so an ID nobody
has is refused, even when it is someone's username. An account created with `--provider` gets no password setup link.

`--id` gives the new account a Trellis user ID you choose instead of a new
one. Use it when people move from another Trellis installation, so that
services which stored their user IDs need no change. The ID must be a ULID,
and Auth refuses one that any user, service or device in the installation has
or had, with `already_in_use`.

In the
Console, the **OIDC sign-ins** panel of a user's page lists their sign-ins and
links or unlinks one.

A linked sign-in counts as never seen until its first sign-in. Until then it
adds no name, email or picture to the profile, and nothing about it is
verified. That first sign-in is an ordinary OIDC sign-in: it refreshes the
profile's OIDC fields and applies the provider's role mappings, exactly as for
a sign-in the person linked themselves. Fields an administrator set, such as
the display name `Users.Create` requires, stay as set.

### Pre-linking users for a migration

Moving people from another system whose data is keyed by user ID goes like
this. Add the OIDC provider and mark the roles new users should get. For each
person, call `Users.Create` with their name and `identities: [{ providerId,
subject }]`, using the `sub` the provider gives them, and record the old ID
against the returned `user.principalId` to re-key your own data. The person's
first sign-in through that provider then opens that account, even with self
sign-up and local users turned off, and creates no second account. A matching
email never links accounts, so the `sub` is the only key. Rerunning the script
is safe: `Users.Find { identity }` shows who already has an account, and a
repeated `Users.Identities.Link` changes nothing.

## Profile photos

Directory responses can include `avatarUrl`, a Trellis-hosted image-only bearer
valid for at most ten minutes. Use it in an `<img referrerpolicy="no-referrer">`,
render local initials when absent or broken, and reread the authorized directory
to renew it. Do not log or share this URL: access to the URL permits reading only
that image until expiry. Profile edits invalidate earlier URLs. Revoking
`usersRead` prevents issuing new URLs but does not instantly revoke existing ones.

Your picture is the Gravatar for your email, verified or not, unless you
choose a linked OIDC sign-in's picture. An account created by an OIDC
sign-in starts with that provider's picture. When the chosen picture is missing,
Trellis falls back to Gravatar. Gravatar receives a normalized SHA-256 email
hash, not raw email; the hash is still personal information. Your browser
contacts Trellis only, and failed image retrieval does not prevent login or
directory lookup.

Operators can set `[auth.avatar] external_fetch_enabled = false` to disable all
remote-derived images, including cached photos. Safe retrieval permits only
public HTTPS destinations with no redirects, and stores sanitized small PNGs.

## Own directory profile

Select `trellis.auth.v1:account.edit` and call `Users.Profile.Update`
to edit your own display/given/family names, email, and picture source:
`pictureProviderId` for a linked OIDC sign-in's picture, or `useGravatar`. Use `Users.Resolve`'s `user.account.profileRevision` as
`expectedRevision`, plus a fresh `idempotencyKey`. There is no target user ID.
Omitted fields stay unchanged. `clearEmail` restores an eligible linked-provider
email; do not send a value and its clear flag together, or both picture
choices.
Manually entered email is not verified and never links or authenticates accounts.
Manual choices survive future OIDC logins. Provider unlink removes only its
nonmanual profile contributions, not your other sign-in methods.

Choose how people sign in on the Methods tab of the Console's **Platform →
Sign-in** page, or with `trellis sign-in set` and `trellis sign-in providers`.
Username and password sign-in, self sign-up and the minimum password length
(8 by default, 8 to 128) apply at once; the minimum covers new and changed
passwords, not existing ones. OIDC sign-in is on while at least one provider is,
and each provider turns on and off by itself. Trellis refuses a change that
would leave nobody able to sign in.

Self sign-up also needs the app to allow it (`trellis clients sign-up`). A
device signing in with a code is different: the Portal asks for the account
before the code, when it doesn't know the app yet, so it offers sign-up if
any native app allows it. After the code is typed, an account created for an
app that doesn't allow sign-up is refused with "This app doesn't allow new
accounts". The account remains, with no access to that app.

Configure upstream providers with `openid profile email` when you want standard
profile claims; Trellis preserves explicitly configured scope choices. Optional
UserInfo fills missing signed-token profile fields only when its `sub` matches.
An unavailable or malformed enrichment response does not block valid login.

Trellis reaches a provider through the server's system trust store. An OIDC
provider on a private CA needs its certificate issuer pinned by SHA-256
fingerprint: saving the provider without one is refused with the fingerprint
the provider presents. Confirm that value with the provider's operator, then
enter it in the provider's Certificate fingerprint field, or pass
`trellis sign-in providers add|edit --certificate-fingerprint <sha256>`. Trellis
checks it against the chain the provider presents and trusts that issuer for
that provider only. Clearing the field, or
`trellis sign-in providers edit --no-certificate-fingerprint`, removes the pin
even while the provider still needs it; its sign-ins then fail until you pin it
again.

Some providers need extra parameters on the sign-in request. An Auth0
application that requires an organization refuses every sign-in with
"parameter organization is required for this client" until the request names
one. Add it under the provider's Authorization parameters in the Console, or
with `trellis sign-in providers add|edit --authorization-param
organization=org_123` (repeat the flag for more). Trellis sends them on every
sign-in and account link through that provider. `edit` replaces the list when
you give any and keeps it when you don't; `--clear-authorization-params`
removes them all. Names Trellis sets itself, such as `redirect_uri`, `state`
and `prompt`, are refused. Trellis doesn't check the provider honored a
parameter: it doesn't compare an Auth0 ID token's `org_id` with the
organization you set.

## Installed participants

Every participant is installed in Auth before it can be granted access. An
installed revision identifies verified source-package evidence, its semantic
digest, and the participant's exact lexical path. Revisions are immutable,
so Auth can always explain the exact API surface behind an issued context.

The CLI (`trellis-app.cli@v1`), Console (`trellis-app.console@v1`), activation
Portal (`trellis-app.portal@v1`), and Auth runtime are installed built-ins.
Their IDs do not grant access by themselves.

## Capabilities

Capabilities are approval and explanation groups. Approval is constrained by
current policy and is converted to exact selected grants; it is not itself a
runtime permission token.

The consent page lists what the app requires without checkboxes: approving
grants it. Optional capabilities start checked, administration included,
except anything the person declined before. Administration then takes a
separate confirmation step. The page remembers every answer,
approvals and declines alike; there is no switch to forget them. A person who
clears an optional capability isn't asked about it again at sign-in. The app
asks again when they use it, and the page shows it unchecked. An
app registered with access it may have without asking (`impliedConsent`) keeps
that access in its grant even before the person holds it, so a role an
administrator assigns later reaches the open app at its next authority renewal,
within `context_lifetime_seconds`, with no new sign-in. That is how the Console
picks up a new role. An app that has to ask gets new access only after the
person approves it.

## Effective authority

Auth computes a user's capabilities from roles, direct capability grants, and
fresh OIDC role mappings. An application acts only within that set, intersected
with the capabilities it requested and those the user approved in its OAuth
grant. Services and devices are bounded by their deployment. Platform privileges
such as `principals.manage` are a separate finite set and never imply
application capabilities. Auth evaluates this policy whenever it issues signed
session authority; see [Participant grants](/guides/concepts/deployment-authority).

## Managing users and access in Console

The user edit screen keeps profile, access grants, sign-in methods, and active
sessions together. Profile and access edits save separately because they are
independent transactions.

Administrative profile updates preserve the immutable user ID and local username.
Users without a local password can establish a local sign-in method through a
password-reset account flow.

Generate a password-reset link for the selected user, then securely give that
one-use, time-limited link to them. Generating the link does not reset their
password; completing the flow does, and revokes their existing login sessions.
Identity-link flows also target the selected user. Removing a sign-in method
cannot remove the last method or bypass protected-administrator safeguards.

The access section assigns direct capability grants and roles. A role grants
its capabilities for as long as the user holds it, so changing a role changes
every member's access. Platform privileges are separate and never imply
application capabilities. Service and device access belongs to their
deployment; changing it can affect other instances in that deployment.

End a session to revoke that login. Disconnect a connection to terminate a
physical connection; these are distinct actions.

## Session keys

Each live installation proves possession of its session key. The key identifies
the installation cryptographically but does not grant authority by itself.

## Authorization contexts

When a principal bootstraps or refreshes, Auth rereads the current credential,
binding, installed participant, resource evidence, and online issuer in one
transaction. It signs an authorization context containing that exact snapshot
and stores it permanently by digest before use.

OAuth sessions retain the refresh family verified at CONNECT. Every authority
or admission renewal consults that exact family's absolute deadline and
revocation state, even if another family of the same grant remains valid.
Rotation does not extend the family deadline. Capability-specific deadlines
remain separate from this shared credential-root limit.

A live OAuth caller can use `Admission.Renew` with its own context digest and
ordinary session-key proof to obtain a fresh, same-key signed admission. Auth
rechecks policy and validates the candidate authority in the same transaction;
the admission lasts at most 300 seconds and never outlives that authority or
the family. This does not rotate refresh tokens or create durable reconnect
credentials. Provisioned admission must still be signed locally with the
client's identity key (`provisioned_signature_required` otherwise).

Applications carry only the context digest, a message proof, message-specific
metadata, and a small `session-key` header that must equal the key inside the
signed context. The header is redundant checked metadata, never independent
authority.

Providers resolve an unknown context by digest, verify the issuer signature and
explicit revocation state, verify the message proof, and authorize the exact
action from the grant. Their bounded in-memory caches do not call Auth on a
cache hit.

## Bootstrap and NATS

Services and devices send a signed provisioned admission in NATS CONNECT. Auth
Callout resolves the deployment, participant evidence, capabilities, resources,
and inbox server-side and returns signed session authority with exact transport
permissions, which the runtime verifies.

A client CONNECTs with its session NKey signature and its
`trellis.oauth-admission.v1` or `trellis.provisioned-admission.v1` credential;
it presents no client JWT. Auth Callout validates the signed OAuth admission or
identity-bound provisioned admission, rechecks current issuable state, and
installs the exact transport policy signed into the selected session authority.
It never derives authority from subject strings or the redundant session key.

### User directory

Request `trellis.auth.v1:users.read` to search active profiles through the generated
`Users.List` and `Users.Get` actions. Consent covers names, email and photos.
Responses identify users by immutable `principalId`; display names and emails
are not unique and never grant access to application-owned objects.

Ordinary results omit `account`. That projection contains username, local-password
status and principal lifecycle/revision, and requires both delegated and currently
eligible `principals.manage`. Only qualified administrators can inspect inactive
users or search usernames. `Users.Resolve`, authorized by `accountRead`, returns
only the caller's own profile and account. Account mutations retain their separate
capabilities and administrative checks.

## Browser login and the portal

Browser applications use OAuth Authorization Code with PKCE and DPoP against
the Trellis origin. No application shares it except Trellis's own Console at
`/console`, which can be given an origin of its own instead. Trellis's portal
on that origin
performs local-password or OIDC sign-in and explicit capability consent under its
own cookie and CSRF controls. The token response carries a signed Trellis
admission for the browser's fresh runtime key; the client connects with it and
then retrieves its signed authority over NATS.

Only the browser's OAuth custody is durable: a non-extractable DPoP key and an
encrypted refresh credential in IndexedDB. Ephemeral runtime keys, signed
authority, issuer keys, grants, and transport endpoints remain in process
memory.

## Devices

Device enrollment uses `/auth/device/enroll` to create or resume a review.
Approval does not install runtime credentials. Once approved, the device's NATS
CONNECT carries a signed provisioned admission that proves its durable identity
plus a fresh session key, the same admission path a service uses.

## Revocation and history

Authorization-changing transactions commit state, affected-context
revocations, typed events, and connection-kick intents together. Explicit
revocation invalidates proofs. Ordinary expiry, connection closure, login
revocation, grant replacement, and issuer rotation retain signed contexts for
historical event validation.

Events history stores context-digest references rather than duplicate contexts
or grant sets. Events signed inside their validity window may remain historically
valid after ordinary expiry, but never after an applicable explicit revocation.

### Rotating issuer keys

An administrator with `platform.manage` calls `Issuers.Rotate` with the
expected current key ID and an idempotency key, or runs
`trellis issuers rotate <current-key-id>`. Auth generates the successor
locally and signs its public key with the current issuer, allowing clients to
verify the chain without trusting an unrelated key. A repeated request returns
the original result. A stale expected key is refused with a validation error
that names the current key.

The new key is announced first. Its chain link goes out at once, so every
client can verify the key before it is used, and Trellis keeps signing with the
current key until the activation delay has passed:
`auth.authorization.issuer_activation_delay_seconds`, 60 seconds by default.
The command prints the time the new key takes over. Until then another
rotation is refused, and sessions keep running throughout; they move onto the
new key when Trellis switches.

`Issuers.Revoke` (`trellis issuers revoke <key-id>`) revokes a retired key. It
can't revoke the current signing key, since Trellis would have nothing left to
sign with; the refusal says to rotate first, or, while a new key is announced,
when the switch happens. An unknown key ID is `not_found`
("no issuer key ..."), and a key revoked once is refused as already revoked.
A revoke sent with a session still on authority the key signed is refused too,
since its reply would not verify once the key is revoked; the CLI connects
afresh for each command, so this only affects a long-running session in the
moment after the switch. Every refusal leaves the key as it was.

Successor keys are private files in a sibling directory obtained by replacing
the configured issuer seed file's extension with `.keys`. Back up that directory,
the original seed, Auth's database, and the independent OAuth storage key
together. Rotation does not change the OAuth storage key or revoke existing
sessions. Restart uses the database-selected issuer and refuses missing or invalid
custody rather than replacing it or falling back to the original key.

## Device companions

A device may declare one named app or agent companion as its lexical child. The
child cannot start a standalone browser or native bootstrap; the parent device's
activation flow is its only admission path.

The activation operation returns server-computed `companionConsent`. The
approving user submits a separate child `Approval` fenced by its decision
digest, installed revision, expected grant revision, and exact eligible
capability and resource selections. Changed consent or authority makes that
approval stale and Auth rejects it. Auth commits the approval and device
authority before creating the child login session.

The companion then runs as the user with its own OAuth grant, login session,
session authority, resources, and connection. Parent and child grants,
connections, expiry, and revocation remain separate; device credentials never
substitute for unavailable user authority.
