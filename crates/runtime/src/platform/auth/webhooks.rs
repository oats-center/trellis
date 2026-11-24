//! Auth's part of webhook endpoints: their URLs, whether they are enabled,
//! the runtime settings a deployment chose, and the settings and secrets
//! their verifiers read. Intake reads all of it as one [`WebhookEndpointRecord`]
//! per endpoint the installed build declares.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use rusqlite::{params, OptionalExtension};
use trellis_idl::{ResourceDefinition, WebhookDefinition, WebhookVerifier};
use trellis_runtime_apis::__types::trellis::{
    AuthWebhookChange, AuthWebhookSetting, AuthWebhooksUpdateRequest, AuthWebhooksUpdateResponse,
};

use super::administration::Administration;
use super::config::WebhookSettingValue;
use super::oauth::storage::OAuthStorage;
use super::sqlite::{decode, Connection, Mutation, SqliteAuthorizationStore};
use super::AuthError;

/// A new random URL ID: 128 random bits as 22 characters of base64url.
fn url_id() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>())
}

/// The URL's service segment: the deployment's name, lowercased, with runs of
/// anything but letters and digits made one `-`.
pub(super) fn service_name(title: &str) -> String {
    let mut name = String::new();
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            name.push(character);
        } else if !name.ends_with('-') && !name.is_empty() {
            name.push('-');
        }
    }
    let name = name.trim_end_matches('-');
    let name: String = name.chars().take(63).collect();
    if name.is_empty() {
        "service".to_owned()
    } else {
        name
    }
}

/// Record an endpoint the deployment's build declares, giving it a URL the
/// first time. An endpoint declared again keeps its URL and enabled state.
pub(super) fn declare_endpoint(
    connection: &Connection,
    deployment: &str,
    endpoint: &str,
    now: i64,
) -> Result<(), AuthError> {
    let title: String = connection.query_row(
        "SELECT title FROM auth_deployments WHERE deployment_id=?1",
        [deployment],
        |row| row.get(0),
    )?;
    let inserted = connection.execute(
        "INSERT INTO auth_webhook_endpoints(deployment_id,endpoint,service_name,enabled,created_at) \
        VALUES(?1,?2,?3,1,?4) ON CONFLICT(deployment_id,endpoint) DO NOTHING",
        params![deployment, endpoint, service_name(&title), now],
    )?;
    if inserted == 1 {
        connection.execute(
            "INSERT INTO auth_webhook_urls(url_id,deployment_id,endpoint,current,created_at) \
            VALUES(?1,?2,?3,1,?4)",
            params![url_id(), deployment, endpoint, now],
        )?;
    }
    Ok(())
}

/// The settings name a deployment-wide setting is stored under.
pub(crate) const DEPLOYMENT_WIDE: &str = "";

/// The stored name of a runtime setting, and whether it applies to the whole
/// deployment rather than one endpoint.
pub(crate) fn setting_name(
    setting: &AuthWebhookSetting,
) -> Result<(&'static str, bool), AuthError> {
    Ok(match setting {
        AuthWebhookSetting::RequestsPerSecond => ("requestsPerSecond", false),
        AuthWebhookSetting::Burst => ("burst", false),
        AuthWebhookSetting::FailuresPerMinute => ("failuresPerMinute", true),
        AuthWebhookSetting::MaxBody => ("maxBody", false),
        AuthWebhookSetting::DedupeWindow => ("dedupeWindow", false),
        AuthWebhookSetting::SyncDecideDeadline => ("syncDecideDeadline", false),
        AuthWebhookSetting::AsyncDecideDeadline => ("asyncDecideDeadline", false),
        AuthWebhookSetting::RotationOverlap => ("rotationOverlap", false),
        AuthWebhookSetting::RecordRetention => ("recordRetention", true),
        AuthWebhookSetting::IntakeRetention => ("intakeRetention", true),
        AuthWebhookSetting::IntakeMaxBytes => ("intakeMaxBytes", true),
        AuthWebhookSetting::Unknown(_) => {
            return Err(AuthError::Invalid("unknown webhook setting".into()))
        }
    })
}

