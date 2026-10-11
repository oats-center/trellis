# Design: Trellis Authentication And Authorization

Status: Revision 7. The authoritative Auth core is implemented through Phase 2.
Phase 3 includes OAuth storage, local/OIDC login, consent, device authorization,
signed admission, isolated portal hosting, TLS, and browser/native custody
components. Production startup, Callout/ACL lifecycle wiring, native CLI flows,
and complete legacy-path removal remain unfinished; component verification does
not establish production deployment readiness. Phase 4 has started with shared
Rust/WASM provider-response and issuer-metadata verification, and SDK signing
of catalog-stamped request/event transcripts with retained request digests.
These primitives are not yet wired through all application transports. Shared
SDK authority ownership, provider/resource integration, and the Console cutover
remain unfinished. This is a review checkpoint, not phase acceptance or a
release. This is an unreleased clean break,
not a compatibility path for GrantBinding or the former browser bind protocol.

## Authority Ownership

Auth's platform SQLite store owns principals, local credentials, verified login
sessions, registered clients, OAuth grants, provisioned identities, role/direct
entitlements, OIDC mappings, accepted APIs, deployments, issuer keys, logical
authorization sessions, immutable signed material, and durable effects.

Providers verify compact signed results. They do not resolve roles, reproduce
OAuth consent, track source floors, or subscribe separately for every context.
NATS KV is a rebuildable distribution mirror, never authoritative policy.

### Installation Identity And Authorization Listener

Fresh initialization generates `auth.instance_id` as a canonical ULID. Retain it
with the installation configuration and database across restarts and all key
rotations; startup requires it rather than inventing a new identity.

`auth.oauth` explicitly configures the dedicated listener's `origin`,
`bind_address`, `port`, and `transport`. TLS mode requires `certificate_chain`
and `private_key` PEM paths resolved relative to the runtime configuration.
`loopback_development` is an explicit plaintext mode restricted to loopback
origins and addresses, never a TLS fallback. The OAuth origin must differ from
all configured application origins. A shared hostname is allowed but requires a
prominent cookie-scope warning, because cookies are not port-scoped. Console
assets and application routers are never mounted on the OAuth listener.

### OAuth Storage Key Custody

Fresh initialization provisions `auth/oauth-storage.key`, independently of the
online issuer, as an owner-only `0600` file containing a base64url 32-byte key.
`auth.oauth_storage_key_file` selects it; relative paths resolve against the
runtime configuration file. Retain and back up this key together with Auth's
database. Issuer rotation does not replace it. Bootstrap refuses to overwrite
existing material. The OAuth adapter refuses missing, malformed, symlinked, or
non-private key files and authenticates existing encrypted OAuth material before
accepting new writes. Key files must be owned by the process user, have no other
hard links, and fit the bounded encoded-key format. Opening refuses symlinks
without following them. Startup never generates a replacement key. Production
startup wiring remains part of the Phase 3 cutover.

### Authorization HTTP Boundary

The OAuth adapter bounds form bodies and per-peer/origin request admission,
rejects untrusted Host/HTTP2 authority and cross-origin portal mutations, and
limits CORS to origins from validated client callbacks. Forwarding headers do
not select the peer identity. Expiry cleanup removes transient portal, proof,
device, and token state without deleting immutable authority or audit evidence.

Portal documents use `Referrer-Policy: same-origin`: cross-origin destinations
receive no referrer, while same-origin forms retain the Origin required by the
strict CSRF check. Accepting `Origin: null` is not a fallback. Own-context
introspection binds its reply to the ephemeral key's derived inbox namespace;
the responder checks that routing before publishing either success or errors.

Console and OAuth portal are separate required frontend apps. Their build tools
now use Node 24 LTS, npm workspaces, and Vite; embedded builds compile into
staging before replacing only owner-marked output. This tooling change does not
migrate the Console's older SDK authentication to Revision 7. The required
Console build currently fails on retired participant imports, and SDK builds
fail on retired protocol APIs; no placeholder assets or optional Console build
are substituted to conceal that later-phase dependency.

Dex is exclusively a real upstream OIDC test dependency, not a deployment
component. Trellis validates upstream nonce, signature, issuer, and audience
before translating verified claims through its own role mapping and consent.

## One Capability Compiler

The compiler reads one coherent SQLite transaction. It unions independently
valid role, direct, and fresh verified-OIDC entitlement paths before
intersecting them with the application/deployment selection and the OAuth
grant's approved capability identities and consent revisions. Services/devices
are bounded by their deployed native participant selection. Platform privileges
are a separate finite set, not capabilities or arbitrary action grants.

The result includes approved capabilities, required-missing and
optional-unavailable selections, accepted API generations/revisions, provider
certificates, ownership bindings, privileges, explanations, and the earliest
applicable deadline. Pending backing resources do not remove an ordinary user's
capabilities. Freshness expiry removes only the affected OIDC entitlement path;
another valid direct or role path can preserve the capability.

Approval never adds an unknown or declined optional capability at renewal.
Compatible API expansion is available to a renewed context only while the
capability remains approved at the same consent revision.

Fresh explicit approval records grant-local consent provenance. Older remembered
declines or expired approvals cannot veto "approve once," and that one-off
choice does not rewrite remembered policy. Reused remembered choices must still
be approved and inherit their optional expiry. Later shared decisions and
explicit withdrawals still constrain existing grants. Approval commits scoped
enforcement work atomically when it changes remembered consent or platform
delegation.

