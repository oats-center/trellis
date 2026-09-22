use std::time::Duration;
use std::{future::Future, pin::Pin, sync::Arc};

use async_nats::jetstream::{self, AckKind};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use trellis_rs::service::EventVerificationFailure;
use trellis_rs::service::ServerError;

use crate::storage::{now_timestamp_string, EventsStore, EventsStoreError, ProjectedEvent};

pub(crate) type EventMessageStream = trellis_rs::service::EventsMessageStream;

const EVENT_STREAM: &str = "trellis";
const EVENT_SUBJECT_WILDCARD: &str = "events.v1.>";
const PROJECTOR_BATCH_SIZE: usize = 100;
const PROJECTOR_CONCURRENCY: usize = 32;
const EVENT_ID_HEADER: &str = "Nats-Msg-Id";
const EVENT_TIME_HEADER: &str = "Trellis-Event-Time";
pub(crate) const CONSUMER_METADATA_MANAGED_BY: &str = "trellis.managed_by";
pub(crate) const CONSUMER_METADATA_DEPLOYMENT_ID: &str = "trellis.deployment_id";
pub(crate) const CONSUMER_METADATA_CONTRACT_ID: &str = "trellis.contract_id";
pub(crate) const CONSUMER_METADATA_GROUP: &str = "trellis.group";
pub(crate) const CONSUMER_METADATA_RESOURCE_ID: &str = "trellis.resource_id";

/// Events transport facade over the Trellis event JetStream stream.
pub type EventsRuntime = trellis_rs::service::EventsRuntime;

/// Raw event fields passed to the runtime-owned local authorization verifier.
#[derive(Clone, Debug)]
pub struct EventAuthorizationInput {
    pub subject: String,
    pub descriptor_identity: String,
    pub payload: Vec<u8>,
    pub session_key: String,
    pub proof: String,
    pub authorization_context: String,
    pub event_id: String,
    pub event_time: String,
}

/// Publisher fields proven by a context-bound event proof.
#[derive(Clone, Debug)]
pub struct VerifiedEventPublisher {
    pub kind: String,
    pub deployment_id: Option<String>,
    pub instance_id: Option<String>,
    pub participant_id: String,
    pub principal_id: String,
    pub connection_id: String,
    pub login_session_id: Option<String>,
    pub context_digest: String,
    pub owner_contract_id: String,
    pub owner_event_name: String,
}

/// Runtime-owned local event verifier callback.
pub type EventVerifier = Arc<
    dyn Fn(
            EventAuthorizationInput,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<VerifiedEventPublisher, EventVerificationFailure>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

/// Handle for the background Events projector task.
pub struct EventsProjectorHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl EventsProjectorHandle {
    /// Stop the projector task.
    pub async fn stop(self) {
        let Some(task) = self.task else {
            return;
        };
        task.abort();
        let _ = task.await;
    }

    /// Drop a completed projector task after observing its result.
    pub fn discard_completed(&mut self) {
        self.task = None;
    }

    /// Wait for the projector task to finish.
    pub async fn wait(&mut self) -> Result<(), ServerError> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(ServerError::Nats(format!(
                "Events projector loop task failed: {error}"
            ))),
        }
    }
}

