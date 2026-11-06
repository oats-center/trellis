---
title: "Keep Trellis running"
description: "Upgrade the server, keep certificates current, reset the administrator password, run a second Trellis on one machine, and start over."
---

## Upgrade

```sh
trellis-server upgrade --prerelease
```

Restart the server to run the new version. `trellis-server upgrade --check`
reports whether there is one.

If the new version changes one of Trellis's own services, the first start
installs the new build under the same identity, and the Console lists it as an
applied update. A version whose changes the installation can't take stops
before serving anything and names the service. The installation is left as it
was, so starting the version you had before still works.

## Generated certificates

An installation with `mode = "generated"` serves a certificate Trellis made
from its own CA. Trellis reissues it from the same CA when it has less than 30
days left, checking twice a day, so nobody has to accept anything new. The
listener picks up the new certificate within seconds.
`trellis-server tls status` shows when it expires and the CA's fingerprint.

Certificates you supply (`mode = "tls"`) are yours to renew. Replace the files
in place; Trellis reloads them by itself.

## Reset the administrator password

Start the server once with `--reset-admin`. It prints a reset link that lasts
five minutes and cancels any earlier ones. Resetting keeps the account and
everything it's allowed to do, and signs it out everywhere.

This is the only way to reset the first administrator; other administrators
can't do it from the Console.

## Run a second Trellis on the same machine

Give it its own folder, then its own ports:

```sh
trellis-server --dev --config ~/trellis-two
```

The first start creates the folder with the usual ports, so if the first
installation is running it stops at the port that's taken. Edit `port` under
`[http]` and the ports in `nats.conf`, then start it again. For one run,
`--local-nats-ports=4223,8223,8081` (native, monitoring, WebSocket) moves NATS
without touching the installation.

A folder installation keeps everything the server writes in its `var/` folder:
databases, NATS data, logs and the pid file. Two installations share nothing
but the NATS download in `~/.cache/trellis`.

Never point one installation's database at another's NATS data. Trellis
notices and refuses to start, but it's better not to find out that way.

## Start over

To throw away the development installation and everything it stored, stop
the server and delete its folder:

```sh
rm -rf ~/.local/share/trellis/dev
```

The next `trellis-server --dev` creates a fresh one. Any other installation is
the folder holding its `config.toml`, `secrets.toml` and `var/`. A `--system`
installation keeps its data in `/var/lib/trellis` instead. Deleting a
production installation's `secrets.toml` loses its keys, and every session and
service registration with them; its NATS accounts stay on the NATS servers
until the operator removes them.
