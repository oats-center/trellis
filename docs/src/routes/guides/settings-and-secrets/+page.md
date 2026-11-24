---
title: "Settings and secrets"
description: "Declare typed settings and secrets in IDL, read them in a service, and set them from the Console or the trellis CLI."
---

# Settings and secrets

A service's database URL, log level, and database password don't belong in
environment variables. The service declares them as `config` members in its
IDL, an administrator sets the values in the Console or with `trellis svc <ID>
config`, and Auth keeps them. The service reads them when it connects and hears
when they change. The only value a deployment keeps on disk is the instance's
identity seed.

There is no environment fallback. Local development and tests set settings
through the same `Auth.Config.*` RPCs that production uses.

Services, devices, and apps can declare settings. Only services can declare
secrets.

## Declare settings

Each `config` member declares one field:

```trellis
enum LogLevel { "info"; "debug"; }
type Port = int32(min=1, max=65535);
type Region = string(min_length=1, max_length=32);

service OrdersService {
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
  config webhookKey {
    title "Webhook signing key";
    description "Signs outgoing webhooks. Webhooks go unsigned without it.";
    type string | null;
    secret;
  }
  config logLevel {
    title "Log level";
    description "Lowest level written to the service log.";
    type LogLevel;
    default "info";
  }
  config port {
    title "Port";
    description "Port the health endpoint listens on.";
    type Port;
    default 8080;
    restart;
  }
  config region {
    title "Region";
    description "Where new orders are placed, when it matters.";
    type Region | null;
  }
}
```

`trellis check` compiles it. The rules:

- The name is lowerCamelCase. `title` and `description` are required and show
  in the Console.
- `type` is any IDL type expression: a scalar, an enum, a model, `list<T>`,
  `map<T>`, or `T | null`. Constraints (`min`, `max`, `min_length`,
  `max_length`, `min_items`, `max_items`) go on a package-level type alias, as
  `Port` and `Region` show. Enums are open, so `logLevel` also accepts strings
  other than `"info"` and `"debug"`.
- `default` is a JSON scalar: a quoted string, a number, `true`, `false`, or
  `null`. The compiler checks it against the field's JSON Schema, the same
  schema Auth checks values against, so a default that doesn't fit the type
  fails `trellis check`.
- A field without a default is required. The exception is a field whose type
  allows null, directly or through an alias such as
  `type OptionalKey = string | null;`. It reads null until someone sets it, so
  nothing waits for it. `region` above is optional.
- `restart` marks a field the service reads only when it connects. A change
  reaches the service when it next connects, not before.
