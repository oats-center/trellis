---
title: "Start Trellis"
description: "Run a development Trellis with trellis-server --dev: it creates the installation, runs NATS for you, and prints the link for setting the admin password."
---

This gets a working Trellis running on your machine in a few minutes: the
server, the NATS server it uses, and the Console you manage it from. Nothing
runs in a container and nothing needs root.

You need `trellis-server` on your `PATH`. [Install Trellis](/guides/install-trellis)
has a one-line installer.

## Run it

```sh
trellis-server --dev
```

The first time, there's no development installation yet, so Trellis creates
one: its settings, its keys, and a NATS server it starts and stops itself.
It only listens on `localhost` and uses plain HTTP, so there are no
certificates to deal with. Browsers treat `localhost` as secure, so signing
in works the same as it would over HTTPS.

A development installation is the only kind where Trellis runs NATS. For
anything other people depend on, you run NATS and write the configuration
yourself; [Run Trellis in production](/guides/running-trellis/production)
walks through it.

If you don't have `nats-server` installed, Trellis asks first:

```text
nats-server is not on your PATH. Download NATS 2.14.4 (checksum-verified) into /home/you/.cache/trellis/nats? [Y/n]
```

For a script or a fresh VM, `trellis-server --dev --nats-download` answers
yes up front. Without a terminal to ask on, the server doesn't guess: it stops
and tells you which flag to add.

Leave the terminal open. Ctrl-C stops Trellis and NATS together. Add
`--verbose` to see debug logging and NATS's own output.

## Where everything went

The installation is one folder, `~/.local/share/trellis/dev`:

| Path                | What it is                                                    |
| ------------------- | ------------------------------------------------------------- |
| `config.toml`       | The server's settings                                         |
| `secrets.toml`      | Every private key Trellis created. Only you can read it.      |
| `nats.conf`         | The NATS server's settings and its operator                   |
| `nats/system.creds` | The NATS system user, which Trellis manages accounts with     |
| `nats/operator.nk`  | The NATS operator signing key Trellis signs its accounts with |
| `var/`              | Databases, NATS data, logs, and certificates it generated     |

Only the NATS download lives elsewhere, in `~/.cache/trellis`, shared by every
installation. `--config <folder>` keeps a development installation somewhere
else; `trellis-server --dev --config ~/trellis-two` creates a second one.

## Set the administrator password

The first start creates Trellis's databases and NATS accounts, then prints the
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
administrator's password.
`trellis-server admin --config ~/.local/share/trellis/dev --username operator --password-stdin`
does the same without starting the server.

## Open the Console

Go to `http://localhost:3000/console`. Signing in takes you to Trellis's
sign-in page and back again. The Console is part of `trellis-server`, so
there's nothing else to start.

The CLI logs in to Trellis itself: `trellis login http://localhost:3000`.

These are the ports in use, all on `localhost` only:

| Port   | Who uses it                                                     |
| ------ | --------------------------------------------------------------- |
| `3000` | Trellis: sign-in, the Console, and NATS WebSockets for browsers |
| `4222` | Services connecting to NATS                                     |
| `8080` | NATS's WebSocket listener, which only port 3000 forwards to     |
| `8222` | NATS monitoring (`/healthz`)                                    |

If one is taken, change `port` under `[http]` in `config.toml`, and the
`listen`, `http` and WebSocket `listen` lines in `nats.conf`. They're plain
text, meant to be edited.

## Check that it's healthy

From another terminal:

```sh
curl http://localhost:3000/healthz
curl http://localhost:3000/readyz
curl http://127.0.0.1:8222/healthz
```

The first two answer with the version and what the server is running:

```json
{ "version": "0.100.0-rc.25", "mode": "all", "processId": 2264453 }
```

`/healthz` answers as soon as the server is up. `/readyz` returns `503` until
startup has finished, then `200`. The third asks NATS directly and should say
`{"status":"ok"}`.

With the server stopped, `trellis-server check --dev` goes further without
changing anything. It starts NATS on its own for a moment, connects with every
configured credential, and checks that the streams, buckets, databases and
signing keys Trellis expects are all there.

## Next

- [Reach Trellis from other machines](/guides/running-trellis/other-machines).
- [Keep it running](/guides/running-trellis/upkeep): password resets and
  starting over.
- [Write a service](/guides/write-a-service) that runs on it.
