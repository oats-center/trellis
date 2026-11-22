---
title: Instance Enrollment
description: Enrollment secrets that let a service process register itself as a new instance of its deployment, and the retirement of instances that stay offline.
---

# Design: Instance Enrollment

> **Status: implemented.** This document describes the as-built behaviour.
> [Decisions](#decisions) at the end records how the questions raised in
> review were settled, and [as built](#as-built-details) the details the
> implementation fixed.

Without enrollment, an administrator registers every service instance by hand. That works
for a service that runs on two known hosts. It does not work for a Kubernetes
Deployment that scales from 3 pods to 30 and back, or for a rolling release
that starts a new pod for each old one. Enrollment lets an administrator
give a deployment an **enrollment secret** instead. A process that holds the
secret makes its own key when it starts and registers itself as a new
instance. Instances that stay offline for longer than a set time are
**retired**, so the instance list does not fill up with pods that no longer
exist.

## Prerequisites

- [trellis-auth.md](./trellis-auth.md): deployments, instances, identity keys,
  and admission
- [service-updates.md](./service-updates.md): how a build that differs from
  the installed one asks to be installed
- [device-activation.md](./device-activation.md): the existing flow where a
  participant proves a key and Auth records it
- [config-patterns.md](../core/config-patterns.md): instance settings and the
  storage key
- [data-retention.md](../core/data-retention.md): how long the runtime keeps
  each kind of record

## Background

A few terms from the rest of the auth design, since enrollment builds on
them.

A **deployment** is one installed service: its build, the capabilities it was
granted, and the resources (KV buckets, stores, job queues) it owns. An
**instance** is one running copy of that service. Every instance of a
deployment has the same capabilities and the same resources, and all of them
join the same queue groups, so requests are shared between them.

Each instance has its own **identity key**, an Ed25519 key pair. The service
process keeps the private half (its "seed"). Auth stores only the public half,
in `auth_provisioned_identities`, next to the instance it belongs to. When the
process connects, it signs a proof with the seed. Auth looks up the public key,
finds the instance and deployment, and admits the connection with that
deployment's authority. This step is called **admission**. The process never
tells Auth which deployment it is; the key decides that.

An administrator can still create each instance and its key with
`ServiceInstances.Provision` (`trellis svc <id> provision`). The
[upgrade guide](../../docs/src/routes/guides/upgrade-without-downtime/+page.md)
tells operators to give each process its own instance so the Console can tell
them apart. Kubernetes pods of one Deployment that share one seed show up as
one instance. Enrollment secrets solve both.

## The problem

Per-key provisioning has three costs once a service scales.

1. **Every new process needs a person.** An autoscaler cannot call
   `ServiceInstances.Provision`: that RPC needs an administrator's
   `trellis.auth.v1:users.manage`, and handing that capability to an
   autoscaler would let it manage every service and device.
2. **Sharing one key hides the processes.** The workaround is to give every pod
   the same seed. Auth then sees one instance with several connections. The
   Console cannot show which pod runs which build, and removing the instance
   cuts off every pod at once.
3. **Nothing cleans up.** Instances that an administrator provisioned for pods
   that are gone stay in the list as `offline` until someone removes them.

## The enrollment secret

An enrollment secret is a random value that lets its holder create instances
of one deployment, and nothing else. It is a credential for registering a key.
It is never used to connect.

### Issuing

An administrator issues a secret for one deployment with
`Auth.EnrollmentSecrets.Issue`. The request names the deployment, a title
(such as "orders, production cluster"), and optional limits. Issuing needs
`trellis.auth.v1:users.manage`, the capability `ServiceInstances.Provision`
already needs. A deployment that holds an administrative capability also needs
`trellis.auth.v1:platform.manage` to issue one, for the same reason approving
such a deployment does: every instance the secret creates holds that
capability.

A deployment may have several active secrets at once. That is what makes
rotation possible without downtime (see [rotation](#rotation-and-revocation)).

### How it is shown and stored

The secret is 32 random bytes, written as text with a prefix and the secret's
ID, for example `trellis-enroll-01JD3K...-<random part>`. The ID lets an
operator see which secret a process was given, and it lets audit records name
the secret without containing it.

Auth shows the full text once, in the response to `Issue`. It stores the
SHA-256 hash of the text and never the text itself. A hash is enough because
Auth only has to check a presented secret, never to read one back. Sealing it
with the storage key, as settings secrets are, would let Auth decrypt and show
it again, which is exactly what a shown-once credential should not allow. The
secret has 256 bits of entropy, so a fast hash is safe; a password hash adds
nothing. Device provisioning secrets are stored the same way today.

### Scope

A secret belongs to exactly one deployment. An instance it creates is an
ordinary instance of that deployment: it runs the deployment's installed
build, holds the deployment's capabilities, and uses the deployment's
resources. A secret cannot create a new deployment, change what a deployment
is allowed to do, or create instances of any other deployment.

### Limits

Each secret carries its own limits. All of them can be changed later with
`Auth.EnrollmentSecrets.Update`.

- **Expiry** (`expiresAt`, optional). After it, the secret creates no more
  instances. Instances it already created keep running. A secret without
  expiry works until it is revoked.
- **Instance cap** (`maxInstances`). The most instances this secret may have
  at once, counting every instance it created that is not yet retired or
  removed. New secrets take the installation default, 20 unless changed. When
  the secret is at its cap, a new enrollment first retires the secret's
  instance that has been offline longest. An instance enrolled less than the
  first-connect grace period ago (`firstConnectGrace`, 10 minutes by default)
  that has not connected yet is not offline for this purpose: it cannot be
  replaced until it has connected once or the grace period ends. Without that,
  a rollout that starts many pods at once could retire a new pod before it
  finished connecting. If no instance can be replaced, the enrollment is
  refused with `instance_limit`. Retiring the longest-offline one keeps a
  crash-looping pod from using up the cap with instances that will never come
  back. A cap lowered below the number of instances the secret already has
  retires none of them, and replaces none either: every new enrollment is
  refused with `instance_limit` until retirement, removal, or revocation brings
  the count down to the cap. From there, replacement works as above.
- **Rate limit** (`enrollmentsPerHour`). The most new instances this secret may
  create in any hour. New secrets take the installation default, 100 unless
  changed. Repeating an enrollment for a key that is already enrolled does not
  count.
- **Current build only** (`currentBuildOnly`, off by default). Instances this
  secret creates may run only the build the deployment has installed. Auth
  cannot see what code a process runs, so this is enforced where Auth can see
  it: such an instance's `ServiceUpdates.Submit` is refused, and the SDK stops
  with an error naming `trellis svc <id> apply`. The point is that a process
  holding only the secret cannot propose a new build. A new build is installed
  by an administrator first, and enrolled processes then pick it up. Why this
  matters is covered under [security](#what-a-leaked-secret-allows).

The installation defaults for the cap and the rate are Auth DB settings,
changed at runtime (see [policy settings](#policy-settings)). A secret copies
the defaults when it is issued, so its record always shows the limits it
actually has.

### Rotation and revocation

To rotate, issue a second secret, give it to the processes (for example by
updating a Kubernetes Secret and rolling the Deployment), then revoke the
first. Instances the old secret created keep running through this, because
they connect with their own keys.

`Auth.EnrollmentSecrets.Revoke` stops a secret from creating instances. By
default it leaves the instances it created alone. With `retireInstances` it
also retires every instance the secret created, which revokes their keys and
disconnects them. The revocation and the retirements commit in one
transaction, and the disconnects are recorded with them as described under
[what retired means](#what-retired-means). The CLI reports success once the
revocation is committed and shows how many connections are still being
closed. That is the response to a leaked secret: revoke it with
`retireInstances`, issue a new one, and roll the processes. A revoked secret
stays in the list, marked revoked, so later attempts to use it can be reported
(see [alerts](#alerts)).

### CLI and Console

```text
trellis svc <id> enrollment issue [--title <text>] [--expires <date|duration>]
                                  [--max-instances <n>] [--per-hour <n>]
                                  [--current-build-only]
trellis svc <id> enrollment list
trellis svc <id> enrollment edit <secret-id> [same limit flags]
trellis svc <id> enrollment revoke <secret-id> [--retire-instances]
```

`issue` prints the secret once on stdout, so it can be piped straight into a
secret store, for example `kubectl create secret generic orders-enroll
--from-file=secret=/dev/stdin`. `list` shows each secret's title, limits, how
many instances it has, when it was last used, and whether it is revoked.

The Console adds an Enrollment section to each service page. Issuing opens a
form with the limits filled in from the defaults, then shows the secret once
with a copy button and the environment variable line to paste. The section
lists every secret of the service, reading all pages of
`Auth.EnrollmentSecrets.List`. Each secret row offers Edit and Revoke; Revoke
asks whether to retire the secret's instances too. The edit form shows the
expiry as a date; a save that leaves the date unchanged keeps the exact expiry
time, and a changed date expires the secret at the start of that day (UTC).

## Enrollment at startup

### What the process does

1. The SDK generates a new Ed25519 identity key in memory.
2. It sends `POST /auth/service/enroll` to the Trellis origin. The body holds
   the enrollment secret, the new public key, the participant ID of the
   service's build, the build's participant digest, a title (the host name by
   default), a request ID, and the time it was sent. The new identity key signs
   the whole body. The request goes over the same HTTPS connection, with the
   same certificate checks, that the SDK already uses to reach Trellis.
   Enrollment requests don't follow redirects: the TypeScript and Rust SDKs
   send this request and `POST /auth/service/identity` only to the Trellis
   origin, and a redirect response fails the request with an error naming the
   status and the location. The TypeScript device enrollment request
   (`POST /auth/device/enroll`) does the same; the Rust one refuses the
   redirect with its HTTP status.
3. Auth checks the signature and that the send time is within a few minutes of
   its own clock, before it looks at the secret. That proves the process holds
   the private key it is registering.
4. Auth looks the public key up. If it is already registered to an instance
   this secret enrolled, Auth returns that instance at once, before any cap or
   rate check (see [retries](#retries)).
5. Auth checks the secret: it exists, is not revoked or expired, its rate
   allows one more, its deployment is enabled, and the participant ID matches
   the deployment's. A mismatch catches a common mistake, such as starting the
   billing service with the orders service's secret.
6. In one Auth DB transaction, Auth checks the cap, retires an instance to make
   room if the cap requires it, and creates the principal, the instance, and
   the identity key, exactly as `ServiceInstances.Provision` does. Two
   enrollments that race for the last slot therefore cannot both take it. The
   instance
   gets the deployment's installed revision. Auth records that the instance was
   enrolled and which secret enrolled it, and writes an audit entry.
7. Auth answers with the new instance's ID and title. The SDK logs them.
8. The SDK connects with the new key through ordinary admission.

From step 8 on, nothing is new. The instance is admitted like any provisioned
instance, and admission does not need to know how the key was registered. That
keeps the enrollment secret out of the NATS connect path entirely: it travels
once, to Auth's HTTP listener, and never again.

Device activation has the same shape (an HTTP enrollment that proves a key,
followed by ordinary admission), with one difference: a device waits for an
administrator to approve each enrollment. An enrolled service instance needs
no approval, because the administrator already approved it in bulk by issuing
the secret.

### Retries

Enrollment is idempotent per key. If the response to step 7 is lost, the SDK
sends the same request again and gets the same instance back. Because step 4
looks the key up before the cap and rate checks, the retry succeeds even when
the lost enrollment filled the secret's last slot or its hourly rate. Auth
refuses a key that is already registered to some other instance or
deployment.

### Fit with service updates

An enrolled instance starts on the deployment's installed revision, as a
provisioned one does. If the process runs a different build, the SDK notices
the digest mismatch after admission and submits a
[service update](./service-updates.md), as it does today. A build that asks
for nothing new is applied at once. A build that asks for new access waits for
an administrator, and the process waits with it, printing the review link.
Instances of one deployment that run the same build share one pending update,
so 30 new pods of a new build produce one review, not 30.

With `currentBuildOnly`, step 5 also refuses a build whose digest differs from
the installed one, so the process fails at once instead of after admission.
The process states that digest itself, so this early check only catches
mistakes; the refusal of `ServiceUpdates.Submit` is what enforces the limit.

### How the SDK takes the secret

The secret is a runtime option, in place of the seed: `enrollment` in
`TrellisService.connect` in TypeScript, and an enrollment form of
`ServiceConnectOptions` in Rust. When neither a seed nor an enrollment secret
is given, the SDK reads `TRELLIS_ENROLLMENT_SECRET` from the environment, the
way it reads `TRELLIS_CERTIFICATE_FINGERPRINT` today. Giving both a seed and a
secret is an error.

The secret never goes into IDL. IDL describes a build, and one build runs in
many deployments; the IDL is also published with the package. The secret
belongs to one deployment and has to stay private.

### Restarts

There are two ways a restarted process could come back.

- **A new instance each time.** The key lives only in memory. Every process
  start enrolls a new instance, and the old one is retired.
- **The same instance again.** The SDK writes the seed to a local file after it
  generates it. A restarted process reads the file and keeps its instance.

**Recommendation: a new instance each time, by default.** Containers usually
have no disk that survives a restart, and a Kubernetes pod that restarts on
another node is a new pod anyway. One instance per process lifetime also
matches what an operator expects to see: one row per running process. It keeps
the seed off disk, which is one less file to protect.

To keep the list clean, an instance with a memory-only key retires itself when
it stops gracefully. After the SDK's stop has drained its work (see the
upgrade guide's "What a stop does"), it calls `ServiceInstances.RetireOwn`
and then closes. Every enrolled instance holds the capability for that call
automatically, the way every instance holds `serviceUpdates.submit`. Its key
is about to vanish with the process, so the instance could never connect again
anyway. A rolling release therefore leaves no offline instances behind. A
process that crashes or is killed cannot do this, and
[timed retirement](#retiring-offline-instances) removes its instance later.

The second form stays available for hosts with a disk, through a `keyFile`
option. It suits a systemd service on a long-lived machine, where an operator
may want instance settings overrides and history to survive a restart. The SDK
writes the file with owner-only permissions before it enrolls, so a crash
between the two steps cannot lose a key Auth has already registered. On every
start the SDK works through these steps:

1. It connects with the stored key through ordinary admission. The enrollment
   secret is not used.
2. If the connection fails for a transient reason, such as Trellis being
   unreachable, it retries with the same key.
3. If Auth reports that an administrator removed the instance, the process
   stops with an error. An administrator's removal is a decision about that
   process; quietly enrolling again would undo it.
4. Only if Auth reports the key as unknown or retired does the SDK make a new
   key, replace the file, and enroll a new instance with the enrollment
   secret.

A host that holds a valid key therefore restarts without the secret, so
rotating or revoking the secret does not break its routine restarts.

In either form, a running process that loses its connection for longer than
the retirement period comes back to find its instance retired. The SDK cannot
swap the identity of a live connection, so it stops the service with an error
instead (TypeScript: the service's connection ends with it; Rust:
`ServiceRuntimeError::InstanceRetired` from `run`). While the connection is
down for more than a few seconds, the SDK asks Auth what became of its key; on
`retired` or `removed` it stops. The process's supervisor restarts it, and the
restart enrolls a new instance while the secret is still valid.

## Security

### What a leaked secret allows

Whoever holds an enrollment secret can start processes that Trellis treats as
genuine instances of that deployment. Such a process can call every API the
deployment may call, serve every API the deployment provides, and read and
write the deployment's KV buckets, stores, and jobs. Because it joins the same
queue groups, it receives a share of real callers' requests. This is the same
access a leaked instance seed gives, multiplied: a seed is one identity that an
administrator can remove, while a secret mints new identities until it is
revoked.

A leaked secret cannot widen the deployment's access. A build that asks for new
capabilities or resources still waits for an administrator. A process holding
the secret can, however, submit a build that asks for less, such as one that drops an API, and such an
update is applied without approval. Applying it stops the deployment's other
instances, which still serve the dropped API (see the upgrade guide's "A build
that drops an API"). `currentBuildOnly` closes that door for deployments where
an administrator always installs new builds first.

### What bounds the damage

- **Instance cap.** At most `maxInstances` instances exist for the secret at a
  time, so a leak cannot flood the queue groups or the instance list.
- **Rate limit.** At most `enrollmentsPerHour` new instances per hour, so
  retiring and enrolling in a loop is slow.
- **Expiry.** A secret given to a one-off job can be made to expire.
- **Revocation with `retireInstances`.** One action stops new enrollments and
  disconnects everything the secret created.
- **Audit.** Every enrollment, refusal, retirement, and change to a secret is
  in the security audit log with the secret's ID.
- **Alerts in the Console**, described under [alerts](#alerts).

The enrollment endpoint is reachable without signing in, like the device
enrollment endpoint. Guessing a 256-bit secret is not feasible, and the
endpoint sits behind the listener's existing per-client request limit. A
refusal names its reason (revoked, expired, at its cap) only for a secret that
exists, so a guess can only ever learn that the secret is unknown.

### Compared with per-key provisioning

| Question                         | Per-key provisioning      | Enrollment secret                          |
| -------------------------------- | ------------------------- | ------------------------------------------ |
| Who approves each new instance   | An administrator, by hand | Nobody; approved in bulk when issued       |
| What one leaked credential gives | One instance              | Up to `maxInstances` instances at a time   |
| How to cut off a leak            | Remove that instance      | Revoke the secret with `retireInstances`   |
| Can processes be told apart      | Only with one key each    | Always; every process has its own key      |
| Who holds the instance's seed    | The process (or a file)   | The process, in memory unless `keyFile` is |

Provisioning stays available and unchanged. Nothing forces a deployment to have
a secret.

### Why the secret is not the instance identity

Using the secret itself to connect would be simpler and worse.

- Auth could not tell processes apart: every process would present the same
  credential, which is the shared-seed problem from [the problem](#the-problem).
- Cutting off one process would cut off all of them.
- A bearer secret on every connection would travel to the broker on every
  reconnect. An identity key never leaves the process: it signs a proof bound
  to one connection's session key, so a captured proof is useless elsewhere.
- Rotating the secret would mean restarting every process at once. With
  separate keys, rotation affects only processes that have not enrolled yet.

## Retiring offline instances

### The rule

An instance that has been offline longer than the deployment's retirement
period is retired automatically. Offline means Auth has no admitted connection
for it. Auth records each instance's `lastSeenAt` whenever one of its
connections is admitted or closes. A pass every five minutes retires the
instances whose `lastSeenAt` is older than the period.

The period is runtime state in the Auth DB, set per deployment, with an
installation default for deployments that set none. The default is 24 hours,
and the shortest allowed period is one hour. A deployment can also turn
retirement off. See [policy settings](#policy-settings).

### What "retired" means

Retiring an instance, in one Auth DB transaction:

1. sets its state to `retired` and records when and why;
2. revokes its identity key, so admission refuses it from then on;
3. revokes its sessions and records the disconnects of any connection it still
   has (normally none, since it was offline); and
4. writes an audit entry.

The disconnects themselves cannot happen inside the transaction, because they
are actions on the NATS server. Auth records them durably with the change, in
`auth_enforcement_work` and `auth_post_commit_actions`, the
[enforcement pipeline](./trellis-auth.md#enforcement-pipeline) it already uses
for other revocations. The pipeline retries each disconnect until it happens,
so a crash right after the commit cannot leave a retired instance connected.
Until then, the revoked key already keeps the instance from being admitted
again.

A retired instance cannot be enabled again. A process that comes back after
its instance was retired enrolls again with a new key, if it still holds a
valid secret. Retirement revokes the key just as an administrator's removal
does. It is a separate state so that the list and the SDK can tell "the system
cleaned up" from "a person decided".

Each retirement records one of four reasons:

- `offline`: offline past the retirement period;
- `stopped`: the instance retired itself at a graceful stop;
- `replaced`: its secret was at its cap and a new enrollment took its place;
- `secretRevoked`: its secret was revoked with `retireInstances`.

### How long the record stays

The retired instance's record stays for 30 days by default (an installation
setting), so an operator can see what ran and when. Then Auth deletes the
instance row, its identity key row, and its instance settings overrides,
including any instance secrets, in transactions of at most 1,000 rows like
other SQL retention. The principal row stays, because permanent records refer to
it: every authorization context the instance was ever issued, and the audit
log. This matches what happens to a deleted user account today.

Auth also keeps a permanent tombstone for the identity key: its fingerprint
and its terminal outcome, either removed by an administrator or retired
automatically. Tombstones never expire. Each is a few bytes, and the principal
row is kept forever anyway. With the tombstone, a host that returns after the
rows are gone is still recognised. A removed key gets the removal answer, and
the SDK stops. A retired key gets the retired answer, and the SDK makes a new
key and enrolls. Without it, a removed host's key would read as unknown and
the host would quietly enroll again.
[data-retention.md](../core/data-retention.md) lists retired and removed
instances. Auth writes the tombstone when the key is revoked by a retirement,
an instance removal, or a deployment removal, and again (idempotently) when
cleanup deletes the rows.

### Which instances are eligible

By default, only enrolled instances are retired. A hand-provisioned instance
was a deliberate choice by an administrator, often a long-lived machine that
may be switched off for a week. Its process has no secret, so once retired it
could not come back by itself. A deployment can opt in with
`retireProvisioned`, which applies the same period to its provisioned
instances.

### Long outages

Two kinds of outage could make healthy instances look offline.

- **Trellis itself was down.** Every instance looks offline when Auth starts
  again. Auth therefore skips retirement for the first 15 minutes after it
  starts, which gives running processes time to reconnect.
- **The network between an instance and Trellis was down** while Trellis kept
  running. Auth cannot tell that apart from a process that is gone. Choose a
  period longer than any partition you want to ride out. If an instance is
  retired anyway, its process re-enrolls when the network returns, because the
  SDK keeps the secret in memory.

When Auth has missed a disconnect and still believes a connection is admitted,
the instance counts as online and is not retired. That mistake keeps an
instance too long; it never retires one too early.

### Disabled deployments

While a deployment is disabled, none of its instances can connect, so all of
them would drift past the period. Retirement does not run for a disabled
deployment, and enrollment into it is refused. When the deployment is enabled
again, the offline clock of each instance starts from that moment, so every
instance gets a full period to reconnect. (Auth records the deployment's
`enabled_at`; an instance's offline time counts from the later of its
`lastSeenAt`, its creation, and that moment.) An instance an administrator
disabled is still retired once it has been offline past the period.

### Policy settings

The retirement period, the first-connect grace period, and the secret defaults live in one Auth DB record, read
and changed with `Auth.InstancePolicy.Get` and `Auth.InstancePolicy.Set`. The
record has an installation level and a per-deployment level. No server
configuration file holds any of these values.

| Setting                    | Level                       | Default  |
| -------------------------- | --------------------------- | -------- |
| `retireOfflineAfter`       | installation and deployment | 24 hours |
| `retireProvisioned`        | installation and deployment | off      |
| `retiredRecordDays`        | installation                | 30       |
| `firstConnectGrace`        | installation                | 10 min   |
| `secretMaxInstances`       | installation                | 20       |
| `secretEnrollmentsPerHour` | installation                | 100      |

A deployment that sets nothing follows the installation level, and later
changes to the installation level reach it. Changing the policy needs
`trellis.auth.v1:users.manage`.

```text
trellis instance-policy show
trellis instance-policy set [--retire-offline-after <duration|off>]
                            [--retire-provisioned on|off] [--retired-record-days <n>]
                            [--first-connect-grace <duration>]
                            [--secret-max-instances <n>] [--secret-per-hour <n>]
trellis svc <id> instance-policy show
trellis svc <id> instance-policy set [--retire-offline-after <duration|off|default>]
                                     [--retire-provisioned on|off|default]
```

The Console shows the deployment level in the service page's Enrollment
section, and the installation level on the Services page's settings.

## Observability

### The instance list

`ServiceInstances.List` gains, for each instance:

- `origin`: `provisioned`, or `enrolled` with the secret's ID and title;
- `lastSeenAt`: when it was last connected;
- `retiredAt` and `retiredReason`, for retired instances; and
- `retired` as a new value of `state`, next to `active`, `disabled`, and
  `removed`.

`trellis svc <id> instances` hides retired instances unless `--all` is given,
and adds an origin column. The Console's service page lists retired instances
in a collapsed group under the live ones, with their reason and when they were
last seen.

### Audit events

Each of these writes a security audit entry naming the actor (the
administrator, or the system for automatic actions), the deployment, and the
secret ID where there is one:

- `enrollmentSecret.issued`, `enrollmentSecret.updated`,
  `enrollmentSecret.revoked`;
- `instance.enrolled`, with the instance ID, the key's ID, and the title the
  process sent;
- `instance.enrollmentRefused`, with the reason (`revoked`, `expired`,
  `instance_limit`, `rate_limited`, `deployment_disabled`,
  `participant_mismatch`, `build_mismatch`);
- `instance.retired`, with the reason; and
- `instancePolicy.changed`.

A refusal for a secret nobody issued has no secret ID to record. It is not
audited, so a flood of guesses cannot fill the audit log.

Audit entries are permanent under the current retention rules. With memory-only
keys, each process lifetime writes about two entries (enrolled, retired), so a
deployment that starts 1,000 pods a day adds about 2,000 rows a day.

### Alerts

The service page shows a warning, and the Services list a marker, when in the
last 24 hours:

- someone presented a revoked or expired secret, which means a copy of it is
  still out there;
- a secret hit its instance cap or its hourly rate; or
- any enrollment was refused for `participant_mismatch` or `build_mismatch`.

Each warning names the secret and links to the matching audit entries.

## Out of scope

- **Workload identity from the platform**, such as Kubernetes service account
  tokens or cloud instance identity documents. They would remove the shared
  secret altogether and could later be a second way to enroll, but each
  platform needs its own verifier.
- **Devices.** Devices keep [device activation](./device-activation.md), where
  every device is reviewed.
- **Services the administrator does not know yet.** Enrollment needs an
  existing deployment. A third party's service still starts as a
  [service claim](./service-updates.md#service-claims).
- **Moving an instance between deployments.** A secret creates instances of its
  own deployment only.
- **Choosing which instance gets a request.** All instances of a deployment
  stay equal members of its queue groups.

## Decisions

Each question raised in review was settled with its recommendation.

1. **A new instance per start by default, with a key file as the opt-in.**
   Memory-only keys enroll per process lifetime and retire themselves at a
   graceful stop; `keyFile` keeps the instance.
2. **A new secret has an instance cap**, 20 from the installation default.
   The cap cannot be cleared; it can be raised to at most 1,000,000.
3. **24 hours** of offline time before retirement and **30 days** of keeping
   retired records.
4. **Provisioned instances are not retired by default**; `retireProvisioned`
   opts a deployment or the installation in.
5. **`currentBuildOnly` is off by default.** The Console's form explains it.
6. **The SDK reads `TRELLIS_ENROLLMENT_SECRET`** when no seed or secret is
   given; the explicit option wins.
7. **Removed instances' rows are deleted after the same 30 days**, keeping the
   key's tombstone, so a returning host is still told to stop. Cleanup covers
   service instances only; device records are unchanged.

## As-built details

### Wire formats

`POST /auth/service/enroll` takes `AuthServiceEnrollmentRequest`:
`requestId`, `iat` (Unix milliseconds), `enrollmentSecret`, `identityKeyId`
(the SHA-256 of the public key), `identityPublicKey`, `participantId`,
`participantDigest`, `title`, and `proof`. The proof is a
`trellis.session-proof.v1` signature of purpose `serviceEnrollment`, bound to
the Trellis origin, the route, the key ID, and the canonical request without
`proof`. Auth accepts a send time within five minutes either way. Success
returns `{ instanceId, deploymentId, title }`. Refusals are
`{ "error": { "code" } }`:

| Status | Code                                                                                  | Meaning                                                                    |
| ------ | ------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| 400    | `invalid_request`, `clock_skew`                                                       | Malformed or not signed by the key; send time outside the window           |
| 403    | `enrollment_denied`                                                                   | No such secret (not audited)                                               |
| 403    | `revoked`, `expired`, `deployment_disabled`, `participant_mismatch`, `build_mismatch` | A known secret refused (audited)                                           |
| 409    | `key_in_use`                                                                          | The key belongs to another instance, a service claim, or has a tombstone   |
| 429    | `instance_limit`, `rate_limited`                                                      | Over the cap, or at it with nothing to replace; the hourly rate is used up |
| 503    | `unavailable`                                                                         | Try again with the same key                                                |

A repeated request for a key this secret already enrolled, whose key is still
active, returns that instance before any limit or validity check. Auth
materializes the deployment's resources after each successful enrollment,
including a repeated one, as `ServiceInstances.Provision` does.

`POST /auth/service/identity` takes `requestId`, `iat`, `identityKeyId`,
`identityPublicKey`, and a `serviceIdentityStatus` proof, and answers
`{ "state" }`: `active`, `disabled` (the instance, its deployment, or its
principal is off), `removed`, `retired`, or `unregistered`. It answers from
the tombstone once the rows are gone. A device's identity key, current or
tombstoned, is `unregistered` here, the same as a key Auth never saw. A
key-file host calls it after a failed
connect; any answer other than `removed`, `retired`, or `unregistered` (or no
answer) means retry with the same key, with backoff up to 30 seconds.

### Secrets

The secret's text is `trellis-enroll-<secret ID>-<43 base64url characters>`.
`Issue`'s idempotent replay returns the record without `text`, because Trellis
never stores the text. Revoking keeps the row; revoked secrets cannot be
edited. Issuing, editing, and revoking need `users.manage`, and
`platform.manage` too when the deployment's required or optional capabilities
include an administrative one. The hourly rate counts the secret's instances
created in the last hour, retired ones included.

`Revoke` returns `retiredInstances` and `closingConnections`, the number of
those instances' connections that were open when the revocation committed. The
enforcement pipeline closes them.

### Instances

Enrolled instances hold `trellis.auth.v1:instances.retireOwn`, granted at
enrollment and included in their session authority, which lets them call
`ServiceInstances.RetireOwn`. That call retires the caller's own enrolled
instance with reason `stopped`; a provisioned instance is refused. The
caller's own connection is closed only after it has the reply.

Retiring an instance marks it `retired`, revokes its principal and identity
key, writes the tombstone, queues principal-scope enforcement work (which
retires its sessions and kicks its connections), and writes
`instance.retired`. `lastSeenAt` is updated when a connection is admitted,
when it closes, and when a kick closes it. The sweep runs every five minutes
from Auth's ordinary maintenance loop, at most 1,000 instances per
transaction, and retention cleanup runs with it. Provider certificates keep
the instance ID without a foreign key, so cleanup can delete instance rows.

### Policy

`Auth.InstancePolicy.Get { deploymentId? }` returns `installation`, and with a
deployment its own `deployment` record and the `effective` rules.
`Auth.InstancePolicy.Set` replaces the installation record
(`retireOffline`, `retireOfflineAfterSeconds`, `retireProvisioned`,
`retiredRecordDays`, `firstConnectGraceSeconds`, `secretMaxInstances`,
`secretEnrollmentsPerHour`) or, with a deployment, that deployment's record,
where an absent field follows the installation. A deployment's
`retireOffline: false` turns retirement off for it.

### Audit and alerts

Besides the generic entry every Auth mutation writes, Auth writes
`enrollmentSecret.issued`, `enrollmentSecret.updated`,
`enrollmentSecret.revoked`, `instance.enrolled`,
`instance.enrollmentRefused`, `instance.retired`, and `instancePolicy.changed`.
Refusals and secret changes name the secret as their target, so a secret's
alerts are its refusal entries of the last 24 hours, grouped by reason, and
`EnrollmentSecrets.List` returns them as `alerts`.
