# Design: Auth Operations

Status: Revision 7 operational requirements.

## Configuration

Runtime Auth configuration (`[auth]`) includes:

- `instance_id`, the installation identity retained across restarts;
- `oauth_storage_key_file`, the OAuth storage custody key;
- `[auth.authorization]`: `issuer_signing_seed_file`,
  `context_lifetime_seconds`, `allowed_clock_skew_seconds`, and
  `oauth_admission_lifetime_seconds`;
- `[auth.oauth]`: the dedicated listener's `origin`, `bind_address`, `port`,
  `transport`, and `trusted_proxies`; and
- `[auth.local_identity]`: `enabled` and `password_min_length`.

Login, OAuth token, device-code, portal, and request-admission lifetimes and
budgets are fixed in the runtime, not configured. User login expiry bounds
authority issued for that login. Native services and devices are bounded by
credential, deployment, resource, and issuer evidence rather than a login TTL.

There is no offline trust-root, certificate, manifest, or rollback-floor
configuration.

## Deployment Checklist

- create secure runtime/config/data directories;
- generate and protect the online issuer seed;
- configure Auth public origin and exact transport endpoints;
- initialize a fresh Auth database through the current migration chain;
- confirm built-in Auth/CLI/Console/Portal participant installation;
- complete first-admin setup;
- provision service/device identities and retain one-use secrets only until
  consumed;
- verify the revocation bucket allows per-entry TTLs at least as long as the
  latest issued authority;
- verify authority/revocation publication completes before readiness; and
- monitor post-commit backlog, issuer expiry, rejected proofs, and failed kicks.

## Revocation

Grant, principal, credential/login, resource, participant, or issuer changes
commit explicit affected-context revocations before requesting exact connection
kicks. Kick failure is operational and retried; it never rolls back durable
authorization state. Short NATS JWT lifetimes bound reauthorization even if a
kick cannot reach an already absent connection.

Ordinary context expiry and issuer rotation preserve historical context rows.
Explicit revocation remains authoritative for historical event validation.

## Rotation

Online issuer rotation installs the new issuer and moves current issuance to it.
Old issuer metadata and signed contexts remain available for historical
verification. Explicit issuer revocation invalidates every context signed by
that issuer. Runtime startup must reject an invalid/unavailable configured
issuer seed rather than generating one silently.

Service/device durable identity rotation is performed through provisioning
lifecycle, not by mutating private keys in Trellis. Session keys are ephemeral
and rotate on every CONNECT admission and login.

## Recovery

Auth SQL is authoritative. On restart the runtime rebuilds all signed-context
and revocation mirrors, resumes durable post-commit actions, and retains exact
prepared event bytes/proofs across retries. Operators do not repair authority by
editing KV, NATS ACLs, or client caches.

## Accepted Browser Risk

Browser session seed persistence permits same-origin script to act as the user;
XSS prevention remains required. IndexedDB generation fencing prevents stale
tabs from overwriting or clearing newer login credentials but is not an XSS
sandbox.
