---
title: Service Configuration
description: Typed settings and write-only secrets that Trellis keeps for services and devices, read at connect and followed live.
---

# Design: Service Configuration

Services used to take their settings from the environment: a database URL, a
password, a log level, an upstream endpoint. Each deployment needed its own
wiring to set them, a change meant a restart, and nothing checked a value until
the service tripped over it. The only value a service really has to bring is
its identity seed, and Trellis already knows which deployment and instance that
seed belongs to. So Trellis holds the rest.

A participant declares its settings in IDL. Administrators set values through
the CLI, the Console, or any app holding the capability. The participant reads
them when it connects and is told when they change. A deployment needs one
secret on disk, the seed.

Services, devices, and apps have settings. A service's or device's declaration
comes from the build its deployment has installed. Auth knows an app only as an
OAuth client, so an administrator attaches the app's declaration to its client,
for example with `trellis clients put --source`.

## Principles

- Settings are declared, typed, and owned by the participant's contract, the
  same way resources are. Whether a stored value fits is a question each build
  answers when it reads it, not one Auth answers when an administrator saves
  it. Auth stores values for any field name, so they can be set before Trellis
  has a service's IDL and for a build that isn't installed yet. A value that
  doesn't fit is reported where administrators look, and a required one holds
  the participant with a link to fix it.
- Settings and secrets are different things. A setting can be shown, copied,
  and compared. A secret can be set or replaced, never read back by an
  administrator, and only the deployment's own instances receive it.
- Values belong to the deployment. An instance (a service replica or a device)
  can override individual fields; everything it does not override comes from
  the deployment.
- The participant decides how to apply a change. Trellis tells it a newer
  revision exists; it swaps a log level in place or rebuilds a connection pool.
  A field it cannot apply live is declared `restart`.
- No environment fallback. A second path would live forever and every service
  would pick its own precedence. Local development and tests set settings
  through the same RPCs production uses.
