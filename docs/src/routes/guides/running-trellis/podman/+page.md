---
title: "Run Trellis with Podman"
description: "Rootless Podman Quadlet units for NATS and Trellis on a network of their own, serving HTTPS from your own certificate or from a proxy in front."
---

This runs NATS and Trellis as two rootless containers under your own user,
started by systemd through
[Podman Quadlet](https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html).
They share a Podman network called `trellis`. NATS publishes no ports at all;
everything public goes through Trellis's port, including the NATS WebSocket
that browsers, the CLI, services and devices connect over.

You need Podman 5, a DNS name such as `trellis.example.com` pointing at the
machine, and a certificate for it. Nothing has to be installed besides Podman:
the `trellis-server` image carries the tools the setup uses.

The image is `ghcr.io/oats-center/trellis-server`, tagged with each release's
version, `v` included: `v0.100.0-rc.27`. The examples use that tag. Pick the
newest from the
[releases](https://github.com/oats-center/trellis/releases).

## Make the NATS server

```sh
mkdir -p ~/trellis/nats ~/trellis-nats
podman run --rm --userns keep-id:uid=10001,gid=10001 \
  -v ~/trellis-nats:/etc/nats:Z \
  ghcr.io/oats-center/trellis-server:v0.100.0-rc.27 \
  nats new --servers 1 --listen 0.0.0.0 --hosts trellis-nats --out /etc/nats
mv ~/trellis-nats/system.creds ~/trellis-nats/trellis-operator.nk ~/trellis/nats/
```

`nats new` writes `nats-1.conf` for a server named after its container,
`trellis-nats`, listening on the container's own network: clients on 4222,
WebSockets on 8080, monitoring on 8222. Its JetStream data and accounts go in
`~/trellis-nats/data`. `--userns keep-id` makes the files yours, not the
image's user.

The two files moved into `~/trellis/nats` belong to Trellis. `system.creds` is
the NATS system user, which Trellis uses to send the server its accounts and
to close connections it revokes. `trellis-operator.nk` is an operator signing
key; anyone holding it can create accounts on your NATS server, so keep it as
private as Trellis's own secrets.

## Write `config.toml`

`~/trellis/config.toml`:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 3000
# Every WebSocket upgrade on this port goes to NATS.
nats_websocket_upstream = "trellis-nats:8080"

[http.transport]
mode = "tls"
certificate_chain = "tls/trellis.crt"
private_key = "tls/trellis.key"

[nats]
servers = "nats://trellis-nats:4222"
private_network = true
system_credentials = "nats/system.creds"
operator_signing_key = "nats/trellis-operator.nk"

[client]
nats_servers = []
```

The folder is `/etc/trellis` inside the container, and relative paths resolve
from it.

`public_origin` is the address everyone uses. The certificate has to name its
hostname. Put the chain and key in `~/trellis/tls`. Trellis reloads them when
the files change, so renewal is a matter of replacing them.

`nats_websocket_upstream` is the NATS container's WebSocket listener as
`host:port`, never a URL. Trellis refuses to start when it can't parse it.

`[nats] servers` reaches NATS by its container name. Plain `nats://` is only
allowed off loopback when you say the network belongs to this installation
alone, which is what `private_network = true` does. `[client] nats_servers = []`
keeps that address from clients, which couldn't reach it. They get
`wss://trellis.example.com` instead.

`podman run --rm ghcr.io/oats-center/trellis-server:v0.100.0-rc.27 config example`
prints every setting with a comment saying what it does.

## The units

They go in `~/.config/containers/systemd/`. `trellis.network`:

```toml
[Network]
NetworkName=trellis
```

`trellis-nats.container`:

```toml
[Container]
ContainerName=trellis-nats
Image=docker.io/library/nats:2.14.4-alpine
Network=trellis.network
Volume=%h/trellis-nats:/etc/nats:Z
Exec=-c /etc/nats/nats-1.conf
ReadOnly=true

[Service]
Restart=always

[Install]
WantedBy=default.target
```

`trellis.container`:

```toml
[Unit]
Requires=trellis-nats.service
After=trellis-nats.service

[Container]
ContainerName=trellis
Image=ghcr.io/oats-center/trellis-server:v0.100.0-rc.27
Network=trellis.network
PublishPort=443:3000
Volume=%h/trellis:/etc/trellis:Z
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

The image runs `trellis-server --system` as uid 10001: the installation in
`/etc/trellis`, its databases in `/var/lib/trellis`, here a named volume.
`UserNS=keep-id` maps that uid to you, so Trellis can write `secrets.toml`
into `~/trellis` and you can still read your own files.

Rootless Podman can't publish ports below 1024 until the kernel allows it:

```sh
sudo sysctl net.ipv4.ip_unprivileged_port_start=443
echo net.ipv4.ip_unprivileged_port_start=443 | sudo tee /etc/sysctl.d/90-trellis.conf
```

User services also stop when you log out unless your user lingers:
`sudo loginctl enable-linger $USER`.

## The first start

```sh
systemctl --user daemon-reload
systemctl --user start trellis
journalctl --user -u trellis
```

Starting `trellis` starts `trellis-nats` first. Trellis's first start creates
`~/trellis/secrets.toml`, readable by you only. It holds the keys for sign-in,
for NATS's auth callout, and for Trellis's two NATS accounts, which Trellis
then sends to the NATS server. Back it up. Without it the installation's
sessions, service registrations and NATS accounts can't be recovered.

The log also has one line you need:

```json
{ "adminAccountUrl": "https://trellis.example.com/account/reset?token=..." }
```

Open it and choose a password for the `admin` account. There's no default
password. Until one is set, each start prints a new link and the old ones stop
working.

After the first start Trellis doesn't write to `~/trellis` again. Change that
volume to `Volume=%h/trellis:/etc/trellis:ro,Z` if you like.

## Check it

```sh
curl https://trellis.example.com/readyz
trellis login https://trellis.example.com
```

`/readyz` returns `503` while Trellis starts and `200` with its version after.
The login opens the device sign-in. Once you approve it, the CLI connects to
NATS over the WebSocket on port 443, through Trellis to `trellis-nats`, which
proves the whole path works. The Console is at
`https://trellis.example.com/console`.

Settings for the container go under `[Container]` as `Environment=` lines,
`Environment=RUST_LOG=info,trellis_runtime=debug` for more logging, followed by
`systemctl --user daemon-reload` and a restart.

To upgrade, change the tag in `Image=`, then reload and restart. Trellis and
NATS upgrade separately.

## Behind an HTTPS proxy

A proxy can hold port 443 and the certificate instead, and talk plain HTTP to
Trellis. In `config.toml`, replace `[http.transport]` with:

```toml
[http.transport]
mode = "proxy"
```

Where the containers publish their ports depends on where the proxy runs. The
sysctl for port 443 isn't needed in either case.

Leave `trusted_proxies` out of a Podman installation. Rootless Podman hands
Trellis published connections from the network's own address, so Trellis
can't tell the proxy from anyone else. What keeps everyone else out is where
the port is published, or a firewall. Without `trusted_proxies`, Trellis takes
each client's address from the last `X-Forwarded-For` entry, which its
per-client limits count against.

For the proxy's health check, use `/readyz`. Trellis answers it whatever
`Host` header the check sends.

### A proxy on the same machine

Publish Trellis on loopback only. In `trellis.container`, replace the
`PublishPort=` line with:

```toml
PublishPort=127.0.0.1:3000:3000
```

NATS still publishes nothing. The proxy forwards everything for
`trellis.example.com` to `127.0.0.1:3000`, WebSocket upgrades included, and
keeps the original `Host` header. Trellis hands the WebSockets on to NATS.
Caddy does both by default:

```text
trellis.example.com {
	reverse_proxy 127.0.0.1:3000
}
```

nginx needs `proxy_set_header Host $host`, and the `Upgrade` and
`Connection` headers passed on for WebSockets.

### A proxy on another machine

The proxy has to reach the port across the network, so publish it on the
machine's address. In `trellis.container`:

```toml
PublishPort=3000:3000
```

`3000:3000` listens on every interface. To listen on one, name its address:
`PublishPort=10.0.1.20:3000:3000`. Either way, firewall port 3000 so only the
proxy reaches it. The proxy forwards to port 3000 on this machine, with the
same rules as above: WebSocket upgrades included and the `Host` header kept.
NATS still publishes nothing, since Trellis forwards the WebSockets to it.

### A load balancer that sends WebSockets to NATS itself

A load balancer, such as an AWS Application Load Balancer, can route WebSocket
upgrades straight to NATS. Then both containers publish a port the load
balancer reaches. `trellis-nats.container`:

```toml
[Container]
ContainerName=trellis-nats
Image=docker.io/library/nats:2.14.4-alpine
Network=trellis.network
PublishPort=8080:8080
Volume=%h/trellis-nats:/etc/nats:Z
Exec=-c /etc/nats/nats-1.conf
ReadOnly=true

[Service]
Restart=always

[Install]
WantedBy=default.target
```

`trellis.container`:

```toml
[Unit]
Requires=trellis-nats.service
After=trellis-nats.service

[Container]
ContainerName=trellis
Image=ghcr.io/oats-center/trellis-server:v0.100.0-rc.27
Network=trellis.network
PublishPort=3000:3000
Volume=%h/trellis:/etc/trellis:Z
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

Only 8080, the WebSocket listener, is published from NATS. Ports 4222 and
8222 stay on the `trellis` network, where Trellis reaches them by name.

`config.toml` names the address clients use instead of forwarding:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 3000

[http.transport]
mode = "proxy"

[nats]
servers = "nats://trellis-nats:4222"
private_network = true
system_credentials = "nats/system.creds"
operator_signing_key = "nats/trellis-operator.nk"

[client]
nats_servers = []
ws_nats_servers = ["wss://trellis.example.com"]
```

There's no `nats_websocket_upstream`: nothing reaches Trellis's port asking
for a WebSocket. Keep the `[client]` heading. Without it the two lines belong
to `[nats]`, clients are told `nats://trellis-nats:4222`, and the Console
can't sign in. Check what clients are told with:

```sh
curl -s https://trellis.example.com/.well-known/oauth-authorization-server | grep -o '"natsEndpoints":[^]]*]'
```

It should print `"natsEndpoints":["wss://trellis.example.com"]`.

The load balancer's HTTPS listener needs two rules, in this order, each to a
target group on this machine:

| Condition                                                  | Forward to | Health check            |
| ---------------------------------------------------------- | ---------- | ----------------------- |
| Host `trellis.example.com` and header `Upgrade: websocket` | HTTP 8080  | `/`, success code `400` |
| Host `trellis.example.com`                                 | HTTP 3000  | `/readyz`               |

A NATS WebSocket port answers a plain request with `400`, so that's the
success code. Raise the load balancer's idle timeout above its 60-second
default, or it cuts quiet NATS connections. Let only the load balancer reach
ports 3000 and 8080; in AWS, that's the instance's security group allowing
them from the load balancer's security group alone. NATS logs
`Websocket not configured with TLS. DO NOT USE IN PRODUCTION!` when it starts;
behind a load balancer that terminates TLS, that's expected.

If sign-in then fails with `Authorization Violation` and `trellis-nats` logs
nothing, the WebSocket rule reaches some other NATS server. Compare the
`server_id` that `wss://trellis.example.com` sends first with the
`Server ID` line in `podman logs trellis-nats`.

## Other settings

A NATS cluster, an `nsc` operator you already have, the Console on its own
hostname, and turning the Console off work the same in a container. They're in
[Run Trellis from the binaries](/guides/running-trellis/binaries#other-settings).
For a cluster, run one container per server, each named after its
`nats new --hosts` name. They all mount `~/trellis-nats`, so use the shared
label, `Volume=%h/trellis-nats:/etc/nats:z` with a lowercase `z`. `:Z` gives
the folder a label private to one container, and each broker that starts
takes it away from the ones already running.

[Keep it running](/guides/running-trellis/upkeep) covers upgrades, backups
and resetting the administrator's password.
