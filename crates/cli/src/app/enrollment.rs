//! `trellis svc <id> enrollment …`, `trellis svc <id> instance-policy …`, and
//! `trellis instance-policy …`.

use futures_util::TryStreamExt as _;
use miette::IntoDiagnostic;
use serde_json::{json, Value};
use trellis_runtime_apis::types as auth_types;

use crate::app::deploy::DeploymentKind;
use crate::app::{connect_authenticated_cli_client, wire};
use crate::cli::*;
use crate::output;

type AuthClient = trellis_runtime_apis::apps::trellis_cli::Client;

async fn client() -> miette::Result<(AuthClient, trellis_rs::generated::Client)> {
    let (_, connected) = connect_authenticated_cli_client().await?;
    Ok((AuthClient::from_generated(connected.clone()), connected))
}

fn idempotency_key() -> miette::Result<auth_types::AuthIdempotencyKey> {
    wire(ulid::Ulid::generate().to_string())
}

/// Seconds in a duration such as `90s`, `10m`, `24h`, `30d` or `2w`.
fn seconds(text: &str) -> miette::Result<u64> {
    let text = text.trim();
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| miette::miette!("`{text}` is not a duration such as 10m, 24h or 30d"))?;
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => miette::bail!("`{text}` is not a duration such as 10m, 24h or 30d"),
    };
    number
        .checked_mul(scale)
        .ok_or_else(|| miette::miette!("`{text}` is too long"))
}

fn u32_seconds(text: &str) -> miette::Result<u32> {
    u32::try_from(seconds(text)?).map_err(|_| miette::miette!("`{text}` is too long"))
}

/// An expiry given as a date, a time, or a duration from now.
fn expiry(text: &str) -> miette::Result<Option<trellis_rs::generated::Timestamp>> {
    use time::format_description::well_known::Rfc3339;
    if text == "never" {
        return Ok(None);
    }
    let at = if let Ok(at) = time::OffsetDateTime::parse(text, &Rfc3339) {
        at
    } else if let Ok(at) = time::OffsetDateTime::parse(&format!("{text}T00:00:00Z"), &Rfc3339) {
        at
    } else {
        let offset = i64::try_from(seconds(text)?).into_diagnostic()?;
        time::OffsetDateTime::now_utc() + time::Duration::seconds(offset)
    };
    trellis_rs::generated::Timestamp::from_unix_seconds(at.unix_timestamp())
        .map(Some)
        .map_err(|_| miette::miette!("`{text}` is not a valid expiry"))
}

fn on_off(text: &str, flag: &str) -> miette::Result<bool> {
    match text {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        _ => miette::bail!("{flag} takes on or off"),
    }
}

fn limit(value: u32) -> miette::Result<auth_types::AuthEnrollmentLimit> {
    if !(1..=1_000_000).contains(&value) {
        miette::bail!("limits are between 1 and 1000000");
    }
    Ok(auth_types::AuthEnrollmentLimit(value))
}

