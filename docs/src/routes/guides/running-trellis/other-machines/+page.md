---
title: "Reach Trellis from other machines"
description: "A development installation other machines can use, such as on a Tailscale name: one port, one certificate to accept in the browser."
---

A localhost development installation only listens on `localhost`. For other
machines, create a development installation for the hostname they'll use,
before the first start. Answer "From other machines, for development" when the
first `trellis-server` asks, or create it directly. On a Tailscale tailnet
where the machine is `eta.oats`:

```sh
trellis-server init --dev --host eta.oats
trellis-server
```

Everything a browser needs comes through one address, `https://eta.oats:3000`:
the Console at `/console`, sign-in at `/auth`, and NATS at `/nats`. Trellis
makes its own certificate for it. The first time a browser opens the Console,
it warns that it doesn't know who issued the certificate; check that the
issuer's SHA-256 fingerprint matches the one `trellis-server init` printed, and
accept it. Nothing gets installed on the other machine.

The CLI does the same check. `trellis login https://eta.oats:3000/auth` shows
the fingerprint, asks whether to accept it, and remembers the answer.

Services and devices do the same the first time they connect: started from a
terminal, they show the fingerprint and ask, then remember the answer. For a
service started by systemd or in a container, set
`TRELLIS_CERTIFICATE_FINGERPRINT` to the fingerprint instead, or
`TRELLIS_ACCEPT_SELF_SIGNED=1` to take whatever certificate it finds on that
first connection. A later different certificate is refused. The certificate
also names `localhost`, so a service on the same machine can connect locally.

Already started once for `localhost`? Then there's an installation in the way.
[Start over](/guides/running-trellis/upkeep#start-over) first, or create the
new one somewhere else with `--out <folder>` and start it with
`trellis-server --config <folder>/config.toml`.

A development installation shares one origin between the Console and sign-in,
which is fine for people you trust on your network. For anything else, set up
a [production installation](/guides/running-trellis/production) with
certificates browsers already trust.

## The settings you can choose

These go on `trellis-server init --dev`:

| Flag                                                   | Default                | Use it when                                              |
| ------------------------------------------------------ | ---------------------- | -------------------------------------------------------- |
| `--host eta.oats`                                      | `localhost`            | Other machines reach this one by that name               |
| `--trellis-port`                                       | `3000`                 | That port is taken                                       |
| `--nats-port`, `--nats-monitor-port`, `--nats-ws-port` | `4222`, `8222`, `8080` | Those ports are taken                                    |
| `--bind-address eta.oats`                              | all interfaces         | The Console's port should listen on one address only     |
| `--name "Lab Trellis"`                                 | `Trellis`              | You want a different display name                        |
| `--out <folder>`                                       | `~/.config/trellis`    | You want a self-contained installation in its own folder |
| `--external-nats`                                      | Trellis runs NATS      | You run NATS yourself                                    |

`--bind-address` takes a hostname or an IP and listens on the address of this
machine it resolves to. It only covers the Console's port; NATS listens on
every interface. `init` refuses port `0`, the same port used twice, and a NATS
port that clashes with `--trellis-port`.
