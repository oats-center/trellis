//! `trellis svc|dev <ID> config`: settings Trellis keeps for a deployment and
//! its instances.

use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};

use super::deploy::{resolve_deployment_id, resolve_waiting_claim, DeploymentKind};
use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::{ConfigArgs, ConfigCommand, OutputFormat};
use crate::output;

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
            let target = match resolve_deployment_id(kind, id).await {
                Ok(deployment) => json!({ "deploymentId": deployment }),
                // A service added without its IDL takes settings before its
                // service connects; approving it carries them over.
                Err(error) => match kind {
                    DeploymentKind::Service => match resolve_waiting_claim(id).await {
                        Ok(claim) => {
                            miette::ensure!(
                                args.instance.is_none(),
                                "{id} has no instances until its service connects and is approved; leave out --instance"
                            );
                            json!({ "claimId": claim })
                        }
                        Err(_) => return Err(error),
                    },
                    DeploymentKind::Device => return Err(error),
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
        ConfigCommand::Set { values, null } => {
            let mut parsed = serde_json::Map::new();
            for name in null {
                parsed.insert(name, Value::Null);
            }
            for pair in values {
                let (name, text) = pair.split_once('=').ok_or_else(|| {
                    miette::miette!(
                        help = "Write each setting as FIELD=VALUE, such as logLevel=debug.",
                        "`{pair}` has no `=`"
                    )
                })?;
                parsed.insert(name.to_owned(), parse_value(&current, name, text)?);
            }
            let updated = auth
                .config_set(&wire(set(json!({ "values": parsed })))?)
                .await
                .into_diagnostic()?;
            changed(format, &label, prefix, id, &to_value(updated)?)
        }
        ConfigCommand::SetSecret {
            field,
            from_file,
            null,
        } => {
            let value = if null {
                Value::Null
            } else {
                let text = if from_file == std::path::Path::new("-") {
                    std::io::read_to_string(std::io::stdin()).into_diagnostic()?
                } else {
                    std::fs::read_to_string(&from_file).into_diagnostic()?
                };
                let text = text.trim_end_matches(['\r', '\n']);
                miette::ensure!(!text.is_empty(), "the secret for {field} is empty");
                parse_value(&current, &field, text)?
            };
            let updated = auth
                .config_set(&wire(set(json!({ "secrets": { field: value } })))?)
                .await
                .into_diagnostic()?;
            changed(format, &label, prefix, id, &to_value(updated)?)
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
        let key = reference.trim_start_matches("#/$defs/");
        return wants_text(&root["$defs"][key], root);
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
            if let Some(invalid) = field["invalid"].as_str() {
                notes.push(format!(
                    "saved value doesn't fit this build, which uses the one before it: {invalid}"
                ));
            }
            let value = if field["secret"] == json!(true) {
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
    let missing = output::list(current, "missing")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        output::warning(format!(
            "Its instances wait until these are set: {}",
            missing.join(", ")
        ));
        output::next(&format!(
            "trellis {prefix} {id} config set {}=…",
            missing[0]
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
