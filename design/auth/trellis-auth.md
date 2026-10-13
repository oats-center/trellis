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

The bundled Console's OAuth registration is runtime-owned. Initialization
registers it against the configured Console application origin using the ordinary
registered-client store, then exposes its public client ID and validated callback
URI through Console configuration. Operators do not manually register the bundled
Console. Its client ID is neither a deployment ID nor a secret. Production
administrator seeding initializes the administrator and registers `trellis.console`
and `trellis.cli` in one idempotent aggregate transaction. CLI is an explicitly
registered native Device Code/refresh client without a callback listener or
implicit loopback trust. Both registrations have finite eligibility for the six
administrative privileges. Registration uses the newly created
administrator's ordinary `clients.manage` and `privileges.manage` authority.
Replay returns the saved result without re-registering or re-enabling a modified
client; ordinary startup does not change client registration. Console login's
SDK migration remains pending. Registration does not itself grant the user
capabilities or platform privileges: finite delegation still requires the user's
holdings and explicit consent.

Both local/container and native fresh-bootstrap generators provision the four
built-in provider identity files under the existing `live-providers/` custody
paths. They create private files without replacing existing material. Normal
startup only reads those seeds and fails on missing, unsafe, or corrupt custody;
it does not generate replacement identities or persist private seeds in Auth SQL.

Lease-owning startup provisions all four generated built-in provider descriptors
through ordinary API acceptance and deployment approval, using independently
keyed initialization mutations per installation and participant. Replay retains
the actual ULID deployment and instance; disabled, expired, retired, or mismatched
identities are rejected rather than re-enabled. Installed native KV declarations
are materialized and committed before native admission. Providers without
declarations get a projection derived from that empty declaration set. The
current startup materializer rejects companions and non-KV resources; it does not
invent consumer, job, or object-store bindings. Native connection uses configured
NATS endpoints, pinned store trust, and the retained identity seed, then checks
installed signed identity against the provisioned record—not reserved deployment
names or caller-supplied admission assertions.
Modes without Platform ownership only read an already provisioned identity from
the configured shared SQLite database. Lookup uses the same native descriptor,
active binding, revision, expiry, and identity checks in a read-only snapshot;
it neither requires administrator authority to read nor provisions, repairs, or
records a mutation. An unknown credential is `NotFound`, not an initialization
fallback.

Explicitly initialized or administrator-registered development clients may be
eligible for platform privileges, including a localhost Console or a locally
developed Console using a remote installation. Registration still requires
`clients.manage`; configuring privilege eligibility requires `privileges.manage`.
The authenticated user must possess each requested privilege and explicitly
consent to delegating it to that registered client. Neither development mode nor
a localhost redirect establishes trust. CIMD and unregistered clients remain
ineligible for platform privileges, and OAuth/application origin isolation is
unchanged.

Dex is exclusively a real upstream OIDC test dependency, not a deployment
component. Trellis validates upstream nonce, signature, issuer, and audience
before translating verified claims through its own role mapping and consent.

## CIMD Retention

Provisional CIMD metadata and its encrypted OAuth engine client artifact are
admitted atomically, with a shared limit of 128 unapproved clients and 16 MiB.
Both receive a five-minute lifetime. Expired, unreferenced pairs are evicted
before admission; exhaustion returns `temporarily_unavailable`. A live pending
authorization retains its exact reviewed metadata until that transaction expires
and rejects replacement with a different document during that interval. Approved
grants retain their client protocol identity and redirect information, not the
full fetched metadata, until their remaining lifecycle ends. Cleanup preserves
refresh-reuse evidence for a still-live family and never deletes authority history
or administrative client registration.

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

Code and device consent presentations are retained in their encrypted,
expiring authorization artifacts. Approval submits the presentation identity
and explicitly selected optional capabilities. The grant transaction consumes
that exact presentation and rechecks the displayed capability identity
generation, consent revision, API generation, and current availability. Changed
capability meaning, a superseded form, or lost required availability requires
a fresh presentation; unrelated entitlement changes do not invalidate the form.

