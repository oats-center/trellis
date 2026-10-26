# Design: Service Updates

Status: implemented on `oauth`.

A running service carries its own IDL. When a new build connects with a
participant that differs from the revision its instance runs, the service
submits its IDL to Auth. Auth applies an update that asks for nothing new at
once, and holds one that asks for new access until an administrator approves it.

The administrator-driven path (`trellis apis accept` then `trellis svc apply`)
stays. A service update is the same install, started by the service.

## Principles

- An instance runs exactly one installed participant revision. It moves to a
  newer revision only when that revision's participant digest equals the digest
  of the binary that connects.
- What a service provides needs no approval. A participant may implement any API
  whose accepted definition it stays compatible with, and Auth accepts a
  compatible new revision of an API it implements while applying the update.
- What a service consumes does. A capability the deployment did not already
  select, or a resource it did not already declare or now declares differently,
  waits for an administrator.
- Dropping something only takes access away. An omitted resource is detached,
  and its data stays until an administrator removes it with `Resources.Remove`.
  Declaring it again reattaches the same storage, so a bad update followed by a
  fix loses nothing.

## Flow

1. The service connects with its provisioned identity seed as usual. Admission
   authorizes the instance's installed revision, and the signed bootstrap
   authorization names that revision's participant digest.
2. The SDK compares that digest with its generated participant digest. Equal
   digests continue the ordinary connection.
3. On a mismatch the SDK calls `Auth.ServiceUpdates.Submit` with its package
   evidence. Every service instance holds `trellis.auth.v1:serviceUpdates.submit`
   automatically, the way it holds `trellis.health.v1:health.report`.
4. Auth compiles the evidence, checks it names the deployment's participant, and
   compares it with the deployment's current installed revision:
   - The same digest moves the instance to that revision (an administrator
     already applied it). The result is `applied`.
   - Otherwise Auth computes the changes. With no requested change it installs
     the new revision, moves the instance, and returns `applied`.
   - With requested changes it records a `pending` update, superseding any older
     pending update for the deployment, and returns its review URL.
5. On `applied` the SDK closes its connection and connects again, now under the
   new revision. On `pending` it logs the review URL and prints it as a terminal
   QR code, then polls `Auth.ServiceUpdates.Status` with bounded backoff until
   the update is applied, rejected, or superseded. It never serves the new
   build's handlers under the old revision's authority.
6. An implemented API whose definition changed incompatibly with the accepted
   one fails the submission. That change breaks callers and needs an
   administrator to force-replace the API first.

Moving an instance re-grants its direct capabilities to match the new revision's
selection, removes capabilities the revision no longer selects, and enqueues
deployment enforcement, so sessions of the old revision are re-evaluated.
Resource materialization for the new revision runs after the commit through the
same owner as instance provisioning.

## Changes

`AuthServiceUpdateChanges` is what the review screen shows:

- `addedCapabilities` and `addedResources`, `changedResources`: what the update
  asks for; any of them makes the update wait for approval.
- `removedCapabilities`, `removedResources`: access the service gives up.
  Removed resources keep their data.
- `addedApis`, `removedApis`, `acceptedApis`: what the service starts or stops
  providing, and which compatible API revisions applying it accepts.

## Approval

The review URL is
`<origin>/console/admin/services/<deploymentId>?update=<updateId>`. The Console
opens the update over the service's page; the CLI shows the same changes with
`trellis svc <deployment-id> updates` and decides with
`trellis svc <deployment-id> approve|reject <update-id>`.

`Auth.ServiceUpdates.Decide` requires `trellis.auth.v1:users.manage`, and
`trellis.auth.v1:platform.manage` when the update adds an administrative
capability. Approval rechecks inside its transaction that the update is still
pending and that the deployment's binding has not changed since submission; a
changed binding supersedes the update and the service submits again.

## Non-goals

- Updating device participants this way: devices use activation review.
- Approving incompatible API replacements from the update screen.
- Deleting resources as part of an update.
