---
title: "Devices"
description: "Build a Trellis device that displays a QR activation URL and completes the device activation flow."
---

# Build and activate a device

Devices are durable Trellis principals. Their root/identity key survives
restarts; login sessions, admission credentials, session authority, participant
artifacts, grants, and deployment assignments do not belong in device config.

## 1. Declare the participant

Define a native `device` participant with its exact owned/used APIs and optional
resource/state selections, then run `trellis update` and `trellis generate`.
Generated TypeScript or Rust facades carry the compile-time participant/API
evidence.

## 2. Create and apply the deployment

```sh
trellis dev field-device create --format json
trellis dev "$DEPLOYMENT_ID" apply \
  --source . \
  --participant acme.field-device
trellis dev "$DEPLOYMENT_ID" show --format json
```

Direct apply installs the participant revision, updates device deployment
policy/resources, and writes the relevant binding. There is no authority
proposal or reconcile step.

## 3. Provision the device

```sh
trellis dev "$DEPLOYMENT_ID" provision --format json
```

Provisioning creates the device/instance principal and returns one-use material.
Store the root or identity seed on the device. Trellis stores public identity
and a hash of any one-use provisioning secret, never private key material.

## 4. Enroll for review

TypeScript:

```ts
import { checkDeviceActivation } from "@oatscenter/trellis/device";
import { participants } from "field-device-trellis";

const activation = await checkDeviceActivation({
  trellisUrl: "https://auth.example.com",
  participant: participants.fieldDevice.participant,
  rootSecret,
  provisioningSecret,
});
if (activation.status === "not_ready") {
  throw new Error(`Device is not ready: ${activation.reason}`);
}
if (activation.status === "activation_required") {
  showQrCode(activation.activationUrl, activation.confirmationCode);
  await activation.waitForOnlineApproval();
}
```

`/auth/device/enroll` verifies the durable identity and a fresh ephemeral session
key, then creates or resumes one review; polling reuses that attempt. The status
is `activated`, `activation_required`, or `not_ready`. An offline device can
call `activation.acceptConfirmationCode(code)` instead of waiting online.
Enrollment does not install runtime credentials.

Display the returned activation URL and confirmation code. The code is a setup
signal, never an online credential.

## 5. Approve in Portal or Console

Open the activation link on any browser, sign in with a Trellis account or your
company sign-in, and enter the code the device displays. The Portal shows the
device's name, type, and software, and asks you to approve or reject it.
Administrators can do the same from **Devices → Requests** in the Console.
Either way, approving requires the `principals.manage` administrator privilege,
and only a code that matches the device unlocks the decision.

Review policy may permit automatic approval, require an administrator, or
disable activation.

## 6. Bootstrap and connect

After approval, connect with the generated device facade. There is no HTTP
bootstrap request: the runtime presents a signed provisioned admission in NATS
CONNECT that proves the device's durable identity key and a fresh runtime
session key. Auth resolves the server-owned deployment/instance assignment,
accepted participant revision, current authority, resources, and transport
permissions from its own records.

Admission never creates a review. Pending, rejected, disabled, removed, or
unknown devices fail closed.

The ephemeral session key returned by the successful activation attempt is used
for the connection. Application requests/events include the context digest,
matching redundant `session-key`, and message proof. Reconnect refreshes current
authorization once per disconnected episode.

## Offline QR transport

An application may transfer one-use enrollment material by QR code. Auth has no
separate QR endpoint or trust path: the device still submits the ordinary
proof-bound `/auth/device/enroll` request when online.

## Lifecycle

Enable, disable, and remove operations atomically update the principal/device,
events, authority revocations, and connection-kick intents. An old local session
authority cannot bypass current lifecycle state because renewal and Auth Callout
revalidate it.
