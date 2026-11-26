---
title: "Moving an installation"
description: "Export a whole Trellis installation, with its users, roles, apps, sign-in setup, themes, services and devices, and import it into another installation or a newer release."
---

# Moving an installation

`trellis-server export` writes a whole installation's configuration into one
file, and `trellis-server import` adds that file to another installation.
Users keep their Trellis user IDs, so services that stored them need no
change, and services and devices keep their keys, so they reconnect with the
seeds they have. The file holds plain records rather than database files, so
it also moves an installation to a newer release, including from one release
candidate to the next, which a backup cannot do.

The file carries:

- users, with their IDs, usernames and password hashes, OIDC sign-ins and
  profiles;
- roles, their members, and capabilities granted directly to users, as far
  as the old installation still gives them: expired grants and role
  assignments, and capabilities its catalog no longer has, stay behind;
- the sign-in settings, the OIDC providers with their client secrets, and
  their role mappings;
- apps, with their client IDs, and the consent people gave them; where the
  new installation describes what a capability grants differently, people
  are asked again;
- the State apps and devices keep for people and devices;
- Portal themes and package registries;
- every service and device deployment, with its build, keys, settings and
  secrets, disabled if it was disabled, and each service's data and waiting jobs, the way
  [`trellis svc export`](/guides/move-a-service) carries one.

It leaves out sessions, so everyone signs in again; the NATS operator and
credentials, the TLS certificates and the issuer signing keys, which the new
installation makes for itself; history such as the audit and finished jobs;
unused password setup links; and the old administrator's account, whose roles
and grants go to the new installation's administrator.

## Export

Run the export on the old installation's host. The server may keep running;
its NATS has to be reachable.

```sh
trellis-server export --out trellis.trellis-installation --passphrase-file pass.txt
```

The file holds every secret of the installation, so the passphrase needs at
least 12 characters. Give it with `--passphrase-file PATH`, or `-` to read
standard input. The export never replaces an existing file.

For the final move, stop the services first, so that nothing they write after
the export is lost, as for a [service move](/guides/move-a-service).

## Import

Set up the new installation and start it once, so that it has its keys and
database, then stop the server. Leave its NATS running. Check the import
first; it changes nothing. It works on a copy of the database, which takes as
long as the database needs; it fails only if other programs keep changing or
locking the database so the copy makes no progress for 10 seconds:

```sh
trellis-server import trellis.trellis-installation --passphrase-file pass.txt --check
trellis-server import trellis.trellis-installation --passphrase-file pass.txt
```

Then start the server. Point each service and device at the new URL and
restart it with the seed it has; set `TRELLIS_CERTIFICATE_FINGERPRINT` if the
new certificate is self-signed.

The old administrator account is not imported. The administrator you created
when you set up the new installation stays, and receives the roles and grants
the old one held; the report's `administrator` entry lists them. People's
State comes along, so apps and devices find what they stored; each value gets
a new revision token.

The export reads State from the old installation's NATS without changing
it. If people or devices registered State and that NATS has lost the State
bucket, for example after its JetStream was reset, the export stops instead
of leaving their values out: restore the JetStream from a backup first.

## Importing into an installation in use

You can import into an installation that already has users, apps or
services. The import lists what is there before it starts, then works through
the file with three rules.

**Settings take the file's values.** The sign-in settings, the OIDC providers'
settings and client secrets, the Portal themes, and who may sign up through
the Console and CLI become the old installation's, even where this one changed
them. The report's `changed` list shows each old and new value (never a
secret). A provider keeps the state it had: one disabled there arrives
disabled, with the sign-ins and role mappings through it, and they work once
you enable it.

**The same person gets what the file gives them.** A user with the same
Trellis user ID and the same username, or who signs in with the same OIDC
account (the same subject at a provider with the same issuer), is the same
person. Their account stays as it is, and they get the roles, grants, consent
and State the file has for them. The report lists them under `matched`.

**Anything else in the way is skipped with a warning, and nothing changes:**

- a user whose username another account here has, or whose ID belongs to an
  account with another username; that account gets nothing;
- an app whose client ID is in use with other settings, with the consent,
  State and theme that belong to it;
- a role under the same ID with another name or other capabilities, with its
  members;
- an OIDC provider whose ID is used by another issuer, with the sign-ins
  and role mappings through it; a person who signed in through it, or through
  a provider the old installation deleted, still moves with their other
  sign-ins, such as their password, and a role mapping through a deleted
  provider is left out;
- a registry or role mapping that exists with other settings;
- State the same person has here in another form, or a value where another
  one is stored;
- a deployment whose ID or keys are in use.

The report lists each skipped item with the reason. A username alone never
makes two accounts the same person: to have the import treat an existing
account as someone from the old installation, give it the same user ID with
`trellis users create --id`.

Both commands check first that the NATS they reach belongs to the
installation and to its database, as `trellis-server backup` does. If
`[nats] servers` names another installation's broker, or one that was reset,
they stop before reading or changing anything.

Some data cannot move in one run. The import lists it under "Not finished
yet", with what finishes it:

- a service whose deployment or instances are all disabled, or that has no
  instance, gets its records, but its resources and waiting jobs move only
  once it has an instance that can run: enable the deployment and an
  instance, or provision one, then run the same import again;
- a service whose data stopped loading partway finishes when you run the
  same import again.

Records already imported are skipped, and the data loads where it stopped.
`--check` already lists a service with no instance that can run, with the
same step. It does not load data, so a load that stops partway shows only
when you run the import.

## Backups and releases

A backup restores the same installation on the same release. Once Trellis has
stable releases, a backup an earlier stable release took restores onto a
newer one and is migrated forward. Before then, `trellis-server restore`
refuses a backup from another release candidate: restore it with the release
that took it, then move the installation with an export and import.
