//! `trellis svc|dev <ID> config`: settings Trellis keeps for a deployment and
//! its instances.

use std::io::IsTerminal as _;

use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};

use super::deploy::{lookup_deployment_id, lookup_waiting_claim, DeploymentKind};
use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::{ConfigArgs, ConfigCommand, OutputFormat};
use crate::{output, prompt};

/// Whose settings a command names.
pub(super) enum Scope<'a> {
    Deployment(DeploymentKind, &'a str),
    Client(&'a str),
}

/// Run one settings command against a deployment or an app's client.
pub(super) async fn run(
    format: OutputFormat,
    scope: Scope<'_>,
    args: ConfigArgs,
) -> miette::Result<()> {
    let (mut target, prefix, id) = match scope {
        Scope::Deployment(kind, id) => {
            let target = match lookup_deployment_id(kind, id).await? {
                Some(deployment) => json!({ "deploymentId": deployment }),
                // A service added without its IDL takes settings before its
                // service connects; approving it carries them over.
                None => match kind {
                    DeploymentKind::Service => match lookup_waiting_claim(id).await? {
                        Some(claim) => {
                            miette::ensure!(
                                args.instance.is_none(),
                                "{id} has no instances until its service connects and is approved; leave out --instance"
                            );
                            json!({ "claimId": claim })
                        }
                        None => return Err(miette::miette!("deployment not found: {id}")),
                    },
                    DeploymentKind::Device => {
                        return Err(miette::miette!("deployment not found: {id}"))
                    }
                },
            };
            (
                target,
                match kind {
                    DeploymentKind::Service => "svc",
                    DeploymentKind::Device => "dev",
                },
                id,
            )
        }
        Scope::Client(client) => {
            miette::ensure!(
                args.instance.is_none(),
                "an app has no instances; leave out --instance"
            );
            (json!({ "clientId": client }), "clients", client)
        }
    };
    let (_state, connected) = connect_authenticated_cli_client().await?;
    let auth = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected)
        .trellis_auth_v1();
    let mut current = to_value(
        auth.config_get(&wire(json!({ "target": target }))?)
            .await
            .into_diagnostic()?,
    )?;
    let mut label = if prefix == "clients" {
        format!("app {id}")
    } else {
        format!("{prefix}/{id}")
    };
    if let Some(instance) = &args.instance {
        let found = output::list(&current, "instances")
            .iter()
            .find(|entry| {
                entry["instanceId"].as_str() == Some(instance.as_str())
                    || entry["title"].as_str() == Some(instance.as_str())
            })
            .ok_or_else(|| {
                miette::miette!(
                    help = format!("List them with `trellis {prefix} {id} config`."),
                    "{label} has no active instance named {instance}"
                )
            })?;
        target["instanceId"] = found["instanceId"].clone();
        label = format!("{label} instance {}", output::field(found, "title"));
        current = to_value(
            auth.config_get(&wire(json!({ "target": target }))?)
                .await
                .into_diagnostic()?,
        )?;
    }
    let set = |change: Value| {
        let mut request = json!({
            "target": target,
            "idempotencyKey": ulid::Ulid::generate().to_string(),
        });
        if let Some(revision) = current.get("revision") {
            request["expectedRevision"] = revision.clone();
        }
        for (key, value) in change.as_object().into_iter().flatten() {
            request[key] = value.clone();
        }
        request
    };
    match args.command.unwrap_or(ConfigCommand::Show) {
        ConfigCommand::Show => show(format, &label, prefix, id, &current),
        ConfigCommand::Set {
            values,
            null,
            secret,
            values_file,
            env_file,
            secret_keys,
        } => {
            // Everything named here goes into one revision; a field named
            // twice is a mistake, not an override.
            let mut plain = serde_json::Map::new();
            let mut secrets = serde_json::Map::new();
            let mut add = |name: String, value: Value, as_secret: bool| {
                miette::ensure!(
                    !plain.contains_key(&name) && !secrets.contains_key(&name),
                    "{name} is given more than once"
                );
                if as_secret {
                    secrets.insert(name, value);
                } else {
                    plain.insert(name, value);
                }
                Ok(())
            };
            for name in null {
                add(name, Value::Null, false)?;
            }
            for pair in values {
                let (name, text) = pair.split_once('=').ok_or_else(|| {
                    miette::miette!(
                        help = "Write each setting as FIELD=VALUE, such as logLevel=debug.",
                        "`{pair}` has no `=`"
                    )
                })?;
                // A secret on the command line would sit in the shell
                // history and the process list.
                miette::ensure!(
                    !declared_secret(&current, name),
                    help = format!("Read it from a file: --secret {name}=@PATH"),
                    "{name} is a secret, so its value isn't taken on the command line"
                );
                add(name.to_owned(), parse_value(&current, name, text)?, false)?;
            }
            for spec in secret {
                let (name, value) = secret_from_file(&current, &spec)?;
                add(name, value, true)?;
            }
            let mut listed = Vec::new();
            if let Some(path) = values_file {
                for (name, value) in read_values_file(&path)? {
                    let as_secret = declared_secret(&current, &name) || secret_keys.contains(&name);
                    listed.push(name.clone());
                    add(name, value, as_secret)?;
                }
            }
            if let Some(path) = env_file {
                for (name, text) in read_env_file(&path)? {
                    let value = parse_value(&current, &name, &text)?;
                    let as_secret = declared_secret(&current, &name) || secret_keys.contains(&name);
                    listed.push(name.clone());
                    add(name, value, as_secret)?;
                }
            }
            if let Some(stray) = secret_keys.iter().find(|name| !listed.contains(name)) {
                miette::bail!("--secret-keys names {stray}, which no file given sets");
            }
            let updated = auth
                .config_set(&wire(set(json!({ "values": plain, "secrets": secrets })))?)
                .await
                .into_diagnostic()?;
            changed(format, &label, prefix, id, &to_value(updated)?)
        }
        ConfigCommand::SetSecret {
            fields,
            from_file,
            null,
            env_file,
        } => {
            let mut secrets = serde_json::Map::new();
            match fields.as_slice() {
                // One field: from a file, piped in, asked for, or null.
                [field] if !field.contains('=') => {
                    let value = if null {
                        Value::Null
                    } else {
                        let text = match from_file {
                            Some(path) if path != std::path::Path::new("-") => {
                                std::fs::read_to_string(&path).into_diagnostic()?
                            }
                            // A terminal never waits silently for end of
                            // input: it asks, or without questions fails
                            // naming the flag.
                            None if std::io::stdin().is_terminal() => {
                                let mut form = prompt::Form::new(true);
                                let typed = form.required_secret(
                                    &format!("Value for {field}"),
                                    "--from-file PATH",
                                )?;
                                form.finish()?;
                                typed
                            }
                            _ => std::io::read_to_string(std::io::stdin()).into_diagnostic()?,
                        };
                        secret_value(&current, field, &text)?
                    };
                    secrets.insert(field.clone(), value);
                }
                specs => {
                    miette::ensure!(
                        from_file.is_none() && !null,
                        help = "Give each as FIELD=@PATH, or one FIELD with --from-file or --null.",
                        "--from-file and --null take a single FIELD"
                    );
                    for spec in specs {
                        let (name, value) = secret_from_file(&current, spec)?;
                        miette::ensure!(
                            secrets.insert(name.clone(), value).is_none(),
                            "{name} is given more than once"
                        );
                    }
                }
            }
            if let Some(path) = env_file {
                for (name, text) in read_env_file(&path)? {
                    let value = secret_value(&current, &name, &text)?;
                    miette::ensure!(
                        secrets.insert(name.clone(), value).is_none(),
                        "{name} is given more than once"
                    );
                }
            }
            let updated = auth
                .config_set(&wire(set(json!({ "secrets": secrets })))?)
                .await
                .into_diagnostic()?;
            changed(format, &label, prefix, id, &to_value(updated)?)
        }
        ConfigCommand::Reveal { field } => {
            let revealed = to_value(
                auth.config_secrets_reveal(&wire(json!({ "target": target, "field": field }))?)
                    .await
                    .into_diagnostic()?,
            )?;
            if output::is_json(format) {
                return output::print_json(&revealed);
            }
            // The value is the output, so a script can capture it; on a
            // terminal it stays in the scrollback, which is worth knowing.
            if std::io::stdout().is_terminal() {
                output::warning(format!(
                    "The value of {field} is shown below and stays in this terminal's scrollback."
                ));
            }
            println!("{}", display(&revealed["value"]));
            Ok(())
        }
        ConfigCommand::Unset { fields } => {
            let updated = auth
                .config_set(&wire(set(json!({ "unset": fields })))?)
                .await
                .into_diagnostic()?;
            changed(format, &label, prefix, id, &to_value(updated)?)
        }
        ConfigCommand::History => {
            let revisions = auth
                .config_revisions_list_items(wire(json!({ "target": target }))?)
                .try_collect::<Vec<_>>()
                .await
                .into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&json!({ "revisions": revisions }));
            }
            let rows = revisions
                .iter()
                .map(|revision| {
                    let value = serde_json::to_value(revision).unwrap_or_default();
                    let names = |key: &str| {
                        output::list(&value, key)
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                    };
                    let changes = names("changed")
                        .into_iter()
                        .map(str::to_owned)
                        .chain(
                            names("changedSecrets")
                                .into_iter()
                                .map(|name| format!("{name} (secret)")),
                        )
                        .collect::<Vec<_>>();
                    json!({
                        "revision": value["revision"],
                        "current": if value["current"] == json!(true) { "current" } else { "" },
                        "changed": changes.join(", "),
                        "createdAt": value["createdAt"],
                        "createdBy": value["createdBy"],
                    })
                })
                .collect::<Vec<_>>();
            output::table(
                &rows,
                &[
                    output::muted("REVISION", "revision"),
                    output::state_col("", "current"),
                    output::col("CHANGED", "changed"),
                    output::time_col("WHEN", "createdAt"),
                    output::muted("BY", "createdBy"),
                ],
                &format!("{label} has no saved settings yet."),
            )
        }
        ConfigCommand::Rollback { revision } => {
            let expected = current
                .get("revision")
                .cloned()
                .ok_or_else(|| miette::miette!("{label} has no saved settings to roll back"))?;
            let updated = auth
                .config_rollback(&wire(json!({
                    "target": target,
                    "revision": revision,
                    "expectedRevision": expected,
                    "idempotencyKey": ulid::Ulid::generate().to_string(),
                }))?)
                .await
                .into_diagnostic()?;
            changed(format, &label, prefix, id, &to_value(updated)?)
        }
        ConfigCommand::DeleteRevision { revision } => {
            let updated = auth
                .config_revisions_delete(&wire(json!({
                    "target": target,
                    "revision": revision,
                    "idempotencyKey": ulid::Ulid::generate().to_string(),
                }))?)
                .await
                .into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&updated);
            }
            output::success(format!("Deleted revision {revision} of {label}'s settings"));
            Ok(())
        }
    }
}

