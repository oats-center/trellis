//! Rust-owned participant health projection and API subsystem.

mod query;
mod store;

use std::collections::HashSet;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, AckKind};
use bytes::Bytes;
use futures_util::{stream as futures_stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use trellis_rs::client::{EventDescriptor, PreparedTrellisEvent};
use trellis_rs::service::{
    internal::run_builtin_authenticated_router, DeclaredRpcError, Router, ServerError,
};
use trellis_runtime_apis::__types::trellis::HealthHeartbeatSample;
use trellis_runtime_apis::apis::trellis_health_v1::events::Heartbeat;
use trellis_runtime_apis::apis::trellis_health_v1::events::StatusChanged as HealthStatusChangedEvent;
use trellis_runtime_apis::apis::trellis_health_v1::lives::Watch as HealthWatchLiveDescriptor;
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
pub(crate) const HEALTH_SUBJECT: &str = Heartbeat::SUBJECT;
const INVALIDATION_PREFIX: &str = "health.v1.invalidation";
pub(crate) const DEFAULT_TRANSPORT_RETENTION_HOURS: u64 = 24;
pub(crate) const DEFAULT_TRANSPORT_MAX_BYTES: i64 = 1_073_741_824;
const DEFAULT_HISTORY_RETENTION_DAYS: i64 = 30;

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
    let nats = context.trellis_nats.clone();
    let history_days = context
        .config
        .health
        .as_ref()
        .and_then(|health| health.history_retention_days)
        .map(i64::from)
        .unwrap_or(DEFAULT_HISTORY_RETENTION_DAYS);
    let live_owner = context
        .live_providers
        .receiver(crate::platform::LiveProviderRole::Health);
    let mut event_owner = live_owner.clone();
    let join = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let invalidation_loop = run_invalidation_subscriber(
            nats.clone(),
            invalidation_subject.clone(),
            invalidation_tx,
        );
        let owner_loop = async {
            let Some(event_owner) =
                crate::platform::await_live_owner(&mut event_owner, &task_stop).await
            else {
                return Ok(());
            };
            run_owner(
                nats.clone(),
                jetstream,
                owner,
                store,
                OwnerConfig {
                    projection_id,
                    invalidation_subject,
                    history_days,
                },
                event_owner,
                task_stop.clone(),
            )
            .await
        };
        let api_stop = task_stop.clone();
        let api_loop = async move {
            let mut live_owner = live_owner;
            let Some(owner) = crate::platform::await_live_owner(&mut live_owner, &api_stop).await
            else {
                return Ok(());
            };
            let mut router = router;
            router.set_live_owner(owner.clone());
            let verifier = owner.runtime_validator("trellis.health@v1");
            run_builtin_authenticated_router(owner, router, verifier).await
        };
        tokio::pin!(invalidation_loop, owner_loop, api_loop);
        let result = {
            tokio::select! {
                result = &mut invalidation_loop => result,
                result = &mut owner_loop => result,
                result = &mut api_loop => result.map_err(|error| RuntimeError::Health(error.to_string())),
                () = task_stop.stopped() => Ok(()),
            }
        };
        task_stop.stop();
        result
    }));

    Ok(SubsystemHandle {
        name: SubsystemName::Health,
        stop,
        join,
    })
}

fn build_router(store: HealthStore, invalidations: broadcast::Sender<Invalidation>) -> Router {
    let mut router = Router::new();
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
    let live_store = store.clone();
    router.register_live::<HealthWatchLiveDescriptor, _, _>(move |_context, input| {
        let store = live_store.clone();
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
    event_owner: trellis_rs::service::LiveProviderOwner,
    stop: StopHandle,
) -> Result<(), RuntimeError> {
    tracing::debug!(
        owner_group = ?owner.group,
        lease_key = owner.key.as_str(),
        fence = owner.fence.acquisition_revision(),
        "starting health owner loop"
    );
    // Telemetry samplers own their tasks and never own business lifetime.
    let _samplers = crate::telemetry::snapshots::SamplerOwner::start(vec![Box::pin(
        crate::telemetry::snapshots::run_health_sampler(
            nats.clone(),
            HEALTH_STREAM.to_string(),
            config.projection_id.clone(),
            stop.clone(),
        ),
    )]);
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
                project_message(&nats, &store, &config.invalidation_subject, &event_owner, message).await?;
            }
            _ = deadlines.tick() => {
                if let Some(commit) = store.expire_due(now_ns()).map_err(map_runtime_store_error)? {
                    publish_invalidation(&nats, &config.invalidation_subject, commit).await?;
                }
            }
            _ = outbox.tick() => publish_outbox(
                &store,
                &event_owner,
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
            () = stop.stopped() => {
                _samplers.stop().await;
                return Ok(());
            }
        }
    }
}

