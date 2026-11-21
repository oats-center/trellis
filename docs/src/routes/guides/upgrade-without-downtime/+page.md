---
title: "Upgrade a service without downtime"
description: "Start the new build beside the old one, wait until it serves, then stop the old one: readiness, graceful stop, and what Trellis guarantees while two builds run."
---

# Upgrade a service without downtime

Trellis services upgrade by overlapping. Start the new build beside the old
one, wait until it serves, then stop the old one. Every instance of a
deployment joins the same queue groups, so while both run, requests, jobs and
event deliveries go to either of them, and when the old one stops, the rest go
to the new one.

The SDKs tell you when an instance serves and stop it without dropping its
work, in TypeScript and Rust alike.

## When an instance serves

An instance serves once it is connected, its build is installed (after an
administrator approved it, if it asked for new access), its required settings
are set, and every handler, job worker and consumer registered before
`service.wait()` (TypeScript) or `run()` (Rust) takes work. Three things report
it:

- **systemd.** When `NOTIFY_SOCKET` is set, the service sends `READY=1` once it
  serves and `STOPPING=1` when it begins to stop, with a `STATUS=` line saying
  what it does: connecting, waiting for approval with the review link, waiting
  for settings with their names, serving, reconnecting, stopping. This is what
  `Type=notify` and Quadlet's `Notify=true` wait for. No health check is
  needed.
- **A readiness probe**, for Kubernetes, load balancers and anything else that
  can't use systemd. Set it in the service's runtime options: a file that
  exists only while the service serves, and/or a small HTTP endpoint where
  `GET /ready` answers 200 while it serves and 503 otherwise, and `GET /live`
  answers 200 until it has stopped.
- **The API**, for code that acts on it: `service.ready`, a promise, and
  `service.phase` in TypeScript; `runtime.lifecycle()` in Rust, with `ready()`
  and `phase()`.

Serving goes false again while the connection to Trellis is lost and the SDK
reconnects: `/ready` answers 503, the file is removed, and systemd sees
`STATUS=Reconnecting to Trellis` (`READY=1` is sent once per process, so
systemd does not restart it). It serves again once reconnected. A build that is
no longer installed for its instance, because the deployment moved past it,
stops with an error.

TypeScript:

```ts
const service = await TrellisService.connect({
  trellisUrl,
  participant: participants.OrdersService.participant,
  seed,
  runtime: {
    readiness: { http: { port: 8081 }, file: "/run/orders/ready" },
    stopTimeoutMs: 25_000,
  },
}).orThrow();
await service.handleCreate(createOrder);
void service.ready.then(() => console.log("serving"));
await service.wait();
```

Rust:

```rust
let mut runtime = Participant::connect(
    ServiceConnectOptions::new(server.nats_servers, server.trust, &seed)
        .with_readiness_http("0.0.0.0:8081".parse()?)
        .with_readiness_file("/run/orders/ready")
        .with_stop_timeout(Duration::from_secs(25)),
)
.await?;
// Register providers, then:
let lifecycle = runtime.lifecycle();
tokio::spawn(async move {
    // False when the service stops without ever serving.
    if lifecycle.ready().await {
        tracing::info!("serving");
    }
});
runtime.run().await?;
```

Sending to `NOTIFY_SOCKET` from TypeScript needs Node 26 or newer: Node has no
Unix datagram sockets, so the SDK calls libc through `node:ffi`. On Node 24 the
service runs as before, logs once that systemd will not hear from it, and the
readiness probe still works.

## What a stop does

`SIGTERM`, `Ctrl-C` (`SIGINT`), or `service.stop()` in TypeScript and
`lifecycle.stop()` in Rust:

1. **Takes no new work.** The instance leaves every queue group, so the next
   request, job and event delivery goes to another instance. It sends
   `STOPPING=1`, its probe stops reporting ready, and its heartbeat reports
   `stopping` at once.
2. **Ends its Live streams.** Each caller sees the stream end with
   `local_shutdown` and can open it again; the opening goes to an instance
   that serves.
3. **Finishes what it holds**, up to the stop timeout, 25 s by default
   (`stopTimeoutMs`, `with_stop_timeout`): requests in flight get their
   replies, event deliveries are acknowledged, running jobs finish.
4. **Hands back the rest at once.** At the deadline a running job is cancelled
   (its handler sees the cancellation) and returned to the queue with no
   delay, so another instance starts it right away. This does not wait for
   the handler: one that ignores its cancellation, even after its lease was
   lost, runs on in a stopping TypeScript process until it exits (Rust drops
   it), and what it returns is discarded. An unfinished event delivery is
   returned the same way. A request still running is abandoned; its caller's
   own timeout applies.
5. **Exits.** `wait()` resolves and `run()` returns `Ok(())`. After a signal the
   TypeScript process exits once nothing holds it open, or 5 s after the stop
   timeout at the latest, even if the stop has not finished. A second signal
   ends the process at once.

Operations keep their own recovery: an operation still running when its
instance stops is resumed by another instance once its lease expires.

Choose the stop timeout to fit inside what the supervisor allows before it
kills the process: systemd's `TimeoutStopSec` (90 s by default), Podman's stop
timeout (10 s by default; raise it with `StopTimeout=`), Kubernetes'
`terminationGracePeriodSeconds` (30 s by default).

### What a stop did in 0.100.0-rc.50

For comparison, the same test on 0.100.0-rc.50, the same in both SDKs: a caller
sent one request every 100 ms while the stopping instance held a 2-second
request, a running job, an open Live stream and an event delivery, and a second
instance was serving.