- This is configuration, not a secrets manager. No leases, no dynamic
  credentials, no secret engines. See [Risks](#risks).

## Declaration

Each `config` member of a service or device declares one field: a
lowerCamelCase name, a title, a description, a type from the ordinary IDL type
system, and optionally a default. `secret` marks a field write-only. `restart`
marks a field the participant reads only when it connects.

```trellis
enum LogLevel { "info"; "debug"; }

service Orders {
  implements orders;
  config databaseUrl {
    title "Database URL";
    description "Postgres connection string without the password.";
    type string;
    restart;
  }
  config databasePassword {
    title "Database password";
    description "Password for the orders database role.";
    type string;
    secret;
    restart;
  }
  config logLevel {
    title "Log level";
    description "Lowest level written to the service log.";
    type LogLevel;
    default "info";
  }
}
```

A field without a default is required, unless its type allows null: a
`T | null` field with no default reads null until someone sets it, so nothing
waits for it. The type may allow null through an alias, local or imported,
such as `type OptionalKey = string | null;`. An app's fields must all have defaults,
since an app cannot wait for an administrator the way a service does. A
default is a JSON scalar (a quoted
string, a number, `true`, `false`, or `null`) and the compiler checks it
against the field's JSON Schema, the one Auth checks writes against, after
resolving named types and their constraints in whatever package declares them.
Auth serves a default when nothing is stored, so a default that could not be
written is refused at compile time. A secret cannot have a default, since it would sit in
the source; null is the one exception, so an optional secret is written
`type string | null; secret;`. Only services may declare `secret` fields: a device runs where its
user can read anything it holds. Enums are open in Trellis IDL, so an enum
field accepts strings beyond its listed symbols.

A field's type, default, `secret`, and `restart` are part of the participant
digest. Its title and description are not. Changing the shape of a field is
therefore a new build, which goes through [service updates](../auth/service-updates.md)
like any participant change; since it asks for no new access, it installs
without an administrator.

## Apps

An app or agent request may declare `config` fields. They are public: every
user of the app can read them, so `secret` is refused. The compiler adds
`trellis.auth.v1:config.readApp` to the request's required capabilities, which
every user holds through the `account` group, so the app can read its settings
and hear `Config.Changed` without a separate grant. This needs the request's
package to depend directly on the `trellis` package.

`Config.Apps.Declare` attaches an app build's package evidence and request ID to
its registered OAuth client; Auth then checks settings written for that client
against the declaration. When a new declaration changes what the app reads,
such as a new default for a field no one set, Auth sends `Config.Changed` for
the client in the same transaction, without a revision if the client has no
stored settings, so connected apps reread. `trellis clients put FILE --source
PATH [--app ID]` compiles the source and picks the app before it saves the
registration, then attaches the declaration. If that second step fails, running
the same command again replays the saved registration through its idempotency
key and retries the declaration. A client with no declaration has no settings. An app reads its values with `Config.App.Get`,
which answers for the client of the caller's own session. The TypeScript
browser and native app SDKs expose them as `client.config`, with `current`,
`revision.app`, and `onChange`.

## Storage and revisions

Auth keeps settings in `auth_config_revisions`, one row per revision of a
target. A target is a deployment, one of its instances, an app's client, or a
service claim (a service added without its IDL, waiting for its service to
connect). Each write stores the
target's complete values as a new revision with its author and time, and
records which fields and which secrets changed. The highest revision is
current. Rolling back writes an earlier revision's values as a new revision,
so history only grows forward. A field is stored either as a setting or as a
secret: writing it as one removes it as the other.

Every revision is kept until an administrator deletes it. That is how a
replaced password stops existing anywhere. The current revision cannot be
deleted. Nor can a revision a build in use still takes a value from (see
[Settings ahead of a build](#settings-ahead-of-a-build)): deleting it would
change what that build reads with no new revision and no `Config.Changed`, so
instances that connected before and after the deletion would disagree under
the same revision. The builds in use are the deployment's installed build and
the build each active instance runs. Auth compares what each reads with and
without the revision and refuses, naming the field and the build, when they
differ. Setting a value that fits releases the revision. Deleting a revision
that holds a secret destroys that secret, so it needs `config.manageSecrets`.

An instance's values are its own overrides on top of its deployment's, then
the declared defaults. See [Settings ahead of a build](#settings-ahead-of-a-build)
for how a build reads values it doesn't declare or that don't fit it.

Secrets never go into `values_json`. They are sealed as one blob per revision
with AES-256-GCM under the installation's storage key from `secrets.toml`, the
same key that seals OAuth records and OIDC client secrets. The associated data
names the target, not the revision, so a rollback carries sealed values
forward unchanged. A copied database file does not expose them. A
`trellis-server backup` includes `secrets.toml`, so a complete backup does.
Audit records of a write list secret names, never values.

Encrypting to each instance's own key, so Auth stores values it cannot read,
was considered and declined: an instance's Ed25519 identity key converts to an
X25519 encryption key, but every write would have to encrypt for each
instance, a new instance could not read existing secrets until someone
encrypted them again for it, and Auth already decides what every participant
may reach, which limits the gain.

## Delivery

Every service and device instance holds `trellis.auth.v1:config.readOwn`,
granted when the instance is provisioned like its health capability. It allows
`Config.Own.Get` and subscribing to `Config.Changed`.

When a participant that declares settings connects, the SDK calls
`Config.Own.Get` with `atConnect: true` before any handler starts. Auth
returns the effective values, secrets included, and records the revisions the
instance loaded. The participant sees them through a typed handle:

- TypeScript services and devices: `service.config` / `device.config`, with
  `current`, `revision`, and `onChange(listener)`.
- Rust services: the generated `Provider::config()`; Rust devices: the
  generated client's `config()`. Both return a `ConfigHandle<Config>` whose
  `current()` decodes the generated `Config` struct and whose `changed()` waits
  for the next live change.

`Config.Changed` carries the target and its current revision, never values. Every
service and device may subscribe to it, so the event only prompts the SDK to read
`Config.Own.Get` again over its own authenticated session; its payload is never
trusted for values. The SDK also reads again whenever its subscription is
reestablished, so a change made while it was disconnected is not missed.
The subscription follows the published transport generation. When a successor
is published, the SDK drops the subscription and its lease on the old
generation, so that generation drains, subscribes on the successor, and reads
again once that subscription is in place, so a change made during the cutover
is not missed either.

An instance reads against the declaration of the build it runs, which can
trail its deployment's. A new build's declaration, such as a changed default or an added field,
therefore never changes what an already running instance reads, and Auth sends
no `Config.Changed` when one is installed. Applying a build through
`contracts apply` leaves running instances on their own revision. An instance
moves to a new revision only through its own [service update](../auth/service-updates.md),
after which the SDK connects again and reads its settings at connect before any
handler starts. A device must run exactly its instance's installed build. Apps
differ because an app's declaration lives on its client rather than in a
running build, which is why `Config.Apps.Declare` announces its changes.

A change to a `restart` field does not reach `onChange`. The value loaded at
connect stays in effect until the participant connects again. A field whose
value was removed keeps its last value until then too. Auth records, per
instance, the revisions it loaded at connect, and `Config.Get` reports
`restartPending` for an instance whose own build would now read a different
value for one of its `restart` fields. Values for fields that build doesn't
declare, or that don't fit it, never count, so setting values for the next
build doesn't ask anyone to restart the current one. Trellis never restarts anything:
podman, systemd, or whatever runs the process owns its lifecycle.

## Missing configuration

A required field with no usable value holds the participant the way an update
waiting for approval does. A stored value that doesn't fit the build counts as
no value. `Config.Own.Get` returns the missing fields and, in `invalid`, why each
stored value doesn't fit. The SDK logs which fields are missing and why,
prints a link to the deployment's settings in the Console with a terminal QR
code, and keeps reading with backoff. It never starts handlers with a partial
configuration. Once an administrator saves values that fit, the waiting
participant continues without a restart.

## Settings ahead of a build

Auth stores a value for any lowerCamelCase field name (the IDL's rule for
field names), whether or not a build declares it and whether or not any build
is installed. Two cases need this:

- **A service added without its IDL.** The administrator adds it as a claim
  and sets its settings on the claim (`{ claimId }`) before the service ever
  connects. Once the service connects, its submitted build types the claim's
  settings, so the Console and CLI show declared fields, missing values, and
  mismatches before anyone approves it. Approving the claim moves every
  revision to the new deployment, sealing each revision's secrets again for the
  deployment, so the service starts with them. Removing a waiting claim deletes
  its settings.
- **An upgrade without downtime.** While build 1 runs, the administrator sets
  values for fields only build 2 declares, or values in the type build 2 gives
  a field. When build 2 is installed, its instances find their settings in
  place and start at once.

Each build reads what is stored through its own declaration:

- A field it doesn't declare is ignored. `Config.Get` lists it under `unused`
  ("not used by this build", or, with no build known, saved for when the
  service connects), and the administrator can unset it.
- A stored value that doesn't fit the build's type is skipped for the value
  that field had before it: Auth walks back through the target's earlier
  revisions to the most recent value that fits, stopping where the field was
  not stored, and falls back to the default when there is none. So a value
  set in build 2's type leaves build 1 on its last value, with no change
  event, while build 2 uses the new one. `Config.Get` reports the mismatch in
  the field's `invalid`. A required field with no fitting value is missing.
- A secret stored as a plain setting, or a plain setting stored as a secret,
  is a mismatch of the same kind. The build never reads a plain value as a
  secret or a secret as a plain value, and a secret's value never appears in
  `values_json`, in `Config.Get`, or in the reason given for a mismatch.
- A plain value under a name the installed build declares secret, saved before
  Trellis had the IDL, is withheld from then on. `Config.Get` marks the field
  `storedAsPlain` without a value, `Config.Revisions.List` drops it from each
  revision's `values` and names it in `hiddenValues`, and the Console and CLI
  say to set it as a secret, noting that earlier revisions keep the plain value
  until deleted. Setting the secret removes the plain copy in the same write
  and needs only `config.manageSecrets`. Auth never converts the plain value
  into a secret itself: it was readable to every administrator, and sealing it
  quietly would misrepresent its history.

Auth refuses a write in two cases only. A plain value for a field the
installed build declares secret is refused, since it would be stored where
every administrator reads it. An app's client takes no secrets, since every
user of the app reads its settings. The CLI and Console show what Auth reports
after each save rather than checking types themselves, so every client shares
one validator.

Without a declaration, `trellis svc <ID> config set NAME=VALUE` stores JSON
when the value parses as JSON and text otherwise: `workers=4` is a number,
`region=east` is text, and `'region="4"'` is the text 4. `set-secret NAME`
names the field as a secret under the same rule.

A service update that adds a required field with no default applies as usual;
the instance then waits for the value like any other missing setting. The
approval screen does not collect values.

## API surface

One Trellis-owned RPC set serves every client, so a team can build its own
settings screen:

| RPC                       | What it does                                                                                                                                                   |
| ------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Config.Get`              | Fields with their schema, effective value, source, and status; secrets only as set or not; missing fields; each instance's loaded revisions.                   |
| `Config.Set`              | Set `values`, set `secrets`, or `unset` fields as one new revision. Names the `expectedRevision` it was based on and is refused if another write landed first. |
| `Config.Revisions.List`   | Revisions newest first, with what each changed. Secret values never appear.                                                                                    |
| `Config.Revisions.Delete` | Delete an earlier revision that no build in use still takes a value from.                                                                                      |
| `Config.Rollback`         | Write an earlier revision's values as a new revision.                                                                                                          |
| `Config.Own.Get`          | The calling service or device reads its own effective values.                                                                                                  |
| `Config.Changed` (event)  | A target's settings changed.                                                                                                                                   |

The target is `{ deploymentId }`, `{ deploymentId, instanceId }`,
`{ clientId }`, or `{ claimId }` for a service added without its IDL. An
approved claim names its deployment. `Config.Apps.Declare` attaches an app's declaration to its
client, and `Config.App.Get` is the app's own read.

Three administrative capabilities cover the surface, all in the default
administration group: `config.read` sees settings, `config.manage` changes
them, and `config.manageSecrets` sets secrets. A write that touches secrets
needs `config.manageSecrets`; one that touches settings needs `config.manage`;
a rollback needs whichever its changes touch. Deleting a revision needs either,
and `config.manageSecrets` when the revision holds a secret. So a role can tune
settings without touching credentials. A refusal names the capability missing.

The CLI follows the target-first shape: `trellis svc <ID> config` (or
`trellis dev <ID> config` for a device type) shows the settings, and `set
FIELD=VALUE`, `set-secret FIELD --from-file PATH`, `unset`, `history`,
`rollback REVISION`, and `delete-revision REVISION` change them. `--instance`
selects one instance's overrides. A text setting takes the text as written;
any other is JSON. `set --null FIELD` stores null for a nullable setting, since
a text setting would take `null` as text. `set-secret` reads a file or piped standard input, or on a
terminal asks for the value twice without echo, so a secret never appears in
shell history or the process list. Without a way to ask (`--no-input`, or no
terminal on standard error) and nothing piped in, it fails naming
`--from-file`. `set-secret FIELD --null`
stores null for a secret whose type allows it, such as an instance that should
go without a secret its deployment has; `unset` instead returns the field to
the deployment's value or, failing that, null.

A service ID or title that names a waiting claim targets the claim, so
settings can be set before the service connects. `show` lists values the build
doesn't use and marks values that don't fit it.

`trellis clients config CLIENT` does the same for an app. The Console shows a
Settings section on each service page (a waiting claim's included), on each
device type's page, and in each app's editor that has settings, with a form
for the declared values, the values the build doesn't use, rows to add
settings by name and value (text or JSON, optionally secret), the instance
selector (offered whenever the deployment has an instance), the restart
notice, and earlier versions to restore or delete. Adding a service takes
settings too: typed fields from the IDL when one is chosen, otherwise rows by
name, saved right after the service is created. A field whose type allows null offers Clear,
which stores null as `--null` does in the CLI, beside Reset, which unsets it. Edits stay with the target
they were typed for: changing the selector discards them, and the form waits
until the selected target's settings have loaded. The link a waiting
participant prints opens that section.

## Development and tests

The testkits set settings through the same RPCs. The TypeScript testkit's
`services.createInstance` takes initial `config` values and secrets; the Rust
testkit has `TrellisTestRuntime::set_config`. A test states its settings next
to the service it starts instead of exporting environment variables.

## Risks

- **Scope creep into a secrets manager.** Once secrets live in Trellis there
  will be requests for rotation schedules, dynamic database credentials, and
  leases. Those belong to dedicated tools. Trellis stops at typed settings and
  write-only static secrets.
- **Settings are unavailable while Trellis is down.** A restarting instance
  cannot read them without Trellis. That is close to no change, since a service
  cannot do anything useful without Trellis today, but it does put one more
  read on every participant's connect path.
