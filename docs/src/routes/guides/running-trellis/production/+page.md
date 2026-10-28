---
title: "Run Trellis in production"
description: "Hostnames, file locations, Podman and systemd, a single container, reverse proxies, your own certificates, NATS clusters and backups."
---

[Start Trellis](/guides/starting-trellis) runs everything on one machine for
`localhost`. A long-lived installation needs a few more decisions, and most of
them have to be made before you create it.

## Pick your hostnames first

Trellis serves everything over TLS, and the certificates it generates name
exactly the hostnames you give `trellis-server init`. Changing them later means
a new installation. A production install usually has three:

- the Console, for example `https://console.example.com`
- the sign-in (OAuth) page, on a hostname of its own such as
  `https://auth.example.com`, so login cookies never reach anything else
- NATS, for example `tls://nats.example.com:4222` for services and
  `wss://nats.example.com` for browsers

[Reach Trellis from other machines](/guides/running-trellis/other-machines#the-settings-you-can-choose)
lists the `init` flags that set each one.

## Where an installation keeps its files

`trellis-server init` with no `--out` creates the installation where the server
looks by default: `~/.config/trellis`, or `/etc/trellis` with `--system`. Its
data then goes to the profile's folders: your XDG folders, or `/var/lib/trellis`,
`/var/cache/trellis`, `/run/trellis` and `/var/log/trellis` with `--system`.

`trellis-server init --out <folder>` makes a self-contained installation
instead. Its `config.toml` gets a `[paths]` section that keeps databases, NATS
data, logs and caches in folders next to it, so one folder is one
installation and several can share a host. Start it with
`trellis-server --config <folder>/config.toml`. Point any of those folders
somewhere else by editing `[paths]`:

```toml
[paths]
data = "/srv/trellis/data"
state = "/srv/trellis/state"
cache = "/srv/trellis/cache"
runtime = "/run/my-trellis"
logs = "/srv/trellis/log"
```

Relative paths resolve from the folder holding `config.toml`. Leave a line out
and the server uses the profile's folder for it. Database files you don't name
explicitly get fixed names under the data folder.

The installation's CA lives in its `tls-ca` folder. With `--system` it goes
next to the installation instead, in `<folder>-tls-ca`, so the CA's private key
stays out of anything mounted into a container. `--tls-ca-dir <folder>` puts it
anywhere else outside the installation.

## Who runs NATS

By default `trellis-server` starts the NATS server described by `nats/nats.conf`
and stops it again on exit; `config.toml` says so with `managed = true` in its
`[nats]` section. For NATS you run yourself, in another container or as a
cluster, create the installation with `trellis-server init --external-nats`, or
delete that line. The server then connects to the servers listed in
`[nats] servers` and never starts or stops NATS.

On a single NATS server Trellis keeps one copy of each stream. Set
`nats.jetstream.replicas` to choose, or leave it out and Trellis picks `3` once
it can see enough JetStream servers to hold three copies.

## Run it under systemd with Podman

The repository's `deploy/quadlet` folder has Podman Quadlet units that run
NATS and Trellis as two rootless containers managed by systemd. Create a
private installation first, with your real hostnames:

```sh
install -d -m 0700 "$HOME/trellis" "$HOME/trellis/data"
trellis-server init --out "$HOME/trellis/config" --external-nats \
  --tls-ca-dir "$HOME/trellis/tls-ca" \
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

The image can also run NATS inside the same container. Keep the installation
on a read-only volume and the changing state on a writable one:

```sh
docker volume create trellis-bundle
docker volume create trellis-state
# Create the installation once, as the image's non-root user:
docker run --rm --user 10001:10001 --entrypoint trellis-server \
  --volume trellis-state:/var/lib/trellis ghcr.io/oats-center/trellis-server:latest \
  init --system --out /var/lib/trellis/bundle
# Give the image's user ownership of the state volume:
docker run --rm --user root --entrypoint sh \
  --volume trellis-state:/var/lib/trellis ghcr.io/oats-center/trellis-server:latest \
  -c 'chown -R 10001:10001 /var/lib/trellis'
# Copy the installation into its own volume:
docker run --rm --user root --entrypoint cp \
  --volume trellis-state:/from:ro --volume trellis-bundle:/to ghcr.io/oats-center/trellis-server:latest \
  -a /from/bundle/. /to/
# Its keys are readable by their owner only, so the run user must own them:
docker run --rm --user root --entrypoint chown \
  --volume trellis-bundle:/to ghcr.io/oats-center/trellis-server:latest \
  -R 10001:10001 /to
# Run with a read-only root filesystem and a read-only installation:
docker run --rm --name trellis --read-only --tmpfs /tmp \
  --tmpfs /run/trellis:uid=10001,gid=10001,mode=0700 \
  --tmpfs /var/log/trellis:uid=10001,gid=10001,mode=0700 \
  --user 10001:10001 --publish 127.0.0.1:3000:3000 --publish 127.0.0.1:3001:3001 \
  --volume trellis-bundle:/etc/trellis:ro,Z \
  --volume trellis-state:/var/lib/trellis:rw,Z \
  --entrypoint trellis-server ghcr.io/oats-center/trellis-server:latest \
  --system all --local-nats=/usr/local/bin/nats-server
```

Trellis reads its config, keys and certificates from `/etc/trellis` and runs
the image's own `nats-server`. Everything that changes lands in
`/var/lib/trellis`: the NATS config it renders, NATS's own data, and Trellis's
SQLite databases. The `init` above uses `localhost` names; add the hostname
flags from [Pick your hostnames first](#pick-your-hostnames-first) before you
rely on it.

## Behind your own HTTPS proxy

Console and the sign-in page can also run as plain HTTP behind a proxy that
terminates TLS. Create the installation with `--trusted-proxy 10.0.0.2/32` (the
proxy's network), or set it in `config.toml`:

```toml
[http.transport]
mode = "proxy"
trusted_proxies = ["10.0.0.2/32"]
```

Trellis then answers only connections from those networks, gives everyone else
`403`, and rate-limits clients by the proxy's `X-Forwarded-For` header. The
proxy has to keep the original `Host` header. `[auth.oauth.transport]` takes the same
settings for the sign-in listener.

You can't run all of Trellis as plain HTTP. Sign-in cookies only work over
HTTPS in a browser, so something has to terminate TLS. NATS's native port
always serves its own TLS and can't sit behind a TLS-terminating proxy; only
its WebSocket port can (`--nats-websocket-behind-proxy`).

To bring your own certificates instead of the generated ones, create the
installation with `--tls supplied` and pass `--tls-certificate-chain`,
`--tls-private-key`, `--nats-tls-certificate-chain` and
`--nats-tls-private-key`, plus `--tls-ca-certificate` when your issuer is
private. A listener with its own certificate looks like this in `config.toml`:

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

## Back it up as one piece

Trellis ties its database to the NATS data it was created with. Point a
database at a different or empty NATS store and Trellis refuses to start,
because jobs and stored files would silently go missing. So back up and
restore them together:

- the data folder, including `platform.sqlite` and the `platform.issuer-keys/`
  folder beside it, which holds the sign-in signing keys
- the NATS JetStream data
- the installation folder: `config.toml`, `session.seed`, `auth/`,
  `live-providers/`, `nats/` and `tls/`
- the CA folder (`tls-ca/`, or `<name>-tls-ca/` next to a `--system`
  installation), kept separately
  and never mounted into a container

If a signing key in `platform.issuer-keys/` goes missing, restore it from a
backup; Trellis won't make a new one in its place. Recreating NATS streams or
buckets by hand doesn't bring back lost jobs or files either. For a genuinely
fresh start, create a new database and a new NATS store together, then set up
your deployments again.
