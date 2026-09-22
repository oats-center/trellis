//! Rust-owned participant health projection and API subsystem.

mod query;
mod store;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, AckKind};
use async_nats::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use bytes::Bytes;
use futures_util::{stream as futures_stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use trellis_rs::client::SessionAuth;
use trellis_rs::service::{
    internal::run_builtin_authenticated_router, DeclaredRpcError, Router, ServerError,
};
use trellis_runtime_apis::__types::trellis::HealthHeartbeatSample;
use trellis_runtime_apis::apis::trellis_health_v1::events::StatusChanged as HealthStatusChangedEvent;
use trellis_runtime_apis::apis::trellis_health_v1::feeds::Watch as HealthWatchFeedDescriptor;
use trellis_runtime_apis::apis::trellis_health_v1::rpc::{
    Inspect as HealthInspectRpc, Metrics as HealthMetricsRpc, Query as HealthQueryRpc,
    Summary as HealthSummaryRpc,
};
use trellis_runtime_apis::types::{
    Bytes as WireBytes, HealthSummaryRequest, HealthWatchFrame,
    HealthWatchRequest as HealthWatchInput,
};
use ulid::Ulid;

use crate::ownership::OwnerGroup;
use crate::shutdown::StopHandle;
use crate::supervisor::{RuntimeContext, RuntimeError, SubsystemHandle};
use crate::SubsystemName;

use self::store::{HealthStore, HeartbeatIdentity, ProjectionCommit};

pub(crate) const HEALTH_STREAM: &str = "TRELLIS_HEALTH";
pub(crate) const HEALTH_SUBJECT: &str = "health.v1.heartbeat.>";
const INVALIDATION_PREFIX: &str = "health.v1.invalidation";
const EVENT_TIME_HEADER: &str = "Trellis-Event-Time";
pub(crate) const DEFAULT_TRANSPORT_RETENTION_HOURS: u64 = 24;
pub(crate) const DEFAULT_TRANSPORT_MAX_BYTES: i64 = 1_073_741_824;
const DEFAULT_HISTORY_RETENTION_DAYS: i64 = 30;
const RPC_SUBJECTS: &[&str] = &[
    "rpc.v1.Health.Query",
    "rpc.v1.Health.Inspect",
    "rpc.v1.Health.Metrics",
    "rpc.v1.Health.Summary",
    "feed.v1.Health.Watch",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Invalidation {
    projection_revision: i64,
    changes: Vec<InvalidationChange>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvalidationChange {
    participant_kind: String,
    contract_id: String,
    deployment_id: String,
    instance_id: String,
}

struct OwnerConfig {
    projection_id: String,
    invalidation_subject: String,
    history_days: i64,
}

impl From<ProjectionCommit> for Invalidation {
    fn from(commit: ProjectionCommit) -> Self {
        Self {
            projection_revision: commit.revision,
            changes: commit
                .changes
                .into_iter()
                .map(|change| InvalidationChange {
                    participant_kind: change.participant_kind,
                    contract_id: change.contract_id,
                    deployment_id: change.deployment_id,
                    instance_id: change.instance_id,
                })
                .collect(),
        }
    }
}

pub(crate) async fn start(context: &RuntimeContext) -> Result<SubsystemHandle, RuntimeError> {
    let store = HealthStore::new(context.stores.health()?.open()?)
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let projection_id = store
        .projection_id()
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let jetstream = jetstream::new(context.trellis_nats.clone());
    let owner = context.owner(OwnerGroup::Health)?;
    let invalidation_subject = format!("{INVALIDATION_PREFIX}.{projection_id}");
    let (invalidation_tx, _) = broadcast::channel(256);
    let router = build_router(store.clone(), invalidation_tx.clone());
    let stop = StopHandle::new();
    let task_stop = stop.clone();
    let mut validator_join =
        crate::platform::auth::verifier::ensure_read_only(context, task_stop.clone()).await?;
    let nats = context.trellis_nats.clone();
    let history_days = context
        .config
        .health
        .as_ref()
        .and_then(|health| health.history_retention_days)
        .map(i64::from)
        .unwrap_or(DEFAULT_HISTORY_RETENTION_DAYS);
    let (event_auth, event_context_digest) = load_event_auth(&context.config)?;
    let event_auth = (Arc::new(event_auth), event_context_digest);
    let verifier: std::sync::Arc<dyn trellis_rs::service::RequestValidator> =
        match context.platform_verifier.get() {
            Some(verifier) => std::sync::Arc::new(verifier.clone()),
            None => std::sync::Arc::new(crate::platform::auth::verifier::DenyAllValidator),
        };
    let join = tokio::spawn(async move {
        let invalidation_loop = run_invalidation_subscriber(
            nats.clone(),
            invalidation_subject.clone(),
            invalidation_tx,
        );
        let owner_loop = run_owner(
            nats.clone(),
            jetstream,
            owner,
            store,
            OwnerConfig {
                projection_id,
                invalidation_subject,
                history_days,
            },
            event_auth,
            task_stop.clone(),
        );
        let api_loop = run_builtin_authenticated_router(
            nats,
            "trellis.health@v1",
            RPC_SUBJECTS,
            router,
            verifier,
        );
        tokio::pin!(invalidation_loop, owner_loop, api_loop);
        let result = {
            let validator_exit = async {
                match validator_join.as_mut() {
                    Some(join) => match join.await {
                        Ok(Ok(())) => Err(RuntimeError::Platform(
                            "authorization validator cache exited unexpectedly".to_owned(),
                        )),
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(RuntimeError::Platform(format!(
                            "authorization validator cache task failed: {error}"
                        ))),
                    },
                    None => std::future::pending().await,
                }
            };
            tokio::pin!(validator_exit);
            tokio::select! {
                result = &mut invalidation_loop => result,
                result = &mut owner_loop => result,
                result = &mut api_loop => result.map_err(|error| RuntimeError::Health(error.to_string())),
                result = &mut validator_exit => result,
                () = task_stop.stopped() => Ok(()),
            }
        };
        task_stop.stop();
        if let Some(join) = validator_join {
            let _ = join.await;
        }
        result
    });

    Ok(SubsystemHandle {
        name: SubsystemName::Health,
        stop,
        join,
    })
}

fn build_router(store: HealthStore, invalidations: broadcast::Sender<Invalidation>) -> Router {
    let mut router = Router::new();
    router.set_provider_deployment_id("dep_trellis_health_runtime");
    let query_store = store.clone();
    router.register_rpc::<HealthQueryRpc, _, _>(move |_context, input| {
        let store = query_store.clone();
        async move { store.query(&input, now_ns()).map_err(map_store_error) }
    });
    let summary_store = store.clone();
    router.register_rpc::<HealthSummaryRpc, _, _>(move |_context, input| {
        let store = summary_store.clone();
        async move { store.summary(&input, now_ns()).map_err(map_store_error) }
    });
    let inspect_store = store.clone();
    router.register_rpc::<HealthInspectRpc, _, _>(move |_context, input| {
        let store = inspect_store.clone();
        async move {
            store
                .inspect(&input, now_ns())
                .map_err(map_store_error)?
                .ok_or_else(|| {
                    ServerError::DeclaredRpc(DeclaredRpcError::new(
                        "NotFoundError",
                        "Health participant was not found.",
                        [
                            ("resource", json!("health-participant")),
                            (
                                "id",
                                json!(format!("{}:{}", input.participant_kind, input.contract_id)),
                            ),
                        ],
                    ))
                })
        }
    });
    let metrics_store = store.clone();
    router.register_rpc::<HealthMetricsRpc, _, _>(move |_context, input| {
        let store = metrics_store.clone();
        async move { store.metrics(&input, now_ns()).map_err(map_store_error) }
    });
    let feed_store = store.clone();
    router.register_feed::<HealthWatchFeedDescriptor, _, _>(move |_context, input| {
        let store = feed_store.clone();
        let receiver = invalidations.subscribe();
        let ready = health_watch_frame(json!({
            "type": "ready",
            "projectionRevision": current_revision(&store).unwrap_or_default(),
        }));
        futures_stream::once(async move { ready }).chain(futures_stream::unfold(
            (receiver, input, store),
            |(mut receiver, input, store)| async move {
                loop {
                    match receiver.recv().await {
                        Ok(invalidation) => {
                            let changes = invalidation
                                .changes
                                .iter()
                                .filter(|change| watch_matches(&input, change))
                                .cloned()
                                .collect::<Vec<_>>();
                            if !changes.is_empty() || invalidation.changes.is_empty() {
                                return Some((
                                    health_invalidated_event(
                                        invalidation.projection_revision,
                                        Some(changes),
                                    ),
                                    (receiver, input, store),
                                ));
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            return Some((
                                health_invalidated_event(
                                    current_revision(&store).unwrap_or_default(),
                                    None,
                                ),
                                (receiver, input, store),
                            ));
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        ))
    });
    router
}

async fn run_invalidation_subscriber(
    nats: async_nats::Client,
    subject: String,
    sender: broadcast::Sender<Invalidation>,
) -> Result<(), RuntimeError> {
    let mut subscriber = nats
        .subscribe(subject)
        .await
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    while let Some(message) = subscriber.next().await {
        if let Ok(invalidation) = serde_json::from_slice::<Invalidation>(&message.payload) {
            let _ = sender.send(invalidation);
        }
    }
    Err(RuntimeError::Health(
        "health invalidation subscription ended".to_string(),
    ))
}

async fn run_owner(
    nats: async_nats::Client,
    jetstream: jetstream::Context,
    owner: crate::ownership::OwnerContext,
    store: HealthStore,
    config: OwnerConfig,
    event_auth: (Arc<SessionAuth>, String),
    stop: StopHandle,
) -> Result<(), RuntimeError> {
    tracing::debug!(
        owner_group = ?owner.group,
        lease_key = owner.key.as_str(),
        fence = owner.fence.acquisition_revision(),
        "starting health owner loop"
    );
    let stream = jetstream
        .get_stream(HEALTH_STREAM)
        .await
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let durable = format!("health-projector-{}", config.projection_id).to_lowercase();
    let consumer = stream
        .get_or_create_consumer(
            &durable,
            consumer::pull::Config {
                durable_name: Some(durable.clone()),
                filter_subject: HEALTH_SUBJECT.to_string(),
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let mut deadlines = tokio::time::interval(Duration::from_secs(1));
    let mut outbox = tokio::time::interval(Duration::from_millis(250));
    let mut retention = tokio::time::interval(Duration::from_secs(60 * 60));

    loop {
        tokio::select! {
            message = messages.next() => {
                let Some(message) = message else {
                    return Err(RuntimeError::Health("health projector consumer ended".to_string()));
                };
                let message = message.map_err(|error| RuntimeError::Health(error.to_string()))?;
                project_message(&nats, &store, &config.invalidation_subject, message).await?;
            }
            _ = deadlines.tick() => {
                if let Some(commit) = store.expire_due(now_ns()).map_err(map_runtime_store_error)? {
                    publish_invalidation(&nats, &config.invalidation_subject, commit).await?;
                }
            }
            _ = outbox.tick() => publish_outbox(
                &nats,
                &store,
                &event_auth.0,
                &event_auth.1,
            )
            .await?,
            _ = retention.tick() => {
                let cutoff = now_ns().saturating_sub(
                    config.history_days.saturating_mul(24 * 60 * 60 * 1_000_000_000),
                );
                if let Some(commit) = store.cleanup(cutoff).map_err(map_runtime_store_error)? {
                    publish_invalidation(&nats, &config.invalidation_subject, commit).await?;
                }
            }
            () = stop.stopped() => return Ok(()),
        }
    }
}

async fn project_message(
    nats: &async_nats::Client,
    store: &HealthStore,
    invalidation_subject: &str,
    message: jetstream::Message,
) -> Result<(), RuntimeError> {
    let info = message
        .info()
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let observed_at_ns = i64::try_from(info.published.unix_timestamp_nanos()).map_err(|_| {
        RuntimeError::Health("health message timestamp is out of range".to_string())
    })?;
    let stream_sequence = info.stream_sequence;
    let result = parse_heartbeat(message.subject.as_str(), &message.payload);
    let (identity, sample) = match result {
        Ok(value) => value,
        Err(reason) => {
            store
                .record_rejection(
                    stream_sequence,
                    message.subject.as_str(),
                    observed_at_ns,
                    &reason,
                )
                .map_err(map_runtime_store_error)?;
            message
                .ack_with(AckKind::Term)
                .await
                .map_err(|error| RuntimeError::Health(error.to_string()))?;
            return Ok(());
        }
    };
    let commit = store
        .project_sample(
            &identity,
            &sample,
            observed_at_ns,
            now_ns(),
            stream_sequence,
        )
        .map_err(map_runtime_store_error)?;
    message
        .ack()
        .await
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    if let Some(commit) = commit {
        publish_invalidation(nats, invalidation_subject, commit).await?;
    }
    Ok(())
}

fn parse_heartbeat(
    subject: &str,
    payload: &[u8],
) -> Result<(HeartbeatIdentity, HealthHeartbeatSample), String> {
    if payload.len() > 65_536 {
        return Err("payload exceeds 64 KiB".to_string());
    }
    let parts = subject.split('.').collect::<Vec<_>>();
    if parts.len() != 9 || parts[..3] != ["health", "v1", "heartbeat"] {
        return Err("subject does not match health protocol".to_string());
    }
    let identity = HeartbeatIdentity {
        participant_kind: parts[3].to_string(),
        contract_id: decode_token(parts[4])?,
        contract_digest: decode_token(parts[5])?,
        deployment_id: decode_token(parts[6])?,
        instance_id: decode_token(parts[7])?,
        session_key: parts[8].to_string(),
    };
    if !matches!(identity.participant_kind.as_str(), "service" | "device")
        || identity.session_key.is_empty()
        || !identity
            .session_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err("subject identity is invalid".to_string());
    }
    let sample = serde_json::from_slice::<HealthHeartbeatSample>(payload)
        .map_err(|error| format!("sample JSON is invalid: {error}"))?;
    validate_sample(&identity, &sample)?;
    Ok((identity, sample))
}

fn validate_sample(
    identity: &HeartbeatIdentity,
    sample: &HealthHeartbeatSample,
) -> Result<(), String> {
    if sample.participant.kind.as_str() != identity.participant_kind
        || sample.participant.contract_id.as_ref() != identity.contract_id
        || sample.participant.contract_digest.as_ref() != identity.contract_digest
        || sample.participant.instance_id.as_ref() != identity.instance_id
    {
        return Err("sample identity does not match authorized subject".to_string());
    }
    if !matches!(
        sample.reported_status.as_str(),
        "healthy" | "degraded" | "unhealthy"
    ) || !(1_000..=600_000).contains(&sample.participant.publish_interval_ms.0 .0)
        || sample.checks.len() > 64
        || Ulid::from_string(&sample.sample.id).is_err()
        || time::OffsetDateTime::parse(
            &sample.sample.time,
            &time::format_description::well_known::Rfc3339,
        )
        .is_err()
        || time::OffsetDateTime::parse(
            &sample.participant.started_at,
            &time::format_description::well_known::Rfc3339,
        )
        .is_err()
    {
        return Err("sample fields are invalid".to_string());
    }
    let mut names = HashSet::new();
    for check in &sample.checks {
        if check.name.is_empty()
            || check.name.len() > 128
            || !names.insert(&check.name)
            || !matches!(check.status.as_str(), "ok" | "failed")
            || !check.latency_ms.0 .0.is_finite()
            || !(0.0..=3_600_000.0).contains(&check.latency_ms.0 .0)
        {
            return Err("sample check is invalid".to_string());
        }
    }
    Ok(())
}

async fn publish_outbox(
    nats: &async_nats::Client,
    store: &HealthStore,
    auth: &SessionAuth,
    context_digest: &str,
) -> Result<(), RuntimeError> {
    let jetstream = jetstream::new(nats.clone());
    for transition in store
        .pending_transitions(100)
        .map_err(map_runtime_store_error)?
    {
        let headers = transition_headers(auth, context_digest, &transition)?;
        match jetstream
            .publish_with_headers(
                HealthStatusChangedEvent::SUBJECT.to_string(),
                headers,
                Bytes::from(transition.payload),
            )
            .await
        {
            Ok(ack) => match ack.await {
                Ok(_) => store
                    .mark_transition_published(&transition.event_id, now_ns())
                    .map_err(map_runtime_store_error)?,
                Err(error) => store
                    .mark_transition_failed(&transition.event_id, &error.to_string())
                    .map_err(map_runtime_store_error)?,
            },
            Err(error) => store
                .mark_transition_failed(&transition.event_id, &error.to_string())
                .map_err(map_runtime_store_error)?,
        }
    }
    Ok(())
}

fn transition_headers(
    auth: &SessionAuth,
    context_digest: &str,
    transition: &store::PendingTransition,
) -> Result<HeaderMap, RuntimeError> {
    let event_time = store::rfc3339(transition.created_at_ns).map_err(map_runtime_store_error)?;
    let mut headers = HeaderMap::new();
    headers.insert("Nats-Msg-Id", transition.event_id.as_str());
    headers.insert(EVENT_TIME_HEADER, event_time.as_str());
    let descriptor_identity =
        <HealthStatusChangedEvent as trellis_rs::client::EventDescriptor>::descriptor_identity()
            .map_err(|error| RuntimeError::Health(error.to_string()))?;
    headers.insert("Trellis-Event-Descriptor", descriptor_identity.as_str());
    headers.insert("authorization-context", context_digest);
    headers.insert(
        "proof",
        auth.create_event_proof(
            context_digest,
            &descriptor_identity,
            HealthStatusChangedEvent::SUBJECT,
            &transition.payload,
            &transition.event_id,
            &event_time,
        )
        .map_err(|error| RuntimeError::Health(format!("invalid event proof: {error}")))?
        .as_str(),
    );
    Ok(headers)
}

fn load_event_auth(config: &crate::RuntimeConfig) -> Result<(SessionAuth, String), RuntimeError> {
    let path = config.event_session_seed_file.as_ref().ok_or_else(|| {
        RuntimeError::Health("event_session_seed_file is required for health mode".to_string())
    })?;
    let seed = std::fs::read_to_string(path).map_err(|error| {
        RuntimeError::Health(format!("failed to read '{}': {error}", path.display()))
    })?;
    let auth = SessionAuth::from_seed_base64url(seed.trim())
        .map_err(|error| RuntimeError::Health(format!("invalid event session seed: {error}")))?;
    let digest_path = config.event_context_digest_file.as_ref().ok_or_else(|| {
        RuntimeError::Health("event_context_digest_file is required for health mode".to_string())
    })?;
    let context_digest = std::fs::read_to_string(digest_path).map_err(|error| {
        RuntimeError::Health(format!(
            "failed to read '{}': {error}",
            digest_path.display()
        ))
    })?;
    Ok((auth, context_digest.trim().to_owned()))
}

async fn publish_invalidation(
    nats: &async_nats::Client,
    subject: &str,
    commit: ProjectionCommit,
) -> Result<(), RuntimeError> {
    nats.publish(
        subject.to_string(),
        Bytes::from(
            serde_json::to_vec(&Invalidation::from(commit))
                .map_err(|error| RuntimeError::Health(error.to_string()))?,
        ),
    )
    .await
    .map_err(|error| RuntimeError::Health(error.to_string()))?;
    Ok(())
}

fn watch_matches(input: &HealthWatchInput, change: &InvalidationChange) -> bool {
    matches_filter(input.participant_kinds.as_ref(), &change.participant_kind)
        && matches_filter(input.contract_ids.as_ref(), &change.contract_id)
        && matches_filter(input.deployment_ids.as_ref(), &change.deployment_id)
        && matches_filter(input.instance_ids.as_ref(), &change.instance_id)
}

fn matches_filter<T: AsRef<str>>(filter: Option<&Vec<T>>, value: &str) -> bool {
    filter.is_none_or(|filter| {
        filter.is_empty() || filter.iter().any(|candidate| candidate.as_ref() == value)
    })
}

fn health_invalidated_event(
    projection_revision: i64,
    changes: Option<Vec<InvalidationChange>>,
) -> Result<HealthWatchFrame, ServerError> {
    health_watch_frame(json!({
        "type": "healthInvalidated",
        "projectionRevision": projection_revision,
        "changes": changes,
    }))
}

fn health_watch_frame(value: Value) -> Result<HealthWatchFrame, ServerError> {
    Ok(HealthWatchFrame(WireBytes(serde_json::to_vec(&value)?)))
}

fn decode_token(token: &str) -> Result<String, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| "subject identity token is not base64url".to_string())?;
    String::from_utf8(bytes).map_err(|_| "subject identity token is not UTF-8".to_string())
}

fn current_revision(store: &HealthStore) -> Result<i64, RuntimeError> {
    let response = store
        .summary(
            &HealthSummaryRequest {
                contract_ids: None,
                deployment_ids: None,
                participant_kinds: None,
                search: None,
                statuses: None,
            },
            now_ns(),
        )
        .map_err(map_runtime_store_error)?;
    Ok(response.projection.revision.0 as i64)
}

fn map_store_error(error: store::HealthStoreError) -> ServerError {
    if matches!(error, store::HealthStoreError::InvalidPagination) {
        ServerError::DeclaredRpc(DeclaredRpcError::new(
            "ValidationError",
            error.to_string(),
            std::iter::empty::<(&str, Value)>(),
        ))
    } else {
        ServerError::Nats(error.to_string())
    }
}

fn map_runtime_store_error(error: store::HealthStoreError) -> RuntimeError {
    RuntimeError::Health(error.to_string())
}

fn now_ns() -> i64 {
    i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trellis_rs::client::{verify_event_proof, VerifyEventProofInput};

    #[test]
    fn heartbeat_subject_identity_is_authoritative() {
        let payload = serde_json::to_vec(&json!({
            "sample": {"id": "01J00000000000000000000000", "time": "2026-01-01T00:00:00Z"},
            "participant": {
                "name": "Jobs",
                "kind": "service",
                "instanceId": "rust-1",
                "contractId": "trellis.jobs@v1",
                "contractDigest": "digest-alpha",
                "startedAt": "2026-01-01T00:00:00Z",
                "publishIntervalMs": "30000",
                "runtime": "rust"
            },
            "reportedStatus": "healthy",
            "checks": [{"name": "nats", "status": "ok", "latencyMs": 0}]
        }))
        .expect("serialize sample");
        let (identity, _) = parse_heartbeat(
            "health.v1.heartbeat.service.dHJlbGxpcy5qb2JzQHYx.ZGlnZXN0LWFscGhh.am9icy5kZWZhdWx0.cnVzdC0x.session_key",
            &payload,
        )
        .expect("parse heartbeat");
        assert_eq!(identity.contract_id, "trellis.jobs@v1");
        assert_eq!(identity.deployment_id, "jobs.default");
        assert_eq!(identity.instance_id, "rust-1");
    }

    #[test]
    fn status_transition_headers_are_signed() {
        let auth = SessionAuth::from_seed_base64url("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("session auth");
        let context_digest = "byhVYTUxr4iVywgon-utTJesrl5WZVm1MC0PXqCU06c";
        let transition = store::PendingTransition {
            event_id: "01J00000000000000000000000".to_string(),
            created_at_ns: 1_767_225_600_000_000_000,
            payload: br#"{"status":"offline"}"#.to_vec(),
        };
        let headers =
            transition_headers(&auth, context_digest, &transition).expect("signed headers");
        assert_eq!(
            headers
                .get("authorization-context")
                .expect("authorization-context")
                .as_str(),
            context_digest
        );
        let event_time = headers.get(EVENT_TIME_HEADER).expect("event time").as_str();
        let proof = headers.get("proof").expect("proof").as_str();

        assert!(verify_event_proof(VerifyEventProofInput {
            public_session_key: &auth.session_key,
            context_digest,
            descriptor_identity: headers
                .get("Trellis-Event-Descriptor")
                .expect("event descriptor")
                .as_str(),
            subject: HealthStatusChangedEvent::SUBJECT,
            payload: &transition.payload,
            event_id: &transition.event_id,
            event_time,
            proof_base64url: proof,
        })
        .expect("verify event proof"));
    }
}
