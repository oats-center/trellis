//! `trellis svc <name> webhooks ...`: a service's webhook endpoints, the
//! requests they received, and their runtime settings.

use std::io::Write as _;

use base64::Engine as _;
use miette::IntoDiagnostic as _;
use serde_json::{json, Value};
use trellis_runtime_apis::__types::trellis as types;

use super::connect_authenticated_cli_client;
use super::deploy::{resolve_deployment_id, DeploymentKind};
use crate::cli::{OutputFormat, WebhooksArgs, WebhooksCommand};
use crate::output;

fn wire<T: serde::de::DeserializeOwned>(value: Value) -> miette::Result<T> {
    serde_json::from_value(value).into_diagnostic()
}

/// What a setting measures, which decides the units its value takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dimension {
    /// Milliseconds, typed as `500ms`, `30s`, `5m`, `2h`, or `7d`.
    Duration,
    /// Bytes, typed as `512B`, `256KiB`, `16MiB`, or `1GiB`.
    Size,
    /// A plain number.
    Count,
}

/// What a setting measures, by its stored name.
fn dimension(name: &str) -> Dimension {
    match name {
        "maxBody" | "intakeMaxBytes" => Dimension::Size,
        "requestsPerSecond" | "burst" | "failuresPerMinute" => Dimension::Count,
        _ => Dimension::Duration,
    }
}

/// Parse a setting's value in its dimension's units: a duration such as
/// `5m`, a size such as `512KiB`, a count, or `default` to return to the
/// default. A bare number is milliseconds, bytes, or the count.
fn setting_value(setting: &str, dimension: Dimension, raw: &str) -> miette::Result<Option<u64>> {
    if raw == "default" {
        return Ok(None);
    }
    let example = match dimension {
        Dimension::Duration => "such as 30s, 5m, or 7d",
        Dimension::Size => "such as 512KiB or 16MiB",
        Dimension::Count => "such as 50",
    };
    let split = raw
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(raw.len());
    let (number, unit) = raw.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| miette::miette!("{setting} takes a number, {example}"))?;
    let multiplier = match (dimension, unit) {
        (_, "") => 1,
        (Dimension::Duration, "ms") => 1,
        (Dimension::Duration, "s") => 1_000,
        (Dimension::Duration, "m") => 60_000,
        (Dimension::Duration, "h") => 3_600_000,
        (Dimension::Duration, "d") => 86_400_000,
        (Dimension::Size, "B") => 1,
        (Dimension::Size, "KiB") => 1 << 10,
        (Dimension::Size, "MiB") => 1 << 20,
        (Dimension::Size, "GiB") => 1 << 30,
        _ => {
            return Err(miette::miette!(
                "{setting} does not take '{unit}'; give a number, {example}"
            ))
        }
    };
    number
        .checked_mul(multiplier)
        .map(Some)
        .ok_or_else(|| miette::miette!("{raw} is too large"))
}

fn setting_name(raw: &str) -> miette::Result<&'static str> {
    let camel: String = raw
        .split('-')
        .enumerate()
        .map(|(index, part)| {
            if index == 0 {
                part.to_owned()
            } else {
                let mut characters = part.chars();
                characters
                    .next()
                    .map(|first| first.to_ascii_uppercase().to_string() + characters.as_str())
                    .unwrap_or_default()
            }
        })
        .collect();
    [
        "requestsPerSecond",
        "burst",
        "failuresPerMinute",
        "maxBody",
        "dedupeWindow",
        "syncDecideDeadline",
        "asyncDecideDeadline",
        "rotationOverlap",
        "recordRetention",
        "intakeRetention",
        "intakeMaxBytes",
    ]
    .into_iter()
    .find(|name| *name == camel)
    .ok_or_else(|| {
        miette::miette!(
            help = "Settings: requests-per-second, burst, failures-per-minute, max-body, \
            dedupe-window, sync-decide-deadline, async-decide-deadline, rotation-overlap, \
            record-retention, intake-retention, intake-max-bytes.",
            "unknown webhook setting '{raw}'"
        )
    })
}

