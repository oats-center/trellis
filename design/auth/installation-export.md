---
title: Moving an Installation
description: Export a whole installation's configuration as plain, versioned records with every service and device, and import it into another installation or a newer release.
---

# Design: Moving an Installation

> **Status: implemented.** `trellis-server export` and `trellis-server import`
> are commands beside the server, like `backup` and `restore`.

`trellis-server backup` and `restore` copy database files and stream
snapshots. They bring back the same installation on the same release, and
before the first stable release a backup restores only into the release
candidate that took it, because each candidate may edit its schema's baseline
in place. An installation export carries the configuration as plain records
instead, so it moves between installations and across releases, release
candidates included, without setting anything up again and without re-keying
the user IDs services stored.

## Prerequisites

- [service-export.md](./service-export.md): the service bundle each
  deployment travels in, and how roles and grants are recreated
- [trellis-auth.md](./trellis-auth.md): users, roles, grants, apps and
  deployments
- [tooling/trellis-cli.md](../tooling/trellis-cli.md#recovery): backup and
  restore

## Commands

```text
trellis-server export --out <file> [--passphrase-file <path|->] [--config <path> | --user | --system] [--format text|json]
trellis-server import <file> [--passphrase-file <path|->] [--check] [--config <path> | --user | --system] [--format text|json]
```

Both take the installation the way `backup` does. Export runs while the
installation runs or is stopped, as long as its NATS is reachable; it only
reads the platform database and never migrates it, so the build under test
can export a database an earlier release made and still runs. Import runs
against an installation that has started once (so its `secrets.toml` and
database exist) and is stopped, with its NATS running; it migrates the
database first, as a start would.

The passphrase comes from `--passphrase-file` (`-` for standard input) or
piped standard input. A new export's passphrase has at least 12 characters.
Export refuses an existing `--out`, writes a temporary file beside it readable
only by its owner, and links it into place once whole, so an existing file is
never replaced.

## The file

The file is the passphrase envelope of a service bundle (`trellis_bootstrap::
envelope`) with `trellis-installation v1` as its first line; the key
derivation's labels use the same name, so a service bundle never opens as an
installation export or the other way round. Inside is the content:

```text
TRELLIS-INSTALL\n, then a 4-byte container format (1)
meta.json                   container format, Trellis version, source installation and origin, each kind's format
records/<kind>.json         { "kind", "format", "records": [...] } for each record kind
deployments/<id>.bundle     the content of one service bundle per service or device deployment
```

Each entry is a 2-byte name length, the name, and chunks of a 4-byte length
and that many bytes, ended by a zero length. The passphrase proves the file
whole: a changed or cut file fails a chunk's tag.

**Versioning.** Each record kind carries its own format number. An import
reads a kind's records through a conversion from that format to the one it
reads; every kind is at format 1 today, so there is nothing to convert yet,
and a later format adds the conversion from each earlier one. A kind or format
the build does not know is refused by name. The records are rows of the
platform database written as JSON objects of their columns (blobs as
`{"base64": ...}`), less what belongs to the installation: secrets are opened
and carried in the clear inside the envelope, and installation-local columns
such as revisions are set again on import.

## Record kinds

| Kind                | What it carries                                                                                                                 |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `signInSettings`    | Local users on or off, local sign-up, password minimum length                                                                   |
| `oidcProviders`     | Each live provider with its ID, issuer, client ID, client secret (opened), pinned certificate and settings                      |
| `packageRegistries` | Each registry with its prefix, username and secret (opened)                                                                     |
| `users`             | ID, state, username and password hash, OIDC sign-ins with their provider's issuer, profile, and whether it is the administrator |
| `roles`             | Each active role with its ID, name, description, new-user flag, capabilities and members                                        |
| `grants`            | Each capability granted directly to a user                                                                                      |
| `oidcRoleMappings`  | Each mapping from a provider claim to roles                                                                                     |
| `oauthClients`      | Each app with its client ID, kind, redirect URIs, implied consent and sign-up settings                                          |
| `consents`          | Remembered consent of users to apps, for capabilities whose wording has not changed since                                       |
| `portalThemes`      | The installation's theme and each app's                                                                                         |
| deployments         | Every service and device deployment that is not removed and not Trellis's own, as a service bundle                              |

Trellis's apps are public clients (PKCE and DPoP), so an app has no secret to
carry; its client ID is its identity, and it keeps it. The Console and CLI
clients are registered by each installation for its own origin, so only their
sign-up settings move.

A device deployment travels in a service bundle like a service: its build,
its instances' public keys and its settings. A device whose key was
registered connects to the new installation with its seed, without activating
again. Its State values stay behind.

## Import

Import reads the whole file first, spooling each bundle to an encrypted
temporary file, and verifies each bundle as a service import does before it
changes anything. It then applies every record in one Auth transaction, in
this order: sign-in settings, OIDC providers, registries, users and the
administrator, deployments (through the service import, without their own
access records), roles and grants, OIDC role mappings, apps, consent, and
themes. Each record is applied in a savepoint: one that existing data is in
the way of is rolled back, reported as skipped with its reason, and the rest
goes on. A failure of the database itself stops the import and changes
nothing.

After the transaction commits, each deployment's data loads as a resumed
service import: its KV and store snapshots, then, once an instance can run,
its resources and waiting jobs. A deployment whose data did not finish
loading is reported, and running the same import again finishes it; records
already there are skipped the second time.

`--check` runs the same transaction and rolls it back, and loads nothing.

### What is in the way

Import starts by listing what the installation already has besides what its
own setup made (its default administrator and that administrator's role): users, OIDC
providers, roles, apps, deployments, themes and registries. An installation
with none of these is **empty**. The report shows the list, and the CLI warns
when it is not empty.

- **A user** is skipped when its user ID is in use, its username belongs to
  another user, or one of its OIDC sign-ins does.
- **An OIDC provider** with the same ID and issuer is the same provider. One
  with the same ID and another issuer is skipped, and so are the sign-ins
  through it. One whose issuer a live provider here has is skipped, and its
  sign-ins use that provider.
- **The default administrator.** In an empty installation the exported
  administrator takes the place of the installation's own: the setup
  administrator is deleted as `Users.Delete` deletes an account, with the
  role setup gave it alone, and the exported one becomes the default
  administrator, with its ID and password. An installation in use keeps its
  own administrator, and the exported one is imported as an ordinary user;
  the report says so.
- **Sign-in settings** apply in an empty installation, or one whose settings
  were never changed; otherwise they are skipped.
- **Roles and grants** are recreated as a service import recreates them
  ([service-export.md](./service-export.md#access-roles-grants-and-consent)):
  users found by ID, then by sign-in; a role under the same ID with another
  definition is skipped with its members.
- **An app, registry or OIDC role mapping** whose ID or name exists is
  skipped.
- **A theme** that exists and differs is skipped, except in an empty
  installation, where the exported one replaces it.
- **A deployment** is skipped when the service import refuses it, for
  example because its ID or one of its instance keys is in use.
- **Consent** is carried only for users this import created, to apps and
  capabilities the installation has.

### The report

```text
applied      false for --check
source       installation ID, origin, Trellis version and export time
target       { empty, existing: counts by kind }
created      { kind, id } for each record created
skipped      { kind, id, reason } for each record skipped
access       recreated and skipped roles, members and grants, as a service import reports them
deployments  each deployment's service import report, or the error that stopped its data loading
notCarried   what an installation export leaves out
```

## What is not carried

- Sessions: login sessions, OAuth grants and refresh families, and app
  sessions. Everyone signs in again.
- The NATS operator, accounts and credentials, the installation's TLS, and
  the issuer signing keys. The new installation has its own and announces
  them the usual way; processes trust its certificate by fingerprint or a
  public CA.
- The security audit and job, event, health and webhook request history.
- Earlier settings revisions, as for a service bundle.
- Password reset and setup links that were not used.
- Device activation reviews, device provisioning secrets of devices not yet
  activated, and the values of State resources.
- Deleted users, whose IDs the old installation keeps only as audit
  tombstones, and APIs that no carried deployment provides.

## Security

The file holds every secret of the installation: password hashes, OIDC client
secrets, registry secrets, and every service's settings secrets. It is sealed
with the passphrase and written readable only by its owner. Both commands run
with the installation's own files, so whoever runs them is the operator,
whom Trellis trusts (see
[engineering-patterns.md](../core/engineering-patterns.md#the-operator-is-trusted)).
Import acts in the database as the default administrator, which is how the
service import's capability checks pass.

## Restoring across releases

A backup records the release that took it. `trellis-server restore`:

- restores a backup of its own release as it is;
- from the first stable release, restores a backup an earlier stable release
  took and then runs this release's forward migrations on the restored
  databases, as a start does;
- refuses a backup a newer release took;
- before the first stable release, refuses a backup of any other release,
  saying to restore it with the release that took it and move the
  installation with `trellis-server export` and `import`.

No baseline has later migrations yet, so the forward migration is the code
path a start runs, exercised once a release adds one.