/// Start projecting Trellis event messages into SQLite.
pub async fn start_events_projector(
    runtime: EventsRuntime,
    store: EventsStore,
    verifier: EventVerifier,
) -> Result<EventsProjectorHandle, ServerError> {
    let (first_sequence, _) = runtime
        .stream_bounds()
        .await
        .map_err(|error| ServerError::Nats(format!("failed to inspect Events stream: {error}")))?;
    store
        .record_stream_bounds(first_sequence)
        .map_err(|error| {
            ServerError::Nats(format!("failed to record Events retention bounds: {error}"))
        })?;
    let consumer_name = projector_consumer_name(&store.projection_id().map_err(|error| {
        ServerError::Nats(format!(
            "failed to resolve Events projection identity: {error}"
        ))
    })?);
    let consumer = runtime
        .event_consumer(&consumer_name, true)
        .await
        .map_err(|error| {
            ServerError::Nats(format!(
                "failed to start Events projector consumer '{consumer_name}': {error}"
            ))
        })?;
    tracing::info!(stream = EVENT_STREAM, consumer = %consumer_name, filter = EVENT_SUBJECT_WILDCARD, "started Events projector consumer");

    let task = tokio::spawn(async move {
        loop {
            let mut batch = Vec::new();
            let mut messages = consumer
                .fetch()
                .max_messages(PROJECTOR_BATCH_SIZE)
                .expires(Duration::from_millis(500))
                .messages()
                .await
                .map_err(|error| {
                    ServerError::Nats(format!("Events projector failed to fetch batch: {error}"))
                })?;
            while let Some(message) = messages.next().await {
                collect_message(message, &mut batch)?;
            }
            if batch.is_empty() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            store.record_stream_bounds(
                batch[0]
                    .info()
                    .map_err(|error| {
                        ServerError::Nats(format!(
                            "Events projector message metadata failed: {error}"
                        ))
                    })?
                    .stream_sequence,
            )?;

            batch.sort_by_key(|message| {
                message
                    .info()
                    .map(|info| info.stream_sequence)
                    .unwrap_or(u64::MAX)
            });
            let mut verified = stream::iter(batch.into_iter().map(|message| {
                let store = store.clone();
                let verifier = Arc::clone(&verifier);
                async move { verify_message(store, message, verifier).await }
            }))
            .buffered(PROJECTOR_CONCURRENCY);
            while let Some(result) = verified.next().await {
                if let Some((message, event)) = result? {
                    persist_message(store.clone(), message, event).await;
                }
            }
        }
    });

    Ok(EventsProjectorHandle { task: Some(task) })
}

fn collect_message(
    message: Result<jetstream::Message, impl std::fmt::Display>,
    batch: &mut Vec<jetstream::Message>,
) -> Result<(), ServerError> {
    batch.push(message.map_err(|error| {
        ServerError::Nats(format!("Events projector failed to pull message: {error}"))
    })?);
    Ok(())
}

async fn verify_message(
    store: EventsStore,
    message: jetstream::Message,
    verifier: EventVerifier,
) -> Result<Option<(jetstream::Message, ProjectedEvent)>, ServerError> {
    let stream_sequence = match message.info() {
        Ok(info) => info.stream_sequence,
        Err(error) => {
            tracing::warn!(%error, subject = %message.subject, "retrying Events message with unavailable metadata");
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await;
            return Ok(None);
        }
    };
    let existing_store = store.clone();
    let already_projected = match tokio::task::spawn_blocking(move || {
        existing_store.contains_stream_sequence(stream_sequence)
    })
    .await
    {
        Ok(Ok(already_projected)) => already_projected,
        Ok(Err(error)) => {
            tracing::warn!(%error, subject = %message.subject, "retrying Events duplicate check");
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await;
            return Ok(None);
        }
        Err(error) => {
            tracing::warn!(%error, subject = %message.subject, "retrying Events duplicate-check task");
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await;
            return Ok(None);
        }
    };
    if already_projected {
        let _ = message.ack().await;
        return Ok(None);
    }
    match project_message_inner(&message, verifier).await {
        Ok(event) => Ok(Some((message, event))),
        Err(error) => {
            tracing::warn!(%error, subject = %message.subject, "retrying temporarily unverifiable Events message");
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await;
            Ok(None)
        }
    }
}

async fn persist_message(store: EventsStore, message: jetstream::Message, event: ProjectedEvent) {
    match tokio::task::spawn_blocking(move || store.insert_event(&event)).await {
        Ok(Ok(())) => {
            let _ = message.ack().await;
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, subject = %message.subject, "retrying Events persistence");
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await;
        }
        Err(error) => {
            tracing::warn!(%error, subject = %message.subject, "retrying Events persistence task");
            let _ = message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await;
        }
    }
}