async fn project_message(
    nats: &async_nats::Client,
    store: &HealthStore,
    invalidation_subject: &str,
    verifier: &trellis_rs::service::LiveProviderOwner,
    message: jetstream::Message,
) -> Result<(), RuntimeError> {
    let info = message
        .info()
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
    let observed_at_ns = i64::try_from(info.published.unix_timestamp_nanos()).map_err(|_| {
        RuntimeError::Health("health message timestamp is out of range".to_string())
    })?;
    let stream_sequence = info.stream_sequence;
    let original = trellis_protocol::OriginalEventPublication {
        published_at: info
            .published
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|error| RuntimeError::Health(error.to_string()))?,
        stream_sequence: trellis_protocol::U64s::new(stream_sequence),
    };
    let owner = verifier.runtime_authorization_owner();
    let transport = verifier.runtime_nats();
    let verified = async {
        let header = |name: &str| {
            message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(name))
                .map(|value| value.as_str())
                .ok_or_else(|| {
                    trellis_rs::client::TrellisClientError::Bootstrap(
                        "missing health event proof".into(),
                    )
                })
        };
        let event: trellis_protocol::SessionEvent =
            serde_json::from_str(header("Trellis-Session-Event")?)?;
        let proof =
            trellis_protocol::SessionEventProof::parse(header("proof")?).map_err(|error| {
                trellis_rs::client::TrellisClientError::Bootstrap(error.to_string())
            })?;
        if event.subject != message.subject.as_str()
            || event.api_id != Heartbeat::API_ID
            || event.action != "Heartbeat"
        {
            return Err(trellis_rs::client::TrellisClientError::Bootstrap(
                "health event descriptor mismatch".into(),
            ));
        }
        let action = trellis_protocol::CatalogActionIdentity {
            kind: trellis_protocol::ApiSurfaceKind::Event,
            name: "Heartbeat".into(),
            direction: trellis_protocol::PermissionAction::Publish,
        };
        let status = owner
            .historical_status(&transport, &event.authority_digest)
            .await?;
        owner
            .verify_historical_event(
                &transport,
                trellis_rs::client::HistoricalEventVerificationInput {
                    event: &event,
                    payload: &message.payload,
                    proof: &proof,
                    action: &action,
                    original_publication: &original,
                    status: &status,
                },
            )
            .await
    }
    .await;
    let result = match verified {
        Ok(verified) => parse_heartbeat(verified.authority().authority(), &message.payload),
        Err(
            trellis_rs::client::TrellisClientError::AuthorizationUnavailable(_)
            | trellis_rs::client::TrellisClientError::Timeout
            | trellis_rs::client::TrellisClientError::NatsRequest(_),
        ) => {
            message
                .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                .await
                .map_err(|error| RuntimeError::Health(error.to_string()))?;
            return Ok(());
        }
        Err(_) => Err("health publication authentication rejected".into()),
    };
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
    authority: &trellis_protocol::UnsignedSessionAuthority,
    payload: &[u8],
) -> Result<(HeartbeatIdentity, HealthHeartbeatSample), String> {
    if payload.len() > 65_536 {
        return Err("payload exceeds 64 KiB".to_string());
    }
    let (kind, participant_id, deployment_id, instance_id) = match &authority.binding {
        trellis_protocol::SessionBinding::Service {
            participant_id,
            deployment_id,
            instance_id,
        } if authority.principal_kind == trellis_protocol::PrincipalKind::Service => {
            ("service", participant_id, deployment_id, instance_id)
        }
        trellis_protocol::SessionBinding::Device {
            participant_id,
            deployment_id,
            instance_id,
        } if authority.principal_kind == trellis_protocol::PrincipalKind::Device => {
            ("device", participant_id, deployment_id, instance_id)
        }
        _ => return Err("health publisher is not a provisioned native identity".into()),
    };
    let metadata = authority
        .extensions
        .get("trellis.nativeParticipant.v1")
        .filter(|metadata| metadata["participantId"].as_str() == Some(participant_id.as_str()))
        .ok_or("missing signed native participant identity")?;
    let digest = metadata["participantDigest"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("missing signed participant digest")?;
    let identity = HeartbeatIdentity {
        participant_kind: kind.into(),
        contract_id: participant_id.clone(),
        contract_digest: digest.into(),
        deployment_id: deployment_id.clone(),
        instance_id: instance_id.clone(),
        session_key: authority.session_public_key.clone(),
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
    store: &HealthStore,
    owner: &trellis_rs::service::LiveProviderOwner,
) -> Result<(), RuntimeError> {
    for transition in store
        .pending_transitions(100)
        .map_err(map_runtime_store_error)?
    {
        let event = PreparedTrellisEvent::restore(
            HealthStatusChangedEvent::SUBJECT.to_string(),
            Bytes::from(transition.payload),
            HealthStatusChangedEvent::descriptor_identity()
                .map_err(|error| RuntimeError::Health(error.to_string()))?,
            transition.event_id.clone(),
            store::rfc3339(transition.created_at_ns).map_err(map_runtime_store_error)?,
        )
        .map_err(|error| RuntimeError::Health(error.to_string()))?;
        match owner.publish_prepared(&event).await {
            Ok(()) => store
                .mark_transition_published(&transition.event_id, now_ns())
                .map_err(map_runtime_store_error)?,
            Err(error) => store
                .mark_transition_failed(&transition.event_id, &error.to_string())
                .map_err(map_runtime_store_error)?,
        }
    }
    Ok(())
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

fn current_revision(store: &HealthStore) -> Result<i64, RuntimeError> {
    let response = store
        .summary(
            &HealthSummaryRequest {
                contract_ids: None,
                deployment_ids: None,
                participant_kinds: None,
                search: None,
                statuses: None,
                extra: Default::default(),
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