/// `Auth.Webhooks.Update`: enable, disable, rotate or retire a URL, or set a
/// runtime setting. Each takes the capability that changes a service's
/// resources.
pub(super) fn update(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthWebhooksUpdateRequest,
    now: i64,
) -> Result<AuthWebhooksUpdateResponse, AuthError> {
    store.mutate(identity, "rpc.webhooks.update", input, now, |connection| {
        SqliteAuthorizationStore::require_administration(
            connection,
            &identity.actor,
            Administration::ManagePrincipals,
        )?;
        let deployment = input.deployment_id.to_string();
        let endpoint = input.endpoint.as_str();
        let enabled: bool = connection
            .query_row(
                "SELECT enabled FROM auth_webhook_endpoints WHERE deployment_id=?1 AND endpoint=?2",
                params![deployment, endpoint],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                AuthError::NotFound(format!(
                    "deployment {deployment} has no webhook endpoint {endpoint}"
                ))
            })?;
        let enabled = match &input.change {
            AuthWebhookChange::Enable | AuthWebhookChange::Disable => {
                let enable = input.change == AuthWebhookChange::Enable;
                connection.execute(
                    "UPDATE auth_webhook_endpoints SET enabled=?3 WHERE deployment_id=?1 AND endpoint=?2",
                    params![deployment, endpoint, enable],
                )?;
                enable
            }
            AuthWebhookChange::RotateUrl => {
                connection.execute(
                    "UPDATE auth_webhook_urls SET current=0 WHERE deployment_id=?1 AND endpoint=?2",
                    params![deployment, endpoint],
                )?;
                connection.execute(
                    "INSERT INTO auth_webhook_urls(url_id,deployment_id,endpoint,current,created_at) \
                    VALUES(?1,?2,?3,1,?4)",
                    params![url_id(), deployment, endpoint, now],
                )?;
                enabled
            }
            AuthWebhookChange::RetireUrl => {
                let retired = connection.execute(
                    "DELETE FROM auth_webhook_urls WHERE deployment_id=?1 AND endpoint=?2 AND current=0",
                    params![deployment, endpoint],
                )?;
                if retired == 0 {
                    return Err(AuthError::Invalid(
                        "the endpoint has no earlier URL to retire; rotate it first".into(),
                    ));
                }
                enabled
            }
            AuthWebhookChange::Set => {
                let setting = input
                    .setting
                    .as_ref()
                    .ok_or_else(|| AuthError::Invalid("set needs a setting".into()))?;
                let (name, deployment_wide) = setting_name(setting)?;
                let scope = if deployment_wide { DEPLOYMENT_WIDE } else { endpoint };
                match &input.value {
                    Some(trellis_protocol::model::Nullable::Value(value)) => {
                        let value = value.0;
                        let value = i64::try_from(value)
                            .map_err(|_| AuthError::Invalid("value is too large".into()))?;
                        check_setting(name, value)?;
                        connection.execute(
                            "INSERT INTO auth_webhook_settings(deployment_id,endpoint,setting,value) \
                            VALUES(?1,?2,?3,?4) ON CONFLICT(deployment_id,endpoint,setting) \
                            DO UPDATE SET value=excluded.value",
                            params![deployment, scope, name, value],
                        )?;
                    }
                    Some(trellis_protocol::model::Nullable::Null) | None => {
                        connection.execute(
                            "DELETE FROM auth_webhook_settings \
                            WHERE deployment_id=?1 AND endpoint=?2 AND setting=?3",
                            params![deployment, scope, name],
                        )?;
                    }
                }
                enabled
            }
            AuthWebhookChange::Unknown(_) => {
                return Err(AuthError::Invalid("unknown webhook change".into()))
            }
        };
        Ok(AuthWebhooksUpdateResponse {
            endpoint: endpoint.to_owned(),
            enabled,
            extra: Default::default(),
        })
    })
}

/// Bounds a deployment's value must respect, so a setting cannot disable a
/// protection by accident or exceed what intake can honor.
fn check_setting(name: &str, value: i64) -> Result<(), AuthError> {
    let (minimum, maximum) = match name {
        "requestsPerSecond" | "burst" | "failuresPerMinute" => (1, 1_000_000),
        "maxBody" => (1, 64 << 20),
        "dedupeWindow" => (1_000, 30 * 86_400_000),
        "syncDecideDeadline" => (1, 10_000),
        "asyncDecideDeadline" => (1, 60_000),
        "rotationOverlap" => (0, 30 * 86_400_000),
        "recordRetention" => (3_600_000, 3_650 * 86_400_000),
        // Longer than the longest async decide retry chain before every 10 min.
        "intakeRetention" => (3_600_000, 365 * 86_400_000),
        "intakeMaxBytes" => (1 << 20, 1 << 40),
        _ => return Err(AuthError::Invalid("unknown webhook setting".into())),
    };
    if value < minimum || value > maximum {
        return Err(AuthError::Invalid(format!(
            "{name} must be between {minimum} and {maximum}"
        )));
    }
    Ok(())
}

/// The deployments an administrator removed. Removal is final, so their
/// intake streams can go.
pub(crate) fn removed_deployments(storage: &OAuthStorage) -> Result<Vec<String>, AuthError> {
    storage.authority.read(|connection| {
        Ok(connection
            .prepare("SELECT deployment_id FROM auth_deployments WHERE state='revoked'")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    })
}

/// Every deployment's own deployment-wide webhook settings, by deployment,
/// whether or not it still has an endpoint, so its records keep expiring.
pub(crate) fn load_retention(
    storage: &OAuthStorage,
) -> Result<BTreeMap<String, BTreeMap<String, u64>>, AuthError> {
    storage.authority.read(|connection| {
        let mut statement = connection.prepare(
            "SELECT deployment_id,setting,value FROM auth_webhook_settings WHERE endpoint=?1",
        )?;
        let mut settings: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for row in statement.query_map([DEPLOYMENT_WIDE], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })? {
            let (deployment, setting, value) = row?;
            settings
                .entry(deployment)
                .or_default()
                .insert(setting, value);
        }
        Ok(settings)
    })
}

