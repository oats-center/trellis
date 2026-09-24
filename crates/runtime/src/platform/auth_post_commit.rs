use async_nats::jetstream;
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::client::SessionAuth;

use super::auth::resources::{DestroyResourcePayload, ReconcileResourcePayload};
use super::auth::{
    validate_connection_kick_response, AuthConnectionPresence, AuthEphemeralRepository,
    AuthorityEvidenceRepository, AuthorizationContextService, AuthorizationStateError,
    NatsAuthEphemeralRepository, OutboxRepository, PostCommitActionKind, PostCommitActionRecord,
    SessionRepository, SqliteAuthorizationStore,
};
use crate::shutdown::StopHandle;
use crate::supervisor::RuntimeError;

const CLAIM_DURATION_MS: i64 = 30_000;
const IDLE_POLL_MS: u64 = 250;
const BATCH_SIZE: usize = 32;

pub(crate) struct AuthPostCommitRuntime {
    repository: SqliteAuthorizationStore,
    ephemeral: NatsAuthEphemeralRepository,
    auth_client: async_nats::Client,
    system_client: async_nats::Client,
    event_publisher: tokio::sync::Mutex<AuthEventPublisher>,
    contexts: AuthorizationContextService,
}

pub(crate) struct AuthEventPublisher {
    session: SessionAuth,
    identity_key_id: String,
    connection_id: String,
    context_digest: String,
}

