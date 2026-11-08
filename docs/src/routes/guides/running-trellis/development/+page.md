---
title: "Run a development Trellis"
description: "trellis-server --dev creates an installation, runs NATS for you, and prints the link for setting the admin password. Add --host to reach it from other machines."
---

```sh
trellis-server --dev
```

That's the whole setup. The first run creates a development installation:
its settings, its keys, and a NATS server it starts and stops itself. It
listens on `localhost` only, over plain HTTP, so there are no certificates to
deal with. Browsers treat `localhost` as secure, so sign-in works the same as
it would over HTTPS. Nothing runs in a container and nothing needs root.

You need `trellis-server` on your `PATH`;
[Install Trellis](/guides/install-trellis) has a one-line installer.

A development installation is the only kind where Trellis runs NATS. For
anything other people depend on, you run NATS yourself:
[from the binaries](/guides/running-trellis/binaries) or
[with Podman](/guides/running-trellis/podman).

## NATS

If `nats-server` isn't on your `PATH`, Trellis asks before downloading it:

```text
nats-server is not on your PATH. Download NATS 2.14.4 (checksum-verified) into /home/you/.cache/trellis/nats? [Y/n]
```

`--nats-download` says yes up front, for a script or a fresh VM. Without a
terminal to ask on, the server doesn't guess: it stops and names the flag to
add. `--local-nats=/path/to/nats-server` runs one exact file instead.

Leave the terminal open. Ctrl-C stops Trellis and NATS together. `--verbose`
shows debug logging and NATS's own output.

## Set the administrator password

The first start prints one line you need:

```json
{ "adminAccountUrl": "http://localhost:3000/account/reset?token=eUi1Xqst..." }
```

Open it and choose a password for the `admin` account. Trellis never has a
default or temporary password; this link is the only way in on a fresh
installation. It works once. Until a password is set, every start prints a new
link and the old ones stop working, so a lost link costs you a restart.

To skip the browser, pipe the password into the very first start with
`--admin-password-stdin`. `--admin-username` picks a name other than `admin`.
Both only work on a fresh installation and never overwrite an existing
administrator's password.

## Open the Console

Go to `http://localhost:3000/console`. Signing in takes you to Trellis's
sign-in page and back. The Console is part of `trellis-server`, so there's
nothing else to start. The CLI logs in with `trellis login http://localhost:3000`.

These are the ports in use, all on `localhost`:

| Port   | Who uses it                                                     |
| ------ | --------------------------------------------------------------- |
| `3000` | Trellis: sign-in, the Console, and NATS WebSockets for browsers |
| `4222` | Services connecting to NATS                                     |
| `8080` | NATS's WebSocket listener, which only port 3000 forwards to     |
| `8222` | NATS monitoring (`/healthz`)                                    |

If one is taken, the first start stops at it. Change `port` under `[http]` in
`config.toml`, or the `listen`, `http` and WebSocket `listen` lines in
`nats.conf`, and start again. `--local-nats-ports=4223,8223,8081` (native,
monitoring, WebSocket) moves NATS for one run without editing anything.

## Check that it's healthy

From another terminal:

```sh
curl http://localhost:3000/healthz
curl http://localhost:3000/readyz
curl http://127.0.0.1:8222/healthz
```

The first two answer with the version and what the server is running:

```json
{ "version": "0.100.0-rc.27", "mode": "all", "processId": 2264453 }
```

`/healthz` answers as soon as the server is up. `/readyz` returns `503` until
startup has finished, then `200`. The third asks NATS directly and should say
`{"status":"ok"}`.

With the server stopped, `trellis-server check --dev` goes further without
changing anything. It starts NATS on its own for a moment, connects with every
configured credential, and checks that the streams, buckets, databases and
signing keys Trellis expects are all there.

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
installation. `config.toml` and `nats.conf` are plain text, written to be
edited; `trellis-server config example` prints every setting with a comment
saying what it does.

`--config <folder>` keeps a development installation somewhere else.
`trellis-server --dev --config ~/trellis-two` creates a second one beside the
first, as long as you give it other ports.

## Reach it from other machines

A `localhost` installation can't be reached from anywhere else. For other
machines, create one for the hostname they'll use. On a Tailscale tailnet
where the machine is `eta.oats`:

```sh
trellis-server --dev --host eta.oats --config ~/trellis-eta
```

Trellis listens on `https://eta.oats:3000` for sign-in, the Console at
`/console`, and NATS: it hands every request that asks for a WebSocket to the
NATS server it runs. Native NATS stays on this machine's loopback, so other
machines connect over the WebSocket.

The certificate comes from a CA Trellis made, kept in `var/tls/` and renewed
by Trellis itself. The first time a browser opens the Console, it warns that it
doesn't know the issuer. Check that the issuer's SHA-256 fingerprint matches
the one `trellis-server tls status --config ~/trellis-eta` prints (Trellis logs
it as it starts too), then accept it. Nothing gets installed on the other
machine. The Console and sign-in pages show a banner while the installation
serves its own certificate.

`trellis login https://eta.oats:3000` shows the same fingerprint, asks, and
remembers the answer. Services and devices do the same on their first
connection when started from a terminal. Under systemd or in a container
there's nobody to ask, so set `TRELLIS_CERTIFICATE_FINGERPRINT` to the
fingerprint, or `TRELLIS_ACCEPT_SELF_SIGNED=1` to take whatever certificate it
finds that first time. A different certificate later is refused.

`--host` only matters when the installation is created. Later starts read
`config.toml`, so it's `trellis-server --dev --config ~/trellis-eta` from then
on. To rename an existing one, edit `public_origin` under `[http]`; Trellis
makes a new CA for the new name on its next start, and clients accept it
again.

A certificate Trellis made itself is fine for people you trust on your own
network. Past that, run Trellis
[from the binaries](/guides/running-trellis/binaries) or
[with Podman](/guides/running-trellis/podman), with a certificate browsers
already trust.

## Next

- [Keep it running](/guides/running-trellis/upkeep): upgrades, password
  resets and starting over.
- [Write a service](/guides/write-a-service) that runs on it.
