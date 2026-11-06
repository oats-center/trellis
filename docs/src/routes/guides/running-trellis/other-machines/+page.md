---
title: "Reach Trellis from other machines"
description: "A development installation other machines can use, such as on a Tailscale name: one certificate to accept in the browser."
---

A `localhost` development installation only listens on `localhost`. For other
machines, create a development installation for the hostname they'll use. On
a Tailscale tailnet where the machine is `eta.oats`:

```sh
trellis-server --dev --host eta.oats --config ~/trellis-eta
```

Trellis listens on `https://eta.oats:3000` for sign-in, and for NATS: it hands
every request that asks for a WebSocket to the NATS server it runs. The
Console is at `https://eta.oats:3000/console`. Native NATS stays on this
machine's loopback, so other machines connect over the WebSocket.

The certificate is one Trellis made, from a CA it keeps in `var/tls/` and
renews itself. The first time a browser opens the Console, it warns that it
doesn't know who issued the certificate. Check that the issuer's SHA-256
fingerprint matches the one `trellis-server tls status --config ~/trellis-eta`
prints, and the one Trellis logs as it starts, and accept it. Nothing gets
installed on the other machine. The Console and the sign-in pages also show a
banner saying the installation serves its own certificate.

The CLI does the same check. `trellis login https://eta.oats:3000` shows
the fingerprint, asks whether to accept it, and remembers the answer.

Services and devices do the same the first time they connect: started from a
terminal, they show the fingerprint and ask, then remember the answer. For a
service started by systemd or in a container, set
`TRELLIS_CERTIFICATE_FINGERPRINT` to the fingerprint instead, or
`TRELLIS_ACCEPT_SELF_SIGNED=1` to take whatever certificate it finds on that
first connection. A later different certificate is refused.

`--host` only matters the first time, when the installation is created. Later
starts use what's in its `config.toml`; start it with
`trellis-server --dev --config ~/trellis-eta`. To change the hostname of an
existing one, edit `public_origin` under `[http]`. Trellis makes a new CA for
the new name on its next start, so clients accept it again.

A certificate Trellis made itself is fine for people you trust on your
network. For anything else, set up a
[production installation](/guides/running-trellis/production) with
certificates browsers already trust.

## What you can change

The installation's `config.toml` and `nats.conf` are plain text, written to be
edited:

| To change                    | Edit                                                       |
| ---------------------------- | ---------------------------------------------------------- |
| The Trellis port             | `port` under `[http]`, and the port in `public_origin`     |
| The NATS ports               | `listen`, `http` and the WebSocket `listen` in `nats.conf` |
| The address Trellis binds to | `bind_address` under `[http]`                              |
| The name people see          | `instance_name`                                            |

`trellis-server config example` prints every setting with a comment saying
what it does.
