---
title: "Start Trellis"
description: "Create an installation, run Trellis with NATS on your own machine, set the admin password and sign in to the Console."
---

This gets a working Trellis running on your machine in a few minutes: the
server, the NATS broker it uses, and the Console you manage it from. Nothing
runs in a container and nothing needs root.

You need `trellis-server` on your `PATH` (see
[Install Trellis](/guides/install-trellis)). The `trellis` CLI is optional
here, but you'll want it soon after; it's covered in
[Install the Trellis CLI](/guides/install-trellis-cli).

## Create an installation

An installation is one folder holding everything Trellis knows about itself:
its config, its keys, its certificates and, once it runs, its data. Create one
in `~/trellis`:

```sh
trellis-server init --out ~/trellis
```

That sets Trellis up for `localhost`. If other machines will connect to it,
stop here and read [Reach Trellis from other machines](#reach-trellis-from-other-machines)
first, because the hostnames get baked into the certificates.

Inside the folder you'll find:

| Path           | What it is                                                     |
| -------------- | -------------------------------------------------------------- |
| `config.toml`  | The server's settings; paths in it are relative to this folder |
| `auth/`        | Keys and state for signing people and services in              |
| `session.seed` | A private key referenced from `config.toml`                    |
| `nats/`        | NATS's config, accounts and credentials                        |
| `nats/tls/`    | The certificate NATS presents                                  |
| `tls/`         | The certificate Console and sign-in present, plus `ca.crt`     |

Once the server has run, `data`, `state`, `cache`, `runtime` and `logs`
folders appear next to those. Delete the folder and the installation is gone.

Trellis uses TLS on every connection, even locally, so `init` also made a small
certificate authority (CA) of its own and used it to issue those certificates.
The CA can only sign for the hostnames you gave it. Its private key went to
`~/trellis-tls-ca/`, deliberately outside the installation folder. Back that
folder up and don't copy it anywhere Trellis runs.

The last lines `init` printed include the CA's fingerprint:

```text
warning: this installation uses Trellis-generated TLS from its own private CA.
  Browsers and native clients must trust /home/you/trellis/tls/ca.crt explicitly.
  Verify this SHA-256 fingerprint out of band before trusting it:
  E6:E8:DC:C7:78:1B:08:A8:BE:8F:95:39:4E:67:0A:B4:12:DE:ED:21:92:1D:90:87:63:C1:37:E7:5F:76:55:3C
```

Keep it handy. You'll compare against it in a minute.

## Start the server

```sh
trellis-server --dev --nats-download --config ~/trellis/config.toml
```

`--dev` tells `trellis-server` to run NATS itself, as a child process, rather
than connect to a NATS server you run. `--nats-download` fetches the NATS
release Trellis is tested with into `~/trellis/cache`; drop it if `nats-server`
is already on your `PATH`.

Leave this terminal open. Press Ctrl-C and Trellis stops NATS along with
itself. NATS writes its log to `~/trellis/logs`; add `--verbose` to see it, and
Trellis's debug logging, in the terminal too.

Two warnings will scroll by on every start. One says the installation uses its
generated CA, which is expected until you
[bring your own certificates](/guides/install-trellis#behind-your-own-https-proxy).
The other is a `SECURITY WARNING` that sign-in cookies can reach other servers
on the same hostname. On your own machine that's fine: Console and sign-in
both live on `localhost`, just on different ports. A real deployment puts
sign-in on a hostname of its own, and the warning goes away.

With the default settings, these are the ports in use:

| Port   | Who uses it                               |
| ------ | ----------------------------------------- |
| `3000` | Console, and browsers' connection to NATS |
| `3001` | The sign-in page                          |
| `4222` | Services connecting to NATS               |
| `8080` | NATS's WebSocket port                     |
| `8222` | NATS monitoring (`/healthz`), plain HTTP  |

On the first start Trellis creates its databases and NATS streams, then prints
one line you need:

```json
{ "adminAccountUrl": "https://localhost:3001/account/reset?token=eUi1Xqst..." }
```

That's the link for setting the administrator's password. Don't open it yet;
your browser doesn't trust Trellis's certificate so far.

## Trust Trellis's certificate

Open `~/trellis/tls/ca.crt` in your operating system's or browser's certificate
manager and check that its SHA-256 fingerprint matches what `init` printed.
Then import it as a trusted authority. One import covers Console, the sign-in
page and the browser's connection to NATS. Clicking past the browser's
certificate warning instead isn't supported.

Other programs need the CA too:

- The CLI asks for you. `trellis login https://localhost:3001` shows the CA's
  fingerprint, asks whether to trust it, and remembers the answer for this
  installation. If the CA ever changes, the CLI refuses to connect. Pass
  `--ca-fingerprint <sha256>` to skip the question, or
  `--ca-certificate ~/trellis/tls/ca.crt` to trust the file directly.
- Node services: start them with `NODE_EXTRA_CA_CERTS=~/trellis/tls/ca.crt`.
- Rust services: put the path in `RuntimeAuthorizationTrust.root_certificates`,
  or pass it to `discover_native_oauth`.

## Set the administrator password

Now open the `adminAccountUrl` link and choose a password for the `admin`
account. Trellis never has a default or temporary password; this link is the
only way in on a fresh installation.

The link works once. Until a password is set, every start prints a new link and
the old ones stop working, so a lost link only costs you a restart.

If you'd rather set the account up without a browser, start the server with
`--admin-password-stdin` and pipe the password in, or seed it before the first
start:

```sh
trellis-server init admin --config ~/trellis/config.toml --username operator --password-stdin
```

`--admin-username` (or `--username` with `init admin`) picks a name other than
`admin`. These only work on a fresh installation; they never overwrite an
existing administrator's password. An installation run with `--system` needs
`--system` on `init admin` as well.

## Open the Console

Go to `https://localhost:3000/console`. Signing in sends you to the sign-in
page on port 3001 and back again. The Console and the sign-in page are both
part of `trellis-server`, so there's nothing else to start.

## Check that it's healthy

From another terminal:

```sh
curl --cacert ~/trellis/tls/ca.crt https://localhost:3000/healthz
curl --cacert ~/trellis/tls/ca.crt https://localhost:3000/readyz
curl http://127.0.0.1:8222/healthz
```

The first two answer with the version and what the server is running:

```json
{ "version": "0.100.0-rc.12", "mode": "all", "processId": 2264453 }
```

`/healthz` answers as soon as the server is up. `/readyz` returns `503` until
startup has finished, then `200`. The third command asks NATS directly and
should say `{"status":"ok"}`.

For a deeper look that changes nothing, run:

```sh
trellis-server check --config ~/trellis/config.toml
```

It reads the whole config, connects to NATS with every configured credential,
and checks that the streams, buckets, databases and signing keys Trellis
expects are all there. It doesn't open any ports or write anything; database
migrations are tried on temporary copies. Run it before a first start and it
tells you what the first start is going to create.

## Reach Trellis from other machines

Everything above works on `localhost`. For anyone else to connect, decide on
the hostnames first and give them to `init`, since the certificates name
exactly those hosts (plus `localhost` and `127.0.0.1` for NATS, so a server on
the same machine can still reach it). On a Tailscale tailnet where the machine
is `eta.oats`, that looks like:

```sh
trellis-server init --out ~/trellis \
  --public-origin https://eta.oats:3000 \
  --oauth-origin https://eta.oats:3001 \
  --nats-server-url tls://eta.oats:4222
```

A tailnet gives you no subdomains, so sign-in stays on `eta.oats` with its own
port. Browsers reach NATS through the Console's address (`wss://eta.oats:3000/nats`)
unless you say otherwise, so there's no WebSocket hostname to set. To serve
somewhere `init` didn't name, make a new installation for that hostname.

The flags you're most likely to need:

| Flag                                                   | Default                     | Use it when                                                                                |
| ------------------------------------------------------ | --------------------------- | ------------------------------------------------------------------------------------------ |
| `--public-origin https://eta.oats:3000`                | `https://localhost:3000`    | People open the Console at another address                                                 |
| `--oauth-origin https://eta.oats:3001`                 | `https://localhost:3001`    | The sign-in page lives at another address                                                  |
| `--nats-server-url tls://eta.oats:4222`                | `tls://localhost:4222`      | Services connect to NATS at another address                                                |
| `--nats-websocket-url wss://eta.oats:8080`             | relayed through the Console | Browsers should connect to NATS's WebSocket port directly (with `--nats-websocket direct`) |
| `--trellis-port`, `--oauth-port`                       | `3000`, `3001`              | Those ports are taken                                                                      |
| `--nats-port`, `--nats-monitor-port`, `--nats-ws-port` | `4222`, `8222`, `8080`      | Those ports are taken                                                                      |
| `--bind-address 100.64.0.7`                            | all interfaces              | Console and sign-in should listen on one address only                                      |
| `--name "Lab Trellis"`                                 | `Trellis`                   | You want a different display name                                                          |
| `--tls loopback-development`                           | `generated`                 | You want plain HTTP, which only works when everything stays on `localhost`                 |

`--bind-address` only covers Console and sign-in. NATS listens on every
interface whenever TLS is on. `init` refuses port `0`, the same port used
twice, and a NATS port that clashes with `--trellis-port`. Production setups
with a proxy in front, or your own certificates, are covered in
[Install Trellis](/guides/install-trellis#behind-your-own-https-proxy).

## Keep it running

### Renew the certificates

The certificates `init` issued last 90 days. `trellis-server tls status --config ~/trellis/config.toml`
shows when they expire and the CA's fingerprint. Renew well before then:

```sh
trellis-server tls renew --bundle ~/trellis
```

Renewal reuses the CA, so nobody has to trust anything new. Trellis picks up
its new certificate within seconds. NATS doesn't: restart Trellis, or send NATS
`nats-server --signal reload` if you run it yourself. A timer that runs the
renewal needs read access to `~/trellis-tls-ca/`.

### Reset the administrator password

Start the server once with `--reset-admin`. It prints a reset link that lasts
five minutes and cancels any earlier ones. Resetting keeps the account and
everything it's allowed to do, and signs it out everywhere. This is the only
way to reset that first administrator; other administrators can't do it from
the Console, which is the point. It still works when other administrators
exist.

### Run a second Trellis on the same machine

Give it its own folder and its own ports at `init`:

```sh
trellis-server init --out ~/trellis-two --trellis-port 3100 --oauth-port 3101 \
  --nats-port 4223 --nats-monitor-port 8223 --nats-ws-port 8081
trellis-server --dev --nats-download --config ~/trellis-two/config.toml
```

Never point one installation's database at another's NATS data. Trellis
notices and refuses to start, but it's better not to find out that way. To
move NATS to other ports for a single run without touching the installation,
`--local-nats-ports=4223,8223,8081` (native, monitoring, WebSocket) overrides
what's in `nats/nats.conf`.

## Brand the sign-in page

The sign-in and consent page can carry your name, logo and colours, for the
whole installation and for individual apps. Edit it in the Console under
**Platform → Sign-in page**, or with the CLI:

```sh
trellis theme set \
  --name "Acme Cloud" \
  --logo ./branding/logo.svg \
  --stylesheet ./branding/portal.css \
  --color primary=#0050b3 --color primary-hover=#003f8f \
  --dark-color primary=#4d8fe0 \
  --copy "sign_in_title=Sign in to Acme" \
  --copy "footer=Need help? support@acme.example"

# Overrides for one app, layered over the installation theme:
trellis theme set --client trellis.console --name "Acme Console" \
  --logo ./branding/console.svg

trellis theme show --client trellis.console
trellis theme remove --client trellis.console
```

`set` replaces the whole theme, so anything you leave out is cleared. Logos can
be PNG, JPEG, WebP or SVG up to 256 KiB, and the stylesheet (installation theme
only) up to 64 KiB; it loads after the page's own. Colours take hex values for
`page`, `surface`, `ink`, `muted`, `line`, `line-strong`, `primary`,
`primary-hover`, `on-primary` and `soft`. The text you can replace is
`sign_in_title`, `sign_in_subtitle` and `footer`.

A theme changes how the page looks, never what it does. Sign-in stays on its
own address, consent shows exactly what Trellis asks it to, and apps never see
a token. The page picks an app's theme from its own record of the sign-in in
progress rather than from the link, so a crafted link can't dress one app up as
another, and theme files are served so that even an SVG can't run script.
Changing the installation theme takes permission to manage the installation;
an app's theme takes permission to manage apps. Changes apply immediately.