fn to_value(value: impl serde::Serialize) -> miette::Result<Value> {
    serde_json::to_value(value).into_diagnostic()
}

/// Whether the installed build declares `name` secret.
fn declared_secret(current: &Value, name: &str) -> bool {
    output::list(current, "fields")
        .iter()
        .any(|field| field["name"].as_str() == Some(name) && field["secret"] == json!(true))
}

/// A secret's value from what was read for it: without trailing newlines,
/// never empty, and parsed as `set` parses a value.
fn secret_value(current: &Value, name: &str, text: &str) -> miette::Result<Value> {
    let text = text.trim_end_matches(['\r', '\n']);
    miette::ensure!(!text.is_empty(), "the secret for {name} is empty");
    parse_value(current, name, text)
}

/// `FIELD=@PATH`, or `FIELD=@-` for standard input, read as a secret.
fn secret_from_file(current: &Value, spec: &str) -> miette::Result<(String, Value)> {
    let help = "Write each secret as FIELD=@PATH, such as apiKey=@key.txt, or FIELD=@- for standard input.";
    let (name, source) = spec
        .split_once('=')
        .ok_or_else(|| miette::miette!(help = help, "`{spec}` has no `=`"))?;
    let path = source.strip_prefix('@').ok_or_else(|| {
        miette::miette!(
            help = help,
            "a secret's value isn't taken on the command line; read {name} from a file"
        )
    })?;
    let text = if path == "-" {
        std::io::read_to_string(std::io::stdin()).into_diagnostic()?
    } else {
        std::fs::read_to_string(path)
            .into_diagnostic()
            .map_err(|error| error.wrap_err(format!("reading {name} from {path}")))?
    };
    Ok((name.to_owned(), secret_value(current, name, &text)?))
}