fn print_endpoint(endpoint: &Value) {
    let mut details =
        output::Details::new(format!("Webhook {}", output::field(endpoint, "endpoint")))
            .field("Title", output::field(endpoint, "title"))
            .field("URL", output::field(endpoint, "url"))
            .state(
                "State",
                if endpoint["enabled"].as_bool() == Some(true) {
                    "enabled"
                } else {
                    "disabled"
                },
            )
            .field("Mode", output::field(endpoint, "mode"))
            .field("Methods", output::field(endpoint, "methods"))
            .field(
                "Service decides",
                output::yes_no(endpoint["decides"].as_bool() == Some(true)),
            );
    if endpoint["unauthenticated"].as_bool() == Some(true) {
        details = details.field(
            "Warning",
            "checks nothing: anyone with the URL can send requests",
        );
    }
    let previous = output::list(endpoint, "previousUrls");
    if !previous.is_empty() {
        details = details
            .section("Earlier URLs, still working until retired")
            .items(previous.iter().filter_map(Value::as_str));
    }
    details = details.section("Verifiers").items(
        output::list(endpoint, "verifiers")
            .iter()
            .filter_map(Value::as_str),
    );
    let secrets = output::list(endpoint, "secrets");
    if !secrets.is_empty() {
        details = details
            .section("Secrets")
            .items(secrets.iter().map(|secret| {
                let mut line = format!(
                    "{}: {}",
                    output::field(secret, "name"),
                    if secret["isSet"].as_bool() == Some(true) {
                        "set"
                    } else {
                        "not set"
                    }
                );
                if let Some(until) = secret["previousAcceptedUntil"].as_str() {
                    line.push_str(&format!(
                        ", previous value accepted until {}",
                        output::absolute_time(until)
                    ));
                }
                if let Some(matched) = secret["previousLastMatchedAt"].as_str() {
                    line.push_str(&format!(
                        ", previous value last matched {}",
                        output::relative_time(matched)
                    ));
                }
                line
            }));
    }
    let settings = &endpoint["settings"];
    if let Some(settings) = settings.as_object() {
        details = details
            .section("Settings")
            .items(settings.iter().map(|(name, value)| {
                format!(
                    "{name}: {}",
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                )
            }));
    }
    let recent = output::list(endpoint, "recent");
    details = details
        .section("Last 24 hours")
        .items(if recent.is_empty() {
            vec!["no requests".to_owned()]
        } else {
            recent
                .iter()
                .map(|entry| {
                    format!(
                        "{}: {}",
                        output::field(entry, "outcome"),
                        output::field(entry, "count")
                    )
                })
                .collect()
        });
    details.print();
}

