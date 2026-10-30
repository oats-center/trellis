---
title: "Start Trellis"
description: "Run trellis-server for the first time: a few questions set up an installation, it starts NATS, and prints the link for setting the admin password."
---

This gets a working Trellis running on your machine in a few minutes: the
server, the NATS broker it uses, and the Console you manage it from. Nothing
runs in a container and nothing needs root.

You need `trellis-server` on your `PATH`. [Install Trellis](/guides/install-trellis)
has a one-line installer.

## Run it

```sh
trellis-server
```

The first time, there's no installation yet (everything Trellis knows about
itself: its settings and keys), so it asks a few questions to set one up.

```text
? Who runs NATS, the message broker every service talks through?
> Trellis starts and stops it
  I run NATS myself
? Where will people and services use it?
> Only on this machine: localhost development over plain HTTP
  From other machines
```

Take the first answer to both. You get a development Trellis that only
listens on `localhost` and uses plain HTTP, so there are no certificates to
deal with. Browsers treat `localhost` as secure, so signing in works the same
as it would over HTTPS.

"From other machines" goes on to ask for ports, hostnames and certificates;
[Reach Trellis from other machines](/guides/running-trellis/other-machines)
covers those answers. Either way, the wizard ends by printing the
`trellis-server init` command that does the same thing without questions.

If you don't have `nats-server` installed, there's one more question:

```text
nats-server is not on your PATH. Download NATS 2.14.4 (checksum-verified) into /home/you/.cache/trellis/nats? [Y/n]
```

To skip the questions, for a script or a fresh VM, run
`trellis-server --init --nats-download`, which creates the localhost
development installation and downloads NATS. Without a terminal to ask on, the
server doesn't guess: it stops and tells you which flag to add.

Leave the terminal open. Ctrl-C stops Trellis and NATS together. Add
`--verbose` to see debug logging and NATS's own output.

## Where everything went

The installation's settings and keys are in `~/.config/trellis`:

| Path           | What it is                                        |
| -------------- | ------------------------------------------------- |
| `config.toml`  | The server's settings                             |
| `auth/`        | Keys and state for signing people and services in |
| `session.seed` | A private key referenced from `config.toml`       |
| `nats/`        | NATS's config, accounts and credentials           |

Everything the server writes while it runs lives in your user's usual
folders: databases and NATS data in `~/.local/share/trellis` and
`~/.local/state/trellis`, logs in `~/.local/state/trellis/log`, and the NATS
download in `~/.cache/trellis`.

## Set the administrator password

The first start creates Trellis's databases and NATS streams, then prints the
one line you need:

```json
{ "adminAccountUrl": "http://localhost:3000/account/reset?token=eUi1Xqst..." }
```

Open it and choose a password for the `admin` account. Trellis never has a
default or temporary password; this link is the only way in on a fresh
installation.

The link works once. Until a password is set, every start prints a new link and
the old ones stop working, so a lost link only costs you a restart.

To set the account up without a browser, start with `--admin-password-stdin`
and pipe the password in. `--admin-username` picks a name other than `admin`.
Both only work on a fresh installation; they never overwrite an existing
administrator's password. `trellis-server init admin --username operator --password-stdin`
does the same without starting the server.

## Open the Console

Go to `http://localhost:3001/console`. Signing in sends you to Trellis's
sign-in page on port 3000 and back again. The Console is an app like any other,
on a port of its own; both are part of `trellis-server`, so there's nothing
else to start.

The CLI logs in to Trellis itself: `trellis login http://localhost:3000`.

These are the ports in use, all on `localhost` only:

| Port   | Who uses it                                 |
| ------ | ------------------------------------------- |
| `3000` | Trellis: sign-in, and where clients connect |
| `3001` | Console                                     |
| `4222` | Services connecting to NATS                 |
| `8080` | Browsers connecting to NATS                 |
| `8222` | NATS monitoring (`/healthz`)                |

If one is taken, create the installation with
`trellis-server init --tls loopback-development` and the
[port flags](/guides/running-trellis/other-machines#the-settings-you-can-choose).

## Check that it's healthy

From another terminal:

```sh
curl http://localhost:3000/healthz
curl http://localhost:3000/readyz
curl http://127.0.0.1:8222/healthz
```

The first two answer with the version and what the server is running:

```json
{ "version": "0.100.0-rc.13", "mode": "all", "processId": 2264453 }
```

`/healthz` answers as soon as the server is up. `/readyz` returns `503` until
startup has finished, then `200`. The third asks NATS directly and should say
`{"status":"ok"}`.

With the server stopped, `trellis-server check` goes further without changing
anything. It starts NATS on its own for a moment, connects with every
configured credential, and checks that the streams, buckets, databases and
signing keys Trellis expects are all there. Run before the very first start, it
tells you what that start is going to create.

## Next

- [Reach Trellis from other machines](/guides/running-trellis/other-machines).
- [Keep it running](/guides/running-trellis/upkeep): password resets, a second
  Trellis on the same machine, and starting over.
- [Write a service](/guides/write-a-service) that runs on it.
