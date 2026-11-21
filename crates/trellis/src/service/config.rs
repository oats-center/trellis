//! Settings Trellis keeps for a service or device.
//!
//! A participant declares `config` fields in its contract. Administrators set
//! values through the CLI or Console; the participant reads them when it
//! connects and follows live changes. Restart-only fields keep the value read
//! at connect.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::watch;

use crate::client::{TrellisClient, TrellisClientError};
use crate::generated::ConfigField;

use super::ServerError;

const AUTH_API: &str = "trellis.auth@v1";

/// Revisions behind the values in effect.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigRevision {
    /// The deployment's revision, absent before its first write.
    pub deployment: Option<u32>,
    /// This instance's override revision, absent when it has none.
    pub instance: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ConfigState {
    values: Arc<Map<String, Value>>,
    revision: ConfigRevision,
}

/// Live, typed settings for this participant.
///
/// `T` is the generated `Config` struct. Cloning shares the same live values.
pub struct ConfigHandle<T> {
    state: watch::Receiver<ConfigState>,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ConfigHandle<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            marker: PhantomData,
        }
    }
}

impl<T: DeserializeOwned> ConfigHandle<T> {
    pub(crate) fn new(state: watch::Receiver<ConfigState>) -> Self {
        Self {
            state,
            marker: PhantomData,
        }
    }

    /// The values in effect.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if the stored values do not decode as
    /// `T`, which means the generated struct and the running build disagree.
    pub fn current(&self) -> Result<T, ServerError> {
        decode(&self.state.borrow().values)
    }

    /// The revisions behind [`Self::current`].
    #[must_use]
    pub fn revision(&self) -> ConfigRevision {
        self.state.borrow().revision
    }

    /// Wait until an administrator changes a field that applies live, then
    /// return the new values.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] once the participant stopped and no
    /// further change can arrive, or if the values do not decode as `T`.
    pub async fn changed(&mut self) -> Result<T, ServerError> {
        self.state
            .changed()
            .await
            .map_err(|_| ServerError::Internal("the participant stopped".into()))?;
        let values = Arc::clone(&self.state.borrow_and_update().values);
        decode(&values)
    }
}

fn decode<T: DeserializeOwned>(values: &Map<String, Value>) -> Result<T, ServerError> {
    serde_json::from_value(Value::Object(values.clone()))
        .map_err(|error| ServerError::Internal(format!("settings do not decode: {error}")))
}

