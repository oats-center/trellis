---
title: "Install Trellis"
description: "Get trellis-server onto a machine with the one-line installer, by hand, as a container image, or from source."
---

Trellis runs as one program, `trellis-server`. That single binary holds the
Trellis runtime, the Console (the admin web app) and the sign-in page people
see when they log in. It also runs [NATS](https://nats.io), the message broker
every Trellis service talks through, so there's no separate web server,
database or broker to set up.

## The one-line installer

On Linux x86_64, this installs the newest release into `~/.local/bin`:

```sh
curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis-server
```

To upgrade later, run `trellis-server upgrade install --prerelease` and restart
the server.

Then [start Trellis](/guides/starting-trellis). You'll probably want the
`trellis` CLI too, which installs the same way:

```sh
curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis
```

## By hand

Each [GitHub release](https://github.com/oats-center/trellis/releases) carries
a `trellis-server` archive and a checksum file for it. Linux x86_64 is the only
platform with a download today.

```sh
version=0.100.0-rc.12
target=x86_64-unknown-linux-gnu
base=https://github.com/oats-center/trellis/releases/download/v$version
curl -LO "$base/trellis-server-$version-$target.tar.gz"
curl -LO "$base/checksum-$version-$target-trellis-server.sha256"
sha256sum -c "checksum-$version-$target-trellis-server.sha256"
tar xzf "trellis-server-$version-$target.tar.gz"
install -m 755 trellis-server ~/.local/bin/trellis-server
```

## NATS

You don't have to install NATS. When `trellis-server` needs it and
`nats-server` isn't on your `PATH`, it offers to download the release Trellis
is tested with. `--nats-download` says yes without asking.

If you'd rather manage NATS yourself, install `nats-server` from your package
manager or the
[NATS install docs](https://docs.nats.io/running-a-nats-service/introduction/installation)
and Trellis uses it from `PATH`. `--local-nats=/path/to/nats-server` picks one
exact file instead. Trellis refuses a file that isn't a regular executable owned
by root or by you, or one where the file or a parent folder is writable by
others.

## The container image

Every release is also published as `ghcr.io/oats-center/trellis-server`, tagged
with its version (`v0.100.0-rc.12`) and `latest`. The image's entry point is
`trellis-server`. It also contains the `trellis` CLI and a checksum-verified
`nats-server` at `/usr/local/bin/nats-server`.
[Run Trellis in production](/guides/running-trellis/production) has full
examples for Podman and Docker.

## From source

Building from a checkout is how you work on Trellis itself or try something
that isn't released yet. [In-repo development](/guides/in-repo-development)
has the steps; the short version is `cargo xtask install`,
`npm run browser:embedded`, then `cargo build -p trellis-server`.
