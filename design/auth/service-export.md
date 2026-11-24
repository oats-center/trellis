---
title: Moving a Service Between Installations
description: Export one service deployment, with its build, instance keys, settings, secrets, and data, and import it into another Trellis installation.
---

# Design: Moving a Service Between Installations

> **Status: implemented.** `trellis svc <id> export` and `trellis svc import`
> run as the Auth Operations `Deployments.Export` and `Deployments.Import`.
> The [decisions](#decisions) section records how the proposal's open
> questions were settled.

Trellis can back up a whole installation with `trellis-server backup` and put
it back with `trellis-server restore`. That restores everything at once, into
a place that becomes the same installation again. It cannot take one service
out of an installation and add it to a different one.

Operators need that when they split one installation into two, consolidate
two into one, move a service to new hardware run by another team, or start a
fresh installation and bring services over one at a time. Without it, they
create a new deployment by hand, provision new keys for every process, type
every setting and secret again, and find some way to copy the data. The
[workspaces](../contracts/workspaces.md) proposal says the same in its
migration section: data moves only if the service itself offers an export.

`trellis svc <id> export` writes one encrypted file that holds a service
deployment, its build, its instances' public keys, its settings and secrets,
and its data. `trellis svc import` reads that file into another installation
and recreates the deployment there under the same ID. The service's processes
keep their own identity seeds and reconnect to the new installation once they
are pointed at its URL.

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
- [operations/trellis-operations.md](../operations/trellis-operations.md) and
  [files-transfer-patterns.md](../core/files-transfer-patterns.md): the
  Operations and transfers export and import run as
- [tooling/trellis-cli.md](../tooling/trellis-cli.md#recovery): the
  whole-installation backup this builds on

## Terms

- **Installation.** One Trellis server (or a set of split roles) with its own
  Auth database, NATS broker, and `secrets.toml`. Each has an installation ID
  and a public origin, the HTTPS URL that services, apps, and the CLI connect
  to.
- **Deployment.** One installed service. It records the build it runs, the
  capabilities it was granted, and the resources it owns. Its ID is a ULID
  Auth gave it when it was created.
- **Installed build.** The package evidence for the service: its compiled IDL
  and the source of every API it uses. Auth keeps it as numbered revisions of
  the service's participant.
- **Instance.** One running copy of a deployment. Each instance has its own
  **identity key**, an Ed25519 key pair. The process keeps the private half,
  its **seed**. Auth stores only the public half.
- **Storage key.** A 32-byte key in `secrets.toml`. Auth seals settings
  secrets, OAuth records, and registry passwords with it. Two installations
  have different storage keys.
- **Resource.** Storage a deployment owns and Trellis provisions: KV buckets,
  stores, job queues, event consumers, and webhook consumers.
- **Bundle.** The file an export writes: the bundle's **content**, encrypted
  with a passphrase the operator chooses.

## Overview

A move between two installations, `old.example.org` and `new.example.org`,
takes two commands and a restart. The deployment keeps its ID, so one ID
names it in both installations. The administrator signs in to each under a
profile of its own, and every command names the profile it acts on:

```sh
trellis --profile old login https://old.example.org
trellis --profile new login https://new.example.org

trellis --profile old svc orders export --out orders.trellis-service
#   asks for a passphrase twice, prints the bundle's SHA-256

trellis --profile new svc import orders.trellis-service --digest <sha256>
#   asks for the passphrase, prints what moved and what to recreate
```

Then each of the service's processes is pointed at `https://new.example.org`
and restarted with the seed it already has.

Export never changes the old installation. It writes the bundle and an audit
entry, nothing else. The operator stops the old deployment's processes before
the export, so that nothing writes after the export reads, and disables or
removes the old deployment once the service runs in the new installation.
See the [migration procedure](#migration-procedure).

## What a bundle contains

| Part                          | Moves? | How                                                                                |
| ----------------------------- | ------ | ---------------------------------------------------------------------------------- |
| Deployment record             | Yes    | ID, title, expiry, and its own instance retirement policy                          |
| Installed build               | Yes    | The package evidence of the installed revision; the new installation compiles it   |
| Instances                     | Yes    | Instance and principal IDs, title, state, and public identity key; never a seed    |
| Enrollment secrets            | Yes    | Their IDs, hashes, limits, expiry, and revoked state                               |
| Settings                      | Yes    | The current revision of the deployment's settings and of each instance's overrides |
| Secrets                       | Yes    | Opened with the old storage key, carried in the bundle, sealed with the new one    |
| KV and store data             | Yes    | A JetStream snapshot of each resource's stream                                     |
| Waiting jobs                  | Yes    | The lifecycle messages of every job whose last run has not settled                 |
| Webhook endpoints             | Yes    | Endpoint names, service names, URL IDs, and runtime settings                       |
| Settings history              | No     | Only the current revision moves                                                    |
| Job history, finished jobs    | No     | Stay in the old installation                                                       |
| Events not yet consumed       | No     | Counted in the report                                                              |
| Requests waiting for intake   | No     | Counted in the report                                                              |
| Operations                    | No     | Named in the report                                                                |
| Users, roles, grants, consent | No     | Roles and direct grants of the service's APIs are listed in the report             |
| Audit records                 | No     | Stay in the old installation; both sides audit their step                          |

The report is part of the bundle and is printed by both commands. Its
`leftBehind` list says, in plain words, what stayed in the old installation.

### The deployment and its build

The bundle holds the deployment's record and the package evidence of its
installed revision. Package evidence is what `trellis svc create --source`
sends to Auth: the compiled participant and the verified source of every API
it provides or uses. The new installation compiles that evidence again, as it
does for `Deployments.Create`, so it never trusts a compiled result from the
old installation.

Revision numbers are local to an installation, so the build becomes the next
revision of its participant in the new one. The participant digest, which
identifies the build, stays the same. That is what lets existing processes
connect without submitting an update. An instance that still runs an older
build connects, sees that its digest differs, and submits a
[service update](./service-updates.md) as it would after any apply.

Package identity is the full participant ID, and import never rewrites it.

### Instances and their keys

For each active or disabled instance with a registered key, the bundle holds
its instance ID, its principal ID, its title, its state, and its public
identity key. Retired and removed instances stay behind. No seed is ever in
the bundle, because Trellis never has one.

Import registers the same public keys under the same instance and principal
IDs, so a process that still has its seed connects to the new installation
without a new key:

1. The process discovers the new installation's ID and Auth issuer key from
   the origin it is given, over HTTPS, and signs its connection proof for
   that installation. Nothing in the seed ties it to the old one.
2. The new Auth finds the imported instance by the public key and admits it
   with the imported deployment's authority.
3. The instance runs the imported build, whose participant digest matches its
   own, so the SDK continues without an update.

The process has to trust the new installation's TLS certificate. A
certificate from a public CA needs nothing. A self-signed or generated one
needs `TRELLIS_CERTIFICATE_FINGERPRINT` set to the new installation's
fingerprint.

A running process does not switch installations on its own. The SDK keeps
the installation identity it first saw across reconnects and refuses a
different one, so each process is restarted with the new URL. If the new
installation takes over the old hostname, the URL stays the same but a
restart is still needed.

Instances that enrolled with a memory-only key cannot reconnect, because
their key went with the process. Their records move anyway, stay offline, and
are retired by the usual
[timed retirement](./instance-enrollment.md#retiring-offline-instances).
Their replacements enroll with the moved enrollment secret.

### Enrollment secrets

Auth stores an enrollment secret only as the SHA-256 hash of its text, and
the hash is not tied to the storage key. A moved hash therefore works: a
process that presents the same `TRELLIS_ENROLLMENT_SECRET` to the new
installation enrolls there. Pods in a Kubernetes Deployment keep their
secret, and only their Trellis URL changes. Nobody can read a secret's text
from the bundle, since the text was never stored.

While the old deployment is enabled, the same secret works in both
installations. Disabling or removing the old deployment ends that, since a
disabled deployment refuses enrollment.

### Settings and secrets

The bundle holds the current settings revision of the deployment and of each
moved instance's overrides. They become revision 1 of each target in the new
installation. Earlier revisions stay behind: deleting old revisions is how an
administrator makes a replaced password stop existing, and copying history
would undo that.

The old installation seals secrets with its storage key, which the new
installation does not have and must not get. Export opens each secret inside
the old Auth and writes it into the bundle's content. Import seals each one
with the new storage key. The bundle is encrypted with a passphrase, so
whoever holds the bundle and its passphrase can read every secret in it. That
is why export needs `config.manageSecrets` as well as `users.manage`, and why
both commands are audited.

### Resource data

Each KV bucket and store is a JetStream stream of its own. The bundle holds a
snapshot of each one, taken through the same JetStream snapshot API that
`trellis-server backup` uses. A snapshot of one stream is consistent: every
entry, revision, and timestamp as of one moment.

Import restores each stream under the same physical name. The name is built
from the resource ID, a digest of the deployment ID, participant ID, kind and
name, and from the allocation ID, which the bundle carries. Neither depends
on the installation. Because the stream comes back whole, KV revisions stay
the same, so a revision a service kept for compare-and-set is still valid,
and TTLs continue from the original timestamps. The ownership marker in the
stream's description comes back too, so the new Auth's normal ownership check
accepts the stream as the one it made.

Other resources have no stream of their own:

- **Job queues.** A job's lifecycle lives in the installation's shared `JOBS`
  stream under the queue's subjects, and its work message in `JOBS_WORK`,
  sourced from the `created` and `retried` events. Export copies the
  lifecycle messages of each job whose last run has not settled. It reads
  the queue twice: first headers only, keeping the IDs of unfinished jobs,
  then the messages of those jobs, which go straight into the bundle. Memory
  therefore follows the unfinished work, not the finished history the queue
  retains. Import publishes them again on the same subjects, which are
  derived from the deployment ID, the queue's name, and its allocation ID,
  and `JOBS_WORK` sources the work from them. A job that was running at
  export time runs again, which jobs already allow. Finished jobs and their
  history stay behind.
- **Event and webhook consumers.** A consumer is a position in the
  installation's event stream. Import creates each consumer fresh, so it
  starts with events published in the new installation. The report counts the
  events each consumer had not processed.
- **Operations.** A deployment's Operation records name the users who started
  them, and those users do not exist in the new installation. Operations do
  not move, and the report says so.

### Webhook endpoints

A webhook URL is `https://<origin>/webhooks/<service name>/<url id>`. The
service name was fixed when the endpoint was first declared, and the URL ID is
128 random bits. The bundle carries both, for the current URL and for any
earlier URL not yet retired, along with the endpoint's runtime settings.
Import keeps them, so only the origin changes. If the new installation takes
over the old hostname, senders need no change at all. Otherwise the import
report lists every URL to give senders.

Verification secrets are ordinary settings secrets and move with the other
settings. Requests waiting in the old intake stream, request records, and the
dedupe entries stay behind. A sender that retries a request across the
cutover may therefore have it accepted once in each installation.

### Access: roles, grants, and consent

Access is held by principals, and the old installation's users are not in the
new one.

- **The deployment's own grants move**, because they come from the build: the
  new installation derives them from the imported evidence.
- **The APIs the service provides are accepted** in the new installation's
  catalog from the build's evidence, as `Deployments.Create` does. So are APIs
  it uses that the catalog does not hold yet.
- **Roles and grants are reported, not created.** The report lists every role
  that holds a capability of an API the service provides, with those
  capabilities and how many principals held the role, and every such
  capability granted directly to users, with how many held it. An
  administrator recreates what the new installation needs.
- **Role assignments, OAuth grants, and remembered consent do not move.**
  Users of the moved service sign in to the new installation and consent
  there.

## The bundle

The bundle's **content** is a sequence of named entries. It starts with the
16 bytes `TRELLIS-SERVICE\n` and a 4-byte format number, currently 1. Each
entry is a 2-byte name length, the name, and the entry's bytes as chunks of a
4-byte length and that many bytes, ended by a zero length. Chunks let a
stream snapshot of unknown size be written as JetStream sends it.

```text
deployment.json         the deployment, its instance policy, webhook endpoints and settings
evidence.json           package evidence of the installed build
instances.json          instances and their public keys, enrollment secrets
settings.json           current settings and secrets of the deployment and instance overrides
resources/<id>.snapshot one JetStream snapshot per KV bucket or store
jobs.jsonl              lifecycle messages of waiting jobs, one JSON object per line
resources.json          each resource's kind, name, resource and allocation IDs, stream config and state
report.json             what moved, what stayed behind, and the access to recreate
manifest.json           format, Trellis version, source installation, and every entry's size and SHA-256
```

`manifest.json` comes last and lists every other entry. Import reads the
whole content first and refuses it unless every entry is present with the
size and SHA-256 the manifest gives, and nothing else is. The SHA-256 of the
whole content, the **content digest**, is recorded in both installations'
audit entries and printed in the report.

Import accepts only the entry names above, each once, with a snapshot named
after a resource ID, and at most 1024 entries. It holds a JSON document of
up to 16 MiB in memory and spools everything else to disk; a content chunk
is at most 1 MiB and a line of `jobs.jsonl` at most 8 MiB.

**Encryption.** The CLI encrypts the content with the operator's passphrase
as it arrives from Auth, and decrypts it as it sends it to Auth. The bundle
file is a text header followed by the encrypted content:

```text
trellis-service-bundle v1
argon2id m=65536 t=3 p=1 salt=<16 bytes, base64url>
nonce=<16 bytes, base64url>
mac=<HMAC-SHA256 of the three lines above, base64url>
<ChaCha20-Poly1305 chunks>
```

The construction is age's passphrase recipient with Argon2id in place of
scrypt. Argon2id turns the passphrase and salt into a 32-byte key. An HMAC
key derived from it authenticates the header, so a wrong passphrase is
refused before any content is read. A content key derived from it and the
nonce encrypts the content in 64 KiB chunks, each with a 12-byte nonce of an
11-byte counter and a final-chunk flag. Changing, reordering, or cutting off
a chunk fails its tag. Import refuses Argon2id costs above 2 GiB, 16 passes,
or 16 lanes, so a forged header cannot exhaust memory.

The `age` crate itself was not used because it depends on the earlier
RustCrypto generation (`chacha20poly1305` 0.10, `hkdf` 0.12, `sha2` 0.10) and
on `nom` 8, each a second version of a crate Trellis already uses, which the
one-version rule in `deny.toml` refuses. The construction is built from
`argon2` and `ring`, which Trellis already uses for password hashing and for
sealing with the storage key.

**Integrity.** Export prints the SHA-256 and size of the bundle file,
computed from the bytes as it writes them rather than by reading the file
back, so a file changed or replaced afterwards does not change what it
reports. Import reads the file once, into a private temporary file that is
unlinked as it is created, computing the SHA-256 as it copies. It refuses the
bundle unless that digest matches `--digest`, or, on a terminal, unless the
administrator confirms the digest it shows, and then decrypts and sends that
same copy. A file replaced or rewritten after it was read is never sent.
Without a terminal, `--digest` is required.

**Versioning.** The header line and the content's format number both carry a
version. Import refuses a format it does not know. From the first stable
release, every release imports bundles that any earlier stable release
wrote, the same promise Trellis makes for upgrades.

## Commands

```text
trellis svc <id> export [--out <file>] [--passphrase-file <path>]
trellis svc import <file> [--digest <sha256>] [--passphrase-file <path>] [--check]
                   [--acknowledge-shared-apis] [--acknowledge-unauthenticated-webhooks]
```

Export writes `<id>.trellis-service` by default and refuses to overwrite a
file. It writes a temporary file of its own beside the destination, readable
only by its owner, and moves it into place once the download is whole with a
rename that refuses an existing destination (`renameat2` with
`RENAME_NOREPLACE`, or a hard link and unlink where that is missing). An
interrupted export therefore leaves no file that looks finished, and two
exports to the same name never replace each other: the second refuses and
deletes its temporary file. On a terminal it asks for the passphrase twice;
otherwise it reads it from `--passphrase-file`, or `-` or piped standard
input. A new bundle's passphrase has at least 12 characters.

The CLI keeps one saved login by default. `--profile <name>` on any command
uses the login saved under that name instead, so commands for two
installations can each name theirs; `trellis --profile <name> whoami` shows
its server. `trellis svc <id> remove` names the installation it acts on in
its confirmation. For a deployment imported into the installation within the
last day, `trellis svc <id> disable` asks for the same confirmation and
`remove` adds a warning: the installation the service left has a deployment
with the same ID, and cleaning that one up is the step where a command run
against the wrong installation takes down the moved service. A day covers a
move's cleanup without asking forever after. `-f` skips the question.

`import --check` sends the bundle, and Auth verifies it and runs the whole
import in a transaction it rolls back, then reports what the import would do.
Everything Auth itself later decodes from the bundle, each waiting job's
headers and payload included, is decoded during that verification, for
`--check` and for a real import alike, before anything is written. Only the
stream snapshots are read by the broker, when it loads them.

Both commands start an Auth Operation and wait for it:

- `Deployments.Export` takes the deployment ID. Auth reads the deployment
  into the content, writing it to a temporary file encrypted with a key held
  only in memory, and completes with an export ID, the content's size, and
  the report. `Deployments.Exports.Download` then serves the content to the
  same administrator as a download transfer, for an hour. A restart of Auth
  loses the key, and the temporary files are deleted at the next start.
- `Deployments.Import` is a `stream` Operation: the content is its byte
  stream. Auth writes it to temporary files encrypted the same way while it
  checks it, then imports.

Neither Operation carries the passphrase. Operation inputs are kept in the
Operation's record, and a passphrase there would outlive the move; the CLI
does all passphrase work locally instead. An Operation that Auth restarted
in the middle fails, and is run again.

The Console does not offer export or import yet.

### Who may export and import

| Action                                                                   | Requires                                                  |
| ------------------------------------------------------------------------ | --------------------------------------------------------- |
| Export, download                                                         | `trellis.auth.v1:users.manage` and `config.manageSecrets` |
| Import                                                                   | `users.manage` and `config.manageSecrets`                 |
| Export or import of a deployment that holds an administrative capability | `platform.manage` as well                                 |
| Import of a service whose provided or used APIs are not yet accepted     | what `Deployments.Create` requires to accept them         |

Both capabilities are checked in the session that started the Operation and
in current policy. The download is served only to the principal that ran the
export.

### Audit

Each step writes a security audit entry in the installation where it happens:

- `deployment.exported`, with the actor, the deployment, the export ID, the
  content digest and size, the instance count, the names of the secrets, and
  the resource sizes;
- `deployment.imported`, with the actor, the source installation ID and
  origin, the content digest, the instance count, and the names of the
  secrets; and
- `deployment.importRefused`, with the reason.

Secret names appear in the entries, never their values. The two
installations' entries can be matched by the content digest.

## Import

Import works through these steps and changes nothing until step 4.

1. **Receive and verify.** Auth reads the content, checks the format, and
   checks every entry against the manifest. It then checks that the bundle
   names nothing outside the deployment it creates. Auth compiles the build
   and derives from the deployment ID and the build's declarations every
   stream, bucket, allocation and job subject the import will create or
   load; the bundle's own copy of each has to match. A KV or store resource
   has to be one the build declares, with the resource ID its kind and name
   give, its stream `KV_` or `OBJ_` followed by `TRELLIS_RESOURCE_<resource
ID>_<allocation ID>`, and a stream configuration with exactly that name,
   its own subjects and ownership marker, and no mirror, sources, republish
   or subject transform. A waiting job's message has to be on
   `<queue prefix>.<job ID>.<event>` for one of the bundle's queues, with no
   NATS control header but `Nats-Msg-Id`, and its header names, header
   values and base64 payload have to decode as they will when the job is
   published again. Instance, principal, enrollment
   secret, allocation and deployment IDs are ULIDs, a webhook URL ID is 16
   random bytes, and a webhook endpoint is one the build declares. A bundle
   that names a stream of another deployment or of Trellis itself, or any
   other subject, is refused here. So is a Trellis service.
2. **Check for conflicts.** Import refuses, naming what is in the way, when:
   - a deployment with the same ID exists, whatever its state;
   - another deployment of the same participant exists and is not removed;
   - an instance ID, principal ID, or identity key in the bundle is already
     registered, or a key was registered once and removed or retired;
   - an enrollment secret's ID or hash, or a webhook URL ID, is already in
     use; or
   - a KV or store stream with the same physical name exists.

   An import never replaces a deployment, and never gives one a new ID.

3. **Compile.** Auth compiles the package evidence and checks the
   acknowledgements `Deployments.Create` asks for: `--acknowledge-shared-apis`
   when another deployment already serves an API the service provides, and
   `--acknowledge-unauthenticated-webhooks` for a `verify none` endpoint.
4. **Create.** In one Auth transaction, the import accepts the APIs the
   build provides and the unaccepted APIs it uses, creates the deployment
   under its own ID, installs the build, registers the instances and their
   keys, writes the enrollment secrets, the instance policy, the settings
   with their secrets sealed, and the webhook endpoints, records the
   resources as `pending` under their own resource and allocation IDs, and
   records the import with its content digest, the streams it will restore,
   and a random import ID. The deployment starts enabled.
5. **Load the data.** Auth restores each KV and store snapshot with the
   stream configuration from the bundle, adding the import ID to the
   stream's metadata under `trellis.import`. The broker refuses to restore
   over an existing stream, so the stream that exists afterwards is the one
   the import made. Auth checks it against the same rules as step 1 and for
   the import ID, and deletes it and stops if it differs. It then records
   the stream's creation time, and after that records the resource loaded.
   Auth then prepares the
   resources as it does for a new instance, which finds the restored streams
   and makes them `ready`, and publishes the waiting jobs. Whether there is
   an instance to prepare them for is read from this installation: a
   deployment with no active instance stops before preparing the resources,
   and the report says so.
6. **Report.** Import returns the report with the webhook URLs at the new
   origin.

Until every stream is loaded, nothing else prepares the deployment's
resources: an instance that connects meanwhile is refused, since preparing
would create the missing streams empty.

If step 5 fails, or stops for want of an instance, running the same import
again resumes it: Auth finds the import record for the deployment with the
same content digest, deletes a stream it restored and did not record
loaded, restores it again, and loads the rest. It deletes a stream only
when the stream's metadata carries the import ID and its creation time is
the one the import recorded. Any other stream under that name, whether it
has no import ID, another import's, or another creation time, is left as it
is, and the import stops with a conflict naming it. This includes a stream
the import restored just before it stopped and had not yet recorded: Auth
cannot tell it from someone else's, so the operator removes it before
running the import again. Readiness is read again, so an
instance provisioned or enabled since the last attempt lets the import
finish. A resource does not become `ready` empty, following the existing
rule that a missing stream after materialization is storage loss, not
something to recreate. Running a bundle again after its import finished is
refused.

## Consistency

A service writes to several resources, and some writes depend on each other.
An order in KV may refer to a file in a store. An export reads the settings in
one Auth transaction, then snapshots each stream in turn. Each snapshot is
consistent alone, but two resources are read at different moments, so a
write that lands after its resource was read is not in the bundle.

The export is therefore as consistent as the service was still while it ran.
For the final step of a move, stop the service's processes first: Trellis
does not stop them, and export refuses nothing because they run. With the
processes stopped, nothing writes to the deployment's resources while they
are read, and together they form one state. An export taken while the service
runs suits a rehearsal or an extra backup of one service.

What is lost either way:

- events published for the service that its consumers had not processed;
- events and webhook requests that reach the old installation after the
  export; and
- open Operations.

## Migration procedure

This procedure moves a running service with as little downtime as the size
of its data allows. Downtime runs from step 4 to step 6, and most of it is
the time to copy the data.

1. **Prepare the new installation.** Install it, create the administrator,
   and create the user accounts the service's users need. Add registry
   credentials if the service's dependencies come from a private registry.
2. **Rehearse.** Export the running service and import the bundle into a
   spare installation, first with `--check`, then for real. Read the report.
   This shows how long the copy takes and what needs approval.
3. **Prepare the processes.** For each process, have the new URL ready, and
   the new certificate's fingerprint if the certificate is not from a public
   CA. If the new installation takes over the old hostname, have the DNS
   change ready instead.
4. **Stop the service's processes, then export** with the old
   installation's profile. The service stops serving here. Give the
   importing administrator the printed SHA-256 through a channel you trust,
   and the passphrase separately.
5. **Import** into the new installation with `--digest`, with its profile.
6. **Point the processes at the new installation and start them.** The
   service serves again as soon as the first instance connects.
7. **Disable the old deployment**, so its instance keys and enrollment
   secrets stop working there: `trellis --profile old svc <id> disable`.
8. **Update outside senders.** Give webhook senders their new URLs, unless
   the hostname moved. Create the roles and grants the report lists. Users
   sign in to the new installation and consent to the apps they use.
9. **Clean up the old installation** once the new one has run long enough to
   trust it. Remove the old deployment, then destroy its detached resources:
   `trellis --profile old svc <id> remove`, then
   `trellis --profile old resources destroy`.

A service rarely stands alone. Services in the old installation that call
the moved service lose it at step 4. Move services that depend on each other
together: stop them all, export each, import providers before the services
that call them, then start all their processes.

**Rolling back.** Until step 9, the old installation still has the deployment
and its data, since export changed nothing. Enabling the old deployment again
if it was disabled, and pointing the processes back, restores the service as
it was at step 4. Anything written in the new installation since then is not
carried back.

## Security

A bundle is as sensitive as the most sensitive thing in it: the service's
secrets and all of its data.

- **Never in the clear on disk.** Auth keeps the content only in temporary
  files encrypted with a key held in memory, and the CLI encrypts it before it
  writes the bundle file. Nothing writes the content unencrypted.
- **The passphrase stays with the operator.** It is never sent to Auth, so it
  is in no Operation record, log, or audit entry.
- **Passphrase holders read every secret.** Export therefore needs
  `config.manageSecrets`, and both steps are audited with the secret names.
  Choose a long passphrase and send it apart from the bundle.
- **No seeds.** The bundle holds public identity keys only. A stolen bundle
  cannot impersonate an instance.
- **A swapped bundle.** Anyone who can export from some installation can make
  a bundle that registers their own key as an instance of a deployment. The
  digest check stops this: the importing administrator confirms the digest
  the export printed. The report shows the source installation and every
  instance key.
- **Two installations that accept one key.** Until the old deployment is
  disabled or removed, both installations accept the instances' keys and the
  enrollment secrets. Step 7 of the procedure ends that.
- **Audit on both sides.** Every export and import is recorded with the
  content digest, so a bundle found later can be traced to the export that
  made it.

## Out of scope

- **Devices.** Device deployments are activated by their users and owners and
  move by activating again. Export refuses them.
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
  deployment, to undo a bad change, is refused. `trellis-server restore`
  restores the whole installation.
- **The Console.** Export and import are CLI commands for now.

## Decisions

The proposal's open questions were settled as follows.

1. **Encryption.** A passphrase only, chosen by the operator, with secrets
   always included. There are no import keys.
2. **Enrollment secrets** move, as hashes.
3. **Export never changes the old installation.** There is no move mode that
   disables the old deployment; the operator stops the processes and disables
   or removes the old deployment as the procedure says.
4. **The deployment keeps its ID.** An import whose deployment ID, or whose
   participant, already exists is refused, naming the existing deployment.
   Resource names, KV revisions, and instance identities carry over
   unchanged.
5. **Access is reported, not created.** Import creates no roles, role
   assignments, or grants.
6. **Waiting jobs move, job history does not.**
7. **Export and import run as Auth Operations** reached from the CLI, so
   capability checks and audit apply.
8. **No import under a different package identity.** The bundle keys
   settings by field name and resources by kind and name, so a later version
   could add a mapping.
9. **Format.** age's passphrase construction, built from crates already in
   the tree, as described under [the bundle](#the-bundle).