pub(crate) async fn project_message_inner(
    message: &jetstream::Message,
    verifier: EventVerifier,
) -> Result<ProjectedEvent, String> {
    let info = message.info().map_err(|error| error.to_string())?;
    let headers = headers_json(message.headers.as_ref());
    let event_id = header_value(message, EVENT_ID_HEADER).map(str::to_owned);
    let event_time = header_value(message, EVENT_TIME_HEADER)
        .map(str::to_owned)
        .unwrap_or_else(now_timestamp_string);
    let session_key = header_value(message, "session-key").filter(|value| !value.is_empty());
    let proof = header_value(message, "proof").filter(|value| !value.is_empty());
    let authorization_context =
        header_value(message, "authorization-context").filter(|value| !value.is_empty());
    let descriptor_identity =
        header_value(message, "Trellis-Event-Descriptor").filter(|value| !value.is_empty());
    let mut verification_status = if session_key.is_none() || authorization_context.is_none() {
        "missing-session".to_owned()
    } else if proof.is_none() || event_id.is_none() || descriptor_identity.is_none() {
        "missing-proof".to_owned()
    } else if header_value(message, EVENT_TIME_HEADER).is_none() {
        "outside-session-window".to_owned()
    } else {
        "verified".to_owned()
    };
    let publisher = if verification_status == "verified" {
        match verifier(EventAuthorizationInput {
            subject: message.subject.to_string(),
            descriptor_identity: descriptor_identity.unwrap_or_default().to_owned(),
            payload: message.payload.to_vec(),
            session_key: session_key.unwrap_or_default().to_owned(),
            proof: proof.unwrap_or_default().to_owned(),
            authorization_context: authorization_context.unwrap_or_default().to_owned(),
            event_id: event_id.clone().unwrap_or_default(),
            event_time: event_time.clone(),
        })
        .await
        {
            Ok(publisher) => Some(publisher),
            Err(EventVerificationFailure::Rejected(error)) => {
                verification_status = classify_rejection(&error).to_owned();
                None
            }
            Err(EventVerificationFailure::Retryable(error)) => return Err(error),
        }
    } else {
        None
    };
    let traceparent = header_value(message, "traceparent").map(str::to_string);
    let trace_id = traceparent
        .as_deref()
        .and_then(trace_id_from_traceparent)
        .map(str::to_string);
    let (payload_json, payload_text, decode_error) = decode_payload(&message.payload);
    let (owner_contract_id, owner_event_name) = publisher
        .as_ref()
        .map(|publisher| {
            (
                Some(publisher.owner_contract_id.clone()),
                Some(publisher.owner_event_name.clone()),
            )
        })
        .unwrap_or_else(|| resolve_event_identity(message.subject.as_str()));
    let resolution = if owner_contract_id.is_some() {
        "resolved"
    } else {
        "malformed"
    };
    let event_time = normalized_event_time(&event_time);
    Ok(ProjectedEvent {
        stream_sequence: info.stream_sequence,
        event_id,
        event_time,
        subject: message.subject.to_string(),
        owner_contract_id,
        owner_event_name,
        resolution: resolution.to_owned(),
        verification_status,
        publisher_kind: publisher.as_ref().map(|publisher| publisher.kind.clone()),
        publisher_deployment_id: publisher
            .as_ref()
            .and_then(|publisher| publisher.deployment_id.clone()),
        publisher_instance_id: publisher
            .as_ref()
            .and_then(|publisher| publisher.instance_id.clone()),
        publisher_participant_id: publisher
            .as_ref()
            .map(|publisher| publisher.participant_id.clone()),
        publisher_principal_id: publisher
            .as_ref()
            .map(|publisher| publisher.principal_id.clone()),
        publisher_connection_id: publisher
            .as_ref()
            .map(|publisher| publisher.connection_id.clone()),
        publisher_login_session_id: publisher
            .as_ref()
            .and_then(|publisher| publisher.login_session_id.clone()),
        authorization_context_digest: authorization_context.map(str::to_owned),
        trace_id,
        traceparent,
        payload_bytes: message.payload.to_vec(),
        headers_json: headers.to_string(),
        payload_json,
        payload_text,
        decode_error,
        projected_at: now_timestamp_string(),
    })
}

