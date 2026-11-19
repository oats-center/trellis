---
title: "Install a service from an image"
description: "Deploy a published service from an OCI image into a running Trellis environment."
---

This guide covers running a service from a published OCI image. Install the
service's Trellis package first, so its instances connect with nothing left to
approve. The service's release pipeline publishes that package with `trellis
publish`: its `trellis.toml`, its `.trellis` sources, and the exact dependency
releases it was built with. You can also install from the project directory
itself, or let the service introduce itself when it first connects.

## 1. Log in

```sh
trellis login https://localhost:3000
trellis whoami
```

The detached login prints a Portal URL/QR code. Bind creates the CLI login and
the CLI refreshes authorization for its installed `trellis-app.cli@v1`
participant.

## 2. Add the registry

Trellis pulls published packages from registries an administrator names. Add
the one the service is published to once:

```sh
trellis registries set acme --prefix ghcr.io/acme
```

A package's repository is the prefix followed by its name, so `acme-orders`
comes from `ghcr.io/acme/acme-orders`. A public registry needs nothing more.
For a private one, add `--username` and give the password or token with
`--secret-file PATH` (or `-` for standard input; a terminal asks). Trellis seals
the secret with its storage key and never shows it again; `trellis registries
list` shows only the user. In the Console the same list is under **Services**,
**Package registries**.

## 3. Create the deployment

Create the deployment from the published release. `my-service` is the
deployment's display name; `--participant` names the service's identity,
`<package>.<ServiceName>` from its IDL, and is asked for at a terminal when the
package declares several:

```sh
trellis svc my-service create \
  --package acme-orders@1.2.3 \
  --registry acme \
  --participant acme-orders.MyService \
  --format json
trellis svc "$DEPLOYMENT_ID" show --format json
```

`--registry` can be left out when Trellis has only one. Auth pulls the release,
checks the manifest, config, and source digests against what the registry
returned, pulls each dependency by the exact digest the release was published
with, and compiles the service from exactly those packages, never resolving
them again. To install exactly the release someone reviewed, pin it by its
manifest digest, which `trellis publish` prints: `--package
acme-orders@sha256:…`. A release whose content doesn't match its digests is
refused.

`--source ./release/trellis-project` instead takes the project directory with
its `trellis.toml`, `trellis.lock`, and `.trellis` files, and compiles it in the
CLI.

Creating the deployment also accepts the APIs the service provides into the
installation's API catalog: approving the service is approving what it
publishes. `trellis apis review --source` shows them first. Set
`DEPLOYMENT_ID` to the returned `deployment.deploymentId`. Create installs an
immutable participant revision, sets up deployment resources, and writes the
participant-scoped binding; later releases use `apply` or let the service
submit its own build (below).

### From the Console

**Add service** in the Console takes the same release. Give the service a
name, choose **From a published package**, then the registry, the package name,
and its version (or a `sha256:` digest), and **Look up package**. The form
shows the release and the packages it was built with before anything is added;
pick which service when the package declares several.

Choosing **From IDL files** takes the project's `trellis.toml` and `.trellis`
files, or its folder, instead. The Console doesn't need `trellis.lock`: Trellis finds
each dependency itself among the `trellis` package it ships and the packages of
services already installed, picking the highest version that matches, and the
form says which it used. A dependency Trellis doesn't have is listed with the
version it needs; choose that package's `trellis.toml` and `.trellis` files
beside it. From IDL files, Trellis downloads nothing. When the project declares
several services, pick which one. Then either let the Console create the instance's key
(it shows the seed once) or paste a public key the service's operator made, and
skip the provisioning step below.

### Without the release

When you have neither its package nor its project, for instance a third party's
service that should talk to your Trellis, create the service without
`--package` or `--source`:

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

You don't have to wait for its IDL to give it settings. Set them on the claim
by name, with `trellis svc partner-orders config set` or in **Add service**,
and approving the claim moves them to the deployment. See
[Settings and secrets](/guides/settings-and-secrets#a-service-added-without-its-idl).

## 4. Provision an instance

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

In the Console, open the service and choose **Add an instance**. The Instances
table names each instance by its own title, or by its ID when several share a
title, and shows the start of its identity key, whether it is connected, and
the build it last ran. Its **⋯** menu turns the instance off or on, opens its
settings, and removes it. Removing is permanent: the identity stops working.

## 5. Run the container

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

The service's own settings and secrets, such as a database URL and password,
come from Trellis rather than `-e` flags. Set them with `trellis svc
"$DEPLOYMENT_ID" config` or in the service's Settings section in the Console;
see [Settings and secrets](/guides/settings-and-secrets). A service that is
missing a required setting waits and prints a link to set it.

## 6. Upgrade

The smoothest upgrade installs the new release before its image rolls out:

```sh
trellis svc "$DEPLOYMENT_ID" apply --package acme-orders@1.3.0
```

Apply pulls and checks the release the same way create does, and installs the
deployment's own participant from it; `--participant` is needed only to name a
different one, which apply refuses. It accepts compatible changes to the APIs
the service provides, installs the new immutable participant revision, updates
resources, and advances the binding. New instances running that build then
connect at once, even when it asks for new access, because applying it was the
approval. In the Console, the service's page does the same under **Install a
build**, **From a published package**. `apply --source DIR` takes the project
directory instead. Use a new participant `@vN` lineage for breaking API
changes. Existing provisioned identity keys remain valid for the same instance
unless intentionally rotated.

If you roll out the new image without applying it, the service sees that its
build differs from the one its instance runs and submits its own IDL when it
connects:

- If the build asks for nothing new (no new capability, resource, or API), Auth
  installs it and the service connects under it at once.
- If it asks for new access, the service waits. It logs a review link and prints
  it as a QR code; open it, or run `trellis svc "$DEPLOYMENT_ID" updates` and
  `trellis svc "$DEPLOYMENT_ID" approve`. The service connects as soon as the
  update is approved.

Settings a new build needs can be set while the old build runs, so its
instances start at once; see
[An upgrade without downtime](/guides/settings-and-secrets#an-upgrade-without-downtime).

The service's page in the Console keeps the update history: when each build
asked, what it changed, whether it was installed, rejected, or replaced, and
whether a person approved it or Trellis installed it automatically.

A build that drops a resource only gives up access to it; the data stays until
you remove it under Resources. A later build that declares it again asks for it
back, and approving that brings the kept data with it. A build that implements an
API another service already serves can only be approved with an explicit
acknowledgement, because requests would then be split at random between the two
services. To add capacity or move a service, provision more instances of the same
deployment instead. A build whose implemented API changed incompatibly is
refused until you force-replace that API.

Console **Admin / Services** shows the current deployment, installed revision,
resources, instances, and grant state. It does not expose retired authority
plans.

## Practical loop

1. Log in as an administrator, and add the registry the service is published
   to once.
2. Create the deployment from the published package, or `apply` the published
   package of a new release to the existing one.
3. Provision an instance and store its seed securely.
4. Set the service's settings and secrets.
5. Start the container with only endpoint and identity configuration. Its build
   is the installed one, so it connects at once.
6. Confirm expected RPCs, events, jobs, and resource handles.

All required resource evidence must exist before context issuance. Unavailable
optional evidence removes only its affected optional grants. The service never
provisions resources as a bootstrap side effect.
