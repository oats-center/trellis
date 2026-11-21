---
title: Moving a Service Between Installations
description: Proposal to export one service deployment, with its build, instance keys, settings, secrets, and data, and import it into another Trellis installation.
---

# Design: Moving a Service Between Installations

> **Status: proposal awaiting review.** Nothing in this document is
> implemented. It proposes how to export one service from a Trellis
> installation and import it into another one. The
> [open questions](#open-questions) at the end each carry a recommendation for
> review to accept or change.

Trellis can already back up a whole installation with `trellis-server backup`
and put it back with `trellis-server restore`. That restores everything at
once, into a place that becomes the same installation again. It cannot take
one service out of an installation and add it to a different one.

Operators need that when they split one installation into two, consolidate
two into one, move a service to new hardware run by another team, or start a
fresh installation and bring services over one at a time. Today the only way is
to create a new deployment by hand, provision new keys for every process, type
every setting and secret again, and find some way to copy the data. The
[workspaces](../contracts/workspaces.md) proposal says the same thing in its
migration section: data moves only if the service itself offers an export.

This proposal adds `trellis svc <id> export` and `trellis svc import`. An
export is one encrypted file that holds a service deployment, its build, its
instances' public keys, its settings and secrets, and its data. Importing that
file into another installation recreates the deployment there. The service's
processes keep their own identity seeds and reconnect to the new installation
once they are pointed at its URL.

## Prerequisites

- [trellis-auth.md](./trellis-auth.md): deployments, instances, identity keys,
  and admission
- [service-updates.md](./service-updates.md): installed builds, revisions, and
  approval
- [instance-enrollment.md](./instance-enrollment.md): enrollment secrets
- [config-patterns.md](../core/config-patterns.md): settings, secrets, and the
  storage key
- [kv-resource-patterns.md](../core/kv-resource-patterns.md),
  [store-resource-patterns.md](../core/store-resource-patterns.md), and
  [data-retention.md](../core/data-retention.md): resources and their data
- [webhooks.md](../core/webhooks.md): webhook endpoints and their URLs
- [tooling/trellis-cli.md](../tooling/trellis-cli.md#recovery): the
  whole-installation backup this builds on

## Terms

A few terms come from the rest of the design. They are repeated here so this
document can be read on its own.

- **Installation.** One Trellis server (or a set of split roles) with its own
  Auth database, NATS broker, and `secrets.toml`. Each has an installation ID
  and a public origin, the HTTPS URL that services, apps, and the CLI connect
  to.
- **Deployment.** One installed service. It records the build it runs, the
  capabilities it was granted, and the resources it owns. Its ID is the name
  the administrator gave it, such as `orders`.
- **Installed build.** The package evidence for the service: its compiled IDL
  and the source of every API it uses. Auth keeps it as numbered revisions of
  the service's participant.
- **Instance.** One running copy of a deployment. Each instance has its own
  **identity key**, an Ed25519 key pair. The process keeps the private half,
  its **seed**. Auth stores only the public half. When the process connects,
  it signs a proof with the seed, and Auth finds the instance by the public
  key.
- **Storage key.** A 32-byte key in `secrets.toml`. Auth seals settings
  secrets, OAuth records, and registry passwords with it. Two installations
  have different storage keys.
- **Resource.** Storage a deployment owns and Trellis provisions: KV buckets,
  stores (blob storage), job queues, event consumers, and webhook intake
  streams. Each lives in the installation's NATS JetStream.
- **Bundle.** The file an export writes. This proposal defines it.
- **Import key.** A key pair the receiving installation makes so that a bundle
  can be encrypted for it alone. This proposal defines it too.

## Overview

A move between two installations, `old.example.org` and `new.example.org`,
takes three commands and a restart. The CLI talks to the installation it is
signed in to, so the administrator signs in to each in turn.

```sh
# Signed in to the new installation: make a key that bundles can be encrypted to.
trellis login https://new.example.org
trellis import-key create --title "orders move"
#   prints: trellis-import-01JF...:<public key>

# Signed in to the old installation: export the service, encrypted to that key.
trellis login https://old.example.org
trellis svc orders export --move \
  --to trellis-import-01JF...:<public key> --out orders.trellis-service
#   prints the bundle's SHA-256

# Signed in to the new installation again: import it.
trellis login https://new.example.org
trellis svc import orders.trellis-service
```

Then each of the service's processes is pointed at `https://new.example.org`
and restarted with the seed it already has.

`--move` makes the export the last one: the old deployment is disabled first,
so no instance writes anything after the export reads it. Without `--move`,
the export is a copy and the service keeps running. See
[consistency](#consistency).

## What an export contains

| Part                          | Moves? | How                                                                                      |
| ----------------------------- | ------ | ---------------------------------------------------------------------------------------- |
| Deployment record             | Yes    | ID, title, state, and its per-deployment instance policy                                 |
| Installed build               | Yes    | The package evidence of the installed revision; the new installation recompiles it       |
| Instances                     | Yes    | Instance ID, title, state, and public identity key; never a seed                         |
| Enrollment secrets            | Yes    | Their hashes, limits, and state (see [enrollment secrets](#enrollment-secrets))          |
| Settings                      | Yes    | The current revision of the deployment and of each moved instance's overrides            |
| Secrets                       | Yes    | Decrypted with the old storage key, carried encrypted in the bundle, sealed with the new |
| KV and store data             | Yes    | A JetStream snapshot of each resource's stream                                           |
| Pending jobs                  | Yes    | The deployment's unfinished work messages                                                |
| Webhook endpoints             | Yes    | Endpoint names, URL IDs, runtime settings, and the intake stream                         |
| Settings history              | No     | Only the current revision moves                                                          |
| Job, event, health history    | No     | Stays in the old installation                                                            |
| Open operations               | No     | Listed in the export report                                                              |
| Users, roles, grants, consent | No     | Listed in the export report                                                              |
| Audit records                 | No     | Stay in the old installation; both sides audit the move                                  |

### The deployment and its build

The bundle holds the deployment's record and the package evidence of its
installed revision. Package evidence is what `trellis svc create --source`
sends to Auth today: the compiled participant and the verified source of every
API it provides or uses. The new installation compiles that evidence again, as
it would for `Deployments.Create`, so it never trusts a compiled result from
the old installation.

Revision numbers are local to an installation, so the build becomes revision 1
in the new one. The participant digest, which identifies the build, stays the
same. That is what lets existing processes connect without submitting an
update.

Only the installed revision moves. An instance that still runs an older build
connects to the new installation, sees that its digest differs, and submits a
[service update](./service-updates.md), as it would after any apply.

Package identity is the full ID from [workspaces](../contracts/workspaces.md),
such as `ghcr.io/acme/orders.OrdersService`. The bundle records full IDs, and
import never rewrites them. A package with no declared workspace is a
`localhost` package in both installations.

### Instances and their keys

For each active or disabled instance, the bundle holds its instance ID, its
principal ID, its title, its state, and its public identity key. Retired and
removed instances are left out. No seed is ever in the bundle, because Trellis
never has one: the process made it, or `trellis keys new` made it on the
owner's machine.

Import registers the same public keys under the same instance IDs. A process
that still has its seed can then connect to the new installation without any
new key. This works for the following reasons:

1. The process discovers the new installation's ID and Auth issuer key from
   the origin it is given, over HTTPS. It signs its connection proof for that
   installation. Nothing in the seed ties it to the old installation.
2. The new Auth looks up the public key, finds the imported instance, and
   admits it with the imported deployment's authority.
3. The instance runs the imported build, whose participant digest matches the
   process's own, so the SDK continues without an update.

The new installation has to be trusted by the process in one way only: its TLS
certificate. A certificate from a public CA needs nothing. A self-signed or
generated certificate needs `TRELLIS_CERTIFICATE_FINGERPRINT` set to the new
installation's fingerprint, because the SDK remembers an accepted certificate
per address and the new address has none.

A running process does not switch installations on its own. The SDK keeps the
installation identity it first saw across reconnects and refuses a different
one. Each process is restarted with the new URL. If the new installation takes
over the old hostname instead, the URL stays the same, but a restart is still
needed for the same reason.

Instances that enrolled with a memory-only key cannot reconnect: their key was
lost when the process stopped. Their records move anyway, go offline, and are
retired by the usual [timed retirement](./instance-enrollment.md#retiring-offline-instances).
Their replacements enroll with the moved enrollment secret.

### Enrollment secrets

Auth stores an enrollment secret only as the SHA-256 hash of its text. Auth
checks a presented secret by hashing it and comparing, and the hash is not tied
to the installation's storage key. A moved hash therefore works: a process
that presents the same `TRELLIS_ENROLLMENT_SECRET` to the new installation
enrolls there.

The bundle carries each secret's ID, title, hash, limits, expiry, and revoked
state. Pods in a Kubernetes Deployment keep their existing secret, and only
their Trellis URL changes. Nobody can read a secret's text from the bundle,
since the text was never stored.

While the old deployment is enabled, the same secret works in both
installations. `--move` disables the old deployment, and a disabled
deployment refuses enrollment, so after a move only the new installation
accepts it. See [open question 2](#open-questions).

### Settings and secrets

The bundle holds the current settings revision of the deployment and of each
moved instance's overrides. Earlier revisions stay behind. Deleting old
revisions is how an administrator makes a replaced password stop existing, and
copying history would undo that.

Secrets are the hard part. The old installation seals them with its storage
key, which the new installation does not have and must not get. Export
therefore decrypts each secret inside the old Auth and writes it into the
bundle, which is encrypted as a whole (see [the bundle](#the-bundle)). Import
decrypts the bundle inside the new Auth and seals each secret with the new
storage key. With an import key, a secret's plain value exists only in the
memory of the two Auth processes. No administrator sees it, so secrets stay
write-only, as [config-patterns.md](../core/config-patterns.md) requires.

A passphrase-encrypted bundle is different. Anyone with the passphrase can
open it and read every secret, so a passphrase export leaves secrets out
unless `--include-secrets` is given. That option needs more authority, as
described under [who may export and import](#who-may-export-and-import).

### Resource data

Each KV bucket, store, and webhook intake stream is its own JetStream stream.
The bundle holds a JetStream snapshot of each one, the same snapshot
`trellis-server backup` takes for the whole installation. A snapshot of one
stream is consistent: every entry, revision, and timestamp as of one moment.

Import restores each stream under the same physical name. The name is built
from the resource ID and the allocation ID, and neither depends on the
installation. Because the stream comes back whole, KV revisions stay the same,
so a CAS token a service kept is still valid. History and TTLs continue from
the original timestamps. The ownership marker inside the stream also comes
back, so the new Auth's normal ownership check accepts the allocation as its
own.

Some resources do not have a stream of their own:

- **Job queues.** Work messages live in the installation's shared `JOBS_WORK`
  stream under the deployment's subjects. Export copies the deployment's
  unfinished work messages, and import publishes them on the same subjects in
  the new installation. Jobs run at least once already, so a job that was
  running at export time and runs again is ordinary behavior. Job history
  stays behind.
- **Event consumers.** A consumer is a position in the installation's event
  stream. The new installation has a different event stream, so import
  creates each consumer fresh, starting with new events. Events that the old
  consumer had not yet processed are lost to the moved service. The export
  report gives their count.
- **Operations.** Each deployment that implements Operations has a repository
  and a staging store. Their records name the users who started each
  operation, and those users do not exist in the new installation. Operations
  do not move. An export with `--move` refuses while operations are still
  open, unless `--abandon-operations` is given.

Before it writes anything, an export reports each resource's size from the
stream state. `trellis svc orders export --dry-run` prints the sizes and the
report and writes nothing. A store of 40 GiB makes a bundle of about 40 GiB:
the bundle compresses JSON parts but stores snapshots as JetStream wrote them.

### Webhook endpoints

A webhook URL is `https://<origin>/webhooks/<service name>/<random id>`. The
service name is fixed when the endpoint is first declared, and the random ID
is 128 random bits. The bundle carries both, for the current URL and for any
earlier URL that has not been retired, along with the endpoint's runtime
settings.

Import keeps both, so only the origin changes. If the new installation takes
over the old hostname, senders need no change at all. Otherwise each sender
has to be given the new URL, and the import report lists every URL to update.

The deployment's intake stream moves with the other resources, so requests
still waiting for verification are verified in the new installation. The
deployment's entries in the shared dedupe bucket move as well, so a sender
that retries a request across the cutover is still recognized as a duplicate.
Request records (`webhooks.sqlite`) stay behind with the rest of the history.

Verification secrets are ordinary settings secrets and move as described
above.

### Access: roles, grants, and consent

Access in Trellis is held by principals: users, services, and devices. The
users of the old installation are not in the new one unless they are created
there too. That decides what can move.

- **The deployment's own grants move.** These are the capabilities the
  service selected in its IDL and an administrator approved, such as the right
  to call another service's API. They come from the build, so the new
  installation derives them from the imported evidence. The importer approves
  them during import, as for a new deployment.
- **The APIs the service provides are accepted** in the new installation's
  catalog from the build's evidence, as `Deployments.Create` does.
- **Roles are not created.** A role belongs to the installation and often
  mixes capabilities from many APIs. The export report lists every role that
  holds a capability of an API the service provides, with the capabilities
  involved, so the administrator can create matching roles. See
  [open question 5](#open-questions).
- **Role assignments, direct grants, OAuth grants, and remembered consent do
  not move.** They name users and apps of the old installation. Consent is
  also bound to an app's key in the old installation. Users of the moved
  service sign in to the new installation and consent there.
- **Other services' grants are their own.** A service in the old installation
  that calls the moved service keeps its grant, but it has nothing to call
  once the service is gone. See [the procedure](#migration-procedure).

## The bundle

A bundle is a tar archive inside one encrypted envelope, written to a file
named `<deployment>.trellis-service` by default.

Inside the envelope:

```text
manifest.json         format, versions, source installation, times, every part with its SHA-256
deployment.json       deployment record, instance policy, webhook endpoints and settings
evidence/             package evidence documents of the installed build
instances.json        instances and their public keys, enrollment secrets
settings.json         current settings of the deployment and instance overrides
secrets.json          secret values, when included
resources.json        each resource's ID, allocation ID, kind, local name, and approved commitment
resources/<id>.snap   one JetStream snapshot per resource stream
jobs/<queue>.jsonl    unfinished work messages per queue
report.json           what stayed behind, and what the importer must do by hand
```

**Encryption.** The whole archive is encrypted, not just the secrets. Resource
data can be as sensitive as a password, and encrypting everything means no
part of the file leaks by mistake. The envelope is encrypted to one of two
kinds of recipient:

- an **import key** made by the receiving installation (the default), or
- a **passphrase**, for a bundle that has no receiving installation yet, such
  as a backup of one service kept for later.

`trellis import-key create` makes an import key in the receiving
installation. Auth generates the key pair, seals the private half with its
storage key, and prints the public half once, with the key's ID. The key
expires after 7 days by default and can be removed earlier with `trellis
import-key remove`. A bundle encrypted to it can be imported only into that
installation, and only while the key exists. The exporting administrator
needs the public half and nothing else. It is safe to send by email or chat;
the only risk is a swapped key, which the [security](#security) section
covers.

**Integrity.** The envelope's authenticated encryption detects any change to
the file. Inside it, `manifest.json` records the SHA-256 of every part, as the
whole-installation backup does, and import checks all of them before it
changes anything. The export prints the SHA-256 of the finished file. Import
shows the same digest and asks the administrator to confirm it, or takes it
as `--digest` in a script.

**Versioning.** `manifest.json` has a `format` number and the Trellis version
that wrote it. Import refuses a format it does not know. From the first stable
release, every release imports bundles that any earlier stable release wrote,
the same promise Trellis makes for upgrades. Release candidates may refuse each
other's bundles. A bundle written before the workspaces change cannot be
imported after it, since every ID changed.

## Commands and Console

```text
trellis import-key create [--title <text>] [--expires <date|duration>]
trellis import-key list
trellis import-key remove <key-id>

trellis svc <id> export --to <import key> | --passphrase
                        [--move] [--live] [--include-secrets]
                        [--abandon-operations] [--dry-run] [--out <file>]
trellis svc import <file> [--digest <sha256>] [--passphrase]
                   [--acknowledge-shared-apis] [--acknowledge-unauthenticated-webhooks]
                   [--check]
```

`export` refuses while any instance is connected unless `--move` (which
disconnects them by disabling the deployment) or `--live` is given. `--live`
and `--move` cannot be combined. `--passphrase` asks for the passphrase twice
on a terminal, or reads it from standard input.

`import --check` decrypts and verifies the bundle, then prints what the import
would do and what it would need approved, without changing anything.

Export and import run as Auth Operations, the existing mechanism for
long-running work a caller watches. The bundle moves between the CLI and Auth
through the Operation's file transfer, so a 40 GiB bundle is streamed and
never held in memory. The CLI shows progress and can be interrupted and run
again. An interrupted export leaves no file; an interrupted import is
described under [import](#import).

The Console offers the same two actions. A service's page gets **Export**,
which asks for an import key or a passphrase, runs the export, and downloads
the file. The services list gets **Import service**, which takes the file,
shows what the import will do (the same summary as `--check`), and asks for
approval. The Console's installation settings list import keys and create new
ones.

### Who may export and import

No new capability is needed. Each step uses the capabilities that already
guard the same actions.

| Action                                                                   | Requires                                                                   |
| ------------------------------------------------------------------------ | -------------------------------------------------------------------------- |
| `import-key create`, `list`, `remove`                                    | `trellis.auth.v1:users.manage` and `config.manageSecrets`                  |
| Export to an import key                                                  | `users.manage`, and `config.manageSecrets` when the deployment has secrets |
| Export with `--passphrase --include-secrets`                             | `trellis.auth.v1:platform.manage` as well                                  |
| Import                                                                   | `users.manage`, and `config.manageSecrets` when the bundle has secrets     |
| Export or import of a deployment that holds an administrative capability | `platform.manage` as well                                                  |

Export to an import key gives no one a way to read a secret, so it needs only
the right to manage secrets. A passphrase export with secrets does let its
holder read them. That is more than `config.manageSecrets` allows, so it needs
`platform.manage`, the capability for decisions about who controls the
installation.

### Audit

Each step writes a security audit entry in the installation where it happens:

- `importKey.created` and `importKey.removed`, with the key ID;
- `deployment.exported`, with the actor, the deployment, the recipient (an
  import key ID or `passphrase`), whether secrets were included, whether it was
  `--move` or `--live`, the bundle's SHA-256, and the resource sizes;
- `deployment.imported`, with the actor, the source installation ID and
  origin, the bundle's SHA-256, and the import key used; and
- `deployment.importRefused`, with the reason.

Secret names appear in the entries, never their values. The two installations'
entries can be matched by the bundle's SHA-256.

## Import

Import works through these steps. It changes nothing until step 4.

1. **Decrypt and verify.** The new Auth decrypts the bundle with the import
   key or the passphrase, checks every SHA-256 in the manifest, and checks the
   format.
2. **Compile.** Auth compiles the package evidence, exactly as for
   `Deployments.Create`.
3. **Check for conflicts.** Import refuses, naming the conflict, when:
   - a deployment with the same ID exists;
   - an instance ID, principal ID, or public identity key in the bundle is
     already registered (this also catches importing the same bundle twice);
   - a webhook URL ID is already in use;
   - a resource stream with the same physical name exists;
   - an API the build provides or uses is accepted in a form the build is not
     compatible with, as `Deployments.Create` would refuse it.

   A package with the same short ID from a different workspace is a different
   package and is no conflict. The CLI and Console then show both by their full
   IDs, as [workspaces](../contracts/workspaces.md#where-each-form-appears)
   describes. An API the build provides that another deployment in the new
   installation already serves needs `--acknowledge-shared-apis`, and a
   `verify none` webhook endpoint needs `--acknowledge-unauthenticated-webhooks`,
   the same acknowledgements `Deployments.Create` asks for.

4. **Approve and create.** The administrator approves the import after seeing
   the same summary that adding a service shows. The import is that approval:
   no pending service update is left behind. In one Auth transaction, the
   import creates the deployment, installs the build, accepts the APIs it
   provides and the unaccepted APIs it uses, grants the deployment its
   selected capabilities, registers the instances and their keys, writes the
   enrollment secrets, writes the settings, seals the secrets, and records the
   webhook endpoints. The resources are recorded as `pending`.
5. **Load the data.** Auth restores each resource snapshot into JetStream,
   publishes the pending jobs, and copies the webhook dedupe entries. Each
   resource becomes `ready` once its data is in place. If this step fails,
   running the same import again resumes it: Auth finds the deployment it
   created from this bundle by the bundle's SHA-256, deletes any stream it had
   only partly restored, and loads the rest. A resource does not become
   `ready` empty, following the existing rule that a missing stream after
   materialization is storage loss, not something to recreate.
6. **Report.** Import prints the report: webhook URLs to give senders, roles
   to create, APIs the service uses that no deployment provides in the new
   installation, and what stayed behind in the old one.

The deployment starts enabled. Its processes connect when they are pointed at
the new installation.

## Consistency

A service writes to several resources, and some of its writes depend on each
other. An order row in KV may refer to a file in a store. An export is only as
consistent as the service was still while it ran.

**A quiesced export** (`--move`) disables the deployment first. Disabling
already retires the deployment's sessions and refuses new connections, and its
webhook endpoints answer 503 and store nothing. Export then waits until every
instance's connection has closed, and only then takes the snapshots. Nothing
writes to the deployment's resources while they are read, so together they
form one consistent state. Callers get errors while the service is down, and
webhook senders get 503 and retry later, as they would during any outage.

**A live export** (`--live`) takes each snapshot while the service runs. Each
resource is consistent alone, but two resources are captured at different
moments. A write that lands after its resource was read is not in the bundle.
A KV entry may then refer to a file the store snapshot does not have. The
manifest records `quiesced: false`, and import shows a warning. A live export
suits a rehearsal or an extra backup. It does not suit the final step of a move.

Settings are read in the same Auth transaction that starts the export, so they
are consistent with each other in both modes. A setting changed after that
transaction is not in the bundle.

What is lost in either mode:

- events published for the service that its consumers had not yet processed;
- events published for it during the move, which arrive in the old
  installation's event stream;
- open operations, with `--abandon-operations`; and
- in a live export, every write made after its resource was read.

## Migration procedure

This procedure moves a running service with as little downtime as the size of
its data allows. Downtime runs from step 5 to step 7, and most of it is the
time to copy the data.

1. **Prepare the new installation.** Install it, create the administrator, and
   create the user accounts the service's users need. Add registry
   credentials if the service's dependencies are pulled from a private
   registry.
2. **Rehearse.** Make an import key in a spare installation, take a `--live`
   export, and import it there with `--check`, then for real. Read the report.
   This shows how long the copy takes and what needs approval, without
   touching the real target.
3. **Create the import key** in the new installation and give its public half
   to whoever runs the export.
4. **Prepare the processes.** For each process, have the new URL ready, and
   the new certificate's fingerprint if the certificate is not from a public
   CA. If the new installation takes over the old hostname, have the DNS change
   ready instead.
5. **Export with `--move`.** The service stops serving here. Note the digest.
6. **Import** into the new installation and confirm the digest. Approve.
7. **Point the processes at the new installation and restart them.** The
   service serves again as soon as the first instance connects.
8. **Update outside senders.** Give webhook senders their new URLs, unless the
   hostname moved. Create the roles the report lists and assign them. Users
   sign in to the new installation and consent to the apps they use.
9. **Clean up the old installation** once the new one has run long enough to
   trust it. Remove the old deployment with `trellis svc remove`, then destroy
   its detached resources with `trellis resources destroy`.

A service rarely stands alone. Services in the old installation that call the
moved service, and apps whose users reach it there, lose it at step 5. Move
the services that depend on each other together: export each with `--move`,
import each, then restart all their processes. Import providers before the
services that call them, so that the callers' used APIs are already accepted.

**Rolling back.** Until step 9, the old installation still has the deployment
and its data. Enabling the old deployment again and pointing the processes
back restores the service as it was at step 5. Anything written in the new
installation since then is not carried back.

## Security

A bundle is as sensitive as the most sensitive thing in it: the service's
secrets and all of its data. These rules protect it.

- **Encrypted from the first byte.** Auth encrypts the archive as it writes
  it. The CLI receives only encrypted bytes, and an unencrypted bundle never
  exists on disk.
- **Encrypted to one installation by default.** A bundle made for an import
  key opens only in the installation that holds the private half, and only
  until the key expires or is removed. A stolen bundle is useless elsewhere.
- **Secrets stay write-only.** With an import key, no person can read a
  secret from the bundle. A passphrase bundle carries secrets only on request
  and only for a `platform.manage` holder, and the audit entry records it.
- **No seeds.** The bundle holds public identity keys only. A stolen bundle
  cannot impersonate an instance.
- **A swapped key.** If someone gives the exporting administrator the wrong
  public key, the bundle is encrypted for that person. The import key's ID and
  public half are short enough to read over the phone, and the exporting
  administrator should confirm them with the receiving administrator through a
  channel they trust.
- **A swapped bundle.** Anyone with the public half of an import key can make a
  bundle for it, including a bundle that registers their own key as an
  instance. The digest check stops this: the importing administrator confirms
  the digest the export printed. Import also shows the source installation ID
  and every instance key before approval.
- **Two installations that accept one key.** After a plain export, the old
  and new installations both accept the instances' keys and the enrollment
  secrets. `--move` ends that by disabling the old deployment. After a plain
  export, the administrator decides which installation stays live.
- **Audit on both sides.** Every export and import is recorded with the
  bundle's digest, so a bundle found later can be traced to the export that
  made it.

## Out of scope

- **Devices.** Device deployments are activated by their users and owners and
  move by activating again.
- **Trellis's own services.** `trellis.platform`, Jobs, Health, and Events
  belong to each installation and are never exported.
- **Moving users.** Accounts, sign-in methods, and profiles are created in the
  new installation by its administrators.
- **History.** Job, event, health, webhook request, and audit history stay in
  the old installation. `trellis-server backup` keeps them.
- **Incremental copies.** A move copies all data during the downtime. Copying
  most of it first and only the changes during the downtime would shorten a
  large move, and could come later with NATS stream sourcing between
  installations.
- **Forwarding.** The old installation does not redirect webhook senders,
  callers, or processes to the new one. Moving the hostname does that job.
- **Restoring a service in place.** Importing a bundle over an existing
  deployment in the same installation, to undo a bad change, is not part of
  this proposal. `trellis-server restore` restores the whole installation.

## Open questions

1. **Should a bundle be encrypted to an import key, a passphrase, or both?**
   An import key keeps secrets write-only and ties the bundle to one
   installation, but needs the receiving installation to exist first. A
   passphrase works for a backup with no destination, but anyone holding it
   can read every secret. Recommendation: offer both. Make the import key the
   default. A passphrase bundle leaves secrets out unless `--include-secrets`
   is given, which needs `platform.manage`.

2. **Should enrollment secrets move?** Their hashes do work in the new
   installation, which saves replacing the secret in every Kubernetes
   Deployment. The cost is that one secret works in two installations until
   the old deployment is disabled or removed. Recommendation: move them, and
   have `--move` disable the old deployment so only the new installation
   accepts them afterwards.

3. **Should the final export disable the old deployment itself, or leave that
   to the administrator?** Disabling it in the same step is the only way to
   guarantee nothing writes after the snapshot, and it ends the period when
   both installations accept the same keys. Recommendation: `--move` disables
   it, and a plain export never changes the old deployment.

4. **Should import be able to give the deployment a new ID?** A conflict on the
   deployment ID would then have a way out. But the resource IDs, physical
   stream names, and job subjects are all derived from the deployment ID. A new
   ID would mean copying every entry into new streams, which changes every KV
   revision and loses the exact copy. Recommendation: keep the same ID in the
   first version and refuse a conflict, naming the existing deployment.

5. **Should import create the roles the service's users need?** The old
   installation's roles often mix capabilities of many APIs, and the new
   installation may already organize access differently. Recommendation: do
   not create roles. List them in the report with their capabilities, so the
   administrator decides.

6. **Should pending jobs move?** Moving them keeps queued work from being lost.
   Jobs already run at least once, so a job that runs again after the move is
   safe by design. Recommendation: move unfinished work messages. Leave job
   history behind.

7. **Should export and import run in Auth, or as a local `trellis-server`
   command like `backup`?** A local command could read the databases and NATS
   directly, but would need shell access to the host and would bypass the
   capability checks and the audit. Recommendation: run them as Auth
   Operations reached through the CLI and Console, so the same capabilities
   and audit apply as for every other change to a deployment.

8. **Should a bundle be importable as a different package identity?** This is
   the case [workspaces](../contracts/workspaces.md#changing-a-workspace) leaves
   open: a package that moves to another registry gets new IDs and a new
   deployment, and its data has no way across. Settings are keyed by field name
   and resources by kind and local name, and neither contains the workspace, so
   a mapping is possible. Recommendation: not in the first version. Keep the
   bundle format keyed by kind and local name so a later version can add it.

9. **Which encryption format should the envelope use?** The format needs
   recipients by public key and by passphrase, and chunked authenticated
   encryption, so a large bundle streams. The [age](https://age-encryption.org/v1)
   format has all three and a published specification. Recommendation: use
   age, provided its implementation fits the dependency policy in `deny.toml`.
   If it does not, build the same construction from primitives already in the
   tree.