/// `NAME=VALUE` lines of an env file, in order, quoted as dotenv files are.
/// Blank lines and `#` comments are skipped, a leading `export ` is ignored,
/// and a value in single quotes is taken as written. In double quotes, `\n`,
/// `\r`, `\t`, `\"` and `\\` are escapes, and any other backslash is kept as
/// written, so `"C:\data"` stays `C:\data`.
fn read_env_file(path: &std::path::Path) -> miette::Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path)
        .into_diagnostic()
        .map_err(|error| error.wrap_err(format!("reading {}", path.display())))?;
    let mut entries = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (name, value) = line.split_once('=').ok_or_else(|| {
            miette::miette!("{}:{}: expected NAME=VALUE", path.display(), number + 1)
        })?;
        let value = value.trim();
        let value = if let Some(quoted) = value
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
        {
            quoted.to_owned()
        } else if let Some(quoted) = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
        {
            let mut unescaped = String::new();
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    unescaped.push(c);
                    continue;
                }
                match chars.next() {
                    Some('n') => unescaped.push('\n'),
                    Some('r') => unescaped.push('\r'),
                    Some('t') => unescaped.push('\t'),
                    Some(escaped @ ('"' | '\\')) => unescaped.push(escaped),
                    Some(other) => {
                        unescaped.push('\\');
                        unescaped.push(other);
                    }
                    None => unescaped.push('\\'),
                }
            }
            unescaped
        } else {
            value.to_owned()
        };
        let name = name.trim();
        // Within one file the last value would otherwise win unnoticed.
        miette::ensure!(
            !entries.iter().any(|(seen, _)| seen == name),
            "{}:{}: {name} is given more than once",
            path.display(),
            number + 1
        );
        entries.push((name.to_owned(), value));
    }
    Ok(entries)
}

