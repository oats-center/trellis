# Design: Device Activation

Status: Revision 7 device enrollment and independently consented native companion model.

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
- A declared companion is the device's lexical native application request. It
  runs under independently consented OAuth user authority with its own durable
  P-256 key, ephemeral Ed25519 key, refresh family, context, and connection.

## Provisioning

Administrators create a device deployment and provision an instance through the
Auth RPC surface. Provisioning atomically creates the principal, instance, and
device record. `Devices.Provision` with an administrator-supplied public key
registers an already-approved identity. Omitting that key instead returns a
one-use, expiring high-entropy provisioning secret for the server-owned instance.
Only its hash is stored in the secret table; idempotent response replay is
encrypted. Consuming the secret installs the durable public identity key once
and leaves the device pending review. It grants no user authority.

Accepting a deployment with a declared native companion registers the dedicated
client `trellis.device.<deploymentId>.companion`. Its allowed request is derived
from that accepted child descriptor. Registration neither grants capabilities to
a user nor creates an OAuth grant or login.

The provisioned device keeps its root/identity secret. Ready-state configuration
does not retain deployment IDs, instance IDs, participant artifacts, authority
digests, session credentials, or authorization contexts.

## Online Enrollment

`POST /auth/device/enroll` accepts the device identity key, participant ID,
ephemeral session public key, request ID, issue time, challenge digest,
confirmation code, and optional one-use provisioning secret. Both `proof` and
`sessionProof` use the `deviceEnrollment` purpose: the durable identity signs the
complete unsigned body, and the ephemeral key signs the same body with its own
key ID. Auth verifies both possession proofs, origin and bounded issue time
before consuming a secret or touching review state.

The server resolves assignment and installed participant state. It either reuses
the review for the same attempt or creates one aggregate review record.
Responses are:

- `pending`, with review ID, activation URL and bounded polling metadata;
- `rejected` or `expired`; or
- `approved`, which SDK activation helpers expose as ready for native admission.

Retries retain the durable identity and device-local challenge, and reuse the
same bounded review. A resumed helper may rotate its ephemeral key but must prove
possession of both keys again. The device displays its locally derived
confirmation code. Enrollment carries no companion login, installation seed or
authority claim.

## User Approval

An authenticated administrator uses `Devices.Reviews.Get` and
`Devices.Reviews.Decide`, presenting the device's confirmation code and expected
review revision. Auth rechecks expiration, server-owned assignment, current
accepted participant revision and device state inside its SQLite transaction.
Approval enables the parent only; it cannot issue companion credentials.

The companion separately uses ordinary OAuth Device Code with the dedicated
deployment client and explicit user consent to the accepted child request. Its
authenticated user connection calls `DeviceCompanions.Attach` with the approved
review ID, confirmation code, expected revision and idempotency key. Auth derives
the user and grant exclusively from that verified caller. The same transaction
rechecks the accepted child request, exact native client, live grant/login/family,
eligible required capabilities and active assignment, then records the
association. Caller-supplied principal IDs, grant IDs or parent credentials
cannot substitute for this consent.

New OAuth initiation and consent approval both recheck the registered request
against the deployment's current accepted declaration. Omitting the companion or
removing its deployment rejects new and pending approvals; accepting the declaration
again permits a new explicit consent flow. This does not disable the OAuth client
or revoke already-consented independent grants: their ordinary refresh remains
valid after parent disable, declaration omission, or deployment removal.

`DeviceCompanions.Revoke` is authorized to the associated user or an administrator
whose verified session delegates `trellis.managePrincipals` and who still holds
it. It atomically
removes the association and revokes that exact OAuth grant and its families.
It does not revoke the user's login or other grants, or disable the parent.

## Device Bootstrap

After approval the ordinary provisioned-device NATS admission proves possession
of the registered durable key. Auth resolves assignment, accepted descriptor,
native resources and transport permissions server-side; admission never creates
a review or user login. The signed native authorization may advertise the
companion's request ID, client ID, OAuth grant ID and required/optional flag, but
never its tokens or private keys.

The wrapper receives independently obtained native OAuth custody and checks it
against this signed assignment before opening the separate user connection.
A required child must be assigned and available; an optional child may be absent.
Child revocation does not revoke parent authority, although a required wrapper
cannot be newly constructed without a valid child. Parent disablement does not
revoke the independently consented child grant.

## Identity And Session Keys

The durable identity key authorizes provisioned-device admission. Enrollment
additionally proves possession of its ephemeral key. Companion OAuth uses its
own durable P-256 DPoP key and fresh ephemeral Ed25519 session key. None of these
keys is interchangeable with another principal's key.

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

A completed local activation retains its original challenge. Rechecking uses
proof-bound enrollment against that challenge instead of creating another
review or resubmitting the consumed provisioning secret. The local completion
is never authorization: disabled or removed devices still fail enrollment and
connection at Auth.

Session authority, admission credentials, transports, grants, and issuer keys
remain process-memory-only. Reconnect uses proof-bound refresh once per disconnected
episode.

## Non-Goals

- deployment/instance IDs in bootstrap configuration;
- client-supplied participant artifacts or authority state;
- persistent device login sessions;
- parent-derived user approval, login, OAuth custody or browser identity;
- a server QR endpoint; and
- compatibility with retired device-connect or identity-authority payloads.
