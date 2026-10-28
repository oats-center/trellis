---
title: "Start Trellis"
description: "Run trellis-server for the first time: it creates an installation, starts NATS, and prints the link for setting the admin password."
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

The first time, it has two questions.

```text
There is no Trellis installation in /home/you/.config/trellis yet. Create one for this machine (localhost)? [Y/n]
```

An installation is everything Trellis knows about itself: its settings, its
keys and its certificates. Say yes and it's created for `localhost`. If other
machines are going to connect, say no and read
[Reach Trellis from other machines](/guides/running-trellis/other-machines)
first, because the hostnames get written into the certificates.

```text
nats-server is not on your PATH. Download NATS 2.14.4 (checksum-verified) into /home/you/.cache/trellis/nats? [Y/n]
```

[NATS](https://nats.io) is the message broker every Trellis service talks
through, and `trellis-server` runs it for you. If you've installed `nats-server`
yourself you won't see this question.

To skip both questions, for a script or a fresh VM, run
`trellis-server --init --nats-download`. Without a terminal to ask on, the
server doesn't guess: it stops and tells you which flag to add.

Leave the terminal open. Ctrl-C stops Trellis and NATS together. Add
`--verbose` to see debug logging and NATS's own output.

## Where everything went

The installation's settings, keys and certificates are in `~/.config/trellis`:

| Path           | What it is                                                 |
| -------------- | ---------------------------------------------------------- |
| `config.toml`  | The server's settings                                      |
| `auth/`        | Keys and state for signing people and services in          |
| `session.seed` | A private key referenced from `config.toml`                |
| `nats/`        | NATS's config, accounts, credentials and certificate       |
| `tls/`         | The certificate Console and sign-in present, plus `ca.crt` |

Everything the server writes while it runs lives in your user's usual
folders: databases and NATS data in `~/.local/share/trellis` and
`~/.local/state/trellis`, logs in `~/.local/state/trellis/log`, and the NATS
download in `~/.cache/trellis`.

Trellis uses TLS on every connection, even on `localhost`, so creating the
installation also made a small certificate authority (CA) of its own. The CA
can only sign for the hostnames it was created with. Its private key is in
`~/.config/trellis/tls-ca/`. Back it up and keep it private.

When the installation was created, Trellis printed the CA's fingerprint:

```text
warning: this installation uses Trellis-generated TLS from its own private CA.
  Browsers and native clients must trust /home/you/.config/trellis/tls/ca.crt explicitly.
  Verify this SHA-256 fingerprint out of band before trusting it:
  E6:E8:DC:C7:78:1B:08:A8:BE:8F:95:39:4E:67:0A:B4:12:DE:ED:21:92:1D:90:87:63:C1:37:E7:5F:76:55:3C
```

Keep it handy. You'll compare against it in a minute.

## What the log tells you

Two warnings show up on every start. One says the installation uses its
generated CA, which stays true until you
[bring your own certificates](/guides/running-trellis/production#behind-your-own-https-proxy).
The other is a `SECURITY WARNING` that sign-in cookies can reach other servers
on the same hostname. On your own machine that's fine: Console and sign-in
both live on `localhost`, just on different ports. A real deployment gives
sign-in a hostname of its own, and the warning goes away.

The first start creates Trellis's databases and NATS streams, then prints the
one line you need:

```json
{ "adminAccountUrl": "https://localhost:3001/account/reset?token=eUi1Xqst..." }
```

That's the link for setting the administrator's password. Don't open it yet;
your browser doesn't trust Trellis's certificate so far.

These are the ports in use:

| Port   | Who uses it                               |
| ------ | ----------------------------------------- |
| `3000` | Console, and browsers' connection to NATS |
| `3001` | The sign-in page                          |
| `4222` | Services connecting to NATS               |
| `8080` | NATS's WebSocket port                     |
| `8222` | NATS monitoring (`/healthz`), plain HTTP  |

If one is taken, [choose others](/guides/running-trellis/other-machines#the-settings-you-can-choose)
when you create the installation.

## Trust Trellis's certificate

Open `~/.config/trellis/tls/ca.crt` in your operating system's or browser's
certificate manager and check that its SHA-256 fingerprint matches what Trellis
printed. Then import it as a trusted authority. One import covers Console, the
sign-in page and the browser's connection to NATS. Clicking past the browser's
certificate warning instead isn't supported.

Other programs need the CA too:

- The CLI asks for you. `trellis login https://localhost:3001` shows the CA's
  fingerprint, asks whether to trust it, and remembers the answer for this
  installation. If the CA ever changes, the CLI refuses to connect. Pass
  `--ca-fingerprint <sha256>` to skip the question, or
  `--ca-certificate ~/.config/trellis/tls/ca.crt` to trust the file directly.
- Node services: start them with `NODE_EXTRA_CA_CERTS=~/.config/trellis/tls/ca.crt`.
- Rust services: put the path in `RuntimeAuthorizationTrust.root_certificates`,
  or pass it to `discover_native_oauth`.

## Set the administrator password

Now open the `adminAccountUrl` link and choose a password for the `admin`
account. Trellis never has a default or temporary password; this link is the
only way in on a fresh installation.

The link works once. Until a password is set, every start prints a new link and
the old ones stop working, so a lost link only costs you a restart.

To set the account up without a browser, start with `--admin-password-stdin`
and pipe the password in. `--admin-username` picks a name other than `admin`.
Both only work on a fresh installation; they never overwrite an existing
administrator's password. `trellis-server init admin --username operator --password-stdin`
does the same without starting the server.

## Open the Console

Go to `https://localhost:3000/console`. Signing in sends you to the sign-in
page on port 3001 and back again. The Console and the sign-in page are both
part of `trellis-server`, so there's nothing else to start.

## Check that it's healthy

From another terminal:

```sh
curl --cacert ~/.config/trellis/tls/ca.crt https://localhost:3000/healthz
curl --cacert ~/.config/trellis/tls/ca.crt https://localhost:3000/readyz
curl http://127.0.0.1:8222/healthz
```

The first two answer with the version and what the server is running:

```json
{ "version": "0.100.0-rc.12", "mode": "all", "processId": 2264453 }
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

- [Install the Trellis CLI](/guides/install-trellis-cli) and log in with it.
- [Reach Trellis from other machines](/guides/running-trellis/other-machines).
- [Keep it running](/guides/running-trellis/upkeep): certificate renewal,
  password resets, and a second Trellis on the same machine.
- [Write a service](/guides/write-a-service) that runs on it.