- `secret` marks a field kept sealed and shown only on request (see
  [Secrets](#secrets)). A secret can't have a default other than
  `null`, since the default would sit in the source, so an optional secret is
  written `type string | null; secret;`. Only a service may declare `secret`.
  A device runs where its user can read anything it holds, and an app's
  settings are readable by every user of the app.

A field's type, default, `secret`, and `restart` are part of the participant
digest. Its title and description are not. Changing a field's shape makes a new
build, which goes through
[service updates](/guides/install-service-from-image#6-upgrade) like any other
change. It asks for no new access, so Auth installs it without an
administrator.

### App settings

An `app` or `agent` request can declare `config` fields too:

```trellis
app OrdersPanel {
  config pageSize {
    title "Page size";
    description "Orders shown per page.";
    type int32;
    default 25;
  }
}
```

Every user of the app can read its settings, so app fields can't be secret and
each one needs a default. The compiler adds
`trellis.auth.v1:config.readApp` to the request's required capabilities. Every
user holds it through the `account` group. The package must depend directly on
the `trellis` package.

Auth knows an app only as an OAuth client, so an administrator attaches the
app's declaration to its client when registering it:

```sh
trellis clients add --source ./orders-panel --app acme-orders.OrdersPanel
trellis clients edit acme.orders-panel --source ./orders-panel
trellis clients put client.json --source ./orders-panel
```

`--app` is needed only when the project declares several apps. Attach each new
build's declaration the same way. When the new declaration changes what the app
reads, such as a new default for a field nobody set, connected apps read their
settings again. A client with no declaration has no settings. Change an app's
values with `trellis clients config CLIENT`, which takes the same subcommands
as `trellis svc <ID> config` below, or in the Console's app editor.

## Read settings in a service

The generated participant types the values. In TypeScript, a service gets
`service.config` and a device gets `device.config`:

```ts
const service = await TrellisService.connect({
  participant: participants.OrdersService.participant,
  name: "orders-service",
  trellisUrl,
  seed,
}).orThrow();

const pool = openPool(service.config.current.databaseUrl, {
  password: service.config.current.databasePassword,
});
logger.level = service.config.current.logLevel;

const stopWatching = service.config.onChange(({ current, changed }) => {
  if (changed.includes("logLevel")) logger.level = current.logLevel;
});
```

`openPool` and `logger` stand for the service's own code. `current` holds the
values in effect, secrets included. `revision` names the
deployment and instance revisions behind them. `onChange(listener)` runs after
a revision that changes a field that applies live is accepted (see
[Check settings before they apply](#check-settings-before-they-apply)) and
returns a function that removes the listener. A browser or native app gets `client.config` with the
same shape and `revision.app`.

In Rust, the generated `Provider` returns a
`trellis_rs::service::ConfigHandle<Config>`, where `Config` is a generated
struct with one field per setting. A device's generated client has the same
`config()` method.

```rust
let mut config = Provider::new(&mut service).config();
let settings = config.current()?;
let pool = open_pool(&settings.database_url, &settings.database_password);
set_log_level(settings.log_level);
tokio::spawn(async move {
    while let Ok(changed) = config.changed().await {
        set_log_level(changed.log_level);
    }
});
```

Field names are snake_case (`log_level`). `current()` decodes the values in
effect. `revision()` returns the revisions
behind them. `changed().await` waits for the next accepted live change and
returns the new values. The Rust SDK has no settings handle for apps.

### At connect

When a participant that declares settings connects, the SDK reads its values
before any handler starts. `TrellisService.connect`, `TrellisDevice.connect`,
and the Rust `connect` return only once every required field has a value that
fits the build.

A required field with no value, or whose stored value doesn't fit the build's
type, holds the service. It prints to standard error which fields it needs and
why each saved value doesn't fit, then a link to the deployment's Settings
section in the Console and the same link as a terminal QR code:

```text
orders-service needs these settings before it starts: databaseUrl, databasePassword
Set them at https://trellis.example/console/admin/services/<deployment>#settings
or scan:
<QR code>
```

The link appears when Trellis serves the Console. The SDK reads again after
1 second, doubling the wait up to 15 seconds, and prints again only when the
list changes. Once an administrator saves values that fit, the service
continues without a restart. It never starts handlers with a partial
configuration.

### Live changes

A change made through the Console or CLI publishes `Config.Changed`. The event
carries the target and revision, never values. The SDK reads its settings again
over its own authenticated session, and again whenever its subscription is
reestablished, so a change made while it was disconnected is not missed.

What reaches `onChange` and `changed()`:

- A field marked `restart` keeps the value loaded at connect until the
  participant connects again.
- A field whose stored value was removed keeps its last value until the
  participant connects again. At that connect it waits for a value if the field
  is required.
- Values for fields the build doesn't declare, and values that don't fit its
  types, never arrive.

A running instance reads through the declaration of the build it runs. A newer
build installed for the deployment doesn't change what an older instance reads.

### Read settings at each use

`config.current` always holds the revision accepted last, so reading it where
the value is used is how a service follows live settings. A copy taken once
keeps the values it was copied from:

```ts
// Follows changes: read where it is used.
function headers() {
  return { authorization: `Bearer ${service.config.current.apiKey}` };
}

// Pinned: this constant keeps the key the service started with.
const apiKey = service.config.current.apiKey;
```

The same goes for a value built from settings: read a derived value's
`current` at each use, not a copy of it. In Rust, `config.current()` decodes
the values in effect each time it is called, and a `Config` kept from an
earlier call, or cloned out of `changed()`, keeps the values it was read from.

### Check settings before they apply

A live change takes effect in two steps. First the service's checks and
derive builders run against the candidate values. If any of them refuses,
the revision is rejected: `current` keeps the values accepted last, no
`onChange` listener runs, and `changed()` returns nothing. Only an accepted
revision changes the values and reaches `onChange`. A listener that throws is
logged; it never rejects a revision.

```ts
await service.config.check((candidate, { reject }) => {
  if ((candidate.awsKeyId === null) !== (candidate.awsSecret === null))
    reject(["awsKeyId", "awsSecret"], "set both or neither");
});

const stripe = await service.config.derive(
  ["stripeSecretKey"],
  (candidate) => new Stripe(candidate.stripeSecretKey),
  { dispose: (old) => old.close() },
);
// At each use:
await stripe.current.charges.create(charge);
```

A check calls `reject(fields, reason)` for each problem it finds and may be
async. A check that throws rejects every field the candidate changes, with
the error's message. `derive(fields, build, { dispose })` builds its first
value from the settings in effect. After that it rebuilds only during the
check step of a revision that changes one of `fields`; a revision that changes
other fields keeps the value. A builder that throws rejects that revision for
`fields` and keeps the old value. `dispose` receives a replaced value after
its replacement took effect, and the value still in effect when the service
stops or the device's connection closes, so nothing built from settings
outlives the participant. A value still being built when it stops never takes
effect and is disposed too. `service.stop()` returns once those disposers
have. `context.changed` lists the fields the candidate changes.

In Rust:

```rust
use trellis_rs::service::Rejection;

let config = Provider::new(&mut service).config();
config
    .check(|c: &Config| {
        if c.webhook_url.starts_with("https://") {
            Ok(())
        } else {
            Err(Rejection::new(["webhook_url"], "must be an https URL"))
        }
    })
    .await?;
let stripe = config
    .derive(["stripe_secret_key"], |c| Ok(Stripe::new(&c.stripe_secret_key)))
    .await?;
stripe.current().charge(&order).await?; // Arc<Stripe>, read at each use
```

`Rejection::new(fields, reason)` names fields as the generated struct does
(`webhook_url`) or as declared (`webhookUrl`). Any other error, through `?`,
rejects every changed field with its message. `derive_async` takes a builder
that awaits, such as one that opens a connection. A replaced derived value is
dropped once its replacement took effect and no caller holds it.
`changed().await` yields only accepted revisions.

Browser and native apps get `check` and `derive` on `client.config` too. A
rejection keeps the app's values as they were; apps have no instances, so
nothing is reported to Auth.

### At startup

Registering a check or a derive checks the values in effect at once. If they
pass, the `await` returns. If they don't, it keeps waiting: the service prints
which fields were refused and why, with a link to its Settings section and the
same link as a QR code, and reports the rejection to Auth. The `await` returns
once an administrator saves a revision that passes.

```text
orders-service rejected its settings: webhookUrl: must be an https URL
It waits for settings that pass.
Fix them at https://trellis.example/console/admin/services/<deployment>#settings
or scan:
<QR code>
```

Register checks and derives before `service.wait()` (or `run()` in Rust).
While a registration waits, the service has not started serving, so its
readiness stays at starting. A `restart` field never changes live, so a check
that refuses one waits until the instance restarts with a fixed value.

### What each instance did with its settings

Each instance tells Auth what became of every revision it reads, with
`Config.Own.Report`. `Config.Get` returns it per instance as `settings`:

- `applied`: the instance accepted the newest revision. With `restartPending`
  on the instance, it took only the values that apply live and keeps the
  connect-time values of settings that apply after restart until it restarts.
  The Console shows it as Restart needed.
- `rejected`: one of its checks or derive builders refused it. `fields` and
  `reason` say why, and the instance keeps the values it accepted last.
- `pending`: the instance hasn't read the newest revision yet, or is still
  starting. A running instance reads a new revision within seconds; a stopped
  one reads it when it next starts.

An instance reports the revision it starts with as `applied` only once its
startup is done, so the checks and derives it registers have all run first. A
service's startup ends when it first serves, after `service.wait()` (or
`run()` in Rust) begins. A device has no serving phase, so its startup ends
the first time it reads its settings or a derived value. Until then the
status stays `pending`, and a starting value a check refuses shows as
`rejected` without first showing as `applied`. A restarted instance's status
from its earlier run is dropped when it connects, and so is an upgraded
one's: an instance on a new build that waits for a setting the build adds
reads `pending`, not what its earlier build did with the same revision. After startup, each new
revision is reported as soon as its checks have run. A report that doesn't
reach Auth, such as while the server restarts, is sent again with a backoff
until Auth has it or a newer outcome replaces it, so the status catches up
without another save.

Saving never waits for instances, and a rejected revision stays stored, so
`config rollback` can undo it. A rollback undoes one layer: the deployment's
settings, which every instance shares, or with `--instance ID` one instance's
overrides. For a rejected instance, `config` names the layer that holds the
refused values when it knows: an instance without overrides takes them from
its deployment, and `config --instance ID` shows where each of that
instance's values comes from. The deployment's view of an instance with
overrides names both commands. While the newest revision is rejected, the
instance's `settings` health check is degraded with the reason, which makes the
instance degraded. It recovers with the next accepted revision.

### In tests

Tests store values through the same `Config.Set` an administrator uses. In the
TypeScript testkit, pass them when creating the instance:
`runtime.services.createInstance({ ..., config: { values, secrets } })`. In the
Rust testkit, call `TrellisTestRuntime::set_config(&identity, values,
secrets)` with two JSON objects before the service connects.

## Set values

Values belong to the deployment. An instance (a service replica or one device)
can override individual fields, and everything it doesn't override comes from
the deployment, then from the declared defaults. A device type's per-device
values, such as `site`, go in instance overrides.

Every write stores the target's complete values as a new revision with its
author and time. The highest revision is current. A write names the revision it
was based on and is refused if another write landed first, so read and try
again.

### In the Console

The service's page has a **Settings** section, and so does each device type's
page and the editor of each app that has settings. The link a waiting service
prints opens it.

- Each declared field is a row: its title, description, and badges for
  **Required**, **Secret**, and **Applies after restart** on the left, and on
  the right an input that fits its type, where the value comes from, and its
  default.
- The input follows the IDL type. A `bool` is a switch. A number takes only
  numbers and says its bounds; a value outside them is marked beside the
  field and isn't saved. An enum is a list to choose from, with **Other
  value** since enums are open. A `list<T>` is a list of rows, each typed as
  `T`, which you add, remove, and move up or down. A model shows its own
  fields, with **Add** and **Remove** for optional ones. A `map<T>` takes named
  entries. A `timestamp` takes a date and time in your local time. A nullable
  field inside a list or model has **No value**. Only a `json` field, or a
  type nested too deeply for a form, is typed as JSON, and the form says so.
- What's wrong with a value shows under it in plain words while you edit, and
  **Save settings** saves nothing until every edited field fits.
- **For** picks **Every instance** (the deployment's values) or one instance's
  overrides. It appears once the deployment has an instance.
- **Clear** stores null, for a field whose type allows it. **Reset** removes
  the stored value so the default applies. On an instance it reads **Use
  service-wide** and removes the override.
- A secret's field is masked with the same dots whatever its length. Its eye
  shows what you typed or, for a saved secret, asks Auth for the value (see
  [Seeing a secret](#seeing-a-secret)). Hiding it again drops the value from
  the page.
- **Not used by this build** is its own section listing stored values the
  build doesn't declare, secrets included. Each has **Remove**, which asks
  first and then unsets it.
- **Add by name** takes rows of a name, a format (**Text**, **Number**, **On
  or off**, or **JSON** for lists and objects), a value, and a **Secret** box,
  for fields no installed build declares yet.
- **Save settings** writes one new revision. When it changes a `restart` field
  it first asks you to confirm, since you restart the service yourself.
- **Earlier versions** lists older revisions, each with **Restore** and
  **Delete**.
- **Instances** lists each instance with **Applied**, **Rejected**, or
  **Waiting** for the newest settings. An instance that rejected them gets a
  warning naming the fields and its reason.

**Add service** takes settings too. With an IDL chosen, it shows the IDL's
fields. Without one, it takes the same name-and-value rows. They are saved right
after the service is created. If that save fails, the service still exists and
the form says to set them on its page.

### With the CLI

`trellis svc <ID> config` works on a service deployment, `trellis dev <ID>
config` on a device type, and `trellis clients config CLIENT` on an app. The
service ID can be a deployment's ID or title, or a waiting claim's.

| Command                               | What it does                                                                                                                                                      |
| ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `config` or `config show`             | Each setting with its value, where it comes from, and notes (required, applies after restart, doesn't fit), then what each instance did with the newest settings. |
| `config set FIELD=VALUE ...`          | Set one or more settings in one revision.                                                                                                                         |
| `config set --null FIELD`             | Store null for a setting whose type allows it. Repeat for several.                                                                                                |
| `config set ... --secret FIELD=@PATH` | Set secrets from files in the same revision as the settings. `@-` reads standard input. Repeat for several.                                                       |
| `config set --values-file PATH`       | Set the settings in a JSON object, in the same revision as the rest of the command.                                                                               |
| `config set --env-file PATH`          | Set the settings in `NAME=VALUE` lines, read as `FIELD=VALUE` arguments are.                                                                                      |
| `config set-secret FIELD`             | Set a secret: asked for on a terminal, or read from standard input or a file with `--from-file PATH`.                                                             |
| `config set-secret FIELD=@PATH ...`   | Set several secrets from files in one revision. `--env-file PATH` takes every line as a secret.                                                                   |
| `config set-secret FIELD --null`      | Store null for a secret whose type allows it.                                                                                                                     |
| `config reveal FIELD`                 | Print a saved secret to standard output. Needs `config.manageSecrets`, and each reveal is recorded.                                                               |
| `config unset FIELD ...`              | Remove stored values, so the default applies, or the deployment's value for an instance.                                                                          |
| `config history`                      | Revisions, newest first, with what each changed, when, and by whom. Secrets are listed by name.                                                                   |
| `config rollback REVISION`            | Write an earlier revision's values as a new revision.                                                                                                             |
| `config delete-revision REVISION`     | Delete an earlier revision. Not the current one, nor one a build still takes a value from.                                                                        |

`--instance INSTANCE`, by instance ID or title, works with every subcommand and
selects that instance's overrides. A waiting claim has no instances yet, and
an app never does. `--format json` prints what Auth returned.

How `set` reads a value:

- A declared text field (a string, a string enum, or either one or null) takes
  the text as written: `region=east`.
- Any other declared field takes JSON: `port=9090`, `debug=true`,
  `hosts='["a","b"]'`.
- A field the build doesn't declare takes JSON when the value parses as JSON
  and text otherwise. `workers=4` stores the number 4, `zone=east` stores
  text, and `'zone="4"'` stores the text `4`.
- A text field takes `null` as the text "null", so store null with
  `set --null FIELD`.

After a write the CLI prints the saved revision and shows the settings again,
with what Auth reports about them. The CLI doesn't check types itself. Fields
that are still missing are listed as ones the instances wait for, with the
command to set the first.

Each command writes one revision, so values that only make sense together,
such as a key ID and its secret, reach the service in one change:

```sh
trellis svc orders config set level=debug --secret awsKeyId=@key.txt --secret awsSecret=@secret.txt
trellis svc orders config set --env-file orders.env --secret-keys partnerToken
trellis svc orders config set-secret awsKeyId=@key.txt awsSecret=@secret.txt
```

In `--values-file` and `--env-file`, a field the installed build declares
secret is stored as a secret without being marked, since Auth refuses a plain
value for it. `--secret-keys a,b` marks others as secrets, such as fields no
installed build declares yet; every other key is a plain setting. A secret is
never given as `FIELD=VALUE` on the command line, where shell history and the
process list would keep it: `set` refuses that for a declared secret and
names `--secret FIELD=@PATH`. An env file skips blank lines and `#` comments,
ignores a leading `export `, and quotes values as dotenv files do: a value in
single quotes is taken as written, and in double quotes `\n`, `\r`, `\t`,
`\"` and `\\` are escapes while any other backslash stays, so
`dataDir="C:\data"` is `C:\data`. A field named twice in one command is
refused, even twice in one file, and nothing is saved.

`trellis svc <ID> instances` shows the same status in its Settings column:
`applied r12`, `rejected r13: awsSecret: set both or neither`, or
`pending r13`, with `, restart pending` after the revision while the instance
still runs older values for settings that apply after restart.

```sh
trellis svc orders config set databaseUrl=postgres://db.internal/orders logLevel=debug
trellis svc orders config set-secret databasePassword --from-file ./db-password
trellis svc orders config --instance orders-2 set logLevel=info
trellis svc orders config --instance orders-2 unset logLevel
trellis svc orders config history
```

## Values ahead of a build

Auth stores a value for any lowerCamelCase field name, whether or not a build
declares it and whether or not any build is installed. Each build then reads
what is stored through its own declaration.

### A service added without its IDL

A service created without `--source` is a claim until its service connects and
an administrator approves it (see
[Install a service from an image](/guides/install-service-from-image#without-the-release)).
Set its settings on the claim, by name:

```sh
trellis svc partner-orders config set endpoint=https://orders.example workers=4
trellis svc partner-orders config set-secret apiKey --from-file api-key.txt
```

Once the service connects and submits its IDL, `config` shows the declared
fields, what is missing, and what doesn't fit, before anyone approves it.
Approving moves every revision to the new deployment and seals each revision's
secrets again for the deployment, so the service starts with them. Removing a
waiting claim deletes its settings.

### An upgrade without downtime

While build 1 runs, set the values build 2 needs: fields only build 2 declares,
or a value in the type build 2 gives a field. Build 1 is unaffected, and when
build 2 is installed its instances find their settings in place and start at
once.

The whole upgrade can then happen before build 2's image starts. Set its
values, install its published package with `trellis svc <ID> apply --package
WORKSPACE/NAME@VERSION` (or **Install a build** on the service's page), then roll out the
image: its instances have their build and their settings, and nothing waits for
approval. See [Upgrade](/guides/install-service-from-image#6-upgrade).

### Unused and invalid values

- A stored field the build doesn't declare is ignored. `config` lists it as
  "not used by this build", or "no build installed yet" before Trellis has the
  IDL. Remove it with `unset` once no build needs it.
- A stored value that doesn't fit the build's type is skipped. The build uses
  the most recent earlier value that fits, walking back through the target's
  revisions until the field wasn't stored, and then the default. `config`
  shows why the value doesn't fit. A required field with no fitting value is
  missing, and the service waits for it.
- While a build takes a value from an earlier revision this way, that revision
  can't be deleted: deleting it would change what the build uses without a new
  revision or a change event. This covers the deployment's installed build and
  the build each running instance runs. The refusal names the field and the
  build. Set a value that fits, and the revision can go.
- A secret stored as a plain setting, or a plain setting stored as a secret,
  is a mismatch of the same kind. A plain value under a secret's name is never
  shown either; see [Plain values under a secret name](#plain-values-under-a-secret-name).

So a value set in build 2's type leaves build 1 on its previous value with no
change event, while build 2 uses the new one.

Auth refuses a write in two cases: a plain value for a field the installed
build declares secret, and any secret for an app.

A service update that adds a required field with no default installs as usual.
Its instances then wait for the value like any other missing setting. The
approval screen doesn't collect values, so set them ahead of the update or
after it.

## Secrets

A secret is a field declared `secret`, or a value written with `set-secret` or
the **Secret** box for a field no build declares yet.

### Who can set and see them

Three administrative capabilities cover settings. All three are in the default
`administration` group.

| Capability                             | Allows                                                                                   |
| -------------------------------------- | ---------------------------------------------------------------------------------------- |
| `trellis.auth.v1:config.read`          | See settings and their history. Secrets show only whether they are set.                  |
| `trellis.auth.v1:config.manage`        | Change, roll back, and delete settings, and attach app declarations. No secrets.         |
| `trellis.auth.v1:config.manageSecrets` | Set, replace, and see secrets, roll back, and delete revisions. Each reveal is recorded. |

A write that touches a secret needs `config.manageSecrets`, and one that
touches a plain setting needs `config.manage`, so a role can tune settings
without being able to replace credentials. A rollback needs whichever its
changes touch. Deleting a revision that holds a secret destroys that secret,
so it needs `config.manageSecrets`. `config.manage` can delete a revision with
no secrets in it. A refusal names the capability that was missing.

Only `config.manageSecrets` reads a secret, and only on explicit request (see
below). The deployment's own instances receive secret values through
`Config.Own.Get` under `trellis.auth.v1:config.readOwn`, which every service
and device instance holds for itself. In the service, secrets are plain values
in `config.current`.

### Shown only on request

A secret's field in the Console is masked with a fixed row of dots when it is
set, whatever its length, and reads "Not set" otherwise. An unused secret
reads "Secret, set". The CLI's `config show` prints `(secret, set)` or
`(secret, not set)`. `Config.Get` returns only `isSet` for a secret, and the
reason given for a mismatch never contains its value. Revision history and
audit records name the secrets a write changed, never their values.

### Seeing a secret

An administrator holding `config.manageSecrets` can ask for a saved secret's
value. In the Console, the eye on a secret's field fetches it and shows it in
the field. Hiding it again, typing over it, saving, or choosing another
instance drops the value from the page; the Console keeps it nowhere else. On
the command line:

```sh
trellis svc orders config reveal databasePassword
trellis svc orders config --instance orders-2 reveal databasePassword
```

`reveal` prints the value alone on standard output, so a script can capture
it: a text secret as written, any other as JSON, and with `--format json` an
object with `value` and `source`. When standard output is a terminal, it first
warns on standard error that the value stays in the terminal's scrollback.

Both call `Config.Secrets.Reveal` with the target and the field. It works on a
deployment, an instance, and a waiting claim. On an instance without its own
value it shows the deployment's, and `source` says which. Auth records every
reveal in its security audit log, with who asked, the target, the field, and
the revision, never the value. Each administrator can see 30 secrets in any 10
minutes; the next request is refused with `rate_limited` until the window
moves on. `config.read` and `config.manage` reach the same RPC only to be
refused with a message naming `config.manageSecrets`.

### Setting a secret

`set-secret` never takes the value on the command line, so it never appears in
shell history or the process list. On a terminal, it asks for the value without
showing what you type, then asks for it again to catch a typo. A script gives
it a file with `--from-file`, or pipes it to standard input (`--from-file -`
reads standard input too):

```sh
trellis svc orders config set-secret databasePassword
trellis svc orders config set-secret databasePassword --from-file ./db-password
trellis svc orders config set-secret databasePassword < ./db-password
```

When nothing is piped in and it can't ask, because of `--no-input` or because
standard error isn't a terminal, it fails at once and names `--from-file`.

It drops trailing newlines and refuses an empty value. A text secret is stored
as written. Any other declared secret is parsed as JSON, and an undeclared one
follows the same JSON-or-text rule as `set`.

### How Auth stores them

Plain settings are stored as JSON in Auth's platform database. Secrets are kept
apart: each revision's secrets are sealed as one blob with AES-256-GCM under the
installation's storage key, `oauth_storage_key` in `secrets.toml`. It is the
same key that seals OAuth records and OIDC client secrets. The database holds
only the ciphertext, so a copied database file on its own exposes no secret.
Anyone with both the database and `secrets.toml`, such as someone with access
to the server, can decrypt them.

`trellis-server backup` copies `secrets.toml` into the backup along with the
databases (see [Back up](/guides/running-trellis/upkeep#back-up)). A complete
backup folder therefore exposes every service secret, and it needs the same
protection as the server. Keep `secrets.toml` readable only by the account that
runs Trellis. If you copy the database somewhere else, leave `secrets.toml` out
of that copy.

### Old secrets live in old revisions

Every revision is kept until an administrator deletes it, and a revision keeps
the secrets that were current when it was written. Replacing a password
therefore leaves the old one in the earlier revisions, where a rollback can
bring it back. To make a replaced secret stop existing, delete each earlier
revision that holds it with `config delete-revision REVISION`, or **Delete**
under **Earlier versions**. `config history` marks the revisions that changed a
secret with `(secret)`. Deleting a revision that holds a secret needs
`config.manageSecrets`. The current revision can't be deleted, and neither can
one a build still takes a value from (see
[Unused and invalid values](#unused-and-invalid-values)).

### Plain values under a secret name

Auth refuses a plain value for a field the installed build declares secret,
since it would be stored where every administrator can read it. A plain value
can still end up under a secret's name: it was saved before Trellis had the
IDL, and the IDL then declares the name secret. From the moment the installed
build declares it secret:

- The value is never shown again. `Config.Get` reports the field with
  `storedAsPlain` and no value, `Config.Revisions.List` leaves it out of each
  revision's `values` and names it in `hiddenValues`, the Console reads
  "Stored as plain text; set it as a secret", and `config show` prints
  `(stored as plain text)` with the `set-secret` command to run.
- It is never delivered to the service, which doesn't read a plain value as a
  secret. A required field counts as missing, so the service waits and gives
  the reason.

Fix it by setting the field as a secret, with `set-secret` or in the Console.
That write also removes the plain copy from the current revision, since a field
is stored either as a setting or as a secret, and it needs only
`config.manageSecrets`. The earlier revisions still hold the plain value until
you delete them.

Trellis doesn't turn the plain value into a secret on its own. Anyone with
`config.read` could already read it, so sealing it quietly would make it look
like a secret that never left Auth. Treat it as exposed, and set a new value
if it matters.

### Nullable secrets

An optional secret (`type string | null; secret;`) reads null until it is set,
so nothing waits for it. Two commands clear one, with different results:

- `set-secret FIELD --null` stores an explicit null. On an instance, it makes
  that instance go without a secret its deployment has.
- `unset FIELD` removes the stored value. On an instance, the deployment's value
  applies again. On the deployment, the field reads null.

In the Console, **Clear** stores null and **Reset** or **Use service-wide**
unsets.

## Restarts

A `restart` field changes only when the participant connects again. Auth
records, for each instance, the revisions it loaded at connect. `Config.Get`
reports `restartPending` for an instance whose own build would now read a
different value for one of its `restart` fields. Values for fields that build
doesn't declare, or that don't fit it, don't count, so setting values for the
next build doesn't ask anyone to restart the current one.

`trellis svc <ID> config` prints a note for each such instance, and the Console
shows a notice naming them. Trellis never restarts anything. Restart the
instance with whatever runs it, such as podman or systemd.
