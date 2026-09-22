# Design: Device Activation

Status: authoritative device enrollment and bootstrap model after WO-02.

## Principles

- The durable device identity key is the principal. Trellis never stores its
  private material.
- Deployment and instance assignment are server-owned state, not client
  bootstrap assertions.
- Enrollment creates or resumes review; bootstrap admits only an approved
  device.
- Every attempt proves possession of both the durable identity key and the
  ephemeral session key used for the eventual NATS connection.
- Device runtimes have no durable user-login row.
- A declared companion is the device's lexical `app` or `agent` child, but it
  runs under separate user authority with its own login session, context,
  resources, and connection.

## Provisioning

Administrators create a device deployment and provision an instance through the
Auth RPC surface. Provisioning atomically creates the principal, instance, and
device record and may return a one-use high-entropy secret. Only its hash is
stored. Consuming that secret installs the caller-supplied durable public
identity key once; retries return the committed idempotent result.

The provisioned device keeps its root/identity secret. Ready-state configuration
does not retain deployment IDs, instance IDs, participant artifacts, authority
digests, session credentials, or authorization contexts.

## Online Enrollment

`POST /auth/device/enroll` accepts the device identity key, participant ID,
ephemeral session public key, request ID, issue time, `device-enroll` proof, and
optional one-use provisioning secret. The request is strict and proof-bound to
the complete raw body.

The server resolves assignment and installed participant state. It either reuses
the review for the same attempt or creates one aggregate review record.
Responses are:

- `pending`, with activation URL, confirmation code, and polling metadata;
- `rejected`, with the terminal rejection; or
- `ready`, indicating that review approval is complete.

The TypeScript and Rust activation helpers retain one durable identity and one
ephemeral session key/logical connection ID across retries. They do not create
new reviews while polling. When the device declares a companion, enrollment also
carries a proof-bound claim for that exact lexical child and its separate
installation key. The child cannot start an independent browser or native
bootstrap; it is admitted only through this parent activation.

## User Approval

The activation Portal resolves review state from Auth and presents the exact
device, deployment, requested participant, and confirmation code. The signed-in
user approves or rejects through `Auth.DeviceUserAuthorities.Resolve` and the
device review RPCs. For a device companion, operation progress includes the
server-computed `companionConsent` for the child participant. The approving user
must submit a separate child `Approval` that repeats its `decisionDigest`,
`installedRevision`, and `expectedGrantRevision` and selects only eligible
capabilities and resources from that consent. Required eligible entries must be
included. Auth rejects the approval if the consent, installed revision, grant
revision, eligibility, consent digest, or resource commitment is stale or does
not match exactly.

Auth commits the child approval and device authority before creating the child
login session. The device does not lend its principal or grants to the child;
the resulting user-owned child binding, login session, authorization context,
resource bindings, and connection remain distinct from the parent's. The
operation retains pending/approved/rejected progress and events but no
identity-authority payload.

Portal authentication uses the standard browser flow, browser-binding secret,
and OIDC/local-login controls. The Portal participant is installed as a
Trellis-owned built-in but receives no bootstrap `Admin` binding.

## Device Bootstrap

After approval, `POST /bootstrap/device` accepts only:

```text
identityKey, participantId, sessionPublicKey, requestId, issuedAt, proof
```

The proof purpose is `native-bootstrap`. The route never creates a review and
rejects pending/rejected/unrecognized devices. It resolves the current
assignment, installed participant revision, `GrantBinding`, resources, issuer,
and transport data server-side.

The response uses the shared native installation shape documented in
`auth-protocol.md`: `assignment`, `runtime`, and `transports`. The runtime
validates exact participant/API evidence, grant/resource bindings, signed
context, route JWT, session key, inbox, and endpoints before NATS CONNECT.

When activation established a companion, device bootstrap may also return that
exact child installation. The device runtime opens it as a separate ordinary
user connection using the committed child login and authority. A required child
must be available; an optional child may be absent without merging or widening
the parent device authority.

## Identity And Session Keys

The durable identity key authorizes enrollment/bootstrap for the provisioned
device. A fresh ephemeral session key signs the bootstrap proof and
authenticates NATS. These roles are distinct. The session public key in the
request, signed context, route token, NATS credentials, and later `session-key`
message header must all agree.

Device enable/disable/remove mutations atomically update principal/device state,
idempotency, events, context revocations, and post-commit kicks. A disabled or
removed device cannot bootstrap or refresh even if old local key material
remains.

## Offline QR Transport

Offline QR enrollment is application transport, not an Auth server route. The
payload may carry the one-use provisioning material and user-readable context,
but the server still validates it only through the normal proof-bound online
enrollment operation. No alternate trust model or device-connect RPC exists.

## Client Boundaries

TypeScript callers use `deriveDeviceIdentity`, `checkDeviceActivation` or
`waitForDeviceActivation`, then `TrellisDevice.connect`. Rust callers use
`derive_device_identity`, `DeviceActivationOptions`,
`check_device_activation`/`wait_for_device_activation`, and the generated
activated client. Generated participant types supply participant/API evidence;
callers supply only Trellis origin and held key material.

Authorization contexts, route JWTs, transports, grants, and issuer keys remain
process-memory-only. Reconnect uses proof-bound refresh once per disconnected
episode.

## Non-Goals

- deployment/instance IDs in bootstrap configuration;
- client-supplied participant artifacts or authority state;
- persistent device login sessions;
- standalone browser or native bootstrap for a device companion;
- a server QR endpoint; and
- compatibility with retired device-connect or identity-authority payloads.