/// Run one `svc <id> enrollment` command.
pub(super) async fn run(
    format: OutputFormat,
    id: &str,
    command: EnrollmentCommand,
) -> miette::Result<()> {
    let (client, connected) = client().await?;
    let deployment =
        super::deploy::find_deployment(&connected, id, DeploymentKind::Service).await?;
    let deployment_id = output::field(&deployment, "deploymentId");
    let title = output::field(&deployment, "title");
    let auth = client.trellis_auth_v1();
    let list = || async {
        auth.enrollment_secrets_list_items(auth_types::AuthEnrollmentSecretsListRequest {
            deployment_id: wire(&deployment_id)?,
            page: Some(trellis_runtime_apis::CursorQuery {
                cursor: None,
                limit: Some(100),
            }),
            extra: Default::default(),
        })
        .try_collect::<Vec<_>>()
        .await
        .into_diagnostic()
    };
    match command {
        EnrollmentCommand::Issue(args) => {
            let limits = args.limits;
            let issued = auth
                .enrollment_secrets_issue(&auth_types::AuthEnrollmentSecretIssueRequest {
                    deployment_id: wire(&deployment_id)?,
                    title: limits.title.unwrap_or_else(|| title.clone()).into(),
                    expires_at: limits.expires.as_deref().map(expiry).transpose()?.flatten(),
                    max_instances: limits.max_instances.map(limit).transpose()?,
                    enrollments_per_hour: limits.per_hour.map(limit).transpose()?,
                    current_build_only: limits
                        .current_build_only
                        .as_deref()
                        .map(|value| on_off(value, "--current-build-only"))
                        .transpose()?,
                    idempotency_key: idempotency_key()?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            if output::is_json(format) {
                return output::print_json(&issued);
            }
            let text = issued
                .text
                .clone()
                .ok_or_else(|| miette::miette!("Trellis did not return the secret"))?;
            // The secret alone goes to standard output, so it can be piped
            // into a secret store; what it is goes to standard error.
            println!("{text}");
            output::success(format!(
                "Issued enrollment secret {} for {title}. It is shown only now.",
                issued.secret.secret_id
            ));
            output::hint(format!(
                "A process given TRELLIS_ENROLLMENT_SECRET enrolls itself; at most {} \
                instances at once, {} new ones an hour.",
                *issued.secret.max_instances, *issued.secret.enrollments_per_hour
            ));
            Ok(())
        }
        EnrollmentCommand::List => {
            let secrets = list().await?;
            output::emit(format, &json!({ "secrets": secrets }), |value| {
                output::table(
                    output::list(value, "secrets"),
                    &[
                        output::col("Title", "title"),
                        output::col("Instances", "instances"),
                        output::col("Cap", "maxInstances"),
                        output::col("Per hour", "enrollmentsPerHour"),
                        output::col("Current build only", "currentBuildOnly"),
                        output::time_col("Expires", "expiresAt"),
                        output::time_col("Last used", "lastUsedAt"),
                        output::time_col("Revoked", "revokedAt"),
                        output::muted("ID", "secretId"),
                    ],
                    "No enrollment secrets. Issue one with `enrollment issue`.",
                )?;
                for secret in output::list(value, "secrets") {
                    for alert in output::list(secret, "alerts") {
                        output::warning(format!(
                            "{}: {} ({} in the last 24 hours)",
                            output::field(secret, "title"),
                            match output::field(alert, "kind").as_str() {
                                "revokedUsed" => "someone presented it after it was revoked",
                                "expiredUsed" => "someone presented it after it expired",
                                "instanceLimit" => "it reached its instance cap",
                                "rateLimited" => "it reached its hourly rate",
                                "participantMismatch" =>
                                    "a process of another service presented it",
                                _ => "a process with another build presented it",
                            },
                            output::field(alert, "count"),
                        ));
                    }
                }
                Ok(())
            })
        }
        EnrollmentCommand::Edit(args) => {
            let current = list()
                .await?
                .into_iter()
                .find(|secret| secret.secret_id.to_string() == args.secret)
                .ok_or_else(|| {
                    miette::miette!("{title} has no enrollment secret {}", args.secret)
                })?;
            let limits = args.limits;
            let updated = auth
                .enrollment_secrets_update(&auth_types::AuthEnrollmentSecretUpdateRequest {
                    secret_id: current.secret_id,
                    title: limits.title.map_or(current.title.clone(), Into::into),
                    expires_at: match limits.expires.as_deref() {
                        Some(text) => expiry(text)?,
                        None => current.expires_at.clone(),
                    },
                    max_instances: limits
                        .max_instances
                        .map(limit)
                        .transpose()?
                        .unwrap_or(current.max_instances),
                    enrollments_per_hour: limits
                        .per_hour
                        .map(limit)
                        .transpose()?
                        .unwrap_or(current.enrollments_per_hour),
                    current_build_only: limits
                        .current_build_only
                        .as_deref()
                        .map(|value| on_off(value, "--current-build-only"))
                        .transpose()?
                        .unwrap_or(current.current_build_only),
                    expected_revision: current.revision,
                    idempotency_key: idempotency_key()?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &updated, |_| {
                output::success(format!("Changed enrollment secret {}.", args.secret));
                Ok(())
            })
        }
        EnrollmentCommand::Revoke(args) => {
            let current = list()
                .await?
                .into_iter()
                .find(|secret| secret.secret_id.to_string() == args.secret)
                .ok_or_else(|| {
                    miette::miette!("{title} has no enrollment secret {}", args.secret)
                })?;
            let revoked = auth
                .enrollment_secrets_revoke(&auth_types::AuthEnrollmentSecretRevokeRequest {
                    secret_id: current.secret_id,
                    retire_instances: args.retire_instances.then_some(true),
                    expected_revision: current.revision,
                    idempotency_key: idempotency_key()?,
                    extra: Default::default(),
                })
                .await
                .into_diagnostic()?;
            output::emit(format, &revoked, |value| {
                output::success(format!(
                    "Revoked enrollment secret {}; it creates no more instances.",
                    args.secret
                ));
                let retired: u64 = output::field(value, "retiredInstances")
                    .parse()
                    .unwrap_or(0);
                if args.retire_instances {
                    let closing: u64 = output::field(value, "closingConnections")
                        .parse()
                        .unwrap_or(0);
                    output::hint(format!(
                        "Retired {}; {} still being closed.",
                        output::plural(retired as usize, "instance"),
                        output::plural(closing as usize, "connection"),
                    ));
                } else if *current.instances > 0 {
                    output::hint(
                        "The instances it created keep running; revoke with \
                        --retire-instances to disconnect them.",
                    );
                }
                Ok(())
            })
        }
    }
}

fn print_policy(
    format: OutputFormat,
    policy: &auth_types::AuthInstancePolicyResponse,
) -> miette::Result<()> {
    output::emit(format, policy, |value| {
        let retire = |source: &Value| -> String {
            match (
                source["retireOffline"].as_bool(),
                source["retireOfflineAfterSeconds"].as_u64(),
            ) {
                (Some(false), _) => "off".into(),
                (_, Some(seconds)) => format!("after {}h offline", seconds / 3600),
                (Some(true), None) => "on".into(),
                (None, None) => String::new(),
            }
        };
        if let Some(effective) = value.get("effective") {
            let own = &value["deployment"];
            output::Details::new("Retirement for this service")
                .field("Retire offline", retire(effective))
                .field(
                    "Retire provisioned",
                    output::on_off(effective["retireProvisioned"].as_bool().unwrap_or(false)),
                )
                .field(
                    "Set here",
                    [
                        (!retire(own).is_empty())
                            .then(|| format!("retire offline {}", retire(own))),
                        own["retireProvisioned"]
                            .as_bool()
                            .map(|on| format!("retire provisioned {}", output::on_off(on))),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", "),
                )
                .print();
            return Ok(());
        }
        let installation = &value["installation"];
        output::Details::new("Instance policy")
            .field("Retire offline", retire(installation))
            .field(
                "Retire provisioned",
                output::on_off(installation["retireProvisioned"].as_bool().unwrap_or(false)),
            )
            .field(
                "Retired records kept",
                format!("{} days", installation["retiredRecordDays"]),
            )
            .field(
                "First-connect grace",
                format!(
                    "{} min",
                    installation["firstConnectGraceSeconds"]
                        .as_u64()
                        .unwrap_or(0)
                        / 60
                ),
            )
            .field(
                "New secret cap",
                installation["secretMaxInstances"].to_string(),
            )
            .field(
                "New secret rate",
                format!("{} an hour", installation["secretEnrollmentsPerHour"]),
            )
            .print();
        Ok(())
    })
}

/// Run one `svc <id> instance-policy` command.
pub(super) async fn deployment_policy(
    format: OutputFormat,
    id: &str,
    command: DeploymentPolicyCommand,
) -> miette::Result<()> {
    let (client, connected) = client().await?;
    let deployment =
        super::deploy::find_deployment(&connected, id, DeploymentKind::Service).await?;
    let deployment_id: trellis_rs::generated::Ulid =
        wire(output::field(&deployment, "deploymentId"))?;
    let auth = client.trellis_auth_v1();
    let current = auth
        .instance_policy_get(&auth_types::AuthInstancePolicyGetRequest {
            deployment_id: Some(deployment_id),
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    let DeploymentPolicySetArgs {
        retire_offline_after,
        retire_provisioned,
    } = match command {
        DeploymentPolicyCommand::Show => return print_policy(format, &current),
        DeploymentPolicyCommand::Set(args) => args,
    };
    let mut own = current
        .deployment
        .clone()
        .unwrap_or(auth_types::AuthDeploymentInstancePolicy {
            retire_offline: None,
            retire_offline_after_seconds: None,
            retire_provisioned: None,
            extra: Default::default(),
        });
    match retire_offline_after.as_deref() {
        None => {}
        Some("default") => {
            own.retire_offline = None;
            own.retire_offline_after_seconds = None;
        }
        Some("off") => own.retire_offline = Some(false),
        Some(text) => {
            own.retire_offline = Some(true);
            own.retire_offline_after_seconds = Some(retirement(text)?);
        }
    }
    match retire_provisioned.as_deref() {
        None => {}
        Some("default") => own.retire_provisioned = None,
        Some(text) => own.retire_provisioned = Some(on_off(text, "--retire-provisioned")?),
    }
    let cleared = own.retire_offline.is_none()
        && own.retire_offline_after_seconds.is_none()
        && own.retire_provisioned.is_none();
    let changed = auth
        .instance_policy_set(&auth_types::AuthInstancePolicySetRequest {
            deployment_id: Some(deployment_id),
            installation: None,
            deployment: (!cleared).then_some(own),
            idempotency_key: idempotency_key()?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    print_policy(format, &changed)
}

fn retirement(text: &str) -> miette::Result<auth_types::AuthRetirementSeconds> {
    let seconds = u32_seconds(text)?;
    if seconds < 3600 {
        miette::bail!("the shortest retirement period is one hour");
    }
    Ok(auth_types::AuthRetirementSeconds(seconds))
}

/// Run one `trellis instance-policy` command.
pub(super) async fn installation_policy(
    format: OutputFormat,
    command: InstancePolicyCommand,
) -> miette::Result<()> {
    let (client, _) = client().await?;
    let auth = client.trellis_auth_v1();
    let current = auth
        .instance_policy_get(&auth_types::AuthInstancePolicyGetRequest {
            deployment_id: None,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    let args = match command {
        InstancePolicyCommand::Show => return print_policy(format, &current),
        InstancePolicyCommand::Set(args) => args,
    };
    let mut policy = current.installation;
    match args.retire_offline_after.as_deref() {
        None => {}
        Some("off") => policy.retire_offline = false,
        Some(text) => {
            policy.retire_offline = true;
            policy.retire_offline_after_seconds = retirement(text)?;
        }
    }
    if let Some(text) = args.retire_provisioned.as_deref() {
        policy.retire_provisioned = on_off(text, "--retire-provisioned")?;
    }
    if let Some(days) = args.retired_record_days {
        if !(1..=36_500).contains(&days) {
            miette::bail!("--retired-record-days is between 1 and 36500");
        }
        policy.retired_record_days = auth_types::AuthRecordDays(days);
    }
    if let Some(text) = args.first_connect_grace.as_deref() {
        let grace = u32_seconds(text)?;
        if grace > 86_400 {
            miette::bail!("the first-connect grace is at most one day");
        }
        policy.first_connect_grace_seconds = auth_types::AuthGraceSeconds(grace);
    }
    if let Some(cap) = args.secret_max_instances {
        policy.secret_max_instances = limit(cap)?;
    }
    if let Some(rate) = args.secret_per_hour {
        policy.secret_enrollments_per_hour = limit(rate)?;
    }
    let changed = auth
        .instance_policy_set(&auth_types::AuthInstancePolicySetRequest {
            deployment_id: None,
            installation: Some(policy),
            deployment: None,
            idempotency_key: idempotency_key()?,
            extra: Default::default(),
        })
        .await
        .into_diagnostic()?;
    print_policy(format, &changed)
}
