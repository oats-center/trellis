# Design: Service Updates

Status: implemented on `oauth`.

A running service carries its own IDL. When a new build connects with a
participant that differs from the revision its instance runs, the service
submits its IDL to Auth. Auth applies an update that asks for nothing new at
once, and holds one that asks for new access until an administrator approves it.

The administrator-driven path (`trellis svc create|apply`) stays; like an
approved update, it accepts the APIs the build provides. A service update is
the same install, started by the service.

## Principles

- An instance runs exactly one installed participant revision. It moves to a
  newer revision only when that revision's participant digest equals the digest
  of the binary that connects.
- An API the deployment already provides may change compatibly without
  approval: Auth accepts the new revision while applying the update.
- Starting to provide an API waits for an administrator. Providing an API
  another deployment already serves also needs the administrator to acknowledge
  it explicitly: every deployment serving an API joins one queue group, so
  callers' requests are split at random between the two services, which keep
  separate data. Capacity, host moves, and rolling upgrades use more instances of
  one deployment instead.
- What a service consumes waits for approval too: a capability the deployment
  did not already select, or a resource it does not declare now or declares
  with more room or more events.
- Dropping something only takes access away. An omitted resource is detached,
  and its data stays until an administrator removes it with `Resources.Remove`.
  Declaring it again asks to reattach that same storage, which needs approval
  like any other resource; approving it brings the kept data back, so a bad
  update followed by a fix loses nothing.

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
same owner as instance provisioning; approving an already applied update again
retries it, which the Console offers as "Try again" when setup fails.

## Changes

`AuthServiceUpdateChanges` is what the review screen shows:

- `addedCapabilities`, `addedResources`, `reattachedResources`,
  `changedResources`, `addedApis`: what the update asks for; any of them makes
  the update wait for approval.
- `sharedApis`: the added APIs another deployment already serves. Approving an
  update with any requires `acknowledgeSharedApis`; the Console asks for it with
  a warning, a checkbox, and a confirmation, and the CLI with
  `--acknowledge-shared-apis`.
- `removedCapabilities`, `removedResources`, `removedApis`: what the service
  gives up. Removed resources keep their data.
- `acceptedApis`: compatible API revisions applying the update accepts.

Approval recomputes the changes, so an API another deployment started serving
after the submission still needs the acknowledgement.

Auth enforces the acknowledgement on every path that makes a deployment serve
an API: `Deployments.Create` and `Deployments.Apply` take the same
`acknowledgeSharedApis` and refuse without it (`trellis svc create|apply
--acknowledge-shared-apis`; the Console's add-service form shows the warning and
checkbox). No client can install a shared API without it.

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

## Service claims

A claim is a service deployment for a service the administrator does not know
beforehand, such as a third party's. `Auth.ServiceClaims.Create` takes a name
and the service's identity public key; the service's owner keeps the private
seed. The claim holds no IDL, so it takes whatever service first connects with
that identity.

- Admission accepts a waiting claim's identity and binds its session to the
  claim with no participant (`SessionBinding::Service` with an empty
  `participantId`). Its only capability is `serviceUpdates.submit`, and its
  bootstrap carries no participant digest and no resources.
- The connecting SDK sees a digest no build has, so it submits itself, naming
  its participant in `Submit`. Nothing is installed, so the update always waits
  for an administrator and lists everything the service asks for.
- Approving it installs a deployment from the submitted build (accepting the
  APIs it provides, like `Deployments.Create`), provisions an instance holding
  the claim's identity, moves the update to that deployment, and retires the
  waiting session. The service connects again as that instance. The claim
  records the deployment it became.
- Rejecting the update leaves the claim waiting for another submission.
  `Auth.ServiceClaims.Remove` turns the identity away and rejects anything
  pending.

`trellis svc <name> create` without `--source` creates a claim and prints a
generated seed; the Console's Add service form takes the public key instead.
While waiting, the review URL names the claim:
`<origin>/console/admin/services/<claimId>?update=<updateId>`.

## Non-goals

- Updating device participants this way: devices use activation review.
- Approving incompatible API replacements from the update screen.
- Deleting resources as part of an update.
