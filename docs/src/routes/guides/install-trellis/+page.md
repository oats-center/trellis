---
title: "Install Trellis"
description: "Get the trellis-server program onto a machine, as a download, a container image, or a source build, and set it up for production."
---

Trellis runs as one program, `trellis-server`. That single binary holds the
Trellis runtime, the Console (the admin web app) and the sign-in page people
see when they log in. There is no separate web server or database to install.

The one thing it leans on is [NATS](https://nats.io), the message broker that
every Trellis service talks through. For a first install you don't need to set
NATS up yourself. `trellis-server` can start and stop a NATS server for you.

This page gets the program onto your machine. Once it's there,
[Start Trellis](/guides/starting-trellis) walks you through the first run.

## Download the server

Linux on x86_64 is the only platform with a ready-made download today. This
installs the newest release into `~/.local/bin`:

```sh
curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis-server
```

The script picks the newest stable release, or the newest prerelease while
there's no stable one yet, and checks the download against the release's
checksum before installing it. Set `TRELLIS_VERSION=0.100.0-rc.12` for a
particular release, or `TRELLIS_INSTALL_DIR` for another folder. Put the
variable after the pipe, as in `| TRELLIS_VERSION=0.100.0-rc.12 sh -s trellis-server`,
so `sh` sees it. To upgrade, run the same line again; the server has no
update command of its own.

To do the same by hand: each
[GitHub release](https://github.com/oats-center/trellis/releases) carries a
`trellis-server` archive and a checksum file for it. Set `version` to the
release you want, then download, check and unpack:

```sh
version=0.100.0-rc.12
target=x86_64-unknown-linux-gnu
base=https://github.com/oats-center/trellis/releases/download/v$version
curl -LO "$base/trellis-server-$version-$target.tar.gz"
curl -LO "$base/checksum-$version-$target-trellis-server.sha256"
sha256sum -c "checksum-$version-$target-trellis-server.sha256"
tar xzf "trellis-server-$version-$target.tar.gz"
```

Stop if `sha256sum` prints anything other than `OK`. The archive holds a single
file, `trellis-server`. Put it on your `PATH`:

```sh
install -m 755 trellis-server ~/.local/bin/trellis-server
trellis-server --version
```

You'll want the `trellis` command-line tool as well, for logging in and
managing the installation from a terminal.
It installs the same way:

```sh
curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis
```

[Install the Trellis CLI](/guides/install-trellis-cli) has the other ways to
get it.

## Get NATS (or let Trellis fetch it)

`trellis-server` can run NATS in one of two ways. If `nats-server` is already
on your `PATH` (from your package manager or the
[NATS install docs](https://docs.nats.io/running-a-nats-service/introduction/installation)),
it uses that one. If not, add `--nats-download` when you start Trellis and it
downloads the NATS release Trellis was tested with, checks it against a pinned
checksum, and keeps it in the installation's `cache` folder. It re-checks that
copy on every start, so a corrupted or swapped binary gets replaced rather than
run.

For a NATS binary you've vetted yourself, `--local-nats=/path/to/nats-server`
uses exactly that file. Trellis doesn't verify it against its pin, which is the
point. It does refuse a file that isn't a regular executable owned by root or
by you, and one where the file or any parent folder is writable by group or
others.

## Or use the container image

Every release is also published as `ghcr.io/oats-center/trellis-server`. The
image's entry point is `trellis-server`, and it already contains the `trellis`
CLI and a checksum-verified `nats-server` at `/usr/local/bin/nats-server`.
[Run it as a single container](#a-single-container) below shows a full
example.

## Or build it from source

Building from a checkout is how you work on Trellis itself, or try something
that isn't released yet. [In-repo development](/guides/in-repo-development)
has the steps; the short version is `cargo xtask install`,
`npm run browser:embedded`, then `cargo build -p trellis-server`.

---

Everything below is for a long-lived installation. For trying Trellis out,
skip to [Start Trellis](/guides/starting-trellis).

## Pick your hostnames first

Trellis serves everything over TLS, and the certificates it generates name
exactly the hostnames you give `trellis-server init`. Choose the names people
and services will really use before you initialize, because changing them later
means a new installation. A production install usually has three:

- the Console, for example `https://console.example.com`
- the sign-in (OAuth) page, on a hostname of its own such as
  `https://auth.example.com`, so login cookies never reach anything else
- NATS, for example `tls://nats.example.com:4222` for services and
  `wss://nats.example.com` for browsers

[Start Trellis](/guides/starting-trellis#reach-trellis-from-other-machines)
lists the `init` flags that set each one.

## Where an installation keeps its files

`trellis-server init` writes an installation folder. Its `config.toml` has a
`[paths]` section that keeps everything the server writes (databases, NATS
data, logs, caches) in folders next to it, so one folder is one installation
and several can share a host. Point any of them somewhere else by editing
`[paths]`:

```toml
[paths]
data = "/srv/trellis/data"
state = "/srv/trellis/state"
cache = "/srv/trellis/cache"
runtime = "/run/my-trellis"
logs = "/srv/trellis/log"
```

Relative paths resolve from the folder holding `config.toml`. Leave a line out
and the server falls back to its profile: your XDG folders normally, or
`/var/lib/trellis`, `/var/cache/trellis`, `/run/trellis` and `/var/log/trellis`
when it runs with `--system`. `trellis-server --system init` writes no
`[paths]` at all, for an installation folder that's mounted read-only and run
with `--system`. Database files you don't name explicitly get fixed names
under the data folder.

## Run it under systemd with Podman

The repository's `deploy/quadlet` folder has Podman Quadlet units that run
NATS and Trellis as two rootless containers managed by systemd. Generate a
private installation folder first, with your real hostnames:

```sh
install -d -m 0700 "$HOME/trellis" "$HOME/trellis/data"
trellis-server init --out "$HOME/trellis/config" \
  --public-origin https://console.example.com \
  --oauth-origin https://auth.example.com \
  --nats-server-url tls://nats.example.com:4222 \
  --nats-websocket-url wss://nats.example.com
```

Then adjust the generated `config.toml` using `deploy/quadlet/config.example.toml`
as a guide: copy over its paths, origins and listener settings, and put your
generated installation ID where the example has a placeholder. The example is
a set of settings to apply, not an installation of its own. Never copy another
installation's identity or private files.

A few things about the units:

- `trellis.container` runs `ghcr.io/oats-center/trellis-server:latest`, the
  newest release. For a server that only changes when you decide, set
  `Image=` to a version tag such as `v0.100.0-rc.12`.
- In `trellis-nats.container`, replace `nats.example.com` in `NetworkAlias`
  with your NATS hostname, so Trellis checks the same broker certificate from
  inside the network as clients do from outside. That container only mounts
  `nats/tls/`, never the sign-in keys or the CA's private key.
- They expect rootless Podman with `keep-id`, mapping your user to the
  runtime's UID 10001. Keep private folders readable by you alone. Trellis
  refuses to start on unsafe permissions, and loosening a folder with `chmod`
  to get past that check defeats it.
- Trellis gets `/run/trellis` from systemd's `RuntimeDirectory=trellis`.

Copy the units into `~/.config/containers/systemd/`, run
`systemctl --user daemon-reload`, and start `trellis-nats.service` and
`trellis.service`. On first start Trellis prints a one-time link for setting
the administrator's password, the same as in
[Start Trellis](/guides/starting-trellis#set-the-administrator-password).

The containers publish to loopback only, so a proxy in front does the
public-facing work:

| Public address                | Forward to              | How                                     |
| ----------------------------- | ----------------------- | --------------------------------------- |
| `https://console.example.com` | `http://127.0.0.1:3000` | terminate TLS, keep the `Host` header   |
| `https://auth.example.com`    | `127.0.0.1:3001`        | TLS passthrough (Trellis terminates it) |
| `nats.example.com:4222`       | `127.0.0.1:4222`        | TLS passthrough (NATS terminates it)    |
| `wss://nats.example.com`      | `127.0.0.1:8080`        | TLS passthrough (NATS terminates it)    |

NATS monitoring on port 8222 stays on loopback. The example trusts the fixed
`trellis.network` subnet as the proxy. If your proxy connects from somewhere
else, Trellis refuses the request and logs the peer's address; add it to
`[http.transport] trusted_proxies`.

When you restart, reuse the same installation folder and data folder. Add
services through the normal administrator flow, never by handing an application
the runtime's own provider keys.

## A single container

The image can also run NATS inside the same container. Override the entry
point, keep the installation folder on a read-only volume and the changing
state on a writable one:

```sh
docker volume create trellis-bundle
docker volume create trellis-state
# Generate the installation folder once, as the image's non-root user:
docker run --rm --user 10001:10001 --entrypoint trellis-server \
  --volume trellis-state:/var/lib/trellis ghcr.io/oats-center/trellis-server:latest \
  --system init --out /var/lib/trellis/bundle
# Give the image's user ownership of the state volume:
docker run --rm --user root --entrypoint sh \
  --volume trellis-state:/var/lib/trellis ghcr.io/oats-center/trellis-server:latest \
  -c 'chown -R 10001:10001 /var/lib/trellis'
# Copy the installation folder into its own volume:
docker run --rm --user root --entrypoint cp \
  --volume trellis-state:/from:ro --volume trellis-bundle:/to ghcr.io/oats-center/trellis-server:latest \
  -a /from/bundle/. /to/
# Its keys are readable by their owner only, so the run user must own them:
docker run --rm --user root --entrypoint chown \
  --volume trellis-bundle:/to ghcr.io/oats-center/trellis-server:latest \
  -R 10001:10001 /to
# Run with a read-only root filesystem and read-only installation folder:
docker run --rm --name trellis --read-only --tmpfs /tmp \
  --tmpfs /run/trellis:uid=10001,gid=10001,mode=0700 \
  --tmpfs /var/log/trellis:uid=10001,gid=10001,mode=0700 \
  --user 10001:10001 --publish 127.0.0.1:3000:3000 --publish 127.0.0.1:3001:3001 \
  --volume trellis-bundle:/etc/trellis:ro,Z \
  --volume trellis-state:/var/lib/trellis:rw,Z \
  --entrypoint trellis-server ghcr.io/oats-center/trellis-server:latest \
  --system all --local-nats=/usr/local/bin/nats-server
```

Trellis reads its config, keys and certificates from `/etc/trellis`. Everything
that changes lands in `/var/lib/trellis`: the NATS config it renders, NATS's
own data, and Trellis's SQLite databases. The `init` above uses `localhost`
names; add the hostname flags from
[Pick your hostnames first](#pick-your-hostnames-first) before you rely on it.

## Behind your own HTTPS proxy

Console and the sign-in page can also run as plain HTTP behind a proxy that
terminates TLS. Initialize with `--trusted-proxy 10.0.0.2/32` (the proxy's
network), or set it in `config.toml`:

```toml
[http.transport]
mode = "proxy"
trusted_proxies = ["10.0.0.2/32"]
```

Trellis then answers only connections from those networks, gives everyone else
`403`, and rate-limits clients by the proxy's `X-Forwarded-For` header. The
proxy has to keep the original `Host` header. Caddy does that already; nginx
needs `proxy_set_header Host $host;`. `[auth.oauth.transport]` takes the same
settings for the sign-in listener.

You can't run all of Trellis as plain HTTP. Sign-in cookies only work over
HTTPS in a browser, so something has to terminate TLS. NATS's native port
always serves its own TLS and can't sit behind a TLS-terminating proxy; only
its WebSocket port can (`--nats-websocket-behind-proxy`).

To bring your own certificates instead of the generated ones, initialize with
`--tls supplied` and pass `--tls-certificate-chain`, `--tls-private-key`,
`--nats-tls-certificate-chain` and `--nats-tls-private-key`, plus
`--tls-ca-certificate` when your issuer is private. A listener with its own
certificate looks like this in `config.toml`:

```toml
[http]
public_origin = "https://trellis.example.com"

[http.transport]
mode = "tls"
certificate_chain = "./tls/runtime.crt"
private_key = "./tls/runtime.key"
```

`[http] origins` lists extra browser origins allowed to talk to Trellis, such
as an app you're developing on another port. It never changes how Console or
the sign-in page are served.

## NATS clusters

On a single NATS server Trellis keeps one copy of each stream. Set
`nats.jetstream.replicas` in `config.toml` to choose, or leave it out and
Trellis picks `3` once it can see enough JetStream servers to hold three
copies. The Quadlet example is one server with one copy; it isn't a cluster.

Without `--dev` or one of the `--local-nats` options, `trellis-server` never
starts NATS. It connects to the servers listed under `[nats] servers` in
`config.toml`.

## Back it up as one piece

Trellis ties its database to the NATS data it was created with. Point a
database at a different or empty NATS store and Trellis refuses to start,
because jobs and stored objects would silently go missing. So back up and
restore them together:

- the data folder, including `platform.sqlite` and the `platform.issuer-keys/`
  folder beside it, which holds the sign-in signing keys
- the NATS JetStream data
- the installation folder: `config.toml`, `session.seed`, `auth/`,
  `live-providers/`, `nats/` and `tls/`
- the CA folder next to the installation folder (`<name>-tls-ca/`), kept
  separately and never mounted into a container

If a signing key in `platform.issuer-keys/` goes missing, restore it from a
backup; Trellis won't make a new one in its place. Recreating NATS streams or
buckets by hand doesn't bring back lost jobs or files either. For a genuinely
fresh start, create a new database and a new NATS store together, then set up
your deployments again.
