---
title: "Keep Trellis running"
description: "Upgrade the server, keep certificates current, back up, reset the administrator password, use the nats CLI, run a second Trellis on one machine, and start over."
---

## Upgrade

```sh
trellis-server upgrade --prerelease
```

Restart the server to run the new version. `trellis-server upgrade --check`
reports whether there is one.

If the new version changes one of Trellis's own services, the first start
installs the new build under the same identity, and the Console lists it as an
applied update. A stable release always upgrades an installation made by an
earlier stable release.

Release candidates can break each other. When one can't take an installation
made by an earlier candidate, it stops before serving anything and names the
service. The installation is left as it was, so the version you had before
still starts.

## Generated certificates

An installation with `mode = "generated"` serves a certificate Trellis made
from its own CA. Trellis reissues it from the same CA when it has less than 30
days left, checking twice a day, so nobody has to accept anything new. The
listener picks up the new certificate within seconds.
`trellis-server tls status` shows when it expires and the CA's fingerprint.

Certificates you supply (`mode = "tls"`) are yours to renew. Replace the files
in place; Trellis reloads them by itself.

## Back it up as one piece

Trellis ties its database to the NATS data it was created with. Point a
database at a different or empty NATS store and Trellis refuses to start,
because jobs and stored files would silently go missing. Back up and restore
these together:

- the installation folder: `config.toml` and `secrets.toml`, with the NATS
  `system.creds` and `trellis-operator.nk` beside them
- the data folder, including `platform.sqlite` and the `platform.issuer-keys/`
  folder beside it, which holds the sign-in signing keys
- each NATS server's data folder: JetStream and the resolver's accounts

If a signing key in `platform.issuer-keys/` goes missing, restore it from a
backup; Trellis won't make a new one in its place. Recreating NATS streams or
buckets by hand doesn't bring back lost jobs or files either.

## Use the `nats` CLI

`trellis-server nats <user>` runs the [`nats` CLI](https://github.com/nats-io/natscli)
as one of the installation's NATS users, with its address and CA already set:

```sh
trellis-server nats system server report jetstream
trellis-server nats trellis stream ls
trellis-server nats trellis --config ~/trellis-two kv ls
```

`system` sees servers, accounts and connections. `trellis` is the account
services run in, with its streams and buckets. The credentials exist on disk
only while `nats` runs. `--print` writes them to standard output instead, for
a saved `nats context`.

Trellis creates its streams with one copy each, NATS's default, so on a
cluster each stream lives on one server. `[leases] replicas` sets the copies of
the lease bucket.

The parts of Trellis that must run once (Auth, jobs, health and events) each
hold a lease in that bucket while they run. A server that loses a lease, for
example because NATS couldn't renew it in time on a loaded machine, stops that
work but keeps running. While it waits, `/readyz` reports it unready. It starts
the work again as soon as the lease is free. A second server for the same NATS
waits this way too, and takes over if the first one stops.

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
