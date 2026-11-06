---
title: "Run Trellis in production"
description: "Hostnames, certificates, file locations, Podman and systemd, a single container, reverse proxies, NATS clusters and backups."
---

[Start Trellis](/guides/starting-trellis) runs everything on one machine for
`localhost`. A long-lived installation needs a few more decisions, and most of
them have to be made before you create it.

## Pick your hostnames first

Trellis serves everything over TLS, and its certificates have to name the
hostnames you give `trellis-server init`. Changing them later means a new
installation. A production install usually has two:

- Trellis itself, for example `https://trellis.example.com`. Sign-in lives
  here, it's the address the CLI, services, devices and apps connect to, and
  the Console is part of it at `https://trellis.example.com/console/`.
- NATS, for example `tls://nats.example.com:4222` for services, and a
  WebSocket for browsers

The Console can have a hostname of its own too; see
[The Console on its own hostname](#the-console-on-its-own-hostname).

These `init` flags set them; `--host` changes the hostname in every default at
once:

| Flag                                            | Default                        | Sets                                                                     |
| ----------------------------------------------- | ------------------------------ | ------------------------------------------------------------------------ |
| `--trellis-origin https://trellis.example.com`  | `https://<host>:3000`          | Trellis's address: sign-in, and where clients connect                    |
| `--console-origin https://console.example.com`  | `/console` on Trellis's origin | The Console's own address, if it has one                                 |
| `--nats-server-url tls://nats.example.com:4222` | `tls://<host>:4222`            | Where services connect to NATS                                           |
| `--nats-websocket-url wss://nats.example.com`   | relayed through Trellis's port | Where browsers connect to NATS directly (with `--nats-websocket direct`) |
| `--trellis-port`                                | `3000`                         | Trellis's port, which also serves the Console                            |

## Bring certificates people already trust

A production installation serves certificates every browser and service
already trusts: from a public CA such as Let's Encrypt, or from your
organization's own CA. Get one for the Trellis hostname (and the Console's,
if it has its own) and one for the NATS hostnames, then give them to `init`:

```sh
trellis-server init --tls supplied \
  --tls-certificate-chain trellis.crt --tls-private-key trellis.key \
  --nats-tls-certificate-chain nats.crt --nats-tls-private-key nats.key \
  --trellis-origin https://trellis.example.com \
  --nats-server-url tls://nats.example.com:4222
```