impl AuthEventPublisher {
    pub(crate) fn new(
        session: SessionAuth,
        identity_key_id: String,
        connection_id: String,
        context_digest: String,
    ) -> Self {
        Self {
            session,
            identity_key_id,
            connection_id,
            context_digest,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventDelivery {
    subject: String,
    descriptor_identity: String,
    event_time: String,
    context_digest: String,
    session_key: String,
    proof: String,
}

impl AuthPostCommitRuntime {
    pub(crate) fn new(
        repository: SqliteAuthorizationStore,
        ephemeral: NatsAuthEphemeralRepository,
        auth_client: async_nats::Client,
        system_client: async_nats::Client,
        event_publisher: AuthEventPublisher,
        contexts: AuthorizationContextService,
    ) -> Self {
        Self {
            repository,
            ephemeral,
            auth_client,
            system_client,
            event_publisher: tokio::sync::Mutex::new(event_publisher),
            contexts,
        }
    }

    pub(crate) async fn run(self, stop: StopHandle) -> Result<(), RuntimeError> {
        loop {
            let dispatched = tokio::select! {
                () = stop.stopped() => return Ok(()),
                result = self.dispatch_ready() => result
                    .map_err(|error| RuntimeError::Platform(error.to_string()))?,
            };
            if dispatched == 0 {
                tokio::select! {
                    () = stop.stopped() => return Ok(()),
                    () = tokio::time::sleep(std::time::Duration::from_millis(IDLE_POLL_MS)) => {}
                }
            }
        }
    }

    pub(crate) async fn dispatch_ready(&self) -> Result<usize, AuthorizationStateError> {
        let now = now_millis()?;
        let actions = self
            .repository
            .list_ready_post_commit_actions(now, BATCH_SIZE)
            .await?;
        let action_count = actions.len();
        let mut dispatches = stream::iter(actions)
            .map(|action| self.dispatch_action(action, now))
            .buffer_unordered(16);
        let mut first_error = None;
        while let Some(result) = dispatches.next().await {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(action_count)
    }

    async fn dispatch_action(
        &self,
        action: PostCommitActionRecord,
        now: i64,
    ) -> Result<(), AuthorizationStateError> {
        let claimed_until = now.saturating_add(CLAIM_DURATION_MS);
        let Some(claim) = self
            .repository
            .claim_post_commit_action(&action.action_id, now, claimed_until)
            .await?
        else {
            return Ok(());
        };
        // One claimed action execution through its persisted acknowledge or
        // failure result.
        let observation = trellis_rs::telemetry::lifecycle::Observation::start(
            trellis_rs::telemetry::instruments::DurationFamily::AuthPostCommit,
            Vec::new(),
            "cancelled",
        );
        let result = match self.dispatch(&claim.action, &claim.token).await {
            Ok(()) => {
                let acknowledged = self
                    .repository
                    .acknowledge_post_commit_action(&claim.action.action_id, &claim.token)
                    .await;
                observation.finish(if acknowledged.is_ok() { "ok" } else { "error" });
                acknowledged
            }
            Err(error) => {
                observation.finish("retry");
                Err(error)
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(
                    action_id = %claim.action.action_id,
                    action_kind = ?claim.action.kind,
                    attempts = claim.action.attempts,
                    error = %error,
                    "Auth post-commit action failed; scheduling retry"
                );
                let delay = retry_delay_ms(claim.action.attempts);
                self.repository
                    .fail_post_commit_action(
                        &claim.action.action_id,
                        &claim.token,
                        now.saturating_add(delay),
                        error.to_string(),
                    )
                    .await
                    .map(|_| ())
            }
        }
    }

    async fn dispatch(
        &self,
        action: &PostCommitActionRecord,
        claim_token: &str,
    ) -> Result<(), AuthorizationStateError> {
        match action.kind {
            PostCommitActionKind::Event => self.publish_event(action, claim_token).await,
            PostCommitActionKind::Kick => self.kick(action).await,
            PostCommitActionKind::ContextPublish => {
                self.dispatch_context(&action.payload, false).await
            }
            PostCommitActionKind::ContextRevoke => {
                self.dispatch_context(&action.payload, true).await
            }
            PostCommitActionKind::ResourceReconcile => {
                if action.payload.get("bindingRevision").is_some() {
                    let payload =
                        serde_json::from_value::<ReconcileResourcePayload>(action.payload.clone())
                            .map_err(|error| {
                                AuthorizationStateError::InvalidRecord(error.to_string())
                            })?;
                    super::auth::resources::reconcile_resource(
                        &self.auth_client,
                        &self.repository,
                        payload,
                        now_millis()?,
                    )
                    .await
                } else {
                    let payload =
                        serde_json::from_value::<DestroyResourcePayload>(action.payload.clone())
                            .map_err(|error| {
                                AuthorizationStateError::InvalidRecord(error.to_string())
                            })?;
                    super::auth::resources::destroy_resource(
                        &self.auth_client,
                        &self.repository,
                        payload,
                    )
                    .await
                }
            }
        }
    }

    async fn dispatch_context(
        &self,
        payload: &Value,
        revocation: bool,
    ) -> Result<(), AuthorizationStateError> {
        let digest = payload
            .get("contextDigest")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "context post-commit digest is required".to_owned(),
                )
            })?;
        self.contexts
            .dispatch_registry_action(digest, revocation, now_millis()? / 1_000)
            .await?;
        if revocation {
            let connections = self
                .ephemeral
                .list_connection_presence_by_context(digest)
                .await?;
            tracing::info!(
                context_digest = digest,
                physical_connection_count = connections.len(),
                "enumerated authoritative physical connections for revoked context"
            );
            for connection in connections {
                self.kick_connection(&connection).await?;
            }
        }
        tracing::debug!(
            context_digest = digest,
            revocation,
            "published authorization context registry action"
        );
        Ok(())
    }

    async fn publish_event(
        &self,
        action: &PostCommitActionRecord,
        claim_token: &str,
    ) -> Result<(), AuthorizationStateError> {
        let payload = &action.payload;
        let event_type = payload
            .get("eventType")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "post-commit eventType is required".to_owned(),
                )
            })?;
        let event_subject = payload
            .get("eventSubject")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord(
                    "post-commit eventSubject is required".to_owned(),
                )
            })?;
        let event_name = event_type.strip_prefix("Auth.").ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("post-commit eventType is invalid".to_owned())
        })?;
        let event_base = trellis_protocol::derive_event_subject("trellis.auth@v1", event_name)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let suffix = event_subject.strip_prefix(&event_base).ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(
                "post-commit eventSubject does not match eventType".to_owned(),
            )
        })?;
        let parameter_count = if suffix.is_empty() {
            0
        } else {
            suffix
                .strip_prefix('.')
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    AuthorizationStateError::InvalidRecord(
                        "post-commit eventSubject has an invalid parameter suffix".to_owned(),
                    )
                })?
                .split('.')
                .count()
        };
        let descriptor_identity = trellis_protocol::encode_event_descriptor_identity(
            "trellis.auth@v1",
            event_name,
            parameter_count,
        )
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let mut payload = payload.clone();
        let payload = payload.as_object_mut().ok_or_else(|| {
            AuthorizationStateError::InvalidRecord(
                "post-commit event payload must be an object".to_owned(),
            )
        })?;
        payload.remove("eventType");
        payload.remove("eventSubject");
        let event_id = payload
            .get("eventId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                AuthorizationStateError::InvalidRecord("post-commit eventId is required".to_owned())
            })?;
        if payload
            .get("occurredAt")
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str()?.parse::<i64>().ok())
            })
            .is_none()
        {
            return Err(AuthorizationStateError::InvalidRecord(
                "post-commit occurredAt is required".to_owned(),
            ));
        }
        let payload = Bytes::from(
            trellis_protocol::canonicalize_json(&Value::Object(payload.clone()))
                .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
        );
        let now = now_millis()?;
        let event_time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(now) * 1_000_000)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?
            .format(&Rfc3339)
            .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        let mut publisher = self.event_publisher.lock().await;
        let context_status = self
            .contexts
            .require_current_context(
                &publisher.connection_id,
                &publisher.context_digest,
                now.div_euclid(1_000),
            )
            .await;
        if let Err(error) = context_status {
            if error != AuthorizationStateError::AuthorityStale {
                return Err(error);
            }
            let request_id = ulid::Ulid::new().to_string();
            let issued = self
                .contexts
                .issue(
                    super::auth::context::AuthorizationContextIssueRequest {
                        connection: super::auth::IssuanceConnection {
                            credential: super::auth::IssuanceCredential::Native(
                                publisher.identity_key_id.clone(),
                            ),
                            connection_id: publisher.connection_id.clone(),
                            session_public_key: publisher.session.session_key.clone(),
                        },
                        request_id: request_id.clone(),
                        request_digest: trellis_protocol::digest_json(&json!({
                            "purpose": "auth.event_session.context",
                            "requestId": request_id,
                            "sessionKey": publisher.session.session_key,
                        }))
                        .map_err(|error| {
                            AuthorizationStateError::InvalidRecord(error.to_string())
                        })?,
                    },
                    now.div_euclid(1_000),
                )
                .await?;
            publisher.context_digest =
                trellis_protocol::parse_authorization_context(&issued.context)
                    .and_then(|context| context.digest())
                    .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        }
        let candidate = EventDelivery {
            subject: event_subject.clone(),
            descriptor_identity: descriptor_identity.clone(),
            event_time: event_time.clone(),
            context_digest: publisher.context_digest.clone(),
            session_key: publisher.session.session_key.clone(),
            proof: publisher
                .session
                .create_event_proof(
                    &publisher.context_digest,
                    &descriptor_identity,
                    &event_subject,
                    &payload,
                    &event_id,
                    &event_time,
                )
                .map(|proof| proof.as_str().to_owned())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        };
        drop(publisher);
        let delivery: EventDelivery = serde_json::from_value(
            self.repository
                .prepare_post_commit_event_delivery(
                    &action.action_id,
                    claim_token,
                    serde_json::to_value(candidate)
                        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
                )
                .await?,
        )
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        if delivery.subject != event_subject {
            return Err(AuthorizationStateError::StorageConflict);
        }
        if delivery.descriptor_identity != descriptor_identity {
            return Err(AuthorizationStateError::StorageConflict);
        }
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", event_id);
        headers.insert("Trellis-Event-Time", delivery.event_time.as_str());
        headers.insert(
            "Trellis-Event-Descriptor",
            delivery.descriptor_identity.as_str(),
        );
        headers.insert("authorization-context", delivery.context_digest.as_str());
        headers.insert("session-key", delivery.session_key.as_str());
        headers.insert("proof", delivery.proof.as_str());
        let ack = jetstream::new(self.auth_client.clone())
            .publish_with_headers(delivery.subject, headers, payload)
            .await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        ack.await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        Ok(())
    }

    async fn kick(&self, action: &PostCommitActionRecord) -> Result<(), AuthorizationStateError> {
        let payload = &action.payload;
        let connections = if let Some(connections) = payload.get("connections") {
            serde_json::from_value::<Vec<super::auth::AuthConnectionPresence>>(connections.clone())
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?
        } else if let Some(session_id) = payload.get("sessionId").and_then(Value::as_str) {
            self.ephemeral
                .list_connection_presence(Some(session_id))
                .await?
        } else if let Some(connection_id) = payload.get("connectionId").and_then(Value::as_str) {
            self.ephemeral
                .list_connection_presence(None)
                .await?
                .into_iter()
                .filter(|connection| connection.connection_id == connection_id)
                .collect()
        } else if let Some(principal_id) = payload.get("principalId").and_then(Value::as_str) {
            let mut connections = Vec::new();
            for session in self
                .repository
                .list_sessions()
                .await?
                .into_iter()
                .filter(|session| {
                    session.principal_id == principal_id
                        && payload
                            .get("exceptSessionId")
                            .and_then(Value::as_str)
                            .is_none_or(|except| session.session_id != except)
                })
            {
                connections.extend(
                    self.ephemeral
                        .list_connection_presence(Some(&session.session_id))
                        .await?,
                );
            }
            connections
        } else if let Some(deployment_id) = payload.get("deploymentId").and_then(Value::as_str) {
            let mut connections = Vec::new();
            for session in self.repository.list_sessions().await? {
                if self
                    .repository
                    .get_session_runtime_binding(&session.session_id)
                    .await?
                    .is_some_and(|binding| binding.deployment_id == deployment_id)
                {
                    connections.extend(
                        self.ephemeral
                            .list_connection_presence(Some(&session.session_id))
                            .await?,
                    );
                }
            }
            connections
        } else {
            return Err(AuthorizationStateError::InvalidRecord(
                "post-commit kick target is required".to_owned(),
            ));
        };
        for connection in connections {
            let mut event = super::auth::connection_event_action::<
                trellis_runtime_apis::apis::trellis_auth_v1::events::ConnectionsKicked,
            >(
                &connection,
                "Auth.Connections.Kicked",
                "kicked",
                payload.get("reason").and_then(Value::as_str),
                now_millis()?,
            )?;
            event.predecessor_action_id = Some(action.action_id.clone());
            self.repository
                .enqueue_post_commit_actions(vec![event])
                .await?;
            self.kick_connection(&connection).await?;
        }
        Ok(())
    }

    async fn kick_connection(
        &self,
        connection: &AuthConnectionPresence,
    ) -> Result<(), AuthorizationStateError> {
        let client_id = connection
            .client_id
            .parse::<u64>()
            .map_err(|_| AuthorizationStateError::InvalidRecord("invalid client id".to_owned()))?;
        let response = self
            .system_client
            .request(
                format!("$SYS.REQ.SERVER.{}.KICK", connection.server_id),
                Bytes::from(
                    serde_json::to_vec(&serde_json::json!({ "cid": client_id }))
                        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?,
                ),
            )
            .await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
        let outcome = validate_connection_kick_response(&response.payload, &connection.server_id)?;
        tracing::info!(
            context_digest = %connection.context_digest,
            runtime_connection_id = %connection.runtime_connection_id,
            server_id = %connection.server_id,
            client_id = %connection.client_id,
            presence_revision = connection.storage_revision,
            ?outcome,
            "processed authorization connection kick"
        );
        self.ephemeral
            .delete_connection_presence(&connection.connection_id, connection.storage_revision)
            .await
    }
}