Canonical signed session authority is limited to 64 KiB by both issuer and
verifier. Raw parsing applies the same allocation bound even when a caller's
local policy allows more. Issuance fails before authority or attachment state
is committed when the shared budget is exceeded.

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

## API Entry Routing and Session Ownership

Ordinary RPC requests, Operation starts, and Live openings use generated
canonical API/action subjects. Auth does not persist caller-to-provider
deployment selections or return such selections in bootstrap metadata.
Providers serving one logical API share the same request queue group on each
supported action. Normal broker operation distributes each request to one
eligible subscriber; this is not an exactly-once execution guarantee, and the
SDK never automatically retries potentially executed work.

Provider authorization is independent of caller capabilities. Signed provider
certificates retain principal, deployment, instance, API generation, and
implemented-action identity. Providers subscribe only to certified actions;
clients verify the actual response against the pending request and certified
provider before exposing either success or a declared error.

Once an opening selects a peer, Live and Transfer controls and frames retain
their authenticated session owner and session-specific subjects. Existing
durable Operation controls and observations retain deployment ownership and
the supported durable coordination mechanism. Canonical entry routing neither
merges service-owned storage nor load-balances established-session traffic.

Providers sharing an API route must be suitable alternatives for that logical
API. Matching IDL alone does not make independent databases or different
physical devices interchangeable. Application code selects an API, while the
SDK retains authenticated peer identity for established work.

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

Provisioned service/device CONNECT proofs identify the installed credential and
bind its identity-key signature to the fresh runtime/session NKey, target
installation/account, and validity window. Clients do not assert deployment,
instance, participant, or implementation authority. Auth resolves those from its
current provisioning records; an ambiguous credential binding is rejected rather
than selected implicitly. Provisioning supplies connection endpoints and pinned
installation/account/issuer trust, not another copy of deployment policy. The
Auth-signed session authority exposes the verified binding to the SDK, which still
checks its generated provider implementation against the accepted evidence.

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
snapshot/update stream. Live authority is unavailable until its initial snapshot
has been verified and applied, and becomes unavailable if coverage is lost.
Replacement streams use an epoch fence: completion of a retired watcher cannot
restore or clear its successor's coverage. Observed revocations remain sticky;
recovery never extends the signed authority's expiry.

One cancellable supervisor retries unexpected stream loss or a temporarily
missing published carrier with bounded backoff and jitter. Private maintenance
can acquire an admitted carrier while application use remains fenced; only a
complete authenticated snapshot on the current generation restores usability.
A retired session remains denied, but fresh ordinary admission may replace it
under a still-valid credential. Explicit close or terminal credential failure
stops recovery. Explicit native authority refresh and scheduled renewal share
one custody worker and await authenticated publication and mirror coverage.

Kicks use the operational system-account client and exact broker inventory. A
reused CID with another attachment marker is never kicked. Unavailable or
malformed inventory is retryable, not proof of absence. `CONNZ`'s filtered
`num_connections`, not its global `total`, establishes CID absence. Effects have
bounded claims, retry backoff, stale-claim recovery, and predecessor ordering.

## Issuers And Bootstrap

Cold OAuth resume reads the bounded public issuer chain from
`GET /.well-known/trellis/issuer-chain?pinnedKeyId=...&targetKeyId=...` on the
selected OAuth origin. Discovery identifies a target but never replaces a
persisted pin: SDKs authenticate each signed successor's sequence, scope and
key continuity using the existing rotation verifier, then persist successor
trust before consuming a refresh token. Missing, compromised or invalid chains
leave the existing credential unspent. If an issuer changes during refresh,
custody retains the known newly returned, unsubmitted token without promoting
unverified admission authority; later recovery spends that successor, not the
already spent predecessor. Ambiguous responses retain the spent marker.

Cold successor pins resolve older immutable verification material through the
existing issuer-signed metadata exchange's `issuerKey` selector. Only an active
issuer authenticated from the pin and forward rotations may sign this reply;
its requested key, canonical bytes, installation/account, nonce, destinations,
and freshness are verified before caching. Returned keys are historical-only:
retired keys can authenticate unchanged catalogs and historical publications,
while compromised keys remain unusable. The read cannot install an active key,
advance the rotation sequence, or replace credential trust. Learned compromise
is sticky. Current session and provider-certificate checks remain independent
of immutable catalog-signature authenticity.