/// The live values and the task following their changes. Dropping it stops
/// the task.
pub(crate) struct LiveConfig {
    pub(crate) state: watch::Receiver<ConfigState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LiveConfig {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read(client: &TrellisClient, at_connect: bool) -> Result<Value, TrellisClientError> {
    let action = trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: "Config.Own.Get".into(),
        direction: trellis_protocol::PermissionAction::Call,
    };
    client
        .request_json_value(
            AUTH_API,
            action,
            &trellis_protocol::derive_rpc_subject(AUTH_API, "Config.Own.Get")?,
            &serde_json::json!({ "atConnect": at_connect }),
        )
        .await
}

fn revision_of(response: &Value) -> ConfigRevision {
    let number = |key: &str| {
        response[key]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
    };
    ConfigRevision {
        deployment: number("deploymentRevision"),
        instance: number("instanceRevision"),
    }
}

fn values_of(response: &Value) -> Map<String, Value> {
    response["values"].as_object().cloned().unwrap_or_default()
}

/// Read the settings at connect. While a required value is missing, print
/// where an administrator sets it and wait; never start with a partial
/// configuration.
pub(crate) async fn load(
    client: &TrellisClient,
    participant: &str,
    on_missing: &(dyn Fn(&str) + Sync),
) -> Result<ConfigState, TrellisClientError> {
    let mut delay = Duration::from_secs(1);
    let mut shown = String::new();
    loop {
        let response = read(client, true).await?;
        let missing = response["missing"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if missing.is_empty() {
            return Ok(ConfigState {
                values: Arc::new(values_of(&response)),
                revision: revision_of(&response),
            });
        }
        // A saved value that doesn't fit this build counts as missing; say why.
        let reasons = response["missing"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| response["invalid"][name.as_str()?].as_str())
            .map(|reason| format!("\n  {reason}"))
            .collect::<String>();
        if shown != format!("{missing}{reasons}") {
            on_missing(&missing);
            let url = response["configUrl"].as_str().map(str::to_owned);
            tracing::warn!(
                participant,
                missing,
                invalid = reasons.trim(),
                config_url = url,
                "Waiting for an administrator to set required settings"
            );
            match url {
                Some(url) => {
                    let qr = qrcode::QrCode::new(url.as_bytes())
                        .map(|code| {
                            code.render::<qrcode::render::unicode::Dense1x2>()
                                .quiet_zone(true)
                                .build()
                        })
                        .unwrap_or_default();
                    eprintln!(
                        "\n{participant} needs these settings before it starts: {missing}{reasons}\nSet them at {url}\nor scan:\n{qr}"
                    );
                }
                None => {
                    eprintln!(
                        "\n{participant} needs these settings before it starts: {missing}{reasons}"
                    )
                }
            }
            shown = format!("{missing}{reasons}");
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(15));
    }
}

/// The next state after a fresh read, if any live field changed. Restart-only
/// fields keep their connect-time value, and a field whose value was removed
/// keeps its last one until the participant connects again.
fn merge(
    fields: &[ConfigField],
    previous: &ConfigState,
    next: &Map<String, Value>,
    revision: ConfigRevision,
) -> Option<ConfigState> {
    let mut values = (*previous.values).clone();
    let mut changed = false;
    for field in fields.iter().filter(|field| !field.restart) {
        if let Some(value) = next.get(field.name) {
            if previous.values.get(field.name) != Some(value) {
                values.insert(field.name.to_owned(), value.clone());
                changed = true;
            }
        }
    }
    changed.then(|| ConfigState {
        values: Arc::new(values),
        revision,
    })
}

/// Follow `Config.Changed` for this participant. An event only prompts a
/// fresh authenticated read, so its payload is never trusted for values.
pub(crate) fn follow(
    client: Arc<TrellisClient>,
    fields: &'static [ConfigField],
    initial: ConfigState,
    deployment: String,
    instance: String,
) -> LiveConfig {
    let (sender, receiver) = watch::channel(initial);
    let task = tokio::spawn(async move {
        let subject = match trellis_protocol::derive_event_subject(AUTH_API, "Config.Changed") {
            Ok(subject) => subject,
            Err(error) => {
                tracing::error!(%error, "cannot derive the settings change subject");
                return;
            }
        };
        let refresh = || async {
            match read(&client, false).await {
                Ok(response) => {
                    let next = values_of(&response);
                    let revision = revision_of(&response);
                    sender.send_if_modified(|state| match merge(fields, state, &next, revision) {
                        Some(changed) => {
                            *state = changed;
                            true
                        }
                        None => {
                            state.revision = revision;
                            false
                        }
                    });
                }
                Err(error) => tracing::warn!(%error, "could not read changed settings"),
            }
        };
        loop {
            // Taken before acquiring, so a publication made while acquiring is
            // still seen as a change.
            let mut publication = client.subscribe_transport_publication();
            let deadline = Instant::now() + Duration::from_millis(client.timeout_ms());
            let lease = match client
                .acquire_transport(&[], std::slice::from_ref(&subject), deadline)
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    tracing::warn!(%error, "settings watch interrupted; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            let mut subscription = match lease.nats().subscribe(subject.clone()).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    tracing::warn!(%error, "settings watch interrupted; retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if let Err(error) = lease.nats().flush().await {
                tracing::warn!(%error, "settings watch interrupted; retrying");
                continue;
            }
            // Changes made while no subscription existed are read now.
            refresh().await;
            loop {
                tokio::select! {
                    message = subscription.next() => {
                        let Some(message) = message else { break };
                        let Ok(event) = serde_json::from_slice::<Value>(&message.payload) else {
                            continue;
                        };
                        if event["deploymentId"].as_str() != Some(deployment.as_str()) {
                            continue;
                        }
                        if event["instanceId"]
                            .as_str()
                            .is_some_and(|target| target != instance)
                        {
                            continue;
                        }
                        refresh().await;
                    }
                    () = lease.wait_lost() => break,
                    // A successor's publication ends this subscription. The
                    // next pass subscribes on the successor and reads again,
                    // and dropping this lease lets the old generation drain.
                    changed = publication.changed() => {
                        if changed.is_err() {
                            // The connection's transport is gone.
                            return;
                        }
                        let published = *publication.borrow_and_update();
                        if published.is_some_and(|id| id != lease.generation_id()) {
                            break;
                        }
                    }
                }
            }
        }
    });
    LiveConfig {
        state: receiver,
        task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIELDS: &[ConfigField] = &[
        ConfigField {
            name: "level",
            required: false,
            secret: false,
            restart: false,
        },
        ConfigField {
            name: "port",
            required: true,
            secret: false,
            restart: true,
        },
    ];

    fn state(values: Value) -> ConfigState {
        ConfigState {
            values: Arc::new(values.as_object().unwrap().clone()),
            revision: ConfigRevision::default(),
        }
    }

    #[test]
    fn live_fields_change_while_restart_and_removed_fields_keep_their_value() {
        let previous = state(serde_json::json!({"level": "info", "port": 80}));
        let next = state(serde_json::json!({"level": "debug", "port": 81}));
        let merged = merge(FIELDS, &previous, &next.values, ConfigRevision::default()).unwrap();
        assert_eq!(
            *merged.values,
            *state(serde_json::json!({"level": "debug", "port": 80})).values
        );

        // Only a restart-only field changed: nothing to tell the service.
        let port_only = state(serde_json::json!({"level": "info", "port": 81}));
        assert!(merge(
            FIELDS,
            &previous,
            &port_only.values,
            ConfigRevision::default()
        )
        .is_none());

        // A removed value is kept until the next connect waits for it.
        let removed = state(serde_json::json!({"port": 80}));
        assert!(merge(
            FIELDS,
            &previous,
            &removed.values,
            ConfigRevision::default()
        )
        .is_none());
    }
}
