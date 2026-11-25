---
title: Service Configuration
description: Typed settings and sealed secrets that Trellis keeps for services and devices, read at connect and followed live.
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
  and compared. A secret can be set or replaced, and is shown to an
  administrator only on explicit request, only to holders of
  `config.manageSecrets`, and every time on the audit record. Only the
  deployment's own instances receive it otherwise.
- Values belong to the deployment. An instance (a service replica or a device)
  can override individual fields; everything it does not override comes from
  the deployment.
- The participant decides how to apply a change. Trellis tells it a newer
  revision exists; it swaps a log level in place or rebuilds a connection pool.
  A field it cannot apply live is declared `restart`. It may also refuse a
  revision its own checks reject, and Trellis shows that refusal where
  administrators look.
- No environment fallback. A second path would live forever and every service
  would pick its own precedence. Local development and tests set settings
  through the same RPCs production uses.
- This is configuration, not a secrets manager. No leases, no dynamic
  credentials, no secret engines. See [Risks](#risks).

## Declaration

Each `config` member of a service or device declares one field: a
lowerCamelCase name, a title, a description, a type from the ordinary IDL type
system, and optionally a default. `secret` marks a field sealed and never
shown unless asked for. `restart`
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
  `current`, `revision`, `onChange(listener)`, `check`, and `derive`.
- Rust services: the generated `Provider::config()`; Rust devices: the
  generated client's `config()`. Both return a `ConfigHandle<Config>` whose
  `current()` decodes the generated `Config` struct, whose `changed()` waits
  for the next accepted live change, and which has `check`, `derive`, and
  `derive_async` (see [Checks and derived values](#checks-and-derived-values)).

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

## Checks and derived values

A live change takes effect in two steps. The participant's checks and derive
builders run first, against the candidate values: the values in effect with
the reading's live fields laid over them. If any refuses, the revision is
rejected. The values in effect stay the ones accepted last, no `onChange`
listener runs, and the Rust watcher yields nothing. Only an accepted revision
switches the values, swaps derived values in, and then runs `onChange`. A
listener's failure is logged and never turns into a rejection, since by then
the revision has taken effect.

- TypeScript: `config.check((candidate, { reject, changed }) => ...)` and
  `config.derive(fields, build, { dispose })`, whose result has `current`.
  `reject(fields, reason)` refuses named fields; a throw refuses every changed
  field with its message. Apps share the same handle, so `client.config` has
  both too, without reporting.
- Rust: `ConfigHandle::check(|c| -> Result<(), Rejection>)`,
  `derive(fields, |c| Ok(..))`, and `derive_async`, whose `Derived<T>`
  returns an `Arc<T>` from `current()`. Fields are named as the generated
  struct names them or as declared. Any `std::error::Error` converts to a
  rejection of every changed field.

A derived value is built at registration from the values in effect. After that
it is rebuilt only when a revision changes one of its fields, during the check
step, so a failing builder rejects the revision for those fields and the old
value stays. Derived values are swapped in before the change is announced, so
a consumer that `onChange` or `changed()` wakes reads the derived values built
for the revision it heard. A replaced value is disposed (TypeScript) or dropped
(Rust) after its replacement took effect. Builders run only once every check
has passed, so a refused revision builds nothing.

A participant's handle lives as long as its connection. Every TypeScript
service, device, and app connection gets a handle of its own, one without
settings included, so nothing registered on one reaches another. When a
TypeScript service stops, once its requests have drained, or a connection
closes, the handle stops following changes and refuses new registrations.
Once the reading or registration in progress has finished, it disposes every
derived value still in effect; a value built for that reading never takes
effect and is disposed with them. Disposals started earlier, of a value a
later revision replaced or one built for a refused revision, may still be
running; retirement waits for them too. `service.stop()` returns after all
those disposers.

Registration is how startup waits. Registering a check or derive tests the
values in effect immediately. If they fail, the registration's `await` waits:
the participant prints the rejected fields and reasons with the Console link
and QR code, as for a missing value, reports the rejection, and the `await`
returns once a revision passes. While a registration waits, every reading is
checked, changed or not. The service has not reached `wait()` or `run()`, so
it is not serving and its readiness stays at starting.

A reason reaches more readers than the secrets do: the stored status that
`Config.Get`, the CLI and the Console show, and the `settings` health check,
which other users read. The participant's own log, and what it prints while
startup waits, are the operator's, who is trusted (see "The operator is
trusted" in `engineering-patterns.md`). The SDKs keep the two apart:

- An explicit reason (`reject`, `Rejection::new`, `Rejection::changed`) is
  the deliberate path and keeps its text. Every occurrence of a value of a
  field the build declares secret, taken from the candidate and from the
  values in effect, is replaced with `[redacted]`, longest first. Values
  shorter than four characters are skipped, since replacing them would blank
  out ordinary words.
- An unexpected error (a TypeScript throw, a Rust error converted through
  `?`) always gets fixed text in the status and health, whatever the build
  declares: `the check failed` for a check, `building the value failed` for a
  builder, with no type name or message. Its name and message are text the
  callback's code made, and it can carry a credential from anywhere the code
  read (an environment variable, a file) in a form redaction can't find. The
  participant's local log and startup output show the error with its
  message, known secret values replaced, so its developer can debug it.

Settings work live only when read at each use: `config.current`, a derived
`current`, or Rust's `current()`. A copy kept in a module-level constant, or a
`Config` cloned out of the watcher, keeps the values it was copied from.

## Reported outcomes

After each reading the SDK calls `Config.Own.Report` with the revisions it read
and whether it applied them or its checks rejected them, with the refused
fields and the reason. An `applied` outcome is held back until startup ends,
so the checks and derives registered at startup have all run: a service ends
startup when it first serves, and a device, which has no serving phase, the
first time it reads its settings or a derived value. A rejection is reported
at once, so a starting value a startup check refuses goes straight from
`pending` to `rejected`. The connect-time read drops the instance's earlier
report, so a restarted instance reads `pending` until this run reports. A
report also belongs to the build the instance connected with, which
`auth_config_connections` records: once a service update installs another
build in place, `Config.Get` shows the instance `pending` at once, before the
new build connects or even while the instance is stopped, and the earlier
build's result never stands for it, even with unchanged revisions while it
waits for a setting only the new build requires. A
repeat of the last confirmed report is not sent again. A failed report is
retried with the shared backoff (250 ms doubling to 30 s) until Auth confirms
it or a newer outcome replaces it, so a report lost while the connection stays
up, such as one sent while the server restarts, still arrives without another
save. `config.readOwn` allows it. Auth keeps one row per instance in
`auth_config_reports`, with the reason cut to its first KiB at a character
boundary and marked with `…`; each field name is at most 128 characters by
the request's type.

`Config.Get` returns each instance's `settings` as `{ revision,
instanceRevision, state, fields, reason, at }`. A report on the instance's
current deployment and instance revisions stands as `applied` or `rejected`.
Anything else is `pending` with the current revisions and the time the newest
was saved: the instance hasn't read them yet. So pending needs no write when a
revision is saved, and a stopped instance shows pending until it starts and
reads. Saving never waits for instances, and a rejected revision stays
stored, so a rollback can undo it. An instance whose build declares no
settings has no `settings`.

The SDK also adds a `settings` health check, the way it adds a
`webhook:<endpoint>` check per endpoint. It is degraded with the reason while
the newest revision is rejected and turns ok with the next accepted one. Its
summary is cut to the 1024 characters a heartbeat allows, since a longer one
would keep the whole heartbeat from being published. A
change of outcome publishes a heartbeat at once, so a running instance shows a
rejection within seconds.

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
user of the app reads its settings. The CLI shows what Auth reports after
each save rather than checking types itself. The Console's form also checks an
edited value against the installed build's schema before saving, so a typo
such as a number past its bound doesn't become a stored value the build skips;
whether a stored value fits is still each build's answer, reported by Auth.

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
| `Config.Secrets.Reveal`   | Show one stored secret to a holder of `config.manageSecrets`. Audited without the value and rate-limited per administrator.                                    |
| `Config.Own.Get`          | The calling service or device reads its own effective values.                                                                                                  |
| `Config.Own.Report`       | The calling service or device reports whether it applied the revision it read or its checks rejected it.                                                       |
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

## Revealing a secret

The original rule was that no administrator ever reads a secret back. It was
reversed because an administrator who can replace a credential can already
take over whatever it protects, and the alternative was copying secrets out of
band to check them. A secret is now shown on explicit request to holders of
`config.manageSecrets`, and only then:

- `Config.Secrets.Reveal { target, field }` returns the value stored at the
  target, or for an instance without its own the deployment's, with `source`
  saying which. It works on deployments, instances, and waiting claims. An
  app's client has no secrets.
- Every settings capability allows the RPC so that a `config.read` or
  `config.manage` caller gets a refusal naming `config.manageSecrets`, as the
  other settings refusals do. The handler checks `config.manageSecrets`.
- The reveal writes a security audit record (kind `rpc.config.secrets.reveal`)
  in the same transaction that reads the value: the actor, the target, the
  field, its source, and the revision, never the value. A value is shown only
  once its reveal is on record.
- Each administrator may see 30 secrets in any 10-minute window, counted from
  those audit records; the next is refused with `rate_limited`. That is enough
  for a sitting with a few services, and too few to copy a large installation
  out unnoticed.
- `Config.Get`, revision history, and audit records still never carry a
  secret's value. The Console holds a revealed value only while it is shown,
  in the field it was asked for, and the CLI's `config reveal FIELD` prints it
  to standard output with a warning on standard error when that is a
  terminal.

Exporting a service for another installation
([service-export.md](../auth/service-export.md)) also opens its secrets: the
current revision's secrets travel in the passphrase-encrypted bundle and are
sealed again with the importing installation's storage key. Export needs
`config.manageSecrets` as well as `users.manage`, and its audit record
(`deployment.exported`) names the secrets without their values. It is not
counted against the reveal limit, which covers reading secrets one at a
time.

The CLI follows the target-first shape: `trellis svc <ID> config` (or
`trellis dev <ID> config` for a device type) shows the settings, `reveal
FIELD` prints a secret, and `set FIELD=VALUE`, `set-secret FIELD --from-file
PATH`, `unset`, `history`, `rollback REVISION`, and `delete-revision REVISION`
change them. `--instance`
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

Each command writes one revision through one `Config.Set`, so related values
arrive in one change. `set` takes plain `FIELD=VALUE` arguments together with
`--secret FIELD=@PATH` (repeatable, `@-` for standard input), `--values-file`
(a JSON object), and `--env-file` (`NAME=VALUE` lines). A field from a file is
stored as a secret when the installed build declares it secret, which is the
safe default since Auth refuses a plain value for one, or when
`--secret-keys` names it; every other is plain. A declared secret given as
`FIELD=VALUE` is refused before anything is sent. `set-secret` takes one
`FIELD` as before, or several `FIELD=@PATH`, or `--env-file` with every line
a secret. `show` lists what each instance did with the newest settings, and
`trellis svc <ID> instances` has a Settings column: `applied r12`,
`rejected r13: awsSecret: set both or neither`, or `pending r13`. An
instance with `restartPending` reads `applied r12, restart pending` in both,
and Restart needed in the Console, since it runs only the live part of the
revision it applied. For a rejected instance, `Config.Get` gives each
refused field's `sources` entry: where its value came from in the revisions
the instance refused (`instance`, `deployment` or `default`), worked out from
those revisions rather than from the overrides stored now. `show`, in the
deployment's view and the instance's alike, advises the rollback of that
layer: the deployment's when the instance has no overrides or none of the
refused values was its override, the instance's (`--instance ID rollback`)
when all of them were. When they came from both, or the check refused the
revision as a whole, it names both. Following it never rolls back values
every instance shares while the override that was refused stays in place,
or the reverse.

Removing an override can expose a deployment value the instance refuses,
while the other instances accept it. Auth keeps, with each report, the
instance revision of the settings the instance applied last
(`applied_instance_revision`), since a rejecting instance keeps running them.
When every refused value from beneath the instance was its own override in
that revision, the status's `overrideRevision` names it, and `show` advises
`--instance ID rollback <overrideRevision>` first, which restores the
overrides it accepted, and the deployment's rollback only as the alternative
that changes every instance, and only when a refused value came from a
deployment revision: a declared default or an unset value exposed by the
removal is not undone by any deployment rollback. An override that was itself refused before it
was removed was never applied, so the advice stays the deployment's.

A service ID or title that names a waiting claim targets the claim, so
settings can be set before the service connects. `show` lists values the build
doesn't use and marks values that don't fit it.

`trellis clients config CLIENT` does the same for an app. The Console shows a
Settings section on each service page (a waiting claim's included), on each
device type's page, and in each app's editor that has settings, with a form
for the declared values, the values the build doesn't use in their own
section, each removable after a confirmation, rows to add settings by name and
value (text, number, on or off, or JSON, optionally secret), the instance
selector (offered whenever the deployment has an instance), the restart
notice, each instance's outcome for the newest settings (applied, rejected
with its fields and reason, or waiting), and earlier versions to restore or
delete. The form reads each field's
JSON Schema, described with the IDL's doc comments, and gives it an input of
its type: a switch for `bool`, a bounded number input, a choice for an enum
(open, so it also takes another value), typed rows for `list<T>` that can be
added, removed, and reordered, nested fields for a model, named entries for
`map<T>`, a date and time for `timestamp`, and a null control for a nullable
field. Only `json` and types nested too deeply fall back to a JSON editor,
which says so. It checks bounds, lengths, required model fields, and item
counts as values are edited and saves nothing while an edited field doesn't
fit, since Auth itself stores any value and leaves fitting to each build. Adding a service takes
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
  static secrets shown only on audited request.
- **Settings are unavailable while Trellis is down.** A restarting instance
  cannot read them without Trellis. That is close to no change, since a service
  cannot do anything useful without Trellis today, but it does put one more
  read on every participant's connect path.