The initial administrator is created through the local production bootstrap
boundary and remains protected against disablement or administrative demotion.
Fresh initialization accepts any missing Trellis-owned built-in APIs from the
production Platform, Health, Jobs, and Events participant descriptors and grants
the administrator each currently accepted active capability of those APIs
(Auth, Core, State, Health, Jobs, and Events). These are explicit whole-capability
policy grants committed atomically with the finite platform privileges and
Console/CLI registration, not wildcard authority. Initialization replay neither
grants capabilities added later nor overwrites registered clients. Domain APIs
are excluded. Application connections still require their actual selected and
approved capabilities, and administrative privileges remain constrained by
client eligibility and consent.
Passwords use Argon2; password change/reset retires the affected login families
in the same transaction. Authentication failures do not identify which
credential check failed.

Account reset and identity linking use five-minute, one-use account actions in
the existing isolated OAuth portal. Action creation replay preserves the original
response under OAuth sealing custody, never a renewed deadline. Reset completion
checks the originating logical session, current accepted `principals.manage`,
credential CAS, and protected-administrator status before atomically changing the
password and revoking login/grant families. Bootstrap-administrator reset remains
on the existing local recovery path. Linking requires the exact initiating portal
user and a fresh nonce/PKCE-verified upstream exchange; provider disablement or a
revision change invalidates the action, and another user's identity cannot be
reassigned. Native forms enforce Origin and CSRF without accepting actor IDs from
the browser.

Identity-link approval never redirects its form POST to an external provider.
It rotates the existing action token into a 60-second, one-use, exact-session
handoff and returns a no-store continuation page with an ordinary navigation
link. The same-origin GET authenticates the reviewed handoff before redirecting
to the verified provider. The continuation and redirect suppress referrers;
`form-action 'self'` remains enforced. Wrong-session, cross-user, expired,
replayed or unreviewed handoffs cannot begin upstream linking.

Issuer rotation installs a signed, preceding-key-bound, monotonic chain record
and retains retired public keys and immutable signed contexts. Rotation commits
the successor's public identity; its private key remains operator-owned and must
be installed as the configured runtime signer before further issuance. A
previous signer cannot continue issuing after that commitment. Rotation itself
does not retire sessions. Explicit key compromise revokes affected sessions and
makes that key unusable; restart must not reinstall it as an active signer.

Refresh-successor checkpoints apply only to an actual in-progress refresh,
never to a separate Device Code authorization. Rejected cross-principal
reauthorization preserves the previous credential. Browser refresh reloads
durable cross-tab trust under the custody lock and discovers the current signer
before spending its token, even when the OAuth client object remains open.
Native renewal leases the current maintenance attachment rather than retaining
the initial socket; that finite lease carries signer resolution through the
existing private preparation and checked authority-promotion boundary.
Cold provider-certificate material reads authenticate the immutable signature,
installation/account binding, identity, and validity bounds without applying
today's clock. Live use still requires a current issuer and unexpired authority
and certificate. Retained events require both signed event time and genuine
original JetStream publication time within those validity windows and before any
signed revocation cutoff; ordinary certificate expiry and issuer retirement do
not invalidate that historical evidence. Compromised issuers remain unusable.
Issuer rotation/revocation requires `privileges.manage`, not an additional
`principals.manage` privilege. Other hard-root revocation retains its own
`principals.manage` gate.
The current `Issuers.Rotate` RPC request contains only the expected current key
and replay key, not successor custody or an installation mechanism. Until an
operator-owned custody source is supplied, a valid request returns the declared
`issuer_custody_required` domain failure; it never creates replacement root
custody.

Already admitted Trellis connections may publish bounded public verification
lookups and request their own context for one exact physical attachment. The
signed AdmissionGet requires canonical `attachmentId` from that socket's broker
marker; its signed response registry must echo that exact ID. Overlapping sockets
of the same runtime/key resolve independently, never by selecting the newest
attachment; foreign, disconnected, and missing attachments deny.

