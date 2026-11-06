---
title: "Run Trellis in production"
description: "Set up NATS with an operator, write config.toml, and run Trellis with Podman and systemd, in containers, or behind an HTTPS proxy."
---

[Start Trellis](/guides/starting-trellis) runs a development Trellis that
starts its own NATS. A production Trellis doesn't. You run NATS, give Trellis
two files from the NATS operator, and write one `config.toml`. Trellis creates
everything else on its first start.

## Set up NATS

NATS servers belong to an operator. Trellis needs two things from it:

- the system account's user, as a `.creds` file. Trellis uses it to send its
  accounts to the servers and to close connections it revokes.
- an operator signing key, as an `.nk` seed file. Trellis signs its own two
  accounts with it.

The servers need a `full` resolver, which accepts account updates from the
system account and keeps them on disk.

### The quick way: `trellis-server nats new`

For a new cluster, `nats new` makes the operator and every server's config:

```sh
trellis-server nats new --servers 3 --out ~/trellis-nats
```

That writes `nats-1.conf`, `nats-2.conf` and `nats-3.conf`, the system user in
`system.creds`, and a signing key for Trellis in `trellis-operator.nk`. The
three servers listen on `127.0.0.1` with client ports 4222 to 4224, routes on
6222 to 6224, WebSockets on 8080 to 8082 and monitoring on 8222 to 8224. Their
JetStream data goes in `~/trellis-nats/data`. Start each with
`nats-server -c ~/trellis-nats/nats-1.conf`, and so on.

JetStream keeps a stream's copies on different servers, so a cluster needs at
least three; NATS 2.14 won't run a cluster of one. For a single server,
`--servers 1` writes one `nats-1.conf` with no cluster at all.

Other options:

| Option                                    | Use it when                                                      |
| ----------------------------------------- | ---------------------------------------------------------------- |
| `--hosts nats-1,nats-2,nats-3`            | Each server runs on its own host or container, on the same ports |
| `--listen 0.0.0.0`                        | Clients or other servers connect from other machines             |
| `--tls-certificate c --tls-private-key k` | The servers serve native NATS and WebSockets over TLS            |
| `--client-port`, `--websocket-port`, ...  | The default ports are taken                                      |

`nats new` prints the `[nats]` section for Trellis's `config.toml` when it's
done. Keep `trellis-operator.nk` as private as `secrets.toml`: anyone with it
can create accounts on your servers.

### With an operator you already have