If your organization runs the CA, add `--tls-ca-certificate <its certificate>`
so Trellis trusts it when it connects to NATS. Behind a proxy that terminates
HTTPS, leave out the Trellis pair; see
[Behind your own HTTPS proxy](#behind-your-own-https-proxy).

For a lab or test installation that nobody else depends on, `--tls generated`
makes Trellis create its own CA instead. Browsers and services then accept it
by its fingerprint, as in
[development mode](/guides/running-trellis/other-machines). The CA lives in
the installation's `tls-ca` folder. With `--system` it goes next to the
installation instead, in `/etc/trellis-tls-ca`, so its private key stays out
of anything mounted into a container. `--tls-ca-dir <folder>` puts it anywhere
else outside the installation.

To test the production layout without a real CA,
`trellis-server tls issue --out <folder> --host <name>` writes a certificate
for those hosts (`server.crt`, `server.key`) and the CA that issued it
(`ca.crt`), ready for `--tls supplied`. Trust that `ca.crt` wherever the test
runs.

## Where an installation keeps its files

`trellis-server init` writes four things, and the server reads nothing else
from the installation:

| Path           | What it is                                                     |
| -------------- | -------------------------------------------------------------- |
| `config.toml`  | The settings. No keys, so it's safe to read, share and diff.   |
| `secrets.toml` | Every private key Trellis reads, owner-only. Back it up.       |
| `nats.conf`    | The NATS server's settings and accounts, in NATS's own format. |
| `tls/`         | The certificates the Trellis port and NATS serve.              |

Where the installation goes, and where the server keeps what it writes while
it runs, depends on the kind you ask for:

| `init`          | Installation        | Databases, NATS data, logs                             | Start it with                      |
| --------------- | ------------------- | ------------------------------------------------------ | ---------------------------------- |
| `--user`        | `~/.config/trellis` | `~/.local/share/trellis`, `~/.local/state/trellis`     | `trellis-server`                   |
| `--system`      | `/etc/trellis`      | `/var/lib/trellis`, `/run/trellis`, `/var/log/trellis` | `trellis-server --system`          |
| `init <folder>` | `<folder>`          | `<folder>/var`                                         | `trellis-server --config <folder>` |

`--user` is the default, except for root on Linux, where it's `--system`. A
bundle is a whole installation in one folder, for development, tests and
several Trellis on one machine. Delete its `var` folder and it's back to just
after `init`. Every kind shares the NATS download, in `~/.cache/trellis` or
`/var/cache/trellis`. Naming a bundle and also passing `--user` or `--system`
is an error.

In a container, the installation is one read-only volume at `/etc/trellis` and
everything else one writable volume at `/var/lib/trellis`. To put NATS's data
on another filesystem, mount a third volume at `/var/lib/trellis/nats`.
Trellis logs to standard output. The log of the NATS it runs goes to
`/var/log/trellis`: give that a `tmpfs`, or set `[paths] logs` to a folder on
the data volume.

To keep some of it somewhere else, add `[paths]` to `config.toml`:

```toml
[paths]
data = "/srv/trellis/data"
state = "/srv/trellis/state"
cache = "/srv/trellis/cache"
runtime = "/run/my-trellis"
logs = "/srv/trellis/log"
```

Relative paths resolve from the folder holding `config.toml`. Leave a line out
and the server uses the default for it. Database files you don't name
explicitly get fixed names under the data folder.

### Using the `nats` CLI

`trellis-server nats <user>` runs the [`nats` CLI](https://github.com/nats-io/natscli)
as one of the installation's NATS users, with its address and CA already set:

```sh
trellis-server nats system server report jetstream
trellis-server nats trellis stream ls
trellis-server nats --config ~/trellis-two trellis kv ls
```

`system` sees servers, accounts and connections. `trellis` is the account
services run in, with its streams and buckets. The credentials exist on disk
only while `nats` runs. `--print` writes them to standard output instead, for
a saved `nats context`.

## Who runs NATS

By default `trellis-server` starts the NATS server described by `nats.conf`
and stops it again on exit; `config.toml` says so with `managed = true` in its
`[nats]` section. For NATS you run yourself, in another container or as a
cluster, create the installation with `trellis-server init --external-nats`, or
delete that line. The server then connects to the servers listed in
`[nats] servers` and never starts or stops NATS.

When that NATS runs on another machine, browsers connect to its WebSocket
themselves, because Trellis's hop to the broker's WebSocket is unencrypted and
Trellis only relays to a broker on the same machine. `init` sends browsers to
the broker's host on `--nats-ws-port` (`wss://nats.example.com:8080` for
`--nats-server-url tls://nats.example.com:4222`); pass `--nats-websocket-url`
when browsers reach it at another address.

On a single NATS server Trellis keeps one copy of each stream. Set
`nats.jetstream.replicas` to choose, or leave it out and Trellis picks `3` once
it can see enough JetStream servers to hold three copies.

## Run it under systemd with Podman

`init --quadlet` also writes [Podman Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html)
units into the installation's `quadlet` folder. They run NATS and Trellis as
two rootless containers on a network of their own under systemd, using the
installation folder where it is: read-only, except its `var` folder, where the
databases, logs and NATS data go.

```sh
trellis-server init ~/trellis --quadlet --tls supplied \
  --trusted-proxy 127.0.0.1/32 \
  --nats-tls-certificate-chain nats.crt --nats-tls-private-key nats.key \
  --trellis-origin https://trellis.example.com \
  --nats-server-url tls://nats.example.com:4222 \
  --nats-websocket-url wss://nats.example.com
cp ~/trellis/quadlet/* ~/.config/containers/systemd/
systemctl --user daemon-reload
systemctl --user start trellis
```

The units take the folder's name, so this one writes `trellis.container`,
`trellis-nats.container` and `trellis.network`, and several installations can
share a host. Starting Trellis starts its NATS. On first start Trellis prints a
one-time link for setting the administrator's password, the same as in
[Start Trellis](/guides/starting-trellis#set-the-administrator-password);
`journalctl --user -u trellis` shows it.

For a lab, `trellis-server init ~/lab --quadlet --dev --host lab.example` is
enough.

What `--quadlet` settles for you:

- NATS runs in its own container, so the installation uses
  [external NATS](#who-runs-nats). Trellis reaches it over the network by the
  container's name, `trellis-nats`. A NATS hostname from `--nats-server-url`,
  the one an operator's certificate names, is added as an alias for it. With
  `--tls proxy` that connection is plaintext: the network carries nothing else.
- The images are pinned: Trellis to the version of `trellis-server` that ran
  `init`, NATS to the version Trellis is tested with. Change `Image=` in the
  units to upgrade.
- Every port is published on all of the host's addresses, or the one
  `--bind-address` names, except NATS monitoring, which stays on loopback.
  Whatever sits behind your proxy is published only at `--bind-address`, and
  on loopback without it. With `--nats-websocket-only`, native NATS isn't
  published at all.
- Rootless Podman delivers every published connection from the network's own
  address (`10.89.229.3`, say), whoever sent it, so Trellis can't check which
  proxy connected. Behind a proxy (`--tls proxy` or `--trusted-proxy`), Trellis
  accepts what arrives over its network, and the `--trusted-proxy` range isn't
  checked. Your firewall decides who reaches the published ports: allow only
  the proxy. `init` gives the network a private range of its own, outside every
  `--trusted-proxy` range.
- A generated CA's private key goes beside the folder, in `~/trellis-tls-ca`,
  never inside it, so no container can read it.
  `trellis-server tls renew --config ~/trellis` finds it there.
- The containers run rootless with `keep-id`, mapping you to the runtime's
  user, with a read-only root and no capabilities. Keep the installation
  readable by you alone. Trellis refuses to start on unsafe permissions, and
  loosening a folder with `chmod` to get past that check defeats it.

A proxy in front does the public-facing work for Trellis and the browser
WebSocket. Native NATS clients connect to `nats.example.com:4222` directly.

| Public address                | Forward to              | How                                   |
| ----------------------------- | ----------------------- | ------------------------------------- |
| `https://trellis.example.com` | `http://127.0.0.1:3000` | terminate TLS, keep the `Host` header |
| `wss://nats.example.com`      | `127.0.0.1:8080`        | TLS passthrough (NATS terminates it)  |

Add services through the normal administrator flow, never by handing an
application the runtime's own provider keys.

## A single container

The image can also run NATS inside the same container. Keep the installation
on a read-only volume and the changing state on a writable one. The
[certificates](#bring-certificates-people-already-trust) go in a folder,
`certs` here, holding `trellis.crt` and `trellis.key` for every hostname:

```sh
docker volume create trellis-bundle
docker volume create trellis-state
# Create the installation once, as the image's non-root user:
docker run --rm --user 10001:10001 --entrypoint trellis-server \
  --volume trellis-state:/var/lib/trellis --volume "$PWD/certs":/certs:ro \
  ghcr.io/oats-center/trellis-server:latest \
  init /var/lib/trellis/bundle --tls supplied \
  --tls-certificate-chain /certs/trellis.crt --tls-private-key /certs/trellis.key \
  --nats-tls-certificate-chain /certs/trellis.crt \
  --nats-tls-private-key /certs/trellis.key
# Give the image's user ownership of the state volume:
docker run --rm --user root --entrypoint sh \
  --volume trellis-state:/var/lib/trellis ghcr.io/oats-center/trellis-server:latest \
  -c 'chown -R 10001:10001 /var/lib/trellis'
# Copy the installation into its own volume:
docker run --rm --user root --entrypoint cp \
  --volume trellis-state:/from:ro --volume trellis-bundle:/to ghcr.io/oats-center/trellis-server:latest \
  -a /from/bundle/. /to/
# secrets.toml is readable by its owner only, so the run user must own it:
docker run --rm --user root --entrypoint chown \
  --volume trellis-bundle:/to ghcr.io/oats-center/trellis-server:latest \
  -R 10001:10001 /to
# Run with a read-only root filesystem and a read-only installation:
docker run --rm --name trellis --read-only --tmpfs /tmp \
  --tmpfs /run/trellis:uid=10001,gid=10001,mode=0700 \
  --tmpfs /var/log/trellis:uid=10001,gid=10001,mode=0700 \
  --user 10001:10001 --publish 127.0.0.1:3000:3000 \
  --volume trellis-bundle:/etc/trellis:ro,Z \
  --volume trellis-state:/var/lib/trellis:rw,Z \
  --entrypoint trellis-server ghcr.io/oats-center/trellis-server:latest \
  --system --local-nats=/usr/local/bin/nats-server
```

Trellis reads its config, keys and certificates from `/etc/trellis` and runs
the image's own `nats-server`. Everything that changes lands in
`/var/lib/trellis`: the NATS config it renders, NATS's own data, and Trellis's
SQLite databases. The `init` above uses `localhost` names; add the hostname
flags from [Pick your hostnames first](#pick-your-hostnames-first) for the
names your certificates carry.

## Behind your own HTTPS proxy

Trellis can also run as plain HTTP behind a proxy that
terminates TLS. Create the installation with `--trusted-proxy 10.0.0.2/32` (the
proxy's network) and `--tls supplied` with only the NATS certificate, or
`--tls proxy` when the proxy also serves the NATS WebSocket and holds every
certificate (see [Only HTTPS in public](#only-https-in-public)). Or set it in
`config.toml`:

```toml
[http.transport]
mode = "proxy"
trusted_proxies = ["10.0.0.2/32"]
```

Trellis then answers only connections from those networks, gives everyone else
`403`, and rate-limits clients by the proxy's `X-Forwarded-For` header. The
proxy has to keep the original `Host` header.

You can't run all of Trellis as plain HTTP. Sign-in cookies only work over
HTTPS in a browser, so something has to terminate TLS. NATS's native port
can't sit behind a TLS-terminating proxy; only its WebSocket port can
(`--nats-websocket-behind-proxy`). Native NATS either serves its own TLS or,
with `--tls proxy`, stays plaintext on loopback, where only Trellis reaches it.

For the proxy's health checks, Trellis answers `/healthz` and `/readyz`,
whatever `Host` header the check sends. `/readyz` returns `503` until the
server has finished starting. Every sign-in page answers only its own
hostname.

### Only HTTPS in public

`--nats-websocket-only` keeps native NATS private. Clients are offered only
the WebSocket, and the `trellis` CLI, services and devices all connect over
it, so nothing but HTTPS has to reach the internet. `--nats-server-url` is then
only where Trellis itself reaches NATS, so leave it at its `localhost` default
when Trellis runs NATS.

The WebSocket can share Trellis's hostname: the proxy sends every request that
asks for a WebSocket upgrade to NATS, and everything else to Trellis. For
example, behind an AWS Application Load Balancer holding a certificate for
`*.example.com`, with the instance in a VPC whose range is `172.31.0.0/16`:

```sh
trellis-server init --tls proxy \
  --trusted-proxy 172.31.0.0/16 \
  --trellis-origin https://trellis.example.com \
  --nats-websocket-url wss://trellis.example.com \
  --nats-websocket-behind-proxy \
  --nats-websocket-only
```

The load balancer's HTTPS listener then has these rules, in this order:

| Condition                                                  | Forward to | Health check            |
| ---------------------------------------------------------- | ---------- | ----------------------- |
| Host `trellis.example.com` and header `Upgrade: websocket` | HTTP 8080  | `/`, success code `400` |
| Host `trellis.example.com`                                 | HTTP 3000  | `/readyz`               |

With `--nats-websocket-behind-proxy`, NATS serves its WebSocket as plain HTTP
on every interface, so a load balancer on another machine reaches it. That
port answers a plain request with `400`, which is what its health check
expects; NATS's monitoring port stays on loopback. Raise the load balancer's
idle timeout above its 60-second default, or it cuts quiet NATS connections.
Allow ports 3000 and 8080 only from the load balancer: nothing else may
reach the plaintext WebSocket.

`--tls proxy` says the load balancer holds every certificate, so `init`
generates none. Native NATS listens on the instance's loopback without TLS,
and Trellis reaches it there. The only plaintext on the network is the load
balancer's hop to ports 3000 and 8080. Reading the loopback hop takes a
process on the instance, and that process could read `secrets.toml` anyway.

To run it under Podman, add `--quadlet` and `--bind-address` with the
instance's own address, the one the load balancer forwards to. The Trellis
and WebSocket ports are published only there, native NATS isn't published at
all, and Trellis reaches NATS over the containers' network in plaintext. The
security group above is what admits only the load balancer:

```sh
trellis-server init ~/trellis --quadlet --tls proxy \
  --bind-address 172.31.13.54 \
  --trusted-proxy 172.31.0.0/16 \
  --trellis-origin https://trellis.example.com \
  --nats-websocket-url wss://trellis.example.com \
  --nats-websocket-behind-proxy \
  --nats-websocket-only
```

When Trellis doesn't run the NATS server (`--external-nats`), it can reach it
through the load balancer like any other client:
`--nats-server-url wss://trellis.example.com`. Trellis checks the load
balancer's certificate against the system trust store.

Without a WebSocket rule in the proxy, leave out `--nats-websocket-url` and
`--nats-websocket-behind-proxy`: Trellis's own port then applies the same rule,
handing every WebSocket upgrade to NATS, and the proxy forwards everything for
`trellis.example.com` to port 3000.

A listener that serves its own certificate looks like this in `config.toml`:

```toml
[http]
public_origin = "https://trellis.example.com"

[http.transport]
mode = "tls"
certificate_chain = "./tls/runtime.crt"
private_key = "./tls/runtime.key"
```

## The Console on its own hostname

The Console runs at `/console` on Trellis's origin, beside the sign-in pages.
A bug that let someone run their own script in the Console could then also
act on those pages as whoever has the Console open, for example approving an
app's access request. Give the Console a hostname of its own and the browser
keeps the two apart; the Console then has only what it was granted:

```sh
trellis-server init ... \
  --trellis-origin https://trellis.example.com \
  --console-origin https://console.example.com
```

`console.example.com` is just another name for Trellis's port: point it at the
same port, through your proxy or DNS. Trellis serves the Console there and
sends anyone who opens `/console` on `trellis.example.com` to it. The
certificate has to name both hostnames.

## Without the Console

An installation you run entirely from the `trellis` CLI can leave the Console
out. Add this to `config.toml` and restart:

```toml
[console]
enabled = false
```

Trellis then serves no Console pages, and sign-in turns the Console away even
if something else answers at its address. A service waiting for approval
prints the `trellis svc <deployment-id> approve` command instead of a link.
Remove the setting to bring the Console back.

## Back it up as one piece

Trellis ties its database to the NATS data it was created with. Point a
database at a different or empty NATS store and Trellis refuses to start,
because jobs and stored files would silently go missing. So back up and
restore them together:

- the data folder, including `platform.sqlite` and the `platform.issuer-keys/`
  folder beside it, which holds the sign-in signing keys
- the NATS JetStream data
- the installation folder: `config.toml`, `secrets.toml`, `nats.conf` and
  `tls/`
- with `--tls generated`, the CA folder (`tls-ca/`, or `/etc/trellis-tls-ca/`
  for a `--system` installation), kept separately and never mounted into a
  container

If a signing key in `platform.issuer-keys/` goes missing, restore it from a
backup; Trellis won't make a new one in its place. Recreating NATS streams or
buckets by hand doesn't bring back lost jobs or files either. For a genuinely
fresh start, create a new database and a new NATS store together, then set up
your deployments again.
