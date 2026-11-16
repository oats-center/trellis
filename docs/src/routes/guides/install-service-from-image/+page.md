---
title: "Install a service from an image"
description: "Deploy a published service from an OCI image into a running Trellis environment."
---

This guide covers running a service from a published OCI image. The release must
also distribute the Trellis project metadata used to install the exact
participant: `trellis.toml`, `trellis.lock`, and native `.trellis` source. The
runtime never accepts artifacts uploaded by the service process itself.

## 1. Log in

```sh
trellis login https://localhost:3000
trellis whoami
```

The detached login prints a Portal URL/QR code. Bind creates the CLI login and
the CLI refreshes authorization for its installed `trellis-app.cli@v1`
participant.

## 2. Create and apply

Create the deployment from the release's participant project. `my-service` is
the deployment's display name; `--participant` names the service's identity,
`<package>.<ServiceName>` from its `trellis.toml` and IDL:

```sh
trellis svc my-service create \
  --source ./release/trellis-project \
  --participant acme.MyService \
  --format json
trellis svc "$DEPLOYMENT_ID" show --format json
```

Creating the deployment also accepts the APIs the service provides into the
installation's API catalog: approving the service is approving what it
publishes. `trellis apis review` with the same flags shows them first. Set
`DEPLOYMENT_ID` to the returned `deployment.deploymentId`. Create compiles
native IDL in memory, installs an immutable participant revision, sets up
deployment resources, and writes the participant-scoped binding; later releases
use `apply` or let the service submit its own build (below).

### From the Console

**Add service** in the Console takes the same project. Give the service a name,
then under **Service IDL** choose the project's `trellis.toml` and `.trellis`
files, or its folder. The Console doesn't need `trellis.lock`: Trellis finds
each dependency itself among the `trellis` package it ships and the packages of
services already installed, picking the highest version that matches, and the
form says which it used. A dependency Trellis doesn't have is listed with the
version it needs; choose that package's `trellis.toml` and `.trellis` files
beside it. Trellis never downloads a package. When the project declares several
services, pick which one. Then either let the Console create the instance's key
(it shows the seed once) or paste a public key the service's operator made, and
skip the provisioning step below.

### Without the release's project

When you don't have the service's project, for instance a third party's service
that should talk to your Trellis, create the service without `--source`:

```sh
trellis svc partner-orders create --public-key <key>
```

The service's owner makes the key with `trellis keys new`, keeps the seed, and
sends you only the public key, so the private seed never leaves them. Without
`--public-key`, `create` generates an identity and prints its `instanceSeed`
once for you to hand over. In the Console, use **Add service** and choose no
IDL: either let the Console create the key (it shows the seed once, and only
its public key reaches Trellis) or choose "I already have a key" and paste the
owner's public key. The first service to connect with the claim's identity
submits its own IDL and waits; `trellis svc <claim-id> updates` shows what it
asks for, and
`trellis svc <claim-id> approve` adds it, accepting the APIs it provides. The
service then connects as an instance of the new deployment, so skip the
provisioning step below.

## 3. Provision an instance

```sh
trellis svc "$DEPLOYMENT_ID" provision --public-key <key> --format json
```

Whoever runs the container makes the identity with `trellis keys new`, puts the
seed in the container platform's secret manager, and gives you the public key.
Trellis stores only the public key, and nobody else handles the seed.

Without `--public-key`, `provision` generates an identity and returns its seed
once. The seed cannot be recovered, so store it then.

Provisioning is idempotent by identity: the Ed25519 identity key is the stable
identity. `--public-key` selects the instance that already has that key, or
registers a new instance for it. A bare `provision` returns the deployment's
current instance when one exists, reactivating a revivable instance in place,
and only mints a new identity when none exists. Pass `--new` to add another
replica deliberately. A bare provision with more than one current instance
fails as ambiguous instead of guessing.

## 4. Run the container

Pass only the runtime URL and the provisioned identity material required by the
service's configuration:

```sh
podman run --rm \
  -e TRELLIS_URL="http://host.containers.internal:3000" \
  -e MY_SERVICE_IDENTITY_SEED="<seed from trellis keys new>" \
  ghcr.io/my-org/my-service:1.2.3
```

NATS CONNECT proves the identity and a fresh session key. Auth resolves the
deployment and instance, exact installed participant and APIs, resource
evidence, and inbox, and returns signed session authority with exact transport
permissions. Do not mount NATS administrator credentials, semantic digests,
deployment IDs, instance IDs, resource bindings, or authority files.

## 5. Upgrade

Roll out the new image. When it connects, the service sees that its build
differs from the one its instance runs and submits its own IDL:

- If the build asks for nothing new (no new capability, resource, or API), Auth
  installs it and the service connects under it at once.
- If it asks for new access, the service waits. It logs a review link and prints
  it as a QR code; open it, or run `trellis svc "$DEPLOYMENT_ID" updates` and
  `trellis svc "$DEPLOYMENT_ID" approve`. The service connects as soon as the
  update is approved.

A build that drops a resource only gives up access to it; the data stays until
you remove it under Resources. A later build that declares it again asks for it
back, and approving that brings the kept data with it. A build that implements an
API another service already serves can only be approved with an explicit
acknowledgement, because requests would then be split at random between the two
services. To add capacity or move a service, provision more instances of the same
deployment instead. A build whose implemented API changed incompatibly is
refused until you force-replace that API.

To install a release before rolling it out instead, apply its project:

```sh
trellis svc "$DEPLOYMENT_ID" apply \
  --source ./new-release/trellis-project \
  --participant acme.MyService
```

Apply accepts compatible changes to the APIs the service provides, validates the replacement, installs the new immutable participant
revision, updates resources, and advances the binding directly. Use a new
participant `@vN` lineage for breaking API changes. Existing provisioned
identity keys remain valid for the same instance unless intentionally rotated.

Console **Admin / Services** shows the current deployment, installed revision,
resources, instances, and grant state. It does not expose retired authority
plans.

## Practical loop

1. Log in as an administrator.
2. Create the deployment.
3. Apply the release's Trellis project and inspect the result.
4. Provision an instance and store its seed securely.
5. Start the container with only endpoint and identity configuration.
6. Confirm expected RPCs, events, jobs, and resource handles.

All required resource evidence must exist before context issuance. Unavailable
optional evidence removes only its affected optional grants. The service never
provisions resources as a bootstrap side effect.
