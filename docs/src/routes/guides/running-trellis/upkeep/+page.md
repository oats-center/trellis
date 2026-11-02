---
title: "Keep Trellis running"
description: "Upgrade the server, renew certificates, reset the administrator password, run a second Trellis on one machine, and start over."
---

## Upgrade

```sh
trellis-server upgrade --prerelease
```

Restart the server to run the new version. `trellis-server upgrade --check`
reports whether there is one.

## Renew the certificates

The certificates Trellis generates last 90 days. `trellis-server tls status`
shows when they expire and the CA's fingerprint. Renew well before then:

```sh
trellis-server tls renew
```

That renews the installation the server uses by default; name another one
as you would to start it, for example `trellis-server tls renew --config ~/trellis-two`.
Renewal reuses the CA, so nobody has to
trust anything new. Trellis picks up its new certificate within seconds. NATS
doesn't: restart `trellis-server`, or send NATS `nats-server --signal reload`
if you run it yourself. A timer that runs the renewal needs read access to the
CA folder (`~/.config/trellis/tls-ca/`).

## Reset the administrator password

Start the server once with `--reset-admin`. It prints a reset link that lasts
five minutes and cancels any earlier ones. Resetting keeps the account and
everything it's allowed to do, and signs it out everywhere.

This is the only way to reset the first administrator; other administrators
can't do it from the Console.

## Run a second Trellis on the same machine

Give it its own folder and its own ports:

```sh
trellis-server init --bundle ~/trellis-two --tls loopback-development \
  --trellis-port 3100 \
  --nats-port 4223 --nats-monitor-port 8223 --nats-ws-port 8081
trellis-server --config ~/trellis-two
```

A bundle keeps everything the server writes in its `var/` folder: databases,
NATS data, logs and the pid file. The two installations share nothing but the
NATS download in `~/.cache/trellis`. Deleting `var/` returns a bundle to just
after `init`. To move NATS to other ports for a single run without touching
the installation, `--local-nats-ports=4223,8223,8081` (native, monitoring,
WebSocket) overrides what's in its `nats.conf`.

Never point one installation's database at another's NATS data. Trellis
notices and refuses to start, but it's better not to find out that way.

## Start over

To throw away the default installation and everything it stored, stop the
server and delete its folders:

```sh
rm -rf ~/.config/trellis \
  ~/.local/share/trellis ~/.local/state/trellis ~/.cache/trellis
```

The next `trellis-server` asks how to set up a fresh one. A bundle is one
folder. A `--system` installation keeps its data in `/var/lib/trellis` and its
generated CA in `/etc/trellis-tls-ca`.