/// One endpoint as intake serves it.
#[derive(Clone, Debug)]
pub(crate) struct WebhookEndpointRecord {
    pub(crate) deployment_id: String,
    pub(crate) service_name: String,
    pub(crate) endpoint: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) definition: WebhookDefinition,
    pub(crate) deployment_enabled: bool,
    pub(crate) endpoint_enabled: bool,
    pub(crate) current_url: String,
    pub(crate) previous_urls: Vec<String>,
    /// Settings the deployment chose: the endpoint's own, then deployment-wide ones.
    pub(crate) settings: BTreeMap<String, u64>,
    /// The settings and secrets the verifiers name.
    pub(crate) values: BTreeMap<String, WebhookSettingValue>,
}

/// Every endpoint an installed build of a deployment that still exists
/// declares. Endpoints a newer build dropped are left out, so their URLs
/// answer 404 until a build declares them again.
pub(crate) fn load_endpoints(
    storage: &OAuthStorage,
) -> Result<Vec<WebhookEndpointRecord>, AuthError> {
    storage.authority.read(|connection| {
        let mut statement = connection.prepare(
            "SELECT e.deployment_id,e.endpoint,e.service_name,e.enabled,d.state,\
            d.participant_id,b.installed_revision,p.projection_json \
            FROM auth_webhook_endpoints e \
            JOIN auth_deployments d USING(deployment_id) \
            JOIN auth_deployment_bindings b USING(deployment_id) \
            JOIN auth_installed_participants p \
            ON p.participant_id=d.participant_id AND p.revision=b.installed_revision \
            WHERE d.state!='revoked' ORDER BY e.deployment_id,e.endpoint",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, u64>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut graphs = BTreeMap::new();
        let mut records = Vec::new();
        for (deployment, endpoint, name, enabled, state, participant, revision, evidence) in rows {
            let graph = match graphs.entry((participant.clone(), revision)) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                    trellis_idl::compile_evidence(decode(&evidence)?)
                        .map_err(|error| AuthError::Invalid(error.to_string()))?,
                ),
            };
            let Some(definition) = graph
                .root_package()
                .participants()
                .values()
                .find(|definition| definition.identity().as_str() == participant)
            else {
                continue;
            };
            let Some((docs, declared)) =
                definition
                    .resources()
                    .iter()
                    .find_map(|(local, resource)| match resource {
                        ResourceDefinition::Webhook {
                            docs,
                            endpoint: declared,
                        } if local.as_str() == endpoint => Some((docs, declared)),
                        _ => None,
                    })
            else {
                continue;
            };
            let mut urls = connection.prepare(
                "SELECT url_id,current FROM auth_webhook_urls \
                WHERE deployment_id=?1 AND endpoint=?2 ORDER BY created_at DESC",
            )?;
            let mut current_url = None;
            let mut previous_urls = Vec::new();
            for url in urls.query_map(params![deployment, endpoint], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
            })? {
                let (url, current) = url?;
                if current {
                    current_url = Some(url);
                } else {
                    previous_urls.push(url);
                }
            }
            let Some(current_url) = current_url else {
                continue;
            };
            let mut settings = BTreeMap::new();
            let mut stored = connection.prepare(
                "SELECT setting,value FROM auth_webhook_settings \
                WHERE deployment_id=?1 AND endpoint IN (?2,'') ORDER BY endpoint DESC",
            )?;
            for setting in stored.query_map(params![deployment, endpoint], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })? {
                let (setting, value) = setting?;
                settings.entry(setting).or_insert(value);
            }
            let mut fields = BTreeSet::new();
            for verifier in &declared.verifiers {
                match verifier {
                    WebhookVerifier::Hmac(hmac) => {
                        fields.insert(hmac.key.clone());
                    }
                    WebhookVerifier::Token { secret, .. } => {
                        fields.insert(secret.clone());
                    }
                    WebhookVerifier::Basic { username, password } => {
                        fields.insert(username.clone());
                        fields.insert(password.clone());
                    }
                    WebhookVerifier::Address { ranges } => {
                        fields.insert(ranges.clone());
                    }
                    WebhookVerifier::None => {}
                }
            }
            let values =
                super::config::webhook_settings(connection, storage, &deployment, &fields)?;
            records.push(WebhookEndpointRecord {
                deployment_id: deployment,
                service_name: name,
                endpoint,
                title: docs.title.clone(),
                description: docs.description.clone(),
                definition: (**declared).clone(),
                deployment_enabled: state == "active",
                endpoint_enabled: enabled,
                current_url,
                previous_urls,
                settings,
                values,
            });
        }
        Ok(records)
    })
}
