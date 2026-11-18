---
title: "Keep Trellis running"
description: "Upgrade the server, keep certificates current, know what a failure costs, back up and restore, reset the administrator password, use the nats CLI, run a second Trellis on one machine, and start over."
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

## Disk use

The Console's Health page lists how much disk each kind of Trellis data takes,
how many records it holds, how old the oldest one is, and how long it's kept.
The same numbers go to your metrics as `trellis.storage.bytes`,
`trellis.storage.records` and `trellis.storage.oldest.age`.

Trellis removes old data on its own. These settings in `config.toml` change how
long it keeps it:

```toml
[events]
history_retention_days = 90      # the event history the Console searches
dead_letter_retention_days = 90  # failed deliveries after they're resolved or dismissed

[jobs]
history_retention_days = 30      # finished jobs; failed and dead jobs stay

[health]
history_retention_days = 30
transport_retention_hours = 24
transport_max_bytes = 1073741824
```

A change takes effect at the next start, and a shorter time removes what it no
longer covers. Failed deliveries nobody has dealt with are kept however old
they are, because a retry needs the original event. `dead_letter_retention_days`
can't be less than 7, the time the event stream itself keeps events.

## What a failure costs

The default installation is one Trellis server and one NATS server, and
Trellis creates each stream with one copy. Its state lives in two places: the
SQLite databases in the data folder, and JetStream on the NATS server, which
holds jobs, events, KV buckets and stored files.

- Trellis crashes, is killed or restarts: nothing is lost. SQLite commits
  before Trellis answers, and a job or file is in JetStream before Trellis
  reports it stored. A job that was running runs again.
- The NATS process restarts: nothing is lost, and services reconnect by
  themselves.
- The machine loses power or its kernel crashes: SQLite loses nothing it
  committed. JetStream flushes to disk every two minutes by default, so writes
  from the last two minutes can be gone. `sync_interval: always` in the
  `jetstream` block of the NATS config flushes every write, at a cost in
  throughput.
- The disk is lost, or a database or the JetStream store is corrupted:
  restore the latest backup. Everything since that backup is gone.

Leases decide which server runs Auth, jobs, health and events. They don't move
data. A second server that takes over a role needs the same NATS, and for Auth
the same platform database, `platform.issuer-keys/` and `secrets.toml`. It
can't take over from a dead server's disk. Put those on storage both servers
reach, or restore the latest backup on the standby before you start it.

Trellis ties its database to the NATS data it was created with. Point a
database at a different or empty NATS store and Trellis refuses to start,
because jobs and stored files would silently go missing. If a signing key in
`platform.issuer-keys/` goes missing, restore it from a backup; Trellis won't
make a new one in its place. Recreating NATS streams or buckets by hand
doesn't bring back lost jobs or files either.

## Back up

```sh
trellis-server backup /backups/trellis-$(date +%F)
```

The server can keep running. NATS has to be reachable, so back up a `--dev`
installation while it runs: its NATS runs only while Trellis does. Name the
installation with `--config`, `--user` or `--system` as for every command. The
folder must be new or empty, and only its owner can read it, since it holds
every key the installation has.

A backup holds:

- `config.toml`, `secrets.toml`, `nats.conf` when there is one, and every file
  the config names: the NATS `system.creds` and operator signing key, the NATS
  CA, and the certificate and key of `tls` transport
- a copy of each database (`platform`, `jobs`, `health`, `events`) taken with
  SQLite's online backup, so a database the server is writing is copied as
  one consistent state
- `platform.issuer-keys/` and the generated TLS in the data folder's `tls/`
- a snapshot of every JetStream stream in Trellis's NATS account, consumers
  included, except the lease bucket
- `manifest.json`, written last, with the versions, the installation's ID,
  the JetStream identity, the time, and a SHA-256 for every file

The databases are copied first and the streams after them, so the streams hold
everything the databases refer to. A folder without `manifest.json` is a
backup that didn't finish.

The NATS server's own config file, such as a `nats-1.conf` that
`trellis-server nats new` wrote, is not in the backup. It holds the operator
and no data. Keep it with the NATS host's other config.

Run a backup at least daily, and before every upgrade. What you can lose is
the time since the last one. Keep the backups off the machine they came from.

## Restore

Stop the server, then restore into a fresh folder, or over the same
installation:

```sh
trellis-server restore /backups/trellis-2026-11-18 --config /srv/trellis
```

`restore` checks every file against the manifest before it writes anything.
It refuses a folder that holds an installation with another ID, and a file the
config names outside the folder (a certificate, say) that exists with other
contents. `--force` replaces both. Files outside the installation folder go
back to the paths they had.

The streams wait in the data folder's `restore/` until the next start. Give
that start a NATS whose JetStream store is empty: a new NATS host, or the old
one with its data folder removed. The first start of the platform role (or
`all`) loads every stream before anything else runs, and refuses a NATS that
already has one of them. A start that stops partway finishes on the next one.
Start the platform role before any split role, which would otherwise create
its streams empty.

After loading, the Jobs, Health and Events projections replay their streams
from the start, so they catch up on whatever reached the streams after their
databases were copied. Leases start over.

Test a restore. Restore a recent backup on a spare machine or folder, start
it, and sign in. Trellis's test suite runs this drill on every change: it
backs up a running installation, deletes its folder and its broker's store,
restores, and checks that a user signs in with the same password, that a
session from before the backup refreshes, that a finished job is still listed,
and that a stored file reads back byte for byte.

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
example because NATS couldn't renew it in time on a loaded machine, stops only
that part and keeps the rest running. It starts the part again as soon as the
lease is free. A second server for the same NATS waits this way too, and takes
over whichever parts the first one stops running, given the state those parts
need (see [What a failure costs](#what-a-failure-costs)). `/readyz` reports a server
unready while Auth runs elsewhere, since Auth's pages and sign-in are what its
listener serves.

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