If you manage NATS with [`nsc`](https://docs.nats.io/using-nats/nats-tools/nsc),
give Trellis a signing key of its own and the system user:

```sh
nsc edit operator --sk generate
nsc generate creds --account SYS --name sys > system.creds
```

The first prints the new signing key's public key. Its seed is in nsc's key
store (`nsc env` shows where) in a file named after that key; copy it to
`trellis-operator.nk`. Each server's config needs the operator, the system
account and a full resolver, which
`nsc generate config --nats-resolver --sys-account SYS` writes. Trellis
creates two accounts, `AUTH` and `TRELLIS`. Don't make accounts with those
names yourself.

## Write `config.toml`

`trellis-server config example` prints a commented configuration to start
from:

```sh
mkdir -p ~/trellis
trellis-server config example > ~/trellis/config.toml
```

The parts every installation sets:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 3000

[http.transport]
mode = "tls"
certificate_chain = "tls/trellis.crt"
private_key = "tls/trellis.key"

[nats]
servers = "tls://nats.example.com:4222"
system_credentials = "/home/you/trellis-nats/system.creds"
operator_signing_key = "/home/you/trellis-nats/trellis-operator.nk"
```

`public_origin` is Trellis's address: sign-in, where the CLI, services,
devices and apps connect, and the Console at `/console`. Its certificate has
to name that hostname. Relative paths resolve from the folder holding
`config.toml`.

`[nats] servers` is where Trellis connects, comma-separated. A server on
another machine needs `tls://`. Plain `nats://` works on loopback, or on a
network only this installation uses, such as a Podman network; say so with
`private_network = true`. When the servers' certificate comes from a private
CA, add `ca_certificate = "<its certificate>"`.

Clients are told to use `[nats] servers` too, unless `[client]` says
otherwise:

```toml
[client]
nats_servers = ["tls://nats.example.com:4222"]
ws_nats_servers = ["wss://nats.example.com"]
```

`nats_servers = []` offers clients only the WebSocket, so nothing but HTTPS
has to reach them.

### Where the certificate comes from

`[http.transport] mode` picks one of three:

- `tls`: certificate files you supply, from Let's Encrypt or your
  organization's CA. Trellis reloads them when you replace them.
- `proxy`: a proxy in front terminates HTTPS and sends plain HTTP to this
  port. See [Behind your own HTTPS proxy](#behind-your-own-https-proxy).
- `generated`: Trellis makes a CA and a certificate for `public_origin` and
  renews it. For development and tests only. Clients have to accept the CA by
  its fingerprint, and Trellis warns about it in its log, the Console and the
  sign-in pages.

To try the production layout without a real CA,
`trellis-server tls issue --out <folder> --host <name>` writes a certificate
for those hosts (`server.crt`, `server.key`) and the CA that issued it
(`ca.crt`). Trust that `ca.crt` wherever the test runs.

## The first start

```sh
trellis-server --config ~/trellis
```

The first start creates `secrets.toml` beside `config.toml`, readable by its
owner only: the keys for sign-in, for the auth callout, and for Trellis's two
NATS accounts. It sends the accounts to the NATS servers, creates its
databases and streams, and prints a one-time link for setting the
administrator's password, as in
[Start Trellis](/guides/starting-trellis#set-the-administrator-password).
`trellis-server admin --config ~/trellis --username admin --password-stdin`
sets it without starting the server.

Every later start sends the accounts again, so the servers always have the
current ones. If NATS refuses the update but already has the accounts,
Trellis logs a warning and starts anyway; closing revoked connections then
waits until the system user works again.

Back up `secrets.toml`. Without it the installation's sessions, service
registrations and NATS accounts can't be recovered.

## Where an installation keeps its files

The installation is the folder holding `config.toml` and `secrets.toml`.
Where the server writes while it runs depends on how you name it:

| Start with                         | Installation        | Databases and logs                                     |
| ---------------------------------- | ------------------- | ------------------------------------------------------ |
| `trellis-server`                   | `~/.config/trellis` | `~/.local/share/trellis`, `~/.local/state/trellis`     |
| `trellis-server --system`          | `/etc/trellis`      | `/var/lib/trellis`, `/run/trellis`, `/var/log/trellis` |
| `trellis-server --config <folder>` | `<folder>`          | `<folder>/var`                                         |

The first is the default, except for root on Linux, where it's `--system`.
To keep some of it somewhere else, add `[paths]`:

```toml
[paths]
data = "/srv/trellis/data"
state = "/srv/trellis/state"
logs = "/srv/trellis/log"
```

Leave a line out and the server uses the default for it.

### Using the `nats` CLI

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

## Run it under systemd with Podman

[Podman Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html)
units run three NATS containers and Trellis on a network of their own. This
example sits behind an HTTPS proxy on the same host, which forwards
`https://trellis.example.com` to port 3000.

Make the cluster, with each server named after its container:

```sh
trellis-server nats new --servers 3 --listen 0.0.0.0 \
  --hosts trellis-nats-1,trellis-nats-2,trellis-nats-3 --out ~/trellis-nats
```

`~/trellis/config.toml`:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 3000
bind_address = "0.0.0.0"
# Browsers, services and devices reach NATS over WebSockets at public_origin.
nats_websocket_upstream = "trellis-nats-1:8080"

[http.transport]
mode = "proxy"

[nats]
servers = "nats://trellis-nats-1:4222,nats://trellis-nats-2:4222,nats://trellis-nats-3:4222"
private_network = true
system_credentials = "nats/system.creds"
operator_signing_key = "nats/trellis-operator.nk"

[client]
nats_servers = []
```

The units go in `~/.config/containers/systemd/`. `trellis.network`:

```toml
[Network]
NetworkName=trellis
```

`trellis-nats-1.container`, and the same for `-2` and `-3`:

```toml
[Container]
ContainerName=trellis-nats-1
Image=docker.io/library/nats:2.14.4-alpine
Network=trellis.network
Volume=%h/trellis-nats:%h/trellis-nats:Z
Exec=-c %h/trellis-nats/nats-1.conf
ReadOnly=true

[Service]
Restart=always

[Install]
WantedBy=default.target
```

`trellis.container`:

```toml
[Unit]
Requires=trellis-nats-1.service trellis-nats-2.service trellis-nats-3.service
After=trellis-nats-1.service trellis-nats-2.service trellis-nats-3.service

[Container]
ContainerName=trellis
Image=ghcr.io/oats-center/trellis-server:0.100.0-rc.25
Network=trellis.network
PublishPort=127.0.0.1:3000:3000
Volume=%h/trellis:/etc/trellis:Z
Volume=%h/trellis-nats/system.creds:/etc/trellis/nats/system.creds:ro,Z
Volume=%h/trellis-nats/trellis-operator.nk:/etc/trellis/nats/trellis-operator.nk:ro,Z
Volume=trellis-data:/var/lib/trellis
ReadOnly=true
Tmpfs=/run
Tmpfs=/var/log
UserNS=keep-id:uid=10001,gid=10001

[Service]
Restart=always

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user start trellis
journalctl --user -u trellis
```

The log shows the link for the administrator's password. Trellis writes
`secrets.toml` into `~/trellis` on that first start; after it, add `:ro` to
that volume.

How this fits together:

- Only port 3000 is published, on loopback, for the proxy. NATS is reachable
  only on the `trellis` network, so it can be plaintext there.
- Clients connect over the WebSocket at `public_origin`, which Trellis hands
  to `trellis-nats-1`. Native NATS clients on the same network can use
  `nats://trellis-nats-1:4222` and the others directly.
- `mode = "proxy"` without `trusted_proxies` serves every connection that
  arrives. Rootless Podman delivers published connections from the network's
  own address, so Trellis couldn't tell the proxy apart anyway. Publishing on
  loopback, or a firewall, decides who connects.
- Change `Image=` to upgrade. Trellis and NATS upgrade separately.

## Plain containers

The same layout works with `docker run`. For one NATS server:

```sh
trellis-server nats new --servers 1 --listen 0.0.0.0 --hosts nats --out ~/trellis-nats
docker network create trellis
docker run -d --name nats --network trellis \
  -v ~/trellis-nats:$HOME/trellis-nats \
  docker.io/library/nats:2.14.4-alpine -c ~/trellis-nats/nats-1.conf
docker run -d --name trellis --network trellis --read-only \
  --tmpfs /run --tmpfs /var/log --publish 127.0.0.1:3000:3000 \
  -v ~/trellis:/etc/trellis -v ~/trellis-nats:/etc/trellis/nats:ro \
  -v trellis-data:/var/lib/trellis \
  ghcr.io/oats-center/trellis-server:0.100.0-rc.25
```

with `servers = "nats://nats:4222"`, `private_network = true` and the same
`system_credentials` and `operator_signing_key` lines as the Quadlet's
`config.toml`. The image runs as uid 10001, so that user has to be able to
read `~/trellis` and write `secrets.toml` into it on the first start.

## Behind your own HTTPS proxy

With `mode = "proxy"`, Trellis serves plain HTTP and the proxy terminates
HTTPS. The proxy has to keep the original `Host` header. To accept only the
proxy, list its networks:

```toml
[http.transport]
mode = "proxy"
trusted_proxies = ["10.0.0.2/32"]
```

Trellis then answers only those networks, gives everyone else `403`, and
rate-limits clients by the proxy's `X-Forwarded-For` header. Without
`trusted_proxies` it serves every connection and takes the client from the
last `X-Forwarded-For` entry. Leave it out only when the network already
admits nothing but the proxy.

Sign-in cookies only work over HTTPS in a browser, so something has to
terminate TLS. NATS's native port can't sit behind a TLS-terminating proxy;
its WebSocket can.

For the proxy's health checks, Trellis answers `/healthz` and `/readyz`
whatever `Host` header the check sends. `/readyz` returns `503` until the
server has finished starting.

### Only HTTPS in public

The WebSocket can share Trellis's hostname: the proxy sends every request
that asks for a WebSocket upgrade to NATS and everything else to Trellis. For
example, behind an AWS Application Load Balancer holding a certificate for
`*.example.com`, with NATS from `nats new --listen 0.0.0.0` on the same
instance and the VPC range `172.31.0.0/16`:

```toml
[http]
public_origin = "https://trellis.example.com"

[http.transport]
mode = "proxy"
trusted_proxies = ["172.31.0.0/16"]

[nats]
servers = "nats://127.0.0.1:4222,nats://127.0.0.1:4223,nats://127.0.0.1:4224"

[client]
nats_servers = []
ws_nats_servers = ["wss://trellis.example.com"]
```

The load balancer's HTTPS listener has these rules, in this order:

| Condition                                                  | Forward to        | Health check            |
| ---------------------------------------------------------- | ----------------- | ----------------------- |
| Host `trellis.example.com` and header `Upgrade: websocket` | HTTP 8080 to 8082 | `/`, success code `400` |
| Host `trellis.example.com`                                 | HTTP 3000         | `/readyz`               |

A NATS WebSocket port answers a plain request with `400`, which is what the
health check expects. Raise the load balancer's idle timeout above its
60-second default, or it cuts quiet NATS connections. Allow ports 3000 and
8080 to 8082 only from the load balancer.

Without a WebSocket rule, set `nats_websocket_upstream = "127.0.0.1:8080"`
under `[http]` instead and leave out `ws_nats_servers`. Trellis's port then
hands every WebSocket upgrade to NATS itself.

Trellis can also reach NATS through the load balancer like any other client:
`servers = "wss://trellis.example.com"`. It checks the load balancer's
certificate against the system trust store.

## The Console on its own hostname

The Console runs at `/console` on Trellis's origin, beside the sign-in pages.
A bug that let someone run their own script in the Console could then also
act on those pages as whoever has the Console open, for example approving an
app's access request. Give the Console a hostname of its own and the browser
keeps the two apart:

```toml
[console]
origin = "https://console.example.com"
```

`console.example.com` is another name for Trellis's port: point it at the
same port, through your proxy or DNS. Trellis serves the Console there and
sends anyone who opens `/console` on `trellis.example.com` to it. The
certificate has to name both hostnames.

## Without the Console

An installation you run entirely from the `trellis` CLI can leave the Console
out:

```toml
[console]
enabled = false
```

Trellis then serves no Console pages, and sign-in turns the Console away even
if something else answers at its address. A service waiting for approval
prints the `trellis svc <deployment-id> approve` command instead of a link.

## Back it up as one piece

Trellis ties its database to the NATS data it was created with. Point a
database at a different or empty NATS store and Trellis refuses to start,
because jobs and stored files would silently go missing. Back up and restore
these together:

- the installation folder: `config.toml` and `secrets.toml`
- the data folder, including `platform.sqlite` and the `platform.issuer-keys/`
  folder beside it, which holds the sign-in signing keys
- each NATS server's data folder: JetStream and the resolver's accounts
- the operator's `system.creds` and `trellis-operator.nk`

If a signing key in `platform.issuer-keys/` goes missing, restore it from a
backup; Trellis won't make a new one in its place. Recreating NATS streams or
buckets by hand doesn't bring back lost jobs or files either.