Already admitted Trellis connections may publish bounded public verification
lookups to `trellis.auth.metadata.get`, regardless of administrative capability.
This dedicated read-only bootstrap uses the existing IssuerMetadataRequest and
SignedIssuerMetadata protocol for AuthorityResolve, AuthorityStatus, and
IssuerChain. Administrative Authority.Status remains a separate protected typed
RPC, never an alternate decoder or alias. The responder shares OwnContext's
lifecycle and bounded concurrency; the ordinary Callout ACL grants only the
exact metadata subject. There is no anonymous listener or account admission.
Replies are bounded and restricted to ordinary `_INBOX.*` destinations, with
exact instance/account/query/nonce/subject/reply binding and a 30-second signed
admission window. SDKs authenticate existing pinned-key rotation proofs before
trusting a new reply signer; broker delivery alone supplies no trust. Missing or
Unavailable never clears known revocation or establishes authority. Lookups
return only the selected immutable verification material, logical status/cutoff,
or pinned-to-target signed issuer chain, never credentials or policy enumeration.

OAuth CONNECT retains the original verified refresh family on its logical
authorization session: immutable `original_refresh_family_id` and
`refresh_family_lookup_hash`, with the hash/grant pair referencing the actual
OAuth family's record. Authority renewal, OwnContextGet, Admission.Renew, and
CONNECT reread that exact family's revocation state and absolute deadline; a
different live family of the same grant cannot replace it. The family deadline
limits the shared session root, separately from per-capability deadlines.
Refresh-token rotation preserves the original family identity and deadline.

`Admission.Renew` accepts the live session's ordinary verified ephemeral-key
request and a context digest owned by that exact principal/runtime/session/key.
In one transaction it rechecks current policy, validates and retains a bounded
candidate authority, and uses the token endpoint's signing owner to return a
short OAuth admission for the same key, runtime, grant, login, client, and
original family. Its lifetime is at most 300 seconds and cannot outlive the
candidate authority or family. It neither rotates a refresh token nor provides
a durable reconnect credential. Invalid roots retire the session and enqueue
the ordinary cutoff and kicks; renewal never revives its retired logical ID.
Provisioned credentials still require the client's identity-key signature:
`Admission.Renew` returns `provisioned_signature_required` for that prerequisite,
and Auth never signs on the client's behalf.

The remaining runtime wiring loads the fresh store, accepts built-ins and
provisions Runtime resources/identity, issues Runtime authority through this
compiler, opens TLS-protected operational NATS clients, starts Callout,
verification distribution and enforcement, then exposes normal admission/OAuth.
There is no anonymous application-account bootstrap or permanent browser admin
token.

The A3 platform bootstrap helper uses ordinary catalog acceptance and deployment
approval under the initialized administrator's existing management privileges;
it never re-enables or reassigns a disabled provisioned identity. Startup KV
materialization reads the installed native declaration set, performs JetStream
allocation outside the SQLite lock, then commits catalog resource/history,
binding evidence, and the actual `ServiceResourceBindings` projection through
the existing idempotent aggregate mutation. Resource identity includes the
deployment, participant, kind, and local name. Existing allocations must prove
that ownership and retain compatible history/TTL; startup does not adopt,
destroy, or recreate foreign or incompatible storage. Desired capacities are not
converted into hard broker limits. The helper connects only after committing
actual bindings, using configured native endpoints, the store's issuer pin, and
the provisioned identity seed through ordinary Callout and signed own-context.

The host must start Callout, ContextGet, and registry publication before invoking
that helper, and own its returned service runtime. Its generated
`Authority.Status` handler projects the persisted logical lifecycle and retained
signed cutoff. The SDK's separate historical issuer-metadata exchange is not
this typed management RPC. Host integration and runtime-owned operation
repository/staging resource provisioning remain unfinished.

## Source Boundaries

Core implementation: `crates/runtime/src/platform/auth/`. Pure signed-authority
and verification-material cryptography: `crates/protocol/src/authorization.rs`
and `crates/protocol/src/catalog.rs`. Native Auth contract authoring:
`crates/runtime/trellis/src/apis/trellis_auth_v1/`. Generated clients and
current source, not the retired HTTP/RPC documentation, define exact APIs during
cutover.
