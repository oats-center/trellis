---
title: "Moving a service to another installation"
description: "Export a service with its build, instances, settings, secrets and data into an encrypted bundle, import it into another Trellis, and point its processes at the new URL."
---

# Moving a service to another installation

`trellis svc <ID> export` writes one service into a bundle file, and `trellis
svc import` adds it to another installation. The deployment keeps its ID, its
instances keep their identity keys, and its data keeps its KV revisions, so
the service's processes carry on in the new installation with the seeds they
already have. You change their Trellis URL and restart them.

A bundle holds:

- the installed build, compiled again by the new installation;
- the instances' public keys (never a seed) and the enrollment secrets'
  hashes;
- the current settings, with every secret in them;
- the webhook endpoints, with the same URL IDs;
- the data of the service's KV buckets and stores, and jobs that no worker
  has finished.

Users, roles, job history, settings history, and webhook request records stay
behind. The import prints the roles and grants that gave people access to the
service, for you to recreate; it creates none itself.

Both commands need `users.manage` and `config.manageSecrets`, and
`platform.manage` too for a service that holds an administrative capability.
Both are written to the security audit of their installation.

## Before you start

Create the accounts the service's users need in the new installation, and add
any package registry its build pulls from. Then try the whole move once
against a spare installation: it tells you how long the copy takes and what
the import reports, and nothing in either real installation changes.

Export never changes the installation it reads. Whatever the service writes
after the export has read it is not in the bundle, so for the real move you
stop the service's processes first.

## Name each installation

The moved deployment keeps its ID, so `orders` names a deployment in both
installations, and a command meant for the old one does the same thing to
the new one if the CLI is signed in there. Give each installation a profile
of its own and name the profile in every command. A profile is a saved
login; `--profile` picks which one a command uses.

```sh
trellis --profile old login https://old.example.org
trellis --profile new login https://new.example.org
trellis --profile old whoami
trellis --profile new whoami
```

`whoami` shows the server a profile acts on. Every command below names its
profile, so you can run them in one shell in any order and each reaches the
installation it says.

## Export

Stop the service's processes, then export from the old installation:

```sh
trellis --profile old svc orders export --out orders.trellis-service
```

The CLI asks for a passphrase twice. It encrypts the bundle with it as the
bundle arrives, so the file is never on disk unencrypted and the passphrase
never leaves your machine. Anyone with the bundle and the passphrase can read
every secret of the service, so use a long passphrase (at least 12
characters) and send it apart from the file. In a script, give it with
`--passphrase-file PATH`, or `-` to read standard input.

The export prints the bundle's SHA-256. Give it to whoever imports, through a
channel you trust: it is how they know the file is the one you exported.

The export writes a temporary file beside `--out` and moves it into place
once the bundle is whole. It refuses to replace a file, including one that
appeared while it ran, such as another export to the same name; the file
that was there stays as it was.

## Import

```sh
trellis --profile new svc import orders.trellis-service --digest <sha256> --check
trellis --profile new svc import orders.trellis-service --digest <sha256>
```

`--check` verifies the bundle and runs the import in a transaction that is
rolled back, so you see what it would do. Without `--digest`, the CLI shows
the file's SHA-256 on a terminal and asks you to confirm it; a script must
pass it. A wrong passphrase, or a file that changed since the export, is
refused before the new installation changes.

The import refuses, and names what is in the way, when the deployment's ID
already exists in the new installation, when the same service is already
deployed there, or when one of its instance keys, enrollment secrets or
webhook URLs is already in use. An import never replaces a deployment.

It ends with a report:

- the webhook URLs at the new origin, to give to senders;
- the roles and direct grants to recreate;
- what stayed behind, such as events a consumer had not processed yet.

If the import stops while it loads the data, run the same command again; it
picks up where it stopped.

## Point the processes at the new installation

Change each process's Trellis URL and start it. It keeps its identity seed.
If the new installation's certificate is self-signed, also set
`TRELLIS_CERTIFICATE_FINGERPRINT` to its fingerprint. Processes that enroll
themselves keep their `TRELLIS_ENROLLMENT_SECRET`: the same secret works in
the new installation.

A process already running does not move on its own. It stays with the
installation it first connected to, so restart it even if the new
installation took over the old hostname.

## Finish the move

These steps act on the old installation, so each names the `old` profile.

1. Disable the old deployment. Until then, both installations accept the
   instances' keys and the enrollment secret.

   ```sh
   trellis --profile old svc orders disable
   ```

2. Give webhook senders their new URLs, unless the hostname moved.
3. Create the roles and grants the report listed, in the new installation,
   and give them to the users who need them. Users sign in to the new
   installation and consent to the apps they use.
4. Once the new installation has run the service long enough to trust it,
   remove the old deployment, then destroy its resources:

   ```sh
   trellis --profile old svc orders remove
   trellis --profile old resources list
   trellis --profile old resources destroy <resource-id> \
     --expected-revision <revision> --confirm-physical-id <physical-id>
   ```

`remove` asks you to type the deployment's name, and says which
installation it acts on. For a deployment imported into an installation in
the last day, `disable` asks the same way and `remove` adds a warning, since
that is the deployment the cleanup would take down by mistake. Pass `-f` to
skip the question in a script.

Until step 4, going back is enabling the old deployment again and pointing
the processes back at the old URL. Anything written in the new installation
since the export stays there.

Services that call each other move together: stop them all, export each,
import the ones that provide APIs before the ones that call them, then start
them all.
