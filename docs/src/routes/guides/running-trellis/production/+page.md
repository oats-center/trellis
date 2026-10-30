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
installation. A production install usually has three:

- Trellis itself, for example `https://trellis.example.com`. Sign-in lives
  here, and it's the address the CLI, services, devices and apps connect to.
- the Console, on a hostname of its own such as `https://console.example.com`.
  It's an app like any other, so it never shares sign-in's origin.
- NATS, for example `tls://nats.example.com:4222` for services, and a
  WebSocket for browsers

These `init` flags set them; `--host` changes the hostname in every default at
once:

| Flag                                            | Default                        | Sets                                                                     |
| ----------------------------------------------- | ------------------------------ | ------------------------------------------------------------------------ |
| `--trellis-origin https://trellis.example.com`  | `https://<host>:3000`          | Trellis's address: sign-in, and where clients connect                    |
| `--console-origin https://console.example.com`  | `https://<host>:3001`          | The Console's address                                                    |
| `--nats-server-url tls://nats.example.com:4222` | `tls://<host>:4222`            | Where services connect to NATS                                           |
| `--nats-websocket-url wss://nats.example.com`   | relayed through Trellis's port | Where browsers connect to NATS directly (with `--nats-websocket direct`) |
| `--trellis-port`, `--console-port`              | `3000`, `3001`                 | Trellis's and the Console's ports                                        |

## Bring certificates people already trust

A production installation serves certificates every browser and service
already trusts: from a public CA such as Let's Encrypt, or from your
organization's own CA. Get one for the Trellis and Console hostnames and one
for the NATS hostnames, then give them to `init`:

```sh
trellis-server init --tls supplied \
  --tls-certificate-chain trellis.crt --tls-private-key trellis.key \
  --nats-tls-certificate-chain nats.crt --nats-tls-private-key nats.key \
  --trellis-origin https://trellis.example.com \
  --console-origin https://console.example.com \
  --nats-server-url tls://nats.example.com:4222
```

If your organization runs the CA, add `--tls-ca-certificate <its certificate>`
so Trellis trusts it when it connects to NATS. Behind a proxy that terminates
HTTPS, leave out the Trellis and Console pair; see
[Behind your own HTTPS proxy](#behind-your-own-https-proxy).

For a lab or test installation that nobody else depends on, `--tls generated`
makes Trellis create its own CA instead. Browsers and services then accept it
by its fingerprint, as in
[development mode](/guides/running-trellis/other-machines). The CA lives in
the installation's `tls-ca` folder. With `--system` it goes next to the
installation instead, in `<folder>-tls-ca`, so its private key stays out of
anything mounted into a container. `--tls-ca-dir <folder>` puts it anywhere
else outside the installation.

To test the production layout without a real CA,
`trellis-server tls issue --out <folder> --host <name>` writes a certificate
for those hosts (`server.crt`, `server.key`) and the CA that issued it
(`ca.crt`), ready for `--tls supplied`. Trust that `ca.crt` wherever the test
runs.

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
private installation first, with your real hostnames and
[certificates](#bring-certificates-people-already-trust):

```sh
install -d -m 0700 "$HOME/trellis" "$HOME/trellis/data"
trellis-server init --out "$HOME/trellis/config" --external-nats \
  --tls supplied --trusted-proxy 10.89.70.0/24 \
  --nats-tls-certificate-chain nats.crt --nats-tls-private-key nats.key \
  --trellis-origin https://trellis.example.com \
  --console-origin https://console.example.com \
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
  `nats/tls/`, never the sign-in keys.
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

| Public address                | Forward to              | How                                   |
| ----------------------------- | ----------------------- | ------------------------------------- |
| `https://trellis.example.com` | `http://127.0.0.1:3000` | terminate TLS, keep the `Host` header |
| `https://console.example.com` | `http://127.0.0.1:3001` | terminate TLS, keep the `Host` header |
| `nats.example.com:4222`       | `127.0.0.1:4222`        | TLS passthrough (NATS terminates it)  |
| `wss://nats.example.com`      | `127.0.0.1:8080`        | TLS passthrough (NATS terminates it)  |

NATS monitoring on port 8222 stays on loopback. The example trusts the fixed
`trellis.network` subnet as the proxy. If your proxy connects from somewhere
else, Trellis refuses the request and logs the peer's address; add it to
`trusted_proxies` under `[http.transport]` and `[console.transport]`.

When you restart, reuse the same installation folder and data folder. Add
services through the normal administrator flow, never by handing an application
the runtime's own provider keys.

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
  init --system --out /var/lib/trellis/bundle --tls supplied \
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
flags from [Pick your hostnames first](#pick-your-hostnames-first) for the
names your certificates carry.

## Behind your own HTTPS proxy

Trellis and the Console can also run as plain HTTP behind a proxy that
terminates TLS. Create the installation with `--trusted-proxy 10.0.0.2/32` (the
proxy's network) and `--tls supplied` with only the NATS certificate, or set it
in `config.toml`:

```toml
[http.transport]
mode = "proxy"
trusted_proxies = ["10.0.0.2/32"]
```

Trellis then answers only connections from those networks, gives everyone else
`403`, and rate-limits clients by the proxy's `X-Forwarded-For` header. The
proxy has to keep the original `Host` header. `[console.transport]` takes the
same settings for the Console's port.

You can't run all of Trellis as plain HTTP. Sign-in cookies only work over
HTTPS in a browser, so something has to terminate TLS. NATS's native port
always serves its own TLS and can't sit behind a TLS-terminating proxy; only
its WebSocket port can (`--nats-websocket-behind-proxy`).

For the proxy's health checks, both ports answer `/healthz` and `/readyz`,
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
trellis-server init --tls generated \
  --trusted-proxy 172.31.0.0/16 \
  --trellis-origin https://trellis.example.com \
  --console-origin https://console.example.com \
  --nats-websocket-url wss://trellis.example.com \
  --nats-websocket-behind-proxy \
  --nats-websocket-only
```

The load balancer's HTTPS listener then has these rules, in this order:

| Condition                                                  | Forward to | Health check            |
| ---------------------------------------------------------- | ---------- | ----------------------- |
| Host `trellis.example.com` and header `Upgrade: websocket` | HTTP 8080  | `/healthz` on port 8222 |
| Host `trellis.example.com`                                 | HTTP 3000  | `/readyz`               |
| Host `console.example.com`                                 | HTTP 3001  | `/readyz`               |

NATS's WebSocket port doesn't answer plain requests, so its health check asks
NATS's monitoring port instead. Raise the load balancer's idle timeout above
its 60-second default, or it cuts quiet NATS connections. Allow ports 3000,
3001, 8080 and 8222 only from the load balancer. Here `--tls generated` only
covers Trellis's own connection to NATS on the instance; people only ever see
the load balancer's certificate.

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
- with `--tls generated`, the CA folder (`tls-ca/`, or `<name>-tls-ca/` next
  to a `--system` installation), kept separately and never mounted into a
  container

If a signing key in `platform.issuer-keys/` goes missing, restore it from a
backup; Trellis won't make a new one in its place. Recreating NATS streams or
buckets by hand doesn't bring back lost jobs or files either. For a genuinely
fresh start, create a new database and a new NATS store together, then set up
your deployments again.
