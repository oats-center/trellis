---
title: "Install the Trellis CLI"
description: "Install the trellis command-line tool from a release, Cargo, or the current checkout."
---

The `trellis` CLI handles runtime and operator tasks such as service deployments, upgrades, bootstrap, and authentication. Install it before following the operator guides.

## Option A: Download a prebuilt binary

Prebuilt binaries are published as [GitHub Releases](https://github.com/oats-center/trellis/releases) for every tagged version.

Each release publishes one archive per binary, with a checksum file for each:

| Binary           | Archive (Linux x86_64)                                     | Checksum                                                            |
| ---------------- | ---------------------------------------------------------- | ------------------------------------------------------------------- |
| `trellis` (CLI)  | `trellis-<version>-x86_64-unknown-linux-gnu.tar.gz`        | `checksum-<version>-x86_64-unknown-linux-gnu-trellis.sha256`        |
| `trellis-server` | `trellis-server-<version>-x86_64-unknown-linux-gnu.tar.gz` | `checksum-<version>-x86_64-unknown-linux-gnu-trellis-server.sha256` |

Linux x86_64 is the only prebuilt target today. Download, verify and extract
the CLI for a release such as `0.100.0-rc.4`:

```sh
version=0.100.0-rc.4
target=x86_64-unknown-linux-gnu
base=https://github.com/oats-center/trellis/releases/download/v$version
curl -LO "$base/trellis-$version-$target.tar.gz"
curl -LO "$base/checksum-$version-$target-trellis.sha256"
sha256sum -c "checksum-$version-$target-trellis.sha256"
tar xzf "trellis-$version-$target.tar.gz"
```

Move the binary somewhere on your `PATH`:

```sh
install -m 755 trellis ~/.local/bin/trellis
```

Only extract and install a binary after `sha256sum -c` reports `OK`. For the
server itself, see [Install Trellis](/guides/install-trellis).

## Option B: Install with Cargo

If you have a Rust toolchain installed:

```sh
cargo install trellis-cli
```

This builds from source and places the `trellis` binary in your Cargo bin directory (usually `~/.cargo/bin`).

## Option C: Build the current checkout

When working against unreleased docs or source, build the CLI from the repository checkout so the available commands match the code you are running:

```sh
git clone https://github.com/oats-center/trellis.git
cd trellis
cargo xtask install
cargo install --path crates/cli
```

`cargo xtask install` generates the SDK crates that the Rust workspace needs before `trellis-cli` is built.

## Verify

```sh
trellis --version
```

## Register a development client

After `trellis login`, an administrator with `clients.manage` can register a
loopback browser app without installing an app participant. Save this generated
`AuthOAuthClientPutRequest` as `client.json`, using a new idempotency key for each
distinct change:

```json
{
  "clientId": "my-local-app",
  "expectedRevision": "0",
  "title": "My local app",
  "description": "Local browser development",
  "kind": "browser",
  "development": true,
  "redirectUris": ["http://localhost:5173/login"],
  "impliedConsent": [],
  "idempotencyKey": "01ARZ3NDEKTSV4RRFFQ69G5FAV"
}
```

```sh
trellis clients put client.json
trellis clients get my-local-app
trellis clients list
```

Browser redirect ports and origins are exact. The remote issuer does not contact
localhost. For updates, use the revision returned by `get`. `disable` and
`delete` take `--expected-revision`; stale revisions fail rather than overwriting
another administrator's work. These commands return generated JSON, including
the next-page cursor for `list --cursor`.

Console and CLI registration use a fixed flow policy on every update:
browser clients require callbacks and support Authorization Code plus refresh;
native clients without callbacks support Device Code plus refresh; native
clients with callbacks support both flows plus refresh. Adding callbacks to an
existing native client enables Authorization Code; removing them disables it.
Native HTTP loopback callbacks do not require the development flag. Browser
loopback callbacks do. Client kind cannot change: register a new client ID when
switching between native and browser binding semantics.

## Review an API catalog change

`trellis apis review --source <project-root> --participant <package>.<Name>`
compiles the package and reviews each API the participant implements, reporting
its digest, compatibility, affected roles and deployments, generation, and
accepted revision. `trellis apis accept` with the same flags reviews again and
accepts every compatible API that is not already accepted at that definition.
Acceptance is revision-fenced and requires `apis.accept`. `--participant` may be
omitted when the package has one participant.

Incompatible replacements use `trellis apis force-replace replacement.json` with
an `AuthApiForceReplaceRequest`. The operator needs `apis.forceReplace` and must
explicitly set both `acknowledgeBreakage` and `acknowledgeConsentImpact`. Include
the reviewed role/mapping edits when required by the review. `trellis apis list`
and `trellis apis get <api-id>` inspect accepted state. The Console exposes the
same operations under **OAuth clients** and **API catalog**.
