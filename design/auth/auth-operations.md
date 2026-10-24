# Design: Auth Operations

Status: Revision 7 operational requirements.

## Configuration

Every `[auth]` key, from `AuthConfig` in `crates/runtime/src/config/mod.rs`:

| Key                                                 | Required    | Meaning                                                                                                                                         |
| --------------------------------------------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `instance_id`                                       | yes         | The installation's ULID, generated at initialization and retained across restarts and key rotations.                                            |
| `oauth_storage_key_file`                            | yes         | Owner-only key sealing durable OAuth material. Back it up with the database.                                                                    |
| `authorization.issuer_signing_seed_file`            | yes         | The bootstrap issuer's Ed25519 seed. Successor keys live in `<database>.issuer-keys/`.                                                          |
| `authorization.context_lifetime_seconds`            | yes         | Longest lifetime of a newly issued session authority, 1–3600.                                                                                   |
| `authorization.allowed_clock_skew_seconds`          | yes         | Verification and Callout clock skew, at most 300 and less than the context lifetime.                                                            |
| `authorization.oauth_admission_lifetime_seconds`    | no (300)    | Longest lifetime of a signed OAuth admission, 1–300.                                                                                            |
| `oauth.origin`, `bind_address`, `port`, `transport` | yes         | The dedicated OAuth listener; `transport` is `tls`, `proxy`, or `loopback_development`.                                                         |
| `oauth.trusted_proxies`                             | no          | CIDR ranges whose `X-Forwarded-For` identifies the client in front of a TLS listener.                                                           |
| `local_identity.enabled`, `password_min_length`     | no (on, 12) | Local username/password sign-in, and the minimum length every local password must meet. Off rejects password sign-in and hides the portal form. |
| `avatar.external_fetch_enabled`                     | no (on)     | Fetch OIDC, override, and Gravatar profile images; off serves local initials only.                                                              |
| `administration.confirm_on_consent`                 | no (on)     | Consent asks for a separate confirmation before delegating administration.                                                                      |
| `administration.require_peer_approval`              | no (on)     | Another `trellis.auth.v1:platform.manage` holder approves a delegated administrative capability.                                                |
| `administration.registered_clients_only`            | no (on)     | Only administrator-registered OAuth clients may receive administrative capabilities.                                                            |

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

Auth SQL is authoritative. On restart the runtime resumes the durable
post-commit actions (publications, revocations, kicks, events) where they left
off, recovering expired claims, and retains exact prepared event bytes and
proofs across retries; it does not rebuild the mirrors from scratch. Operators do not repair authority by
editing KV, NATS ACLs, or client caches.

## Accepted Browser Risk

Browser session seed persistence permits same-origin script to act as the user;
XSS prevention remains required. IndexedDB generation fencing prevents stale
tabs from overwriting or clearing newer login credentials but is not an XSS
sandbox.
