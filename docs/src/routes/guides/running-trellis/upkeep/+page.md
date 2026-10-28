---
title: "Keep Trellis running"
description: "Renew certificates, reset the administrator password, run a second Trellis on one machine, and start over."
---

## Renew the certificates

The certificates Trellis generates last 90 days. `trellis-server tls status`
shows when they expire and the CA's fingerprint. Renew well before then:

```sh
trellis-server tls renew --bundle ~/.config/trellis
```

`--bundle` is the installation folder. Renewal reuses the CA, so nobody has to
trust anything new. Trellis picks up its new certificate within seconds. NATS
doesn't: restart `trellis-server`, or send NATS `nats-server --signal reload`
if you run it yourself. A timer that runs the renewal needs read access to the
CA folder (`~/.config/trellis-tls-ca/`).

## Reset the administrator password

Start the server once with `--reset-admin`. It prints a reset link that lasts
five minutes and cancels any earlier ones. Resetting keeps the account and
everything it's allowed to do, and signs it out everywhere.

This is the only way to reset the first administrator. Other administrators
can't do it from the Console, which is the point, but `--reset-admin` still
works when they exist.

## Run a second Trellis on the same machine

Give it its own folder and its own ports:

```sh
trellis-server init --out ~/trellis-two --trellis-port 3100 --oauth-port 3101 \
  --nats-port 4223 --nats-monitor-port 8223 --nats-ws-port 8081
trellis-server --config ~/trellis-two/config.toml
```

An installation made with `--out` keeps its data, logs and NATS files inside
its folder too, so the two never share anything. To move NATS to other ports
for a single run without touching the installation,
`--local-nats-ports=4223,8223,8081` (native, monitoring, WebSocket) overrides
what's in `nats/nats.conf`.

Never point one installation's database at another's NATS data. Trellis
notices and refuses to start, but it's better not to find out that way.

## Start over

To throw away the default installation and everything it stored, stop the
server and delete its folders:

```sh
rm -rf ~/.config/trellis ~/.config/trellis-tls-ca \
  ~/.local/share/trellis ~/.local/state/trellis ~/.cache/trellis
```

The next `trellis-server` offers to create a fresh one. An installation made
with `--out` is one folder, plus its `<folder>-tls-ca` next to it.
