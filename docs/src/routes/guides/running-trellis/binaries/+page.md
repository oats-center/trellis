---
title: "Run Trellis from the binaries"
description: "One machine, NATS and Trellis as systemd services, HTTPS from your own certificate or from a proxy in front."
---

This puts Trellis on one Linux machine the way you'd run any other server: two
system users, two systemd services, and a public HTTPS address such as
`https://trellis.example.com`. NATS listens on loopback only. Everything
public goes through Trellis's port: sign-in, the Console, and the NATS
WebSocket that browsers, the CLI, services and devices connect over.

You need root, a DNS name pointing at the machine, and these on the machine:

- `trellis-server` and `trellis`, from [Install Trellis](/guides/install-trellis).
  The installer puts them in `~/.local/bin`; copy `trellis-server` to
  `/usr/local/bin` so the service can run it.
- `nats-server` 2.14, from your package manager or the
  [NATS install docs](https://docs.nats.io/running-a-nats-service/introduction/installation),
  also in `/usr/local/bin`.

A development installation doesn't need any of this. If you just want to try
Trellis, [run a development Trellis](/guides/running-trellis/development)
instead.

## Make the NATS server

Each program gets its own system user:

```sh
sudo useradd --system --home-dir /var/lib/nats --shell /usr/sbin/nologin nats
sudo useradd --system --home-dir /var/lib/trellis --shell /usr/sbin/nologin trellis
sudo install -d -o nats -g nats -m 700 /var/lib/nats
```

`trellis-server nats new` writes a NATS server's configuration, an operator
for it, and the two files Trellis needs from that operator:

```sh
sudo -u nats trellis-server nats new --servers 1 --out /var/lib/nats
```

That leaves `/var/lib/nats/nats-1.conf`, with the server listening on
`127.0.0.1`: clients on 4222, WebSockets on 8080, monitoring on 8222. Its
JetStream data and accounts go in `/var/lib/nats/data`. The other two files
belong to Trellis, so move them to its folder:

```sh
sudo install -d -o trellis -g trellis -m 700 /etc/trellis /etc/trellis/nats
sudo install -o trellis -g trellis -m 600 \
  /var/lib/nats/system.creds /var/lib/nats/trellis-operator.nk /etc/trellis/nats/
sudo rm /var/lib/nats/system.creds /var/lib/nats/trellis-operator.nk
```

`system.creds` is the NATS system user, which Trellis uses to send the servers
its accounts and to close connections it revokes. `trellis-operator.nk` is an
operator signing key; anyone holding it can create accounts on your NATS
server, so keep it as private as Trellis's own secrets.

`/etc/systemd/system/nats.service`:

```toml
[Unit]
Description=NATS for Trellis
After=network-online.target
Wants=network-online.target

[Service]
User=nats
ExecStart=/usr/local/bin/nats-server -c /var/lib/nats/nats-1.conf
Restart=always

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now nats
curl http://127.0.0.1:8222/healthz
```

The last line should print `{"status":"ok"}`.

## Write `config.toml`

`/etc/trellis/config.toml`:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 443
# Every WebSocket upgrade on this port goes to NATS.
nats_websocket_upstream = "127.0.0.1:8080"

[http.transport]
mode = "tls"
certificate_chain = "tls/trellis.crt"
private_key = "tls/trellis.key"

[nats]
servers = "nats://127.0.0.1:4222"
system_credentials = "nats/system.creds"
operator_signing_key = "nats/trellis-operator.nk"

[client]
nats_servers = []
```

Relative paths resolve from `/etc/trellis`.

`public_origin` is the address everyone uses. The certificate has to name its
hostname. Put the chain and key in `/etc/trellis/tls`, readable by the
`trellis` user. Trellis reloads them when the files change, so renewal is a
matter of replacing them. With certbot, a `--deploy-hook` that copies
`fullchain.pem` and `privkey.pem` there does it. To try the layout before you
have a real certificate, `trellis-server tls issue --out <folder> --host <name>`
writes one (`server.crt`, `server.key`) and the test CA that issued it
(`ca.crt`), which `trellis login --ca-certificate ca.crt` accepts.

`nats_websocket_upstream` is `host:port`, never a URL. Trellis opens a plain
TCP connection to it for every WebSocket and refuses to start when it can't
parse it.

`[nats] servers` is where Trellis connects. Plain `nats://` is fine here
because it's loopback. `[client] nats_servers = []` stops Trellis from telling
clients about that address, which they couldn't reach anyway. They get
`wss://trellis.example.com` instead, so port 443 is the only one that has to
be open.

`trellis-server config example` prints every setting with a comment saying
what it does.

## Run Trellis

`/etc/systemd/system/trellis.service`:

```toml
[Unit]
Description=Trellis
Requires=nats.service
After=nats.service

[Service]
User=trellis
ExecStart=/usr/local/bin/trellis-server --system
StateDirectory=trellis
StateDirectoryMode=0700
CacheDirectory=trellis
RuntimeDirectory=trellis
RuntimeDirectoryMode=0700
LogsDirectory=trellis
# Port 443 without running as root.
AmbientCapabilities=CAP_NET_BIND_SERVICE
Restart=always

[Install]
WantedBy=multi-user.target
```

`--system` is the installation in `/etc/trellis`, with its databases in
`/var/lib/trellis`. systemd creates those folders for the `trellis` user.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now trellis
sudo journalctl -u trellis
```

The first start creates `/etc/trellis/secrets.toml`, readable by `trellis`
only. It holds the keys for sign-in, for NATS's auth callout, and for
Trellis's two NATS accounts, which Trellis then sends to the NATS server.
Back it up. Without it the installation's sessions, service registrations and
NATS accounts can't be recovered.

Every later start sends the accounts again, so NATS always has the current
ones. If NATS refuses the update but already has them, Trellis logs a warning
and starts anyway; closing revoked connections then waits until the system
user works again.

The log also has one line you need:

```json
{ "adminAccountUrl": "https://trellis.example.com/account/reset?token=..." }
```

Open it and choose a password for the `admin` account. There's no default
password. Until one is set, each start prints a new link and the old ones stop
working. To skip the browser, run
`sudo -u trellis trellis-server admin --system --password-stdin` with the
password on standard input before the first start. Once Trellis has started,
`admin` refuses, because the administrator already exists.

After the first start Trellis doesn't write to `/etc/trellis` again, so it
only needs to be readable by `trellis`.

## Check it

```sh
curl https://trellis.example.com/readyz
trellis login https://trellis.example.com
```

`/readyz` returns `503` while Trellis starts and `200` with its version after.
The login opens the device sign-in. Once you approve it, the CLI connects to
NATS over the WebSocket on port 443, which proves the whole path works. The
Console is at `https://trellis.example.com/console`.

With the service stopped, `sudo -u trellis trellis-server check --system`
connects with every configured credential and checks that the streams,
buckets, databases and signing keys Trellis expects are there, without
changing anything.

## Behind an HTTPS proxy

If something else already holds port 443, or you'd rather a proxy deal with
certificates, Trellis can serve plain HTTP to it. Change `[http]` and
`[http.transport]`:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 3000
bind_address = "127.0.0.1"
nats_websocket_upstream = "127.0.0.1:8080"

[http.transport]
mode = "proxy"
trusted_proxies = ["127.0.0.1/32"]
```

and drop `AmbientCapabilities` from the unit, since port 3000 needs no
privilege. The rest stays as it was.

The proxy has to forward everything for `trellis.example.com` to
`127.0.0.1:3000`, WebSocket upgrades included, and keep the original `Host`
header. Caddy does both by default:

```text
trellis.example.com {
	reverse_proxy 127.0.0.1:3000
}
```

nginx needs `proxy_set_header Host $host`, and the `Upgrade` and
`Connection` headers passed on for WebSockets.

`trusted_proxies` lists the networks the proxy connects from. Trellis turns
away everyone else with `403` and takes each client's address from the
proxy's `X-Forwarded-For` header, which its per-client limits count against. Without
`trusted_proxies`, Trellis answers anyone who reaches the port, so leave it out
only when nothing but the proxy can.

For the proxy's health check, use `/readyz`. Trellis answers it whatever `Host`
header the check sends.

### A proxy on another machine

`bind_address = "127.0.0.1"` keeps Trellis off the network, which only works
for a proxy on the same machine. For one elsewhere, delete that line, so
Trellis listens on every interface, or set it to the address the proxy
reaches, such as `"10.0.1.20"`. Then list the proxy's network in
`trusted_proxies`, such as `["10.0.1.0/24"]`, and firewall port 3000 to it.
The proxy forwards to port 3000 on this machine with the same rules as above.
NATS stays on loopback, since Trellis forwards the WebSockets to it.

### When the proxy sends WebSockets to NATS itself

A load balancer, such as an AWS Application Load Balancer, can route WebSocket
upgrades to NATS without Trellis in between. Then NATS's WebSocket has to
listen where the load balancer can reach it, so make the NATS server with
`--listen 0.0.0.0`:

```sh
sudo -u nats trellis-server nats new --servers 1 --listen 0.0.0.0 --out /var/lib/nats
```

Trellis has to be told the address clients use:

```toml
[http]
public_origin = "https://trellis.example.com"
port = 3000

[http.transport]
mode = "proxy"
trusted_proxies = ["172.31.0.0/16"]

[client]
nats_servers = []
ws_nats_servers = ["wss://trellis.example.com"]
```

`[nats]` stays as it was. There's no `nats_websocket_upstream`: nothing
reaches Trellis's port asking for a WebSocket. `172.31.0.0/16` stands for
your VPC. Keep the `[client]` heading. Without it the two lines belong to
`[nats]`, clients are told the `[nats] servers` address, and the Console
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
ports 3000 and 8080, and nothing reach 4222 or 8222 from outside. NATS logs
`Websocket not configured with TLS. DO NOT USE IN PRODUCTION!` when it starts;
behind a load balancer that terminates TLS, that's expected.

## Other settings

### A NATS cluster

`nats new --servers 3` writes three servers instead of one, on 4222 to 4224
(WebSockets on 8080 to 8082, routes on 6222 to 6224). Run each as its own
service and list all three in `[nats] servers`, comma-separated. JetStream
keeps copies on different servers, so a cluster needs three; NATS 2.14 won't
run a cluster of one. `--hosts` names each server's machine, and
`--listen 0.0.0.0` lets the others reach it. Give servers on other machines a
certificate with `--tls-certificate` and `--tls-private-key`: Trellis connects
to a NATS server that isn't on loopback only over `tls://`, and
`[nats] ca_certificate` names the CA when it's a private one.

### An operator you already have

If you manage NATS with [`nsc`](https://docs.nats.io/using-nats/nats-tools/nsc),
make Trellis a signing key of its own and export the system user:

```sh
nsc edit operator --sk generate
nsc generate creds --account SYS --name sys > system.creds
```

The first prints the new key's public key. Its seed is in nsc's key store
(`nsc env` shows where), in a file named after that key; that file is
`trellis-operator.nk`. The servers need a `full` resolver, which
`nsc generate config --nats-resolver --sys-account SYS` writes. Trellis creates
two accounts, `AUTH` and `TRELLIS`, so don't use those names yourself.

### The Console on its own hostname

The Console runs at `/console`, beside the sign-in pages. A bug that let
someone run a script in the Console could then act on those pages as whoever
has it open. A hostname of its own keeps them apart in the browser:

```toml
[console]
origin = "https://console.example.com"
```

Point `console.example.com` at the same port, through DNS or the proxy, and
get a certificate that names both hostnames. Anyone opening `/console` on
`trellis.example.com` is sent there.

### No Console at all

```toml
[console]
enabled = false
```

Trellis serves no Console pages and sign-in turns the Console away. A service
waiting for approval prints the `trellis svc <deployment-id> approve` command
instead of a link.

### Where the files go

`--system` keeps the installation in `/etc/trellis` and what the server writes
in `/var/lib/trellis`, `/run/trellis` and `/var/log/trellis`. `--config <folder>`
keeps both in one folder, with the writable part in `<folder>/var`. A
`[paths]` section moves any of them:

```toml
[paths]
data = "/srv/trellis/data"
logs = "/srv/trellis/log"
```

[Keep it running](/guides/running-trellis/upkeep) covers upgrades, backups
and resetting the administrator's password.