pub(crate) fn normalized_event_time(value: &str) -> String {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let nanos = timestamp
        .unix_timestamp_nanos()
        .clamp(i64::MIN as i128, i64::MAX as i128);
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

pub(crate) fn resolve_event_identity(subject: &str) -> (Option<String>, Option<String>) {
    if let Some((api_id, event_name)) = builtin_event_identity(subject) {
        return (Some(api_id.to_owned()), Some(event_name.to_owned()));
    }
    let mut tokens = subject.split('.');
    if tokens.next() != Some("events") || tokens.next() != Some("v1") {
        return (None, None);
    }
    let Some(api_token) = tokens.next() else {
        return (None, None);
    };
    let Some(event_name) = tokens.next().filter(|name| !name.is_empty()) else {
        return (None, None);
    };
    let Ok(api_bytes) = URL_SAFE_NO_PAD.decode(api_token) else {
        return (None, None);
    };
    let Ok(api_id) = String::from_utf8(api_bytes) else {
        return (None, None);
    };
    if trellis_protocol::validate_api_id(&api_id).is_err() {
        return (None, None);
    }
    // A trailing token can be either part of a dotted event name or a parameter.
    let event_name = tokens.next().is_none().then(|| event_name.to_owned());
    (Some(api_id), event_name)
}

fn builtin_event_identity(subject: &str) -> Option<(&'static str, &'static str)> {
    use trellis_runtime_apis::apis::{
        trellis_auth_v1::events as auth, trellis_health_v1::events as health,
    };

    [
        (
            auth::ConnectionsClosed::SUBSCRIBE_SUBJECT,
            auth::ConnectionsClosed::API_ID,
            auth::ConnectionsClosed::DESCRIPTOR_NAME,
        ),
        (
            auth::ConnectionsKicked::SUBSCRIBE_SUBJECT,
            auth::ConnectionsKicked::API_ID,
            auth::ConnectionsKicked::DESCRIPTOR_NAME,
        ),
        (
            auth::ConnectionsOpened::SUBSCRIBE_SUBJECT,
            auth::ConnectionsOpened::API_ID,
            auth::ConnectionsOpened::DESCRIPTOR_NAME,
        ),
        (
            auth::DeviceUserAuthoritiesApproved::SUBSCRIBE_SUBJECT,
            auth::DeviceUserAuthoritiesApproved::API_ID,
            auth::DeviceUserAuthoritiesApproved::DESCRIPTOR_NAME,
        ),
        (
            auth::DeviceUserAuthoritiesRequested::SUBSCRIBE_SUBJECT,
            auth::DeviceUserAuthoritiesRequested::API_ID,
            auth::DeviceUserAuthoritiesRequested::DESCRIPTOR_NAME,
        ),
        (
            auth::DeviceUserAuthoritiesResolved::SUBSCRIBE_SUBJECT,
            auth::DeviceUserAuthoritiesResolved::API_ID,
            auth::DeviceUserAuthoritiesResolved::DESCRIPTOR_NAME,
        ),
        (
            auth::DeviceUserAuthoritiesReviewRequested::SUBSCRIBE_SUBJECT,
            auth::DeviceUserAuthoritiesReviewRequested::API_ID,
            auth::DeviceUserAuthoritiesReviewRequested::DESCRIPTOR_NAME,
        ),
        (
            auth::GrantsChanged::SUBSCRIBE_SUBJECT,
            auth::GrantsChanged::API_ID,
            auth::GrantsChanged::DESCRIPTOR_NAME,
        ),
        (
            auth::IssuersRevoked::SUBSCRIBE_SUBJECT,
            auth::IssuersRevoked::API_ID,
            auth::IssuersRevoked::DESCRIPTOR_NAME,
        ),
        (
            auth::SessionsRevoked::SUBSCRIBE_SUBJECT,
            auth::SessionsRevoked::API_ID,
            auth::SessionsRevoked::DESCRIPTOR_NAME,
        ),
        (
            health::StatusChanged::SUBSCRIBE_SUBJECT,
            health::StatusChanged::API_ID,
            health::StatusChanged::DESCRIPTOR_NAME,
        ),
    ]
    .into_iter()
    .find(|(filter, _, _)| nats_subject_matches(filter, subject))
    .and_then(|(_, api_id, descriptor_name)| {
        Some((api_id, descriptor_name.strip_prefix("event.")?))
    })
}

fn nats_subject_matches(filter: &str, subject: &str) -> bool {
    let mut filter = filter.split('.');
    let mut subject = subject.split('.');
    loop {
        match (filter.next(), subject.next()) {
            (Some(">"), Some(_)) => return filter.next().is_none(),
            (Some("*"), Some(_)) => {}
            (Some(expected), Some(actual)) if expected == actual => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn header_value<'a>(message: &'a jetstream::Message, name: &str) -> Option<&'a str> {
    message
        .headers
        .as_ref()?
        .get(name)
        .map(|value| value.as_str())
}

fn headers_json(headers: Option<&async_nats::HeaderMap>) -> Value {
    let mut object = serde_json::Map::new();
    if let Some(headers) = headers {
        for (name, value) in headers.iter() {
            object.insert(
                name.to_string(),
                json!(value.iter().map(|value| value.as_str()).collect::<Vec<_>>()),
            );
        }
    }
    Value::Object(object)
}

fn classify_rejection(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("subject") || error.contains("permission") || error.contains("granted") {
        "subject-denied"
    } else if error.contains("window")
        || error.contains("time")
        || error.contains("expired")
        || error.contains("revoked")
    {
        "outside-session-window"
    } else if error.contains("session") || error.contains("context") {
        "missing-session"
    } else {
        "invalid-signature"
    }
}

fn decode_payload(payload: &[u8]) -> (Option<String>, Option<String>, Option<String>) {
    match std::str::from_utf8(payload) {
        Ok(text) => {
            let payload_json = serde_json::from_str::<Value>(text)
                .ok()
                .map(|value| value.to_string());
            (payload_json, Some(text.to_string()), None)
        }
        Err(error) => (None, None, Some(error.to_string())),
    }
}

fn trace_id_from_traceparent(traceparent: &str) -> Option<&str> {
    traceparent
        .split('-')
        .nth(1)
        .filter(|value| value.len() == 32)
}

fn projector_consumer_name(projection_id: &str) -> String {
    format!(
        "events-projector-{}",
        sanitize_consumer_token(projection_id)
    )
}

pub(crate) fn is_events_projector_consumer(name: &str) -> bool {
    name.starts_with("events-projector-")
}

fn sanitize_consumer_token(value: &str) -> String {
    let token = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect::<String>();
    if token.is_empty() {
        "projection".to_string()
    } else {
        token
    }
}

impl From<EventsStoreError> for ServerError {
    fn from(error: EventsStoreError) -> Self {
        ServerError::Nats(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_rejection, is_events_projector_consumer, normalized_event_time,
        projector_consumer_name, resolve_event_identity,
    };

    #[test]
    fn projector_consumer_name_removes_timestamp_punctuation() {
        assert_eq!(
            projector_consumer_name("1877448-2026-07-09T04:28:06.672734698Z"),
            "events-projector-1877448-2026-07-09T042806672734698Z",
        );
    }

    #[test]
    fn events_consumer_ownership_recognizes_projector_names() {
        assert!(is_events_projector_consumer("events-projector-current"));
        assert!(!is_events_projector_consumer("event-log-projector-legacy"));
    }

    #[test]
    fn permanent_verification_failures_have_stable_integrity_classes() {
        assert_eq!(
            classify_rejection("historical session is revoked"),
            "outside-session-window"
        );
        assert_eq!(
            classify_rejection("event subject is not authorized"),
            "subject-denied"
        );
        assert_eq!(
            classify_rejection("signature mismatch"),
            "invalid-signature"
        );
    }

    #[test]
    fn event_time_normalization_is_safe_and_deterministic() {
        assert_eq!(
            normalized_event_time("9999-12-31T23:59:59.999999999Z"),
            "2262-04-11T23:47:16.854775807Z"
        );
        assert_eq!(normalized_event_time("garbage"), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn event_identity_decodes_unambiguous_qualified_api_subject() {
        assert_eq!(
            resolve_event_identity("events.v1.YWNtZS1vcmRlcnMucnVudGltZUB2MQ.Changed"),
            (
                Some("acme-orders.runtime@v1".to_owned()),
                Some("Changed".to_owned())
            )
        );
        assert_eq!(
            resolve_event_identity("events.v1.not-base64.Created"),
            (None, None)
        );
    }

    #[test]
    fn event_identity_does_not_guess_between_dotted_names_and_parameters() {
        assert_eq!(
            resolve_event_identity("events.v1.YWNtZS1vcmRlcnMucnVudGltZUB2MQ.Changed.customer"),
            (Some("acme-orders.runtime@v1".to_owned()), None)
        );
    }

    #[test]
    fn event_identity_uses_full_generated_descriptor_name() {
        assert_eq!(
            resolve_event_identity(
                trellis_runtime_apis::apis::trellis_auth_v1::events::ConnectionsOpened::SUBJECT,
            ),
            (
                Some("trellis.auth@v1".to_owned()),
                Some("Connections.Opened".to_owned())
            )
        );
    }

    #[test]
    fn event_identity_matches_parameterized_generated_subject() {
        let subject = trellis_runtime_apis::apis::trellis_auth_v1::events::DeviceUserAuthoritiesApproved::SUBJECT
            .replace("{/deploymentId}", "deployment-a");
        assert_eq!(
            resolve_event_identity(&subject),
            (
                Some("trellis.auth@v1".to_owned()),
                Some("DeviceUserAuthorities.Approved".to_owned())
            )
        );
    }
}
