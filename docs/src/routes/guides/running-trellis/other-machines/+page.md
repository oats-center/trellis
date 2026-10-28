---
title: "Reach Trellis from other machines"
description: "Create an installation for the hostnames other people and services will use, such as a Tailscale name."
---

A localhost development installation only listens on `localhost`. For anyone
else to connect, the installation needs the hostnames they'll use, chosen
before the first start: answer "From other machines" when the first
`trellis-server` asks, or create it with `trellis-server init`.

On a Tailscale tailnet where the machine is `eta.oats`, that looks like:

```sh
trellis-server init \
  --public-origin https://eta.oats:3000 \
  --oauth-origin https://eta.oats:3001 \
  --nats-server-url tls://eta.oats:4222
trellis-server
```

A tailnet gives you no subdomains, so sign-in stays on `eta.oats` with its own
port. Browsers reach NATS through the Console's address
(`wss://eta.oats:3000/nats`) unless you say otherwise, so there's no WebSocket
hostname to set. The certificates also name `localhost` and `127.0.0.1` for
NATS, so a service on the same machine can still connect locally.

Already started once for `localhost`? Then there's an installation in the way.
[Start over](/guides/running-trellis/upkeep#start-over) first, or create the
new one somewhere else with `--out <folder>` and start it with
`trellis-server --config <folder>/config.toml`.

Once it's running, browsers on other machines need the installation's CA,
`tls/ca.crt` in the installation folder, imported as a trusted authority. The
CLI shows the CA's fingerprint at `trellis login https://eta.oats:3001`, asks
whether to trust it and remembers the answer. Node services start with
`NODE_EXTRA_CA_CERTS` pointing at the CA; Rust services put its path in
`RuntimeAuthorizationTrust.root_certificates`.

## The settings you can choose

These all go on `trellis-server init`:

| Flag                                                   | Default                     | Use it when                                                                                |
| ------------------------------------------------------ | --------------------------- | ------------------------------------------------------------------------------------------ |
| `--public-origin https://eta.oats:3000`                | `https://localhost:3000`    | People open the Console at another address                                                 |
| `--oauth-origin https://eta.oats:3001`                 | `https://localhost:3001`    | The sign-in page lives at another address                                                  |
| `--nats-server-url tls://eta.oats:4222`                | `tls://localhost:4222`      | Services connect to NATS at another address                                                |
| `--nats-websocket-url wss://eta.oats:8080`             | relayed through the Console | Browsers should connect to NATS's WebSocket port directly (with `--nats-websocket direct`) |
| `--trellis-port`, `--oauth-port`                       | `3000`, `3001`              | Those ports are taken                                                                      |
| `--nats-port`, `--nats-monitor-port`, `--nats-ws-port` | `4222`, `8222`, `8080`      | Those ports are taken                                                                      |
| `--bind-address eta.oats`                              | all interfaces              | Console and sign-in should listen on one address only                                      |
| `--name "Lab Trellis"`                                 | `Trellis`                   | You want a different display name                                                          |
| `--out <folder>`                                       | `~/.config/trellis`         | You want a self-contained installation in its own folder                                   |
| `--external-nats`                                      | Trellis runs NATS           | You run NATS yourself                                                                      |
| `--tls loopback-development`                           | `generated`                 | You want plain HTTP, which only works when everything stays on `localhost`                 |

`--bind-address` takes a hostname or an IP and listens on the address of this
machine it resolves to. It only covers Console and sign-in. NATS listens on every
interface whenever TLS is on. `init` refuses port `0`, the same port used
twice, and a NATS port that clashes with `--trellis-port`. A proxy in front,
your own certificates and the other production settings are in
[Run Trellis in production](/guides/running-trellis/production).