fn retry_delay_ms(attempts: u32) -> i64 {
    1_000_i64.saturating_mul(1_i64 << attempts.min(6))
}

fn now_millis() -> Result<i64, AuthorizationStateError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| AuthorizationStateError::Storage("current time overflow".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::retry_delay_ms;
    use crate::platform::auth::resources::{DestroyResourcePayload, ReconcileResourcePayload};

    #[test]
    fn post_commit_retry_is_bounded() {
        assert_eq!(retry_delay_ms(0), 1_000);
        assert_eq!(retry_delay_ms(6), 64_000);
        assert_eq!(retry_delay_ms(u32::MAX), 64_000);
    }

    #[test]
    fn resource_post_commit_payloads_select_one_lifecycle_operation() {
        let reconcile = serde_json::json!({
            "resourceId": "A".repeat(43),
            "bindingRevision": 2,
            "catalogRevision": 3,
        });
        assert!(reconcile.get("bindingRevision").is_some());
        serde_json::from_value::<ReconcileResourcePayload>(reconcile).unwrap();

        let destroy = serde_json::json!({
            "resourceId": "A".repeat(43),
            "catalogRevision": 4,
        });
        assert!(destroy.get("bindingRevision").is_none());
        serde_json::from_value::<DestroyResourcePayload>(destroy).unwrap();
    }
}
