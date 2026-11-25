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
  profiles, and the administrator account;
- roles, their members, and capabilities granted directly to users;
- the sign-in settings, the OIDC providers with their client secrets, and
  their role mappings;
- apps, with their client IDs, and the consent people gave them;
- Portal themes and package registries;
- every service and device deployment, with its build, keys, settings and
  secrets, and each service's data and waiting jobs, the way
  [`trellis svc export`](/guides/move-a-service) carries one.

It leaves out sessions, so everyone signs in again; the NATS operator and
credentials, the TLS certificates and the issuer signing keys, which the new
installation makes for itself; history such as the audit and finished jobs;
unused password setup links; and the values of device State.

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
first; it changes nothing:

```sh
trellis-server import trellis.trellis-installation --passphrase-file pass.txt --check
trellis-server import trellis.trellis-installation --passphrase-file pass.txt
```

Then start the server. Point each service and device at the new URL and
restart it with the seed it has; set `TRELLIS_CERTIFICATE_FINGERPRINT` if the
new certificate is self-signed.

In a fresh installation, the old administrator takes the place of the one
created when you set up the new installation, and signs in with the same
username and password.

## Importing into an installation in use

You can import into an installation that already has users, apps or
services. The import lists what is there before it starts, then adds
everything it can and skips what is in the way:

- a user whose ID, username or OIDC sign-in another user has here;
- an app whose client ID is in use;
- a role under the same ID with another name or other capabilities;
- an OIDC provider whose ID is used by another issuer;
- a deployment whose ID or keys are in use;
- the sign-in settings and the theme, when this installation set its own.

The installation keeps its own administrator, and the old administrator
becomes an ordinary user. The report lists each skipped item with the reason,
and roles and grants go to the people found here by user ID, or else by the
same username or OIDC sign-in.

If loading a service's data stops partway, run the same import again. Records
already imported are skipped, and the data loads where it stopped.

## Backups and releases

A backup restores the same installation on the same release. Once Trellis has
stable releases, a backup an earlier stable release took restores onto a
newer one and is migrated forward. Before then, `trellis-server restore`
refuses a backup from another release candidate: restore it with the release
that took it, then move the installation with an export and import.