Deadline reductions compare each outstanding context with its shared credential
limit and the current limits of only the rights that context carries. Newly
gained capabilities or privileges cannot shorten older, narrower authority.
Their limits bound newly issued contexts, not the persistent logical-session
root; spent contexts are excluded after their acceptance deadline.

Trusted native device provisioning commits the device record with its principal,
deployment, instance, identity key, and entitlements before authority issuance.

## Catalog Acceptance

One accepted definition exists per API major. Review authenticates
package/source evidence, computes normal compatibility, records
action/capability/consent changes, and binds the accepted and affected policy
revisions. Acceptance checks that review and all expected revisions in its
transaction. Accompanying policy edits either commit with the definition or do
not apply.

Normal acceptance increments the local accepted revision. New actions and
capability memberships record introduction revisions: an older signed context
cannot gain them merely because a newer catalog is available.

Force replacement requires its finite privilege, reviewed-digest confirmation,
explicit breakage acknowledgment, and declared meaning changes with increased
consent revisions. It creates a new API generation at revision 1 and makes
displaced providers ineligible for admission. Technical wire breakage alone does
not imply that every delegated meaning changed. Retained signed catalogs support
verification and audit, not runtime compatibility fallback.

## Logical Authorization Sessions

Auth checks the current credential/root, runtime identity, and proof key before
issuing `trellis.session-authority.v2`. It signs canonical bytes and commits the
immutable context, logical-session association, and publication work together.
Resolution by digest returns that exact committed object even after restart.

Renewal may reuse an active logical ID only while no authority has been lost
relative to any still-usable context issued under it. A narrower newest context
cannot hide an older broader live context. Comparison includes provider/API
generations, privileges, ownership bindings, and deadlines. Derived comparison
state expires when those contexts cease to be usable; immutable history remains.

Reduction irreversibly retires the ID. A surviving root can reconnect with a new
logical ID after reevaluation. Disabled principals, revoked grants/logins or
provisioned identities, and compromised keys cannot use this route to regain
authority. Physical attachments bind the logical ID to an exact server/client
association, an attachment marker, and the same ephemeral NKey as its proof key.
Attachment admission reevaluates inside the transaction; delayed enforcement
cannot admit an already-retired or newly-reduced session.

## Durable Enforcement And Distribution

Policy mutations commit small scoped work items through aggregate idempotency.
Bounded transactional pages resume from committed cursors after restart,
recompute through the same compiler, and preserve sessions when an independent
entitlement path means nothing was lost. Session/root retirement persists its
cutoff, reason, signed revocation, and exact attachment kick effects atomically.

Ordinary reevaluation uses retirement commit time as the effective cutoff, not
the earlier policy edit time. Direct logout/family/session retirement records
the cutoff in that operation's transaction. Work progress retains policy commit
time separately. Every still-live digest under the retired ID is affected.

The effect publisher sends signed session-ID revocations before dependent kicks.
Hot entries remain through the maximum context acceptance deadline, including
skew. Durable signed cutoffs remain available for historical verification. A
bounded positive-only verified cache retains known cutoffs through missing
mirror entries; forged updates cannot install or erase a cutoff. A cache miss is
not evidence that unknown authority is valid. Owners use one shared
snapshot/update stream; stopped updates permit already-verified contexts only
through expiry.

Kicks use the operational system-account client and exact broker inventory. A
reused CID with another attachment marker is never kicked. Unavailable or
malformed inventory is retryable, not proof of absence. `CONNZ`'s filtered
`num_connections`, not its global `total`, establishes CID absence. Effects have
bounded claims, retry backoff, stale-claim recovery, and predecessor ordering.

## Issuers And Bootstrap

The initial administrator is created through the local production bootstrap
boundary and remains protected against disablement or administrative demotion.
Passwords use Argon2; password change/reset retires the affected login families
in the same transaction. Authentication failures do not identify which
credential check failed.

Issuer rotation installs a signed, preceding-key-bound, monotonic chain record
and retains retired public keys and immutable signed contexts. Rotation commits
the successor's public identity; its private key remains operator-owned and must
be installed as the configured runtime signer before further issuance. A
previous signer cannot continue issuing after that commitment. Rotation itself
does not retire sessions. Explicit key compromise revokes affected sessions and
makes that key unusable; restart must not reinstall it as an active signer.
Issuer rotation/revocation requires `privileges.manage`, not an additional
`principals.manage` privilege. Other hard-root revocation retains its own
`principals.manage` gate.

The remaining runtime wiring loads the fresh store, accepts built-ins and
provisions Runtime resources/identity, issues Runtime authority through this
compiler, opens TLS-protected operational NATS clients, starts Callout,
verification distribution and enforcement, then exposes normal admission/OAuth.
There is no anonymous application-account bootstrap or permanent browser admin
token.

## Source Boundaries

Core implementation: `crates/runtime/src/platform/auth/`. Pure signed-authority
and verification-material cryptography: `crates/protocol/src/authorization.rs`
and `crates/protocol/src/catalog.rs`. Native Auth contract authoring:
`crates/runtime/trellis/src/apis/trellis_auth_v1/`. Generated clients and
current source, not the retired HTTP/RPC documentation, define exact APIs during
cutover.
