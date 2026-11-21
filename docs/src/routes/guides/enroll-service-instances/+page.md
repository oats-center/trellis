---
title: "Enroll service instances"
description: "Give a deployment an enrollment secret so every process registers itself as its own instance, keep or retire those instances, and respond to a leaked secret."
---

# Enroll service instances

An enrollment secret lets a process add itself as a new instance of one
deployment, with no administrator involved. Use it when processes come and go:
a Kubernetes Deployment that scales, or a rolling release that starts a new pod
for each old one. Every process makes its own identity key, so the CLI and the
Console show each one, and you can cut off one without the others.

The secret only registers keys. Each instance connects with its own key, the
same way a provisioned instance does.

## Issue a secret

```sh
trellis svc orders enrollment issue --title "orders, production" \
  | kubectl create secret generic orders-enroll --from-file=secret=/dev/stdin
```

`issue` prints the secret once, on standard output; Trellis keeps only its
hash. The limits default to the installation's (`trellis instance-policy
show`):

- `--max-instances` (20): how many instances the secret may have at once. At
  the cap, a new enrollment replaces the instance that has been offline
  longest. An instance that enrolled less than 10 minutes ago and has not
  connected yet is never replaced, so a rollout that starts many pods at once
  does not retire its own new pods. With nothing to replace, the enrollment
  is refused.
- `--per-hour` (100): how many new instances it may create in any hour.
- `--expires`: a date or a duration (`90d`). After it the secret creates no
  instances; the ones it created keep running.
- `--current-build-only`: its instances may only run the build the deployment
  has installed. Their own updates are refused, so install each new build with
  `trellis svc orders apply` before you roll it out.

In the Console, the service page's Enrollment section issues secrets and shows
the secret once with a copy button.

A secret belongs to one deployment and creates instances of nothing else. Its
instances get the deployment's installed build, capabilities and resources.

## Start processes with it

Hand the secret to the process as `TRELLIS_ENROLLMENT_SECRET` and leave out the
seed. In TypeScript:

```ts
const service = await TrellisService.connect({
  trellisUrl: process.env.TRELLIS_URL!,
  participant,
  // reads TRELLIS_ENROLLMENT_SECRET; or enrollment: { secret }
}).orThrow();
```

In Rust, connect with `ServiceConnectOptions::enrolling(server.nats_servers,
server.trust, server.issuer)`; it reads the same variable, or takes
`.with_enrollment_secret(secret)`.

By default the key lives in memory. Each start enrolls a new instance, titled
with the host name, and a graceful stop retires it, so a rolling release
leaves nothing behind. A process that crashes is retired later, once it has
been offline for the retirement period.

On a host with a disk, keep the key in a file instead, and the process keeps
its instance across restarts:

```ts
enrollment: {
  keyFile: "/var/lib/orders/identity.key";
}
```

```rust
ServiceConnectOptions::enrolling(servers, trust, issuer).with_key_file("/var/lib/orders/identity.key")
```

On each start it connects with the stored key, retrying while Trellis is
unreachable; it needs no secret for that. It enrolls a new key only when
Trellis says the stored one was retired. If an administrator removed the
instance, the process stops with an error and does not enroll again.

A running process whose instance was retired while it was disconnected stops
with an error, and its restart enrolls a new instance.

## Watch and rotate

```sh
trellis svc orders instances          # each instance, with its origin
trellis svc orders instances --all    # retired and removed ones too
trellis svc orders enrollment list    # limits, instances, last use, warnings
```

`enrollment list`, the service page and the Services list warn when, in the
last 24 hours, someone presented a revoked or expired secret, a secret hit its
cap or hourly rate, or a process of another service or build tried it.

To rotate, issue a second secret, roll it out, then revoke the first.
Instances the old secret created keep running, because they connect with
their own keys.

## Respond to a leaked secret

```sh
trellis svc orders enrollment revoke <secret-id> --retire-instances
```

The secret stops working, every instance it created is retired, and their
connections are closed. The command returns once the revocation is saved and
says how many connections are still being closed. Then issue a new secret and
roll the processes.

## Retire offline instances

An enrolled instance offline for longer than 24 hours is retired. Retirement
skips disabled services, and a service that is enabled again gives each
instance a full period from then. Trellis also waits 15 minutes after it
starts before retiring anything, so processes that were connected before a
restart can reconnect.

```sh
trellis instance-policy set --retire-offline-after 48h
trellis svc orders instance-policy set --retire-offline-after off
trellis svc orders instance-policy set --retire-provisioned on
```

Provisioned instances are only retired with `--retire-provisioned on`, since
their processes cannot enroll again by themselves. Retired and removed
instances stay listed for 30 days (`--retired-record-days`).