- SIGTERM ended the process at once; neither SDK handled the signal.
  `service.stop()` in TypeScript returned within about 50 ms.
- The slow request was lost: when its timeout ran out, its caller got
  `TransportError: The request deadline elapsed.` A request the broker had
  handed to the instance in its last moment could be lost the same way (one in
  300 in one run, none in the others).
- The running job resumed on the other instance about 4 s after SIGTERM, or
  about 5 s after `service.stop()`, which cancelled it and returned it with its
  retry delay.
- The event delivery waited for its acknowledgement deadline, 30 s by default,
  before it was delivered again, to the other instance.
- The Live stream went silent. About 33 s later its caller saw
  `LiveStreamError: live provider silent past the inactivity bound` and had to
  open it again.

## The procedure

Give each process its own instance, so the CLI and the Console show them apart:

```sh
trellis svc "$DEPLOYMENT_ID" provision --new --public-key <key>
```

Two processes that share one identity both serve, but Trellis tracks them as
one instance.

Then, for each upgrade:

1. Start the new build as the instance that is not running.
2. Wait until it serves: `systemctl start` returns, the readiness probe passes,
   or `trellis svc "$DEPLOYMENT_ID" instances` shows it `serving`.
3. Stop the old one.

`trellis svc "$DEPLOYMENT_ID" instances` and the service's page in the Console
show, for each instance, its build and version, whether that is the
deployment's current build or an older one still running, its state
(starting, waiting for approval with the review link, waiting for settings
naming the missing ones, serving, stopping), its health (healthy, degraded,
unhealthy, offline) with each failing check's name and message, when it
connected and its last heartbeat. `--format json` gives a script the same.
During a switch you see the new instance move to serving and then the old one
leave.

If the new build asks for new access, it waits for approval and the old build
keeps serving. Approve it, or install it beforehand with
`trellis svc "$DEPLOYMENT_ID" apply`, so the new instance starts at once. Set
the settings the new build needs while the old one runs; see
[An upgrade without downtime](/guides/settings-and-secrets#an-upgrade-without-downtime).

## What Trellis guarantees while two builds run

- An older instance keeps serving after a newer build installs. Measured on
  0.100.0-rc.50 with a request every 100 ms: no failures.
- A new build that waits for approval leaves the old build serving.
- Both builds answer for the same APIs, and a build may only change an API it
  provides compatibly, so a caller gets a valid answer from either.
- Each job and event delivery goes to one instance at a time, whichever build
  it runs; stopping one hands its work to the other as described above.
- Each build reads the settings it declares, so values only the new build uses
  don't affect the old one.

### A build that drops an API

A build that no longer provides an API, or that provides the next major version
in place of the current one, stops the older builds at once when it is
installed: the deployment no longer accepts what the older build serves, so its
instances lose their authority and stop with an error. During an overlap that is
downtime for that API. A build that drops a job queue the older build declares
stops it the same way.

To avoid it, add the new major version beside the old one:

1. Release a build that implements both `orders@v1` and `orders@v2`, and
   upgrade to it as above. Callers move to `v2` at their own pace.
2. Once nothing calls `v1`, release a build that drops it.

Drop a job queue the same way: first a build that stops creating its jobs and
still works them off, then, once the old builds are gone, one without it.

## systemd and Quadlet

A Quadlet unit for one of the two instances, `orders-a.container`:

```ini
[Unit]
Description=Orders service (instance a)

[Container]
Image=ghcr.io/acme/orders:1.4.0
Notify=true
Secret=orders-a-seed,type=env,target=TRELLIS_IDENTITY_SEED
Environment=TRELLIS_URL=https://trellis.example.com
# The service's stop timeout plus a margin.
StopTimeout=35

[Service]
# A build that waits for approval does not start until it is approved.
TimeoutStartSec=15min

[Install]
WantedBy=default.target
```

`Notify=true` runs the container with `podman run --sdnotify=container`, so
the service's `READY=1` reaches systemd and `systemctl --user start orders-a`
returns once the instance serves. No `HealthCmd` is needed.
`orders-b.container` is the same with its own secret. To upgrade from `a` to
`b`, set `Image=` in `orders-b.container`, then:

```sh
systemctl --user daemon-reload
systemctl --user start orders-b   # returns once b serves
systemctl --user stop orders-a    # a finishes its work, hands back the rest
```

The next upgrade goes from `b` to `a`.

For a service run directly by systemd, use `Type=notify`. A hand-written unit
that starts Podman itself needs `NotifyAccess=all` and `KillMode=mixed`, as
Quadlet's generated units have; with `KillMode=control-group`, systemd signals
every process at once and the stop is cut short.

## Kubernetes

Kubernetes has no notification socket; use the HTTP probe:

```yaml
spec:
  strategy:
    type: RollingUpdate
    rollingUpdate: { maxSurge: 1, maxUnavailable: 0 }
  template:
    spec:
      terminationGracePeriodSeconds: 35
      containers:
        - name: orders
          image: ghcr.io/acme/orders:1.4.0
          ports: [{ name: probe, containerPort: 8081 }]
          readinessProbe:
            httpGet: { path: /ready, port: probe }
            periodSeconds: 2
          livenessProbe:
            httpGet: { path: /live, port: probe }
            periodSeconds: 10
```

With `maxUnavailable: 0` the old pod stops only after the new pod is ready, and
`terminationGracePeriodSeconds` leaves room for the stop timeout. Pods of one
Deployment share its secret and so one instance identity; they all serve, but
the CLI and the Console show them as one instance. For one instance per pod,
run two Deployments, each with its own seed, and switch between them.
