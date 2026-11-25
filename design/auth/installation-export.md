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
database first, as a start would (a preview migrates a copy instead). Before either reads data from NATS or
changes anything, it confirms, as `backup` and a start do, that the broker
records this installation and the JetStream identity of its platform
database; a broker that has no record, belongs to another installation, or
was bound to another platform database is refused, and nothing is read or
written.

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
records/<kind>.json         { "kind", "format", "records": [...] }, one or more per record kind
deployments/<id>.bundle     the content of one service bundle per service or device deployment
```

Each entry is a 2-byte name length, the name, and chunks of a 4-byte length
and that many bytes, ended by a zero length. The passphrase proves the file
whole: a changed or cut file fails a chunk's tag.

An import holds a record entry in memory and reads at most 256 MiB of one.
An export writes no larger entry: a kind with more records than that is
split across several `records/<kind>.json` entries, which the import joins
in order, and a single record larger than that fails the export before the
file is placed.

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

| Kind                | What it carries                                                                                                                  |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `signInSettings`    | Local users on or off, local sign-up, password minimum length                                                                    |
| `oidcProviders`     | Each live provider with its ID, issuer, client ID, client secret (opened), pinned certificate and settings                       |
| `packageRegistries` | Each registry with its prefix, username and secret (opened)                                                                      |
| `users`             | ID, state, username and password hash, OIDC sign-ins with their provider's issuer, profile, and whether it is the administrator  |
| `roles`             | Each active role with its ID, name, description, new-user flag, capabilities and members                                         |
| `grants`            | Each capability granted directly to a user                                                                                       |
| `oidcRoleMappings`  | Each mapping from a provider claim to roles                                                                                      |
| `oauthClients`      | Each app with its client ID, kind, redirect URIs, implied consent and sign-up settings                                           |
| `consents`          | Remembered consent of users to apps, for capabilities whose wording has not changed since                                        |
| `portalThemes`      | The installation's theme and each app's                                                                                          |
| `state`             | Each live user's State resources with their representation, registration revision and history, and value; each device's values   |
| deployments         | Every service and device deployment that is not removed and not Trellis's own, enabled or disabled as it is, as a service bundle |

Trellis's apps are public clients (PKCE and DPoP), so an app has no secret to
carry; its client ID is its identity, and it keeps it. The Console and CLI
clients are registered by each installation for its own origin, so only their
sign-up settings move.

A device deployment travels in a service bundle like a service: its build,
its instances' public keys and its settings. A device whose key was
registered connects to the new installation with its seed, without activating
again.

**State.** State is data apps and devices keep through Trellis, so leaving it
behind would break them. A user's State record names its owner by user ID,
its namespace (the app's client ID, and a browser app's origin), its
resource name, its representation (the declaration `State.Register`
stored), its registration revision and the history of its representations,
and its value as stored, with the times it was created and last written. A
device deployment's record names the deployment, its participant and the
resource name, and holds each device's value under the device's principal,
which keeps its ID. The value's revision is the State bucket's sequence
number, which belongs to the installation; the new installation gives each
value a new revision, and a client that still holds an old revision gets
the usual conflict with the current value. The registration revision moves,
so an app that registers with the revision it knows finds the same
registration.

An export reads the State bucket without creating it. When State resources
are registered and the broker has no State bucket, their values are lost or
out of reach, so the export fails and says so rather than carrying the
values as empty. State keeps a value until it is deleted, so neither side
uses a bucket that expires values: an export from one fails, and an import
that carries State values refuses one before it changes anything.

## Import

Import reads the whole file first, spooling each bundle to an encrypted
temporary file, and verifies each bundle as a service import does before it
changes anything. Each export and import keeps its temporary files in a
folder of its own beside the platform database, made for that run and
removed when it ends, so runs that overlap never touch each other's files.
It then applies every record in one Auth transaction, as
the installation's administrator, in this order: sign-in settings, OIDC
providers, registries, users, deployments (through the service import,
without their own access records), roles and grants, OIDC role mappings,
apps, consent, themes, and users' State. Each record is applied in a
savepoint: one that existing data is in the way of is rolled back, reported
as skipped with its reason, and the rest goes on. A failure of the database
itself stops the import and changes nothing.

After the transaction commits, each deployment's data loads as a resumed
service import: its KV and store snapshots, then, once an instance can run,
its resources and waiting jobs. State values are written last, each where no
value is stored yet: a user's under the State resource the transaction
recorded, a device's under its deployment's State resource once that is
ready. A deployment whose data did not finish loading is reported under
`pending` with what finishes it: one with no instance that can run here
(the deployment or every instance is disabled, or it has none) has its
resources prepared and its waiting jobs moved once the deployment and an
instance are enabled, or one is provisioned, and the import runs again; one
whose loading failed finishes when the import runs again. Records already
there are found the same the second time and change nothing. The text
output lists the pending items under "Not finished yet" with that step.

`--check` changes nothing here, the database's schema included. It copies
the platform database into its working folder, migrates the copy as the
import would migrate the database, runs the same transaction there and rolls
it back, and loads nothing; it only reads the database, NATS and the State
bucket. It reads the State value stored under each key the import would
write, so a value stored here that differs is reported as skipped, as the
import would skip it, and one the same as the export's is not reported. A
device's State waits for its deployment's data, which a preview does not
load, so it is reported as pending.

### What the import decides

Import starts by listing what the installation already has besides what its
own setup made (its default administrator and that administrator's role):
users, OIDC providers, roles, apps, deployments, themes and registries. The
report shows the list, and the CLI warns when it is not empty. What it does
with each record follows three rules.

**Settings take the export's values.** The installation-wide settings are
the intent of whoever ran the old installation, so they replace what this
installation has, even where its administrator changed them. The report
lists each replaced setting with its old and new value (`changed`); a secret
is never shown, only whether it changed.

- **Sign-in settings**: local users, local sign-up and the password minimum.
- **OIDC providers.** A live provider with the same ID and issuer, or one
  with the same issuer under another ID, is the same provider: it takes the
  export's name, client ID, client secret, pinned certificate, settings and
  state, keeping its own ID, and the export's sign-ins through it use it. A
  new issuer adds the provider under its ID. A provider whose ID names
  another issuer here, or a deleted provider, is a conflict: it is left as it
  is, and the sign-ins through it are not brought.
- **Portal themes**: the installation's theme, and each carried app's.
- **Who may sign up** through the Console and the CLI, which each
  installation registers for itself.

**The same person, or the same record, gets what the export gives it.** A
user is found as access records find one
([service-export.md](./service-export.md#access-roles-grants-and-consent)):
the user with the same ID whose sign-ins agree, or, with no user under that
ID, the one user with the same OIDC subject at a provider with the same
issuer. Such a user is not created again and keeps its account as it is,
but receives the export's roles, grants, consent and State (`matched` in the
report, with how it was found). A user found nowhere is created with its ID,
password hash, sign-ins and profile. A sign-in through an OIDC provider the
import did not bring here, such as one deleted on the old installation,
whose links a deletion keeps, or one skipped as a conflict, cannot be used:
it is left out and reported (`userSignIn` under `skipped`), and the account
is still created with its other sign-ins. An app with the same client ID and the
same settings, a registry with the same name and settings, an OIDC role
mapping with the same ID and contents, and a role with the same ID, name and
description whose capabilities are all in the record are the same record;
the role gets the capabilities it is missing.

**Anything else in the way is a conflict: a warning, and nothing changes.**
The record is skipped with its reason, the existing one is left as it is,
and nothing of the export attaches to it.

- **A user** another user is in the way of: another user with the same
  username (a username names an account, not a person), the same user ID
  with another username or with a sign-in another user has, a user ID that
  names a service, a device or a deleted user, or OIDC sign-ins that belong
  to several users. No role, grant, consent or State goes to the existing
  account.
- **An app** with the same client ID and other settings. The consent, State
  and theme that name it are skipped.
- **A role** with the same ID and another definition, with its members, and
  the OIDC role mappings' links to it; **a registry** or **an OIDC role
  mapping** with the same name or ID and other contents, or a mapping of the
  same claim.
- **A State resource** the same user has here with another representation,
  and **a State value** where another value is stored.
- **A deployment** the service import refuses, for example because its ID
  or one of its instance keys is in use.

**The administrator.** The exported administrator is not imported as a
user, into an empty installation or one in use. This installation keeps its
own administrator, which receives the roles and grants the exported one
held, and the report's `administrator` names both and what was given. The
exported administrator's consent and State stay behind, with the reason in
the report.

**Consent** is carried for every user created or found as the same person,
to apps this installation has and that were not skipped, for capabilities
whose wording has not changed.

**State** is recorded under the user it belongs to here. Its resource is
named by that user's ID, the app's namespace and the resource name, as
`State.Register` names it, so a user found under another ID gets the State
under its own. State whose user or app was skipped is skipped with them.

### The report

```text
applied        false for --check
source         installation ID, origin, Trellis version and export time
target         { empty, existing: counts by kind }
created        { kind, id } for each record created, State value written included
matched        { kind, id, userId, matchedBy } for each user or app found here as the same
changed        { kind, id, old, new } for each setting replaced with the export's
skipped        { kind, id, reason } for each record skipped
pending        { kind, id, reason } for each record that waits for a later step, and that step
administrator  the exported administrator's ID, this installation's, and what it received
access         recreated and skipped roles, members and grants, as a service import reports them
deployments    each deployment's service import report, or the error that stopped its data loading
notCarried     what an installation export leaves out
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
- Device activation reviews, and device provisioning secrets of devices not
  yet activated.
- The exported administrator's account, its consent and its State; its roles
  and grants go to this installation's administrator.
- Deleted users, whose IDs the old installation keeps only as audit
  tombstones, and APIs that no carried deployment provides.
- State revision tokens, which each installation assigns.

## Security

The file holds every secret of the installation: password hashes, OIDC client
secrets, registry secrets, every service's settings secrets, and the State
users and devices keep. It is sealed
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