/// The settings in a JSON object file, in order. A name the object repeats is
/// refused: a map would keep only its last value without a word.
fn read_values_file(path: &std::path::Path) -> miette::Result<Vec<(String, Value)>> {
    struct Entries(Vec<(String, Value)>);
    impl<'de> serde::Deserialize<'de> for Entries {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Object;
            impl<'de> serde::de::Visitor<'de> for Object {
                type Value = Entries;
                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a JSON object of settings")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Entries, A::Error> {
                    let mut entries: Vec<(String, Value)> = Vec::new();
                    while let Some((name, value)) = map.next_entry::<String, Value>()? {
                        if entries.iter().any(|(seen, _)| *seen == name) {
                            return Err(serde::de::Error::custom(format!(
                                "{name} is given more than once"
                            )));
                        }
                        entries.push((name, value));
                    }
                    Ok(Entries(entries))
                }
            }
            deserializer.deserialize_map(Object)
        }
    }
    let text = std::fs::read_to_string(path)
        .into_diagnostic()
        .map_err(|error| error.wrap_err(format!("reading {}", path.display())))?;
    serde_json::from_str::<Entries>(&text)
        .map(|entries| entries.0)
        .map_err(|error| miette::miette!("{}: {error}", path.display()))
}

/// One instance's settings status in a few words: `applied r12`,
/// `rejected r13: awsSecret: set both or neither`, or `pending r13`. While
/// a setting that applies after a restart still has its connect-time value,
/// it says so, as in `applied r12, restart pending`: the instance runs only
/// the newest live values.
pub(super) fn settings_label(status: &Value, restart_pending: bool) -> String {
    let revision = match (
        status["revision"].as_u64(),
        status["instanceRevision"].as_u64(),
    ) {
        (Some(deployment), Some(instance)) => format!(" r{deployment} (instance r{instance})"),
        (Some(deployment), None) => format!(" r{deployment}"),
        (None, Some(instance)) => format!(" instance r{instance}"),
        (None, None) => String::new(),
    };
    let restart = if restart_pending {
        ", restart pending"
    } else {
        ""
    };
    let state = format!("{}{revision}{restart}", output::field(status, "state"));
    if status["state"] != json!("rejected") {
        return state;
    }
    let fields = output::list(status, "fields")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let reason = output::field(status, "reason");
    if fields.is_empty() {
        format!("{state}: {reason}")
    } else {
        format!("{state}: {fields}: {reason}")
    }
}

