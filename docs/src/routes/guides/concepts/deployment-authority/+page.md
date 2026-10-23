---
title: "Deployment authority"
description: "Deployment authority, identity authority, authority updates, reconciliation, availability, liveness, grant overrides, and contract evidence."
---

Authorization is current Auth policy, evaluated each time Auth issues signed
session authority. No stored per-participant record grants access by itself.

## Installed participant revisions

Creating or applying a deployment installs an immutable participant revision
from verified source-package evidence and the participant's exact lexical path.
An installed revision defines what may be selected; it does not grant anything.

## Accepted APIs

One accepted definition exists per API major. `Auth.Apis.Review` authenticates
package evidence and computes compatibility and capability changes;
`Auth.Apis.Accept` commits the reviewed definition with any accompanying policy
edits. Request verification uses the exact accepted catalog the caller's
authority references.

## Effective authority

For users and the applications they sign in to:

```text
role, direct, and fresh OIDC entitlements
  intersect the application's requested capabilities
  intersect the OAuth grant's approved capabilities and consent revisions
```

For services and devices, authority is bounded by the deployed participant's
selected capabilities and usable resource commitments. Platform privileges
(`principals.manage`, `roles.manage`, `apis.accept`, `apis.forceReplace`,
`clients.manage`, `privileges.manage`) are a separate finite set, delegated to
registered clients only through explicit consent.

NATS subjects, API packages, resource names, and client-provided artifacts never
add authority.

## Deployments and resources

`Auth.Deployments.Create` and `Auth.Deployments.Apply` install the participant
revision and create or adopt declared resource bindings. Required missing
evidence prevents authority issuance; unavailable optional evidence removes only
the affected optional capabilities.

Services receive typed resource handles from `TrellisService.connect(...)`.
They do not fetch raw bindings or derive physical names.

`Auth.ServiceInstances.Provision` commits the immutable caller-supplied public key
and awaits the same native resource producer used by runtime startup before
returning success. Empty declaration sets and native KV are supported; other
resource declarations currently return an explicit failure rather than empty
bindings. Retry the same provisioning request after a broker failure to resume
materialization without replacing the instance or destroying retained data.

Omitting a resource in a deployment version, disabling a deployment, or removing
the deployment detaches its resources. It does **not** delete their allocations
or data. `Auth.Resources.List` includes detached resources; filter by `ownerKind`
and `ownerId` when needed. Resource IDs are digests, and each row exposes its
actual lifecycle `state`, participant, local name, kind, and revision.

A compatible approved re-add reuses the retained allocation and its data. Changes
that reduce KV history or TTL retention are rejected instead of recreating the
bucket. An administrator may explicitly choose `Auth.Resources.Remove` for a
detached KV resource, supplying its current revision. Active resources cannot be
deleted. Trellis checks the physical ownership marker before deleting only the
selected allocation. If the broker call fails, retry the same request to resume
that manual choice; startup never chooses or resumes deletion automatically.

## Administration

The CLI manages deployments with `trellis svc` and `trellis dev`
(`create`, `apply`, `enable`, `disable`, `remove`, `instances`, `provision`),
API catalogs with `trellis apis` (`list`, `get`, `review`, `accept`,
`force-replace`), and issuers with `trellis issuers`. Console shows the same
deployments, resources, logins, connections, and issuer state.

## Runtime issuance

Services and devices present a signed provisioned admission in NATS CONNECT;
browser and native apps present a DPoP-bound OAuth admission. Auth Callout
rereads current identity, login, grant, deployment, installed revision,
resources, and issuer inside one transaction, signs
`trellis.session-authority.v1`, and returns exact transport permissions. Every
mutation and idempotency replay is authorized in the same SQL transaction against
the actor's current capabilities and privileges.
