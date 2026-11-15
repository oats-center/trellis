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
  same way resources are. Auth checks every write against the build the
  deployment has installed. A bad value is refused when an administrator saves
  it, never discovered when the service starts.
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

A field without a default is required. An app's fields must all have defaults,
since an app cannot wait for an administrator the way a service does. A
default is a JSON scalar (a quoted
string, a number, `true`, `false`, or `null`) and the compiler checks it
against a primitive type. A secret cannot have a default, since it would sit in
the source. Only services may declare `secret` fields: a device runs where its
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
against the declaration. `trellis clients put FILE --source PATH [--app ID]`
registers the client and attaches the declaration in one step. A client with no
declaration has no settings. An app reads its values with `Config.App.Get`,
which answers for the client of the caller's own session. The TypeScript
browser and native app SDKs expose them as `client.config`, with `current`,
`revision.app`, and `onChange`.

## Storage and revisions

Auth keeps settings in `auth_config_revisions`, one row per revision of a
target. A target is a deployment, one of its instances, or an app's client. Each write stores the
target's complete values as a new revision with its author and time, and
records which fields and which secrets changed. The highest revision is
current. Rolling back writes an earlier revision's values as a new revision,
so history only grows forward.

Every revision is kept until an administrator deletes it. That is how a
replaced password stops existing anywhere. The current revision cannot be
deleted.

An instance's values are its own overrides on top of its deployment's, then
the declared defaults. A stored value that no longer matches the installed
build's type is skipped and reported as invalid, so a retyped field falls back
instead of breaking the participant.

Secrets never go into `values_json`. They are sealed as one blob per revision
with AES-256-GCM under the installation's storage key from `secrets.toml`, the
same key that seals OAuth records and OIDC client secrets. The associated data
names the target, not the revision, so a rollback carries sealed values
forward unchanged. A copied database file or backup does not expose them.
Audit records of a write list secret names, never values.

Encrypting to each instance's own key, so Auth stores values it cannot read, is
a possible later step: an instance's Ed25519 identity key converts to an X25519
encryption key. It is not done because every write would have to encrypt for
each instance, a new instance could not read existing secrets until someone
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

`Config.Changed` carries the target and the new revision, never values. Every
service and device may subscribe to it, so the event only prompts the SDK to read
`Config.Own.Get` again over its own authenticated session; its payload is never
trusted for values. The SDK also reads again whenever its subscription is
reestablished, so a change made while it was disconnected is not missed.

A change to a `restart` field does not reach `onChange`. The value loaded at
connect stays in effect until the participant connects again. A field whose
value was removed keeps its last value until then too. Auth reports, per
instance, the revisions it loaded at connect, so the Console and CLI show which
instances still run older restart-only values. Trellis never restarts anything:
podman, systemd, or whatever runs the process owns its lifecycle.

## Missing configuration

A required field with no value holds the participant the way an update waiting
for approval does. The SDK logs which fields are missing, prints a link to the
deployment's settings in the Console with a terminal QR code, and keeps reading
with backoff. It never starts handlers with a partial configuration. Once an
administrator saves the missing values, the waiting participant continues
without a restart.

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
| `Config.Revisions.Delete` | Delete an earlier revision.                                                                                                                                    |
| `Config.Rollback`         | Write an earlier revision's values as a new revision.                                                                                                          |
| `Config.Own.Get`          | The calling service or device reads its own effective values.                                                                                                  |
| `Config.Changed` (event)  | A target's settings changed.                                                                                                                                   |

The target is `{ deploymentId }`, `{ deploymentId, instanceId }`, or
`{ clientId }`. `Config.Apps.Declare` attaches an app's declaration to its
client, and `Config.App.Get` is the app's own read.

Three administrative capabilities cover the surface, all in the default
administration group: `config.read` sees settings, `config.manage` changes
them, and `config.manageSecrets` sets secrets. A write that touches secrets
needs `config.manageSecrets`; one that touches settings needs `config.manage`;
a rollback needs whichever its changes touch. So a role can tune settings
without touching credentials.

The CLI follows the target-first shape: `trellis svc <ID> config` (or
`trellis dev <ID> config` for a device type) shows the settings, and `set
FIELD=VALUE`, `set-secret FIELD --from-file PATH`, `unset`, `history`,
`rollback REVISION`, and `delete-revision REVISION` change them. `--instance`
selects one instance's overrides. A text setting takes the text as written;
any other is JSON. `set-secret` reads a file or standard input, so a secret
never appears in shell history or the process list.

`trellis clients config CLIENT` does the same for an app. The Console shows a
Settings section on each service page, on each device type's page, and in each
app's editor, with a form for the values, the instance selector, the restart
notice, and earlier versions to restore or delete. The link a waiting
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