pub(super) async fn run(format: OutputFormat, id: &str, args: WebhooksArgs) -> miette::Result<()> {
    let deployment = resolve_deployment_id(DeploymentKind::Service, id).await?;
    let (_, connected) = connect_authenticated_cli_client().await?;
    let cli = trellis_runtime_apis::apps::trellis_cli::Client::from_generated(connected);
    let Some(endpoint) = args.endpoint else {
        let listed = cli
            .trellis_webhooks_v1()
            .endpoints_list(&wire(json!({ "service": deployment }))?)
            .await
            .into_diagnostic()?;
        let value = serde_json::to_value(&listed).into_diagnostic()?;
        if output::is_json(format) {
            return output::print_json(&value);
        }
        return output::table(
            output::list(&value, "endpoints"),
            &[
                output::col("ENDPOINT", "endpoint"),
                output::col("MODE", "mode"),
                output::state_col("ENABLED", "enabled"),
                output::col("URL", "url"),
            ],
            &format!("{id} declares no webhook endpoints."),
        );
    };
    let update = |change: &str,
                  setting: Option<&str>,
                  value: Option<Option<u64>>|
     -> miette::Result<types::AuthWebhooksUpdateRequest> {
        let mut request = json!({
            "deploymentId": deployment,
            "endpoint": endpoint,
            "change": change,
            "idempotencyKey": ulid::Ulid::generate().to_string(),
        });
        if let Some(setting) = setting {
            request["setting"] = json!(setting);
        }
        if let Some(value) = value {
            request["value"] = value.map_or(Value::Null, |value| json!(value.to_string()));
        }
        wire(request)
    };
    let changed = |message: String| -> miette::Result<()> {
        if output::is_json(format) {
            return output::print_json(&json!({ "endpoint": endpoint, "changed": true }));
        }
        output::success(message);
        Ok(())
    };
    match args.command {
        None => {
            let listed = cli
                .trellis_webhooks_v1()
                .endpoints_list(&wire(json!({ "service": deployment }))?)
                .await
                .into_diagnostic()?;
            let value = serde_json::to_value(&listed).into_diagnostic()?;
            let found = output::list(&value, "endpoints")
                .iter()
                .find(|entry| entry["endpoint"].as_str() == Some(endpoint.as_str()))
                .cloned()
                .ok_or_else(|| miette::miette!("{id} declares no webhook endpoint {endpoint}"))?;
            if output::is_json(format) {
                return output::print_json(&found);
            }
            print_endpoint(&found);
            output::next(&format!("trellis svc {id} webhooks {endpoint} attempts"));
            Ok(())
        }
        Some(WebhooksCommand::Attempts { outcome, since }) => {
            let mut request = json!({ "service": deployment, "endpoint": endpoint, "limit": 100 });
            if let Some(outcome) = outcome {
                request["outcome"] = json!(outcome);
            }
            if let Some(since) = since {
                let ms = setting_value("--since", Dimension::Duration, &since)?.unwrap_or(0);
                let at = time::OffsetDateTime::now_utc()
                    - time::Duration::milliseconds(i64::try_from(ms).unwrap_or(i64::MAX));
                request["since"] = json!(at
                    .format(&time::format_description::well_known::Rfc3339)
                    .into_diagnostic()?);
            }
            let found = cli
                .trellis_webhooks_v1()
                .attempts_query(&wire(request)?)
                .await
                .into_diagnostic()?;
            let value = serde_json::to_value(&found).into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&value);
            }
            output::table(
                output::list(&value, "attempts"),
                &[
                    output::muted("ATTEMPT", "attemptId"),
                    output::time_col("RECEIVED", "receivedAt"),
                    output::state_col("OUTCOME", "outcome"),
                    output::col("STATUS", "status"),
                    output::col("REASON", "reason"),
                    output::col("CLIENT", "clientAddress"),
                ],
                &format!("{endpoint} has no recorded requests."),
            )
        }
        Some(WebhooksCommand::Attempt { attempt, body }) => {
            let found = cli
                .trellis_webhooks_v1()
                .attempts_inspect(&wire(json!({
                    "service": deployment,
                    "endpoint": endpoint,
                    "attemptId": attempt,
                }))?)
                .await
                .into_diagnostic()?;
            let value = serde_json::to_value(&found).into_diagnostic()?;
            if body {
                let bytes = value["delivery"]["body"]
                    .as_str()
                    .and_then(|body| base64::engine::general_purpose::STANDARD.decode(body).ok())
                    .ok_or_else(|| miette::miette!("the request body is no longer stored"))?;
                std::io::stdout().write_all(&bytes).into_diagnostic()?;
                return Ok(());
            }
            if output::is_json(format) {
                return output::print_json(&value);
            }
            let attempt = &value["attempt"];
            let mut details =
                output::Details::new(format!("Request {}", output::field(attempt, "attemptId")))
                    .time("Received", &output::field(attempt, "receivedAt"))
                    .state("Outcome", &output::field(attempt, "outcome"))
                    .field("Status", output::field(attempt, "status"))
                    .field("Reason", output::field(attempt, "reason"))
                    .field("Matched key", output::field(attempt, "matchedKey"))
                    .field("Decided", output::field(attempt, "verdict"))
                    .field("Client", output::field(attempt, "clientAddress"))
                    .field(
                        "Body",
                        format!(
                            "{} bytes, sha256 {}",
                            output::field(attempt, "bodySize"),
                            output::field(attempt, "bodySha256")
                        ),
                    )
                    .field("Event", output::field(attempt, "deliveryId"));
            let headers = output::list(&value["delivery"], "headers");
            if !headers.is_empty() {
                details = details
                    .section("Headers")
                    .items(headers.iter().map(|header| {
                        format!(
                            "{}: {}",
                            output::field(header, "name"),
                            output::field(header, "value")
                        )
                    }));
            }
            details.print();
            if attempt["stored"].as_bool() == Some(true) {
                output::next(&format!(
                    "trellis svc {id} webhooks {endpoint} attempt {} --body",
                    output::field(attempt, "attemptId")
                ));
            }
            Ok(())
        }
        Some(WebhooksCommand::Reverify { attempt }) => {
            let found = cli
                .trellis_webhooks_v1()
                .attempts_reverify(&wire(json!({
                    "service": deployment,
                    "endpoint": endpoint,
                    "attemptId": attempt,
                }))?)
                .await
                .into_diagnostic()?;
            let value = serde_json::to_value(&found).into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&value);
            }
            output::success(format!(
                "Verified {attempt} again: {}",
                output::field(&value["attempt"], "outcome")
            ));
            Ok(())
        }
        Some(WebhooksCommand::Enable) => {
            cli.trellis_auth_v1()
                .webhooks_update(&update("enable", None, None)?)
                .await
                .into_diagnostic()?;
            changed(format!("Enabled webhook {endpoint}."))
        }
        Some(WebhooksCommand::Disable) => {
            cli.trellis_auth_v1()
                .webhooks_update(&update("disable", None, None)?)
                .await
                .into_diagnostic()?;
            changed(format!(
                "Disabled webhook {endpoint}; its URL answers 503 and stores nothing."
            ))
        }
        Some(WebhooksCommand::RotateUrl) => {
            cli.trellis_auth_v1()
                .webhooks_update(&update("rotateUrl", None, None)?)
                .await
                .into_diagnostic()?;
            changed(format!(
                "Gave webhook {endpoint} a new URL; the old one works until `trellis svc {id} webhooks {endpoint} retire-url`."
            ))
        }
        Some(WebhooksCommand::RetireUrl) => {
            cli.trellis_auth_v1()
                .webhooks_update(&update("retireUrl", None, None)?)
                .await
                .into_diagnostic()?;
            changed(format!(
                "Retired the earlier URLs of webhook {endpoint}; they answer 404."
            ))
        }
        Some(WebhooksCommand::Set { setting, value }) => {
            let name = setting_name(&setting)?;
            let parsed = setting_value(&setting, dimension(name), &value)?;
            cli.trellis_auth_v1()
                .webhooks_update(&update("set", Some(name), Some(parsed))?)
                .await
                .into_diagnostic()?;
            changed(match parsed {
                Some(_) => format!("Set {setting} to {value} for webhook {endpoint}."),
                None => format!("Returned {setting} to its default for webhook {endpoint}."),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{dimension, setting_name, setting_value};

    fn parse(setting: &str, raw: &str) -> Option<u64> {
        let name = setting_name(setting).expect("a known setting");
        setting_value(setting, dimension(name), raw).ok().flatten()
    }

    #[test]
    fn each_setting_takes_only_its_own_units() {
        assert_eq!(parse("max-body", "512KiB"), Some(512 << 10));
        assert_eq!(parse("intake-max-bytes", "1GiB"), Some(1 << 30));
        assert_eq!(parse("dedupe-window", "5m"), Some(300_000));
        assert_eq!(parse("record-retention", "7d"), Some(7 * 86_400_000));
        assert_eq!(parse("burst", "200"), Some(200));
        // A duration is not a size, a size is not a duration, and a count
        // takes no unit.
        assert_eq!(parse("max-body", "5m"), None);
        assert_eq!(parse("intake-max-bytes", "2h"), None);
        assert_eq!(parse("rotation-overlap", "512KiB"), None);
        assert_eq!(parse("sync-decide-deadline", "1B"), None);
        assert_eq!(parse("requests-per-second", "5s"), None);
        assert_eq!(parse("failures-per-minute", "1KiB"), None);
    }
}
