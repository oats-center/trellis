---
title: "Install the Trellis CLI"
description: "Install the trellis command-line tool with the one-line installer, by hand, or from a checkout, and keep it up to date."
---

The `trellis` command is how you manage a Trellis from a terminal: logging in,
deploying and upgrading services, registering apps and reviewing their APIs.

## The one-line installer

On Linux x86_64, this installs the newest release into `~/.local/bin`:

```sh
curl -fsSL https://oats-center.github.io/trellis/install.sh | sh -s trellis
```

It works like the [server's installer](/guides/install-trellis#the-one-line-installer):
it checks the release checksum first, and `TRELLIS_VERSION` and
`TRELLIS_INSTALL_DIR` pick another release or folder.

From then on the CLI updates itself. `trellis upgrade check` says whether a
newer release exists, and `trellis upgrade install` replaces the binary with
it.

## By hand

Each [GitHub release](https://github.com/oats-center/trellis/releases) carries
a `trellis` archive and a checksum file. Linux x86_64 is the only platform with
a download today.

```sh
version=0.100.0-rc.12
target=x86_64-unknown-linux-gnu
base=https://github.com/oats-center/trellis/releases/download/v$version
curl -LO "$base/trellis-$version-$target.tar.gz"
curl -LO "$base/checksum-$version-$target-trellis.sha256"
sha256sum -c "checksum-$version-$target-trellis.sha256"
tar xzf "trellis-$version-$target.tar.gz"
install -m 755 trellis ~/.local/bin/trellis
```

Only install it once `sha256sum -c` reports `OK`.

## From a checkout

When you're working against unreleased docs or source, build the CLI from the
same checkout so its commands match the code:

```sh
git clone https://github.com/oats-center/trellis.git
cd trellis
cargo xtask install
cargo install --path crates/cli
```

`cargo xtask install` generates the SDK code the workspace needs before the
CLI can build.

## Check it works

```sh
trellis --version
```

Next, log in to your Trellis with `trellis login` and the address of its
sign-in page, such as `trellis login https://localhost:3001`.
[Start Trellis](/guides/starting-trellis#trust-trelliss-certificate) explains
the certificate question it asks the first time.