/// A text setting takes the text as written; any other declared one is parsed
/// as JSON, so `port=9090` is a number and `name=9090` stays text. A field the
/// build doesn't declare is JSON when the text parses as JSON and text
/// otherwise: `workers=4` is a number, `region=east` is text, and `'"4"'` is
/// the text 4.
fn parse_value(current: &Value, name: &str, text: &str) -> miette::Result<Value> {
    let Some(field) = output::list(current, "fields")
        .iter()
        .find(|field| field["name"].as_str() == Some(name))
    else {
        return Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())));
    };
    if wants_text(&field["schema"], &field["schema"]) {
        return Ok(Value::String(text.to_owned()));
    }
    serde_json::from_str(text).map_err(|error| {
        miette::miette!(
            help = "Write it as JSON, such as 9090, true, or [\"a\",\"b\"].",
            "{name} is not valid JSON: {error}"
        )
    })
}

/// Whether a schema accepts only strings: a string type or a string enum,
/// possibly through a `$ref` or as the non-null half of a nullable type.
fn wants_text(schema: &Value, root: &Value) -> bool {
    if let Some(reference) = schema["$ref"].as_str() {
        // A JSON pointer (RFC 6901): a workspace's `/` is written `~1`.
        let key = reference
            .trim_start_matches("#/$defs/")
            .replace("~1", "/")
            .replace("~0", "~");
        return wants_text(&root["$defs"][key.as_str()], root);
    }
    if let Some(options) = schema["anyOf"].as_array() {
        let mut real = options
            .iter()
            .filter(|option| option["type"] != json!("null"));
        return match (real.next(), real.next()) {
            (Some(only), None) => wants_text(only, root),
            _ => false,
        };
    }
    schema["type"] == json!("string")
}

fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn show(
    format: OutputFormat,
    label: &str,
    prefix: &str,
    id: &str,
    current: &Value,
) -> miette::Result<()> {
    if output::is_json(format) {
        return output::print_json(current);
    }
    let rows = output::list(current, "fields")
        .iter()
        .map(|field| {
            let mut notes = Vec::new();
            if field["required"] == json!(true) && field["source"] == json!("unset") {
                notes.push("required".to_owned());
            }
            if field["restart"] == json!(true) {
                notes.push("applies after restart".to_owned());
            }
            let plain = field["storedAsPlain"] == json!(true);
            if plain {
                notes.push("set it as a secret".to_owned());
            } else if let Some(invalid) = field["invalid"].as_str() {
                notes.push(format!(
                    "saved value doesn't fit this build, which uses the one before it: {invalid}"
                ));
            }
            let value = if plain {
                "(stored as plain text)".to_owned()
            } else if field["secret"] == json!(true) {
                if field["isSet"] == json!(true) {
                    "(secret, set)".to_owned()
                } else {
                    "(secret, not set)".to_owned()
                }
            } else {
                field.get("value").map(display).unwrap_or_default()
            };
            json!({
                "name": field["name"],
                "value": value,
                "source": field["source"],
                "notes": notes.join("; "),
            })
        })
        .collect::<Vec<_>>();
    // Values kept for a build that declares them, such as the next one.
    let unused_note = if current["declared"] == json!(true) {
        "not used by this build"
    } else {
        "no build installed yet"
    };
    let unused = output::list(current, "unused")
        .iter()
        .map(|field| {
            json!({
                "name": field["name"],
                "value": if field["secret"] == json!(true) {
                    "(secret, set)".to_owned()
                } else {
                    field.get("value").map(display).unwrap_or_default()
                },
                "source": field["source"],
                "notes": unused_note,
            })
        })
        .collect::<Vec<_>>();
    let rows = rows.into_iter().chain(unused).collect::<Vec<_>>();
    output::table(
        &rows,
        &[
            output::col("SETTING", "name"),
            output::col("VALUE", "value"),
            output::muted("FROM", "source"),
            output::col("", "notes"),
        ],
        &if current["declared"] == json!(true) {
            format!("{label} has no settings.")
        } else {
            format!("{label} has no settings yet; set any with `trellis {prefix} {id} config set NAME=VALUE`.")
        },
    )?;
    let secret = |name: &str| {
        output::list(current, "fields")
            .iter()
            .any(|field| field["name"].as_str() == Some(name) && field["secret"] == json!(true))
    };
    let plain = output::list(current, "fields")
        .iter()
        .filter(|field| field["storedAsPlain"] == json!(true))
        .filter_map(|field| field["name"].as_str())
        .collect::<Vec<_>>();
    if !plain.is_empty() {
        output::warning(format!(
            "Stored as plain text, but this build declares them secret, so they are never shown or used: {}. \
            Set each as a secret; the old plain value stays in earlier revisions until you delete them with \
            `trellis {prefix} {id} config delete-revision REVISION`.",
            plain.join(", ")
        ));
    }
    let missing = output::list(current, "missing")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        output::warning(format!(
            "Its instances wait until these are set: {}",
            missing.join(", ")
        ));
    }
    if let Some(first) = plain.first().or(missing.first()) {
        output::next(&if secret(first) {
            format!("trellis {prefix} {id} config set-secret {first}")
        } else {
            format!("trellis {prefix} {id} config set {first}=…")
        });
    }
    // What each instance did with the newest settings.
    let instances = output::list(current, "instances")
        .iter()
        .filter(|instance| instance.get("settings").is_some())
        .map(|instance| {
            json!({
                "title": instance["title"],
                "settings": settings_label(
                    &instance["settings"],
                    instance["restartPending"] == json!(true),
                ),
            })
        })
        .collect::<Vec<_>>();
    if !instances.is_empty() {
        anstream::println!();
        output::table(
            &instances,
            &[
                output::col("INSTANCE", "title"),
                output::col("SETTINGS", "settings"),
            ],
            "",
        )?;
    }
    // A rollback undoes one layer: the deployment's settings, which every
    // instance shares, or one instance's overrides. Name the layer that held
    // the refused values in the revisions refused, and both when they came
    // from both or the check refused the revision as a whole. A refused value
    // from beneath an override the refused revision removed is undone by
    // restoring that override, which leaves the other instances alone.
    for instance in output::list(current, "instances") {
        let settings = &instance["settings"];
        if settings["state"] != json!("rejected") {
            continue;
        }
        let deployment_rollback = format!("`trellis {prefix} {id} config rollback REVISION`");
        let instance_rollback = format!(
            "`trellis {prefix} {id} config --instance {} rollback REVISION`",
            output::field(instance, "instanceId")
        );
        let sources = settings["sources"]
            .as_object()
            .map(|sources| {
                sources
                    .values()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let known = !sources.is_empty();
        let rollback = if let Some(restore) = settings["overrideRevision"].as_u64() {
            let restore = format!(
                "restore the overrides it accepted with `trellis {prefix} {id} config --instance {} rollback {restore}`",
                output::field(instance, "instanceId")
            );
            if sources.iter().all(|source| *source != "instance") {
                format!(
                    "{restore}, or roll back the deployment's settings, which changes every instance, with {deployment_rollback}"
                )
            } else {
                restore
            }
        } else if settings["instanceRevision"].is_null()
            || (known && sources.iter().all(|source| *source != "instance"))
        {
            format!("roll back the deployment's settings with {deployment_rollback}")
        } else if known && sources.iter().all(|source| *source == "instance") {
            format!("roll back its own overrides with {instance_rollback}")
        } else {
            format!(
                "roll back the layer that holds the refused values: its own overrides with {instance_rollback}, or the deployment's settings, which every instance shares, with {deployment_rollback}"
            )
        };
        output::warning(format!(
            "{} rejected the newest settings and keeps the ones it accepted last. Fix them, or {rollback}.",
            output::field(instance, "title"),
        ));
    }
    for instance in output::list(current, "instances") {
        if instance["restartPending"] == json!(true) {
            output::note(format!(
                "{} runs settings from when it connected; restart it to apply settings that apply after restart.",
                output::field(instance, "title"),
            ));
        }
    }
    Ok(())
}

fn changed(
    format: OutputFormat,
    label: &str,
    prefix: &str,
    id: &str,
    updated: &Value,
) -> miette::Result<()> {
    if output::is_json(format) {
        return output::print_json(updated);
    }
    output::success(format!(
        "Saved revision {} of {label}'s settings",
        output::field(updated, "revision")
    ));
    show(format, label, prefix, id, updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_file(text: &str) -> miette::Result<Vec<(String, String)>> {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), text).unwrap();
        read_env_file(file.path())
    }

    #[test]
    fn a_double_quoted_env_value_keeps_backslashes_that_escape_nothing() {
        let entries = env_file(concat!(
            r#"dataDir="C:\data\new\x""#,
            "\n",
            r#"pattern="^\d+\.\w$""#,
            "\n",
            r#"banner="say \"hi\"\tthen\\ \\n""#,
            "\n",
            r"literal='C:\new'",
            "\n",
            r#"trailing="ends\""#,
            "\n",
        ))
        .unwrap();
        assert_eq!(
            entries,
            [
                ("dataDir".to_owned(), "C:\\data\new\\x".to_owned()),
                ("pattern".to_owned(), r"^\d+\.\w$".to_owned()),
                ("banner".to_owned(), "say \"hi\"\tthen\\ \\n".to_owned()),
                ("literal".to_owned(), r"C:\new".to_owned()),
                ("trailing".to_owned(), r"ends\".to_owned()),
            ]
        );
    }

    /// The error a file's entries fail with, as an operator reads it.
    fn refusal(result: miette::Result<impl std::fmt::Debug>) -> String {
        result.expect_err("the file is refused").to_string()
    }

    #[test]
    fn a_setting_named_twice_in_one_file_is_refused_naming_it() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            r#"{ "level": "debug", "endpoint": "https://one.example", "level": "info" }"#,
        )
        .unwrap();
        // The message is what the operator acts on: it names the setting.
        let refused = refusal(read_values_file(file.path()));
        assert!(
            refused.contains("level is given more than once"),
            "{refused}"
        );
        std::fs::write(file.path(), r#"{ "level": "debug", "port": 8080 }"#).unwrap();
        assert_eq!(
            read_values_file(file.path()).unwrap(),
            [
                ("level".to_owned(), json!("debug")),
                ("port".to_owned(), json!(8080)),
            ]
        );

        let refused = refusal(env_file("level=debug\n# again\nexport level=info\n"));
        assert!(
            refused.ends_with(":3: level is given more than once"),
            "{refused}"
        );
    }
}
