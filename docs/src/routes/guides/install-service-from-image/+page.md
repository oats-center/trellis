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
trellis login http://localhost:3000
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
trellis apis accept \
  --source ./release/trellis-project \
  --participant acme.MyService \
  --format json
trellis svc my-service create \
  --source ./release/trellis-project \
  --participant acme.MyService \
  --format json
trellis svc "$DEPLOYMENT_ID" show --format json
```

`apis accept` adds the APIs the service implements to the installation's API
catalog; a deployment cannot be created until they are accepted. Set
`DEPLOYMENT_ID` to the returned `deployment.deploymentId`. Create compiles
native IDL in memory, installs an immutable participant revision, sets up
deployment resources, and writes the participant-scoped binding; later releases
use `apply` or let the service submit its own build (below).

## 3. Provision an instance

```sh
trellis svc "$DEPLOYMENT_ID" provision --format json
```

Provisioning returns private identity material once. Put the seed in the secret
manager used by the container platform. Trellis stores only the immutable public
identity.

Provisioning is idempotent by identity: the Ed25519 identity key is the stable
identity. A bare `provision` returns the deployment's current instance when one
exists, reactivating a revivable instance in place, and only mints a new identity
when none exists. The original private seed cannot be recovered, so store it at
first provision. Pass `--new` to add another replica deliberately, or
`--instance-seed <seed>` to select a known identity. A bare provision with more
than one current instance fails as ambiguous instead of guessing.

## 4. Run the container

Pass only the runtime URL and the provisioned identity material required by the
service's configuration:

```sh
podman run --rm \
  -e TRELLIS_URL="http://host.containers.internal:3000" \
  -e MY_SERVICE_IDENTITY_SEED="<seed from provision>" \
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
trellis apis accept \
  --source ./new-release/trellis-project \
  --participant acme.MyService
trellis svc "$DEPLOYMENT_ID" apply \
  --source ./new-release/trellis-project \
  --participant acme.MyService
```

`apis accept` leaves an API whose definition is unchanged as it is. Apply validates the replacement, installs the new immutable participant
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
