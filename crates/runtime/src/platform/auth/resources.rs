use std::collections::HashMap;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, consumer::FromConsumer, stream};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use trellis_protocol::ParticipantResourceKind;

use super::domain::{
    ApprovedResource, AuthorizationResourceKind, AuthorizationStateError, GrantOwnerKind,
};
use super::evidence::{ParticipantRuntimeProjection, ResourceRuntimeProjection};
use super::sqlite::SqliteAuthorizationStore;

const OPERATION_STORE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const OPERATION_STORE_MAX_VALUE_SIZE: i32 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceCatalogState {
    Detached,
    Pending,
    Ready,
    Failed,
    Destroying,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResourceCatalogRecord {
    pub resource_id: String,
    pub owner_kind: GrantOwnerKind,
    pub owner_id: String,
    pub participant_id: String,
    pub resource_kind: ParticipantResourceKind,
    pub local_name: String,
    pub physical_id: String,
    pub commitment: super::domain::ResourceCommitment,
    pub actual: Option<ResourceActual>,
    pub state: ResourceCatalogState,
    pub readiness_reason: Option<String>,
    pub revision: u64,
    pub binding_revision: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum ResourceActual {
    State,
    Kv {
        history: u64,
        ttl_ms: u64,
        max_value_bytes: Option<u64>,
    },
    Store {
        ttl_ms: u64,
        max_object_bytes: Option<u64>,
        max_total_bytes: Option<u64>,
    },
    Job,
    Consumer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReconcileResourcePayload {
    pub resource_id: String,
    pub binding_revision: u64,
    pub catalog_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DestroyResourcePayload {
    pub resource_id: String,
    pub catalog_revision: u64,
}

pub(crate) struct ResourceReconcileRequest {
    pub catalog: ResourceCatalogRecord,
    pub participant: ParticipantRuntimeProjection,
    pub approved: ApprovedResource,
}

/// Deterministic logical identity over the complete resource identity tuple.
pub(crate) fn resource_id(
    owner_kind: GrantOwnerKind,
    owner_id: &str,
    participant_id: &str,
    kind: ParticipantResourceKind,
    local_name: &str,
) -> String {
    let mut digest = Sha256::new();
    for part in [
        owner_kind_atom(owner_kind).as_bytes(),
        owner_id.as_bytes(),
        participant_id.as_bytes(),
        resource_kind_atom(kind).as_bytes(),
        local_name.as_bytes(),
    ] {
        digest.update((part.len() as u32).to_be_bytes());
        digest.update(part);
    }
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

pub(crate) fn physical_id(kind: ParticipantResourceKind, resource_id: &str) -> String {
    if kind == ParticipantResourceKind::State {
        return resource_id.to_owned();
    }
    let prefix = match kind {
        ParticipantResourceKind::Kv => "tr_kv_",
        ParticipantResourceKind::Store => "tr_store_",
        ParticipantResourceKind::JobQueue => "tr_job_",
        ParticipantResourceKind::EventConsumer => "tr_cons_",
        ParticipantResourceKind::State => unreachable!(),
    };
    format!(
        "{prefix}{}",
        &URL_SAFE_NO_PAD.encode(Sha256::digest(resource_id))[..32]
    )
}

pub(crate) fn job_namespace(resource: &ResourceCatalogRecord) -> String {
    let mut digest = Sha256::new();
    for part in [
        resource.owner_id.as_bytes(),
        resource.participant_id.as_bytes(),
    ] {
        digest.update((part.len() as u32).to_be_bytes());
        digest.update(part);
    }
    format!(
        "tr_jobs_{}",
        &URL_SAFE_NO_PAD.encode(digest.finalize())[..32]
    )
}

pub(crate) fn resource_kind_atom(kind: ParticipantResourceKind) -> &'static str {
    match kind {
        ParticipantResourceKind::State => "state",
        ParticipantResourceKind::Kv => "kv",
        ParticipantResourceKind::Store => "store",
        ParticipantResourceKind::JobQueue => "job",
        ParticipantResourceKind::EventConsumer => "consumer",
    }
}

fn owner_kind_atom(kind: GrantOwnerKind) -> &'static str {
    match kind {
        GrantOwnerKind::Deployment => "deployment",
        GrantOwnerKind::User => "user",
    }
}

pub(crate) async fn reconcile_resource(
    client: &async_nats::Client,
    repository: &SqliteAuthorizationStore,
    payload: ReconcileResourcePayload,
    now: i64,
) -> Result<(), AuthorizationStateError> {
    let Some(request) = repository
        .load_resource_reconcile_request(payload.clone())
        .await?
    else {
        return Ok(());
    };
    let declaration = request
        .participant
        .resources
        .get(&request.catalog.local_name)
        .filter(|declaration| declaration.kind == request.catalog.resource_kind)
        .ok_or_else(|| {
            AuthorizationStateError::InvalidRecord("approved resource is not declared".to_owned())
        })?;
    validate_commitment(declaration, &request.approved)?;

    match reconcile_provider(client, &request).await {
        Ok(actual) => repository.attach_resource(payload, actual, now).await,
        Err(ProvisionError::Incompatible(error)) => {
            repository
                .mark_resource_unavailable(payload, error, now)
                .await
        }
        Err(ProvisionError::Retry(error)) => Err(error),
    }
}

pub(crate) async fn ensure_operation_store(
    client: &async_nats::Client,
    deployment_id: &str,
) -> Result<(), AuthorizationStateError> {
    let bucket = format!("trellis_operations_{deployment_id}");
    let jetstream = async_nats::jetstream::new(client.clone());
    // Opening an existing Trellis-owned store is not a compatibility check: the
    // bucket is created once and reused. Value-size and retention limits are
    // enforced by the store itself at the operation boundary.
    if jetstream.get_key_value(&bucket).await.is_err() {
        jetstream
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket,
                description: "Trellis deployment operation records".to_owned(),
                history: 10,
                storage: async_nats::jetstream::stream::StorageType::File,
                max_age: OPERATION_STORE_MAX_AGE,
                max_value_size: OPERATION_STORE_MAX_VALUE_SIZE,
                ..Default::default()
            })
            .await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    }
    let staging = format!("trellis_operation_staging_{deployment_id}");
    if jetstream.get_object_store(&staging).await.is_err() {
        jetstream
            .create_object_store(async_nats::jetstream::object_store::Config {
                bucket: staging,
                description: Some("Trellis operation upload staging".to_owned()),
                storage: async_nats::jetstream::stream::StorageType::File,
                max_age: std::time::Duration::from_secs(7 * 24 * 60 * 60),
                ..Default::default()
            })
            .await
            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    }
    Ok(())
}

pub(crate) async fn destroy_resource(
    client: &async_nats::Client,
    repository: &SqliteAuthorizationStore,
    payload: DestroyResourcePayload,
) -> Result<(), AuthorizationStateError> {
    let Some(resource) = repository.load_detached_resource(payload.clone()).await? else {
        return Ok(());
    };
    let jetstream = jetstream::new(client.clone());
    let result = match resource.resource_kind {
        ParticipantResourceKind::State => {
            if resource.physical_id == resource.resource_id {
                purge_state(&jetstream, &resource.physical_id)
                    .await
                    .map_err(provision_error)
            } else {
                Err(AuthorizationStateError::InvalidRecord(
                    "State physical identity does not match its catalog resource".to_owned(),
                ))
            }
        }
        ParticipantResourceKind::Kv => delete_owned_stream_if_present(
            &jetstream,
            &format!("KV_{}", resource.physical_id),
            &format!("Trellis auth resource {}", resource.resource_id),
        )
        .await
        .map_err(provision_error),
        ParticipantResourceKind::Store => delete_owned_stream_if_present(
            &jetstream,
            &format!("OBJ_{}", resource.physical_id),
            &format!("Trellis auth resource {}", resource.resource_id),
        )
        .await
        .map_err(provision_error),
        ParticipantResourceKind::JobQueue => {
            validate_owned_consumer_if_present(
                &jetstream,
                "JOBS_WORK",
                &resource.physical_id,
                &resource.resource_id,
            )
            .await?;
            let namespace = job_namespace(&resource);
            let keys_stream = format!("KV_JOBS_KEYS_{namespace}");
            let marker = format!("Trellis auth jobs {}", resource.participant_id);
            validate_owned_stream_if_present(&jetstream, &keys_stream, &marker)
                .await
                .map_err(provision_error)?;
            delete_consumer_if_present(&jetstream, "JOBS_WORK", &resource.physical_id).await?;
            if repository
                .job_namespace_has_other_resources(
                    resource.resource_id.clone(),
                    resource.owner_kind,
                    resource.owner_id.clone(),
                    resource.participant_id.clone(),
                )
                .await?
            {
                if let Some(stream) = get_stream_if_present(&jetstream, &keys_stream)
                    .await
                    .map_err(provision_error)?
                {
                    stream
                        .purge()
                        .filter(format!(
                            "$KV.JOBS_KEYS_{namespace}.{namespace}.{}.>",
                            resource.local_name
                        ))
                        .await
                        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                }
            } else {
                delete_stream_if_present(&jetstream, &keys_stream).await?;
            }
            Ok(())
        }
        ParticipantResourceKind::EventConsumer => {
            let replay_consumer = format!("{}_replay", resource.physical_id);
            validate_owned_consumer_if_present(
                &jetstream,
                "trellis",
                &resource.physical_id,
                &resource.resource_id,
            )
            .await?;
            validate_owned_consumer_if_present(
                &jetstream,
                trellis_events_runtime::REPLAY_STREAM,
                &replay_consumer,
                &resource.resource_id,
            )
            .await?;
            delete_consumer_if_present(&jetstream, "trellis", &resource.physical_id).await?;
            delete_consumer_if_present(
                &jetstream,
                trellis_events_runtime::REPLAY_STREAM,
                &replay_consumer,
            )
            .await
        }
    };
    result?;
    repository.finish_resource_destroy(payload).await
}

async fn purge_state(
    jetstream: &jetstream::Context,
    physical_id: &str,
) -> Result<(), ProvisionError> {
    let Some(mut stream) = get_stream_if_present(jetstream, "KV_trellis_state").await? else {
        return Ok(());
    };
    let config = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .clone();
    if config.description.as_deref() != Some(crate::platform::state::OWNERSHIP_MARKER) {
        return Err(ProvisionError::Incompatible(
            "State provider identity is not Trellis-owned".to_owned(),
        ));
    }
    let store = jetstream
        .get_key_value("trellis_state")
        .await
        .map_err(|error| storage(error.to_string()))?;
    store
        .purge(physical_id)
        .await
        .map_err(|error| storage(error.to_string()))
}

#[derive(Debug)]
enum ProvisionError {
    Incompatible(String),
    Retry(AuthorizationStateError),
}

impl From<AuthorizationStateError> for ProvisionError {
    fn from(error: AuthorizationStateError) -> Self {
        Self::Retry(error)
    }
}

async fn reconcile_provider(
    client: &async_nats::Client,
    request: &ResourceReconcileRequest,
) -> Result<ResourceActual, ProvisionError> {
    let jetstream = jetstream::new(client.clone());
    let commitment = &request.approved.commitment;
    match request.approved.kind {
        AuthorizationResourceKind::State => Ok(ResourceActual::State),
        AuthorizationResourceKind::Kv => {
            reconcile_kv(
                &jetstream,
                &request.catalog,
                commitment.history.unwrap_or(1),
                commitment.ttl_ms.unwrap_or(0),
            )
            .await
        }
        AuthorizationResourceKind::Store => {
            reconcile_store(&jetstream, &request.catalog, commitment.ttl_ms.unwrap_or(0)).await
        }
        AuthorizationResourceKind::Job => {
            let declaration = request
                .participant
                .resources
                .get(&request.catalog.local_name)
                .ok_or_else(|| {
                    ProvisionError::Incompatible("job declaration is missing".to_owned())
                })?;
            reconcile_job(
                client,
                &request.catalog,
                declaration.retry_attempts,
                &declaration.retry_backoff_ms,
            )
            .await?;
            Ok(ResourceActual::Job)
        }
        AuthorizationResourceKind::Consumer => {
            let declaration = request
                .participant
                .resources
                .get(&request.catalog.local_name)
                .ok_or_else(|| {
                    ProvisionError::Incompatible("consumer declaration is missing".to_owned())
                })?;
            reconcile_consumer(
                &jetstream,
                &request.catalog,
                &request.participant,
                declaration,
            )
            .await?;
            Ok(ResourceActual::Consumer)
        }
    }
}

async fn reconcile_kv(
    jetstream: &jetstream::Context,
    catalog: &ResourceCatalogRecord,
    history: u64,
    ttl_ms: u64,
) -> Result<ResourceActual, ProvisionError> {
    if !(1..=64).contains(&history) {
        return Err(ProvisionError::Incompatible(
            "KV history must be between 1 and 64".to_owned(),
        ));
    }
    let stream_name = format!("KV_{}", catalog.physical_id);
    let marker = format!("Trellis auth resource {}", catalog.resource_id);
    let mut stream = match get_stream_if_present(jetstream, &stream_name).await? {
        Some(stream) => stream,
        None => {
            jetstream
                .create_key_value(jetstream::kv::Config {
                    bucket: catalog.physical_id.clone(),
                    description: marker.clone(),
                    history: history as i64,
                    max_age: Duration::from_millis(ttl_ms),
                    ..Default::default()
                })
                .await
                .map_err(|error| storage(error.to_string()))?;
            get_required_stream(jetstream, &stream_name).await?
        }
    };
    let mut config = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .clone();
    if config.description.as_deref() != Some(&marker) {
        return Err(ProvisionError::Incompatible(
            "KV provider identity is not Trellis-owned".to_owned(),
        ));
    }
    let original = config.clone();
    let actual_history = u64::try_from(config.max_messages_per_subject).unwrap_or(0);
    if actual_history > 64 {
        return Err(ProvisionError::Incompatible(
            "KV provider history exceeds 64".to_owned(),
        ));
    }
    if actual_history < history {
        config.max_messages_per_subject = history as i64;
    } else if actual_history > history {
        return Err(ProvisionError::Incompatible(
            "resource_change_requires_admin: KV history reduction is destructive".to_owned(),
        ));
    }
    let requested_ttl = Duration::from_millis(ttl_ms);
    if config.max_age != requested_ttl {
        if config.max_age != Duration::ZERO
            && (requested_ttl == Duration::ZERO || requested_ttl > config.max_age)
        {
            config.max_age = requested_ttl;
        } else {
            return Err(ProvisionError::Incompatible(
                "resource_change_requires_admin: KV TTL would weaken retention".to_owned(),
            ));
        }
    }
    if config != original {
        jetstream
            .update_stream(config)
            .await
            .map_err(|error| storage(error.to_string()))?;
    }
    let mut stream = get_required_stream(jetstream, &stream_name).await?;
    let config = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .clone();
    Ok(ResourceActual::Kv {
        history: u64::try_from(config.max_messages_per_subject).unwrap_or(0),
        ttl_ms: duration_ms(config.max_age)?,
        max_value_bytes: actual_i32(config.max_message_size),
    })
}

async fn reconcile_store(
    jetstream: &jetstream::Context,
    catalog: &ResourceCatalogRecord,
    ttl_ms: u64,
) -> Result<ResourceActual, ProvisionError> {
    let stream_name = format!("OBJ_{}", catalog.physical_id);
    let marker = format!("Trellis auth resource {}", catalog.resource_id);
    let mut stream = match get_stream_if_present(jetstream, &stream_name).await? {
        Some(stream) => stream,
        None => {
            jetstream
                .create_object_store(jetstream::object_store::Config {
                    bucket: catalog.physical_id.clone(),
                    description: Some(marker.clone()),
                    max_age: Duration::from_millis(ttl_ms),
                    ..Default::default()
                })
                .await
                .map_err(|error| storage(error.to_string()))?;
            get_required_stream(jetstream, &stream_name).await?
        }
    };
    let mut config = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .clone();
    if config.description.as_deref() != Some(&marker) {
        return Err(ProvisionError::Incompatible(
            "Store provider identity is not Trellis-owned".to_owned(),
        ));
    }
    let original = config.clone();
    let requested_ttl = Duration::from_millis(ttl_ms);
    if config.max_age != requested_ttl {
        if config.max_age != Duration::ZERO
            && (requested_ttl == Duration::ZERO || requested_ttl > config.max_age)
        {
            config.max_age = requested_ttl;
        } else {
            return Err(ProvisionError::Incompatible(
                "resource_change_requires_admin: Store TTL would weaken retention".to_owned(),
            ));
        }
    }
    if config != original {
        jetstream
            .update_stream(config)
            .await
            .map_err(|error| storage(error.to_string()))?;
    }
    let mut stream = get_required_stream(jetstream, &stream_name).await?;
    let config = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .clone();
    Ok(ResourceActual::Store {
        ttl_ms: duration_ms(config.max_age)?,
        max_object_bytes: None,
        max_total_bytes: actual_i64(config.max_bytes),
    })
}

async fn reconcile_job(
    client: &async_nats::Client,
    catalog: &ResourceCatalogRecord,
    retry_attempts: Option<u32>,
    retry_backoff_ms: &[u64],
) -> Result<(), ProvisionError> {
    let jetstream = jetstream::new(client.clone());
    let namespace = job_namespace(catalog);
    let keys_bucket = format!("JOBS_KEYS_{namespace}");
    let keys_stream = format!("KV_{keys_bucket}");
    let marker = format!("Trellis auth jobs {}", catalog.participant_id);
    let mut stream = match get_stream_if_present(&jetstream, &keys_stream).await? {
        Some(stream) => stream,
        None => {
            jetstream
                .create_key_value(jetstream::kv::Config {
                    bucket: keys_bucket,
                    description: marker.clone(),
                    history: 1,
                    ..Default::default()
                })
                .await
                .map_err(|error| storage(error.to_string()))?;
            get_required_stream(&jetstream, &keys_stream).await?
        }
    };
    let description = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .description
        .clone();
    if description.as_deref() != Some(&marker) {
        return Err(ProvisionError::Incompatible(
            "job key provider identity is not Trellis-owned".to_owned(),
        ));
    }
    let stream = get_required_stream(&jetstream, "JOBS_WORK").await?;
    let max_deliver = i64::from(retry_attempts.unwrap_or(5));
    let backoff = if retry_attempts.is_none() {
        vec![5_000, 30_000, 120_000, 600_000]
    } else {
        retry_backoff_ms.to_vec()
    }
    .into_iter()
    .map(Duration::from_millis)
    .collect::<Vec<_>>();
    ensure_pull_consumer(
        &stream,
        &catalog.physical_id,
        consumer::pull::Config {
            durable_name: Some(catalog.physical_id.clone()),
            filter_subject: format!("trellis.work.{}.{}", namespace, catalog.local_name),
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: backoff.first().copied().unwrap_or(Duration::from_secs(30)),
            max_deliver,
            backoff: backoff.clone(),
            metadata: HashMap::from([(
                "trellis.resource_id".to_owned(),
                catalog.resource_id.clone(),
            )]),
            ..Default::default()
        },
    )
    .await
}

async fn reconcile_consumer(
    jetstream: &jetstream::Context,
    catalog: &ResourceCatalogRecord,
    participant: &ParticipantRuntimeProjection,
    declaration: &ResourceRuntimeProjection,
) -> Result<(), ProvisionError> {
    let mut filters = Vec::new();
    for (api_id, events) in &declaration.consumer_events {
        let api = participant.referenced_apis.get(api_id).ok_or_else(|| {
            ProvisionError::Incompatible(format!("consumer API {api_id} is missing"))
        })?;
        for event in events {
            let action = api.actions.get(&format!("event:{event}")).ok_or_else(|| {
                ProvisionError::Incompatible(format!("consumer event {event} is missing"))
            })?;
            filters.push(
                trellis_protocol::derive_event_wildcard_subject(
                    api_id,
                    event,
                    action.event_parameter_count,
                )
                .map_err(|error| ProvisionError::Incompatible(error.to_string()))?,
            );
        }
    }
    filters.sort();
    filters.dedup();
    if filters.is_empty() {
        return Err(ProvisionError::Incompatible(
            "consumer has no verified event filters".to_owned(),
        ));
    }
    let filter_subject = if filters.len() == 1 {
        filters.remove(0)
    } else {
        String::new()
    };
    let stream = get_required_stream(jetstream, "trellis").await?;
    let max_deliver = i64::from(declaration.retry_attempts.unwrap_or(6));
    let backoff_ms = if declaration.retry_backoff_ms.is_empty() {
        [5_000, 30_000, 120_000, 600_000, 1_800_000]
            .into_iter()
            .take(max_deliver.saturating_sub(1) as usize)
            .collect::<Vec<_>>()
    } else {
        declaration.retry_backoff_ms.clone()
    };
    let backoff = backoff_ms
        .into_iter()
        .map(Duration::from_millis)
        .collect::<Vec<_>>();
    ensure_pull_consumer(
        &stream,
        &catalog.physical_id,
        consumer::pull::Config {
            durable_name: Some(catalog.physical_id.clone()),
            filter_subject,
            filter_subjects: filters,
            deliver_policy: if declaration.consumer_replay_all {
                consumer::DeliverPolicy::All
            } else {
                consumer::DeliverPolicy::New
            },
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: backoff
                .first()
                .copied()
                .unwrap_or(Duration::from_millis(30_000)),
            max_deliver,
            backoff: backoff.clone(),
            max_ack_pending: i64::from(declaration.consumer_concurrency.unwrap_or(1).max(1_000)),
            metadata: HashMap::from([(
                "trellis.resource_id".to_owned(),
                catalog.resource_id.clone(),
            )]),
            ..Default::default()
        },
    )
    .await?;

    let replay_stream =
        get_required_stream(jetstream, trellis_events_runtime::REPLAY_STREAM).await?;
    let replay_consumer = format!("{}_replay", catalog.physical_id);
    ensure_pull_consumer(
        &replay_stream,
        &replay_consumer,
        consumer::pull::Config {
            durable_name: Some(replay_consumer.clone()),
            filter_subject: trellis_events_runtime::dead_letters::replay_filter_subject(
                &catalog.resource_id,
            ),
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: backoff
                .first()
                .copied()
                .unwrap_or(Duration::from_millis(30_000)),
            max_deliver,
            backoff: backoff.clone(),
            max_ack_pending: i64::from(declaration.consumer_concurrency.unwrap_or(1).max(1_000)),
            metadata: HashMap::from([(
                "trellis.resource_id".to_owned(),
                catalog.resource_id.clone(),
            )]),
            ..Default::default()
        },
    )
    .await
}

async fn ensure_pull_consumer(
    stream: &stream::Stream,
    name: &str,
    desired: consumer::pull::Config,
) -> Result<(), ProvisionError> {
    match stream.consumer_info(name).await {
        Ok(info) => {
            let actual = consumer::pull::Config::try_from_consumer_config(info.config)
                .map_err(|error| ProvisionError::Incompatible(error.to_string()))?;
            if actual.metadata.get("trellis.resource_id")
                != desired.metadata.get("trellis.resource_id")
            {
                return Err(ProvisionError::Incompatible(
                    "consumer provider identity is not Trellis-owned".to_owned(),
                ));
            }
            if actual.filter_subject != desired.filter_subject
                || actual.filter_subjects != desired.filter_subjects
                || actual.deliver_policy != desired.deliver_policy
                || actual.ack_policy != desired.ack_policy
                || actual.ack_wait != desired.ack_wait
                || actual.max_deliver != desired.max_deliver
                || actual.backoff != desired.backoff
                || (desired.max_ack_pending > 0
                    && actual.max_ack_pending != desired.max_ack_pending)
            {
                return Err(ProvisionError::Incompatible(
                    "resource_change_requires_admin: retained consumer configuration differs from the approved configuration".to_owned(),
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == jetstream::context::ConsumerInfoErrorKind::NotFound => {
            stream
                .create_consumer(desired)
                .await
                .map_err(|error| storage(error.to_string()))?;
            Ok(())
        }
        Err(error) => Err(storage(error.to_string())),
    }
}

async fn get_stream_if_present(
    jetstream: &jetstream::Context,
    name: &str,
) -> Result<Option<stream::Stream>, ProvisionError> {
    match jetstream.get_stream(name).await {
        Ok(stream) => Ok(Some(stream)),
        Err(error)
            if matches!(
                error.kind(),
                jetstream::context::GetStreamErrorKind::JetStream(ref error)
                    if error.error_code().0 == 10059
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(storage(error.to_string())),
    }
}

async fn get_required_stream(
    jetstream: &jetstream::Context,
    name: &str,
) -> Result<stream::Stream, ProvisionError> {
    get_stream_if_present(jetstream, name)
        .await?
        .ok_or_else(|| storage(format!("required stream {name} is missing")))
}

async fn delete_stream_if_present(
    jetstream: &jetstream::Context,
    name: &str,
) -> Result<(), AuthorizationStateError> {
    if get_stream_if_present(jetstream, name)
        .await
        .map_err(|error| match error {
            ProvisionError::Incompatible(error) => AuthorizationStateError::InvalidRecord(error),
            ProvisionError::Retry(error) => error,
        })?
        .is_none()
    {
        return Ok(());
    }
    jetstream
        .delete_stream(name)
        .await
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    Ok(())
}

async fn delete_owned_stream_if_present(
    jetstream: &jetstream::Context,
    name: &str,
    ownership_marker: &str,
) -> Result<(), ProvisionError> {
    let Some(mut stream) = get_stream_if_present(jetstream, name).await? else {
        return Ok(());
    };
    let config = stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .clone();
    if config.description.as_deref() != Some(ownership_marker) {
        return Err(ProvisionError::Incompatible(
            "provider identity is not Trellis-owned".to_owned(),
        ));
    }
    delete_stream_if_present(jetstream, name)
        .await
        .map_err(|error| storage(error.to_string()))
}

async fn validate_owned_stream_if_present(
    jetstream: &jetstream::Context,
    name: &str,
    ownership_marker: &str,
) -> Result<(), ProvisionError> {
    let Some(mut stream) = get_stream_if_present(jetstream, name).await? else {
        return Ok(());
    };
    if stream
        .info()
        .await
        .map_err(|error| storage(error.to_string()))?
        .config
        .description
        .as_deref()
        != Some(ownership_marker)
    {
        return Err(ProvisionError::Incompatible(
            "provider identity is not Trellis-owned".to_owned(),
        ));
    }
    Ok(())
}

async fn validate_owned_consumer_if_present(
    jetstream: &jetstream::Context,
    stream_name: &str,
    consumer_name: &str,
    resource_id: &str,
) -> Result<(), AuthorizationStateError> {
    let Some(stream) = get_stream_if_present(jetstream, stream_name)
        .await
        .map_err(provision_error)?
    else {
        return Ok(());
    };
    match stream.consumer_info(consumer_name).await {
        Ok(info) => {
            let actual = consumer::pull::Config::try_from_consumer_config(info.config)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
            if actual
                .metadata
                .get("trellis.resource_id")
                .map(String::as_str)
                != Some(resource_id)
            {
                return Err(AuthorizationStateError::InvalidRecord(
                    "consumer provider identity is not Trellis-owned".to_owned(),
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == jetstream::context::ConsumerInfoErrorKind::NotFound => Ok(()),
        Err(error) => Err(AuthorizationStateError::Storage(error.to_string())),
    }
}

async fn delete_consumer_if_present(
    jetstream: &jetstream::Context,
    stream_name: &str,
    consumer_name: &str,
) -> Result<(), AuthorizationStateError> {
    let Some(stream) = get_stream_if_present(jetstream, stream_name)
        .await
        .map_err(provision_error)?
    else {
        return Ok(());
    };
    match stream.delete_consumer(consumer_name).await {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                stream::ConsumerErrorKind::JetStream(error)
                    if error.error_code().0 == 10014
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(AuthorizationStateError::Storage(error.to_string())),
    }
}

fn provision_error(error: ProvisionError) -> AuthorizationStateError {
    match error {
        ProvisionError::Incompatible(error) => AuthorizationStateError::InvalidRecord(error),
        ProvisionError::Retry(error) => error,
    }
}

fn validate_commitment(
    declaration: &ResourceRuntimeProjection,
    approved: &ApprovedResource,
) -> Result<(), AuthorizationStateError> {
    if AuthorizationResourceKind::from(declaration.kind) != approved.kind {
        return Err(AuthorizationStateError::InvalidRecord(
            "resource commitment kind differs from declaration".to_owned(),
        ));
    }
    let commitment = &approved.commitment;
    if commitment.history != declaration.history || commitment.ttl_ms != declaration.ttl_ms {
        return Err(AuthorizationStateError::InvalidRecord(
            "resource commitment differs from declaration".to_owned(),
        ));
    }
    Ok(())
}

fn actual_i32(value: i32) -> Option<u64> {
    u64::try_from(value).ok()
}

fn actual_i64(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

fn duration_ms(value: Duration) -> Result<u64, ProvisionError> {
    u64::try_from(value.as_millis())
        .map_err(|_| ProvisionError::Incompatible("provider TTL exceeds protocol range".to_owned()))
}

fn storage(error: String) -> ProvisionError {
    ProvisionError::Retry(AuthorizationStateError::Storage(error))
}

pub(crate) fn reconcile_action_payload(
    resource_id: &str,
    binding_revision: u64,
    catalog_revision: u64,
) -> Value {
    json!({
        "resourceId": resource_id,
        "bindingRevision": binding_revision,
        "catalogRevision": catalog_revision,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use tokio::io::AsyncReadExt;

    #[test]
    fn resource_id_is_length_framed_and_stable() {
        let first = resource_id(
            GrantOwnerKind::Deployment,
            "ab",
            "c",
            ParticipantResourceKind::Kv,
            "items",
        );
        let second = resource_id(
            GrantOwnerKind::Deployment,
            "a",
            "bc",
            ParticipantResourceKind::Kv,
            "items",
        );
        assert_eq!(first.len(), 43);
        assert_eq!(first, "BcPQNNuCV5jCdFGiGwA2p8liZQ9VUZTO7mT57jAF9_Q");
        assert_ne!(first, second);
        let identities = [
            resource_id(
                GrantOwnerKind::User,
                "owner-a",
                "example.app",
                ParticipantResourceKind::State,
                "settings",
            ),
            resource_id(
                GrantOwnerKind::User,
                "owner-b",
                "example.app",
                ParticipantResourceKind::State,
                "settings",
            ),
            resource_id(
                GrantOwnerKind::User,
                "owner-a",
                "example.agent",
                ParticipantResourceKind::State,
                "settings",
            ),
            resource_id(
                GrantOwnerKind::User,
                "owner-a",
                "example.device",
                ParticipantResourceKind::State,
                "settings",
            ),
            resource_id(
                GrantOwnerKind::User,
                "owner-a",
                "example.app",
                ParticipantResourceKind::State,
                "other",
            ),
        ];
        assert_eq!(
            identities
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
        assert_eq!(
            first,
            resource_id(
                GrantOwnerKind::Deployment,
                "ab",
                "c",
                ParticipantResourceKind::Kv,
                "items"
            )
        );
    }

    #[tokio::test]
    async fn providers_are_created_updated_without_data_loss_and_never_adopted() {
        struct Server(Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let directory = tempfile::tempdir().unwrap();
        let binary = trellis_local_nats::NatsServerBinary::resolve(
            &trellis_local_nats::NatsBinarySource::DownloadPinned,
            Some(&directory.path().join("cache")),
        )
        .unwrap();
        let _server = Server(
            Command::new(binary)
                .args(["-a", "127.0.0.1", "-p", &port.to_string(), "-js"])
                .arg("-sd")
                .arg(directory.path().join("data"))
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let url = format!("nats://127.0.0.1:{port}");
        let mut client = None;
        for _ in 0..100 {
            match async_nats::connect(&url).await {
                Ok(connected) => {
                    client = Some(connected);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        let client = client.expect("NATS did not start");
        let jetstream = jetstream::new(client.clone());

        let state = jetstream
            .create_key_value(jetstream::kv::Config {
                bucket: "trellis_state".to_owned(),
                description: crate::platform::state::OWNERSHIP_MARKER.to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        let state_resource_id = "S".repeat(43);
        let foreign_state_id = "F".repeat(43);
        state
            .put(&state_resource_id, "retained".into())
            .await
            .unwrap();
        state
            .put(&foreign_state_id, "foreign state".into())
            .await
            .unwrap();
        let repository = SqliteAuthorizationStore::open_in_memory().unwrap();
        let catalog_resource_id = state_resource_id.clone();
        repository
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence (package_digest, platform_trusted, accepted_at) VALUES (?1, 0, 1)",
                        ["B".repeat(43)],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence_documents (evidence_digest, package_digest, evidence_json, created_at) VALUES (?1, ?2, '{}', 1)",
                        rusqlite::params!["E".repeat(43), "B".repeat(43)],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                connection
                    .execute(
                        "INSERT INTO auth_installed_participants (participant_id, revision, participant_kind, participant_digest, needs_digest, package_digest, evidence_digest, participant_path, companion_required, projection_json, installed_at) VALUES ('participant-1', 1, 'service', ?1, ?2, ?3, ?4, 'Service', 0, '{}', 1)",
                        rusqlite::params!["C".repeat(43), "D".repeat(43), "B".repeat(43), "E".repeat(43)],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                connection
                    .execute(
                        "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'state', 'saved', '{}', ?1, '{\"kind\":\"state\"}', 'destroying', NULL, 1, 2, 1, 2)",
                        [&catalog_resource_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(state.get(&state_resource_id).await.unwrap().is_some());
        destroy_resource(
            &client,
            &repository,
            DestroyResourcePayload {
                resource_id: state_resource_id.clone(),
                catalog_revision: 2,
            },
        )
        .await
        .unwrap();
        assert!(state.get(&state_resource_id).await.unwrap().is_none());
        assert_eq!(
            state.get(&foreign_state_id).await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"foreign state")
        );
        assert!(matches!(
            repository.inspect_resource(state_resource_id).await,
            Err(AuthorizationStateError::NotFound)
        ));
        let unowned_state_id = "U".repeat(43);
        state
            .put(&unowned_state_id, "unowned state".into())
            .await
            .unwrap();
        let mut state_stream = get_required_stream(&jetstream, "KV_trellis_state")
            .await
            .unwrap();
        let mut state_config = state_stream.info().await.unwrap().config.clone();
        state_config.description = Some("foreign shared State storage".to_owned());
        jetstream.update_stream(state_config).await.unwrap();
        let catalog_unowned_state_id = unowned_state_id.clone();
        repository
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'state', 'unowned', '{}', ?1, '{\"kind\":\"state\"}', 'destroying', NULL, 1, 2, 1, 2)",
                        [&catalog_unowned_state_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                connection
                    .execute(
                        "INSERT INTO auth_resource_history (resource_id, revision, commitment_json, actual_json, state, changed_at) VALUES (?1, 2, '{}', '{\"kind\":\"state\"}', 'destroying', 2)",
                        [&catalog_unowned_state_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            destroy_resource(
                &client,
                &repository,
                DestroyResourcePayload {
                    resource_id: unowned_state_id.clone(),
                    catalog_revision: 2,
                },
            )
            .await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
        assert_eq!(
            state.get(&unowned_state_id).await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"unowned state")
        );
        assert_eq!(
            repository
                .inspect_resource(unowned_state_id.clone())
                .await
                .unwrap()["state"],
            "destroying"
        );
        assert_eq!(
            repository
                .run_read({
                    let unowned_state_id = unowned_state_id.clone();
                    move |connection| {
                        connection
                            .query_row(
                                "SELECT COUNT(*) FROM auth_resource_history WHERE resource_id = ?1",
                                [unowned_state_id],
                                |row| row.get::<_, u64>(0),
                            )
                            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
                    }
                })
                .await
                .unwrap(),
            1
        );
        let catalog = ResourceCatalogRecord {
            resource_id: "A".repeat(43),
            owner_kind: GrantOwnerKind::Deployment,
            owner_id: "deployment-1".to_owned(),
            participant_id: "participant-1".to_owned(),
            resource_kind: ParticipantResourceKind::Kv,
            local_name: "cache".to_owned(),
            physical_id: "tr_kv_provider_test".to_owned(),
            commitment: Default::default(),
            actual: None,
            state: ResourceCatalogState::Pending,
            readiness_reason: Some("reconciling".to_owned()),
            revision: 1,
            binding_revision: 1,
            created_at: 1,
            updated_at: 1,
        };
        assert!(matches!(
            reconcile_kv(&jetstream, &catalog, 65, 0).await,
            Err(ProvisionError::Incompatible(_))
        ));
        assert!(matches!(
            reconcile_kv(&jetstream, &catalog, 2, 0).await.unwrap(),
            ResourceActual::Kv {
                history: 2,
                max_value_bytes: None,
                ..
            }
        ));
        let kv = jetstream.get_key_value(&catalog.physical_id).await.unwrap();
        kv.put("retained", "before increase".into()).await.unwrap();
        let mut kv_stream = get_required_stream(&jetstream, &format!("KV_{}", catalog.physical_id))
            .await
            .unwrap();
        let mut kv_config = kv_stream.info().await.unwrap().config.clone();
        kv_config.max_message_size = 512;
        jetstream.update_stream(kv_config).await.unwrap();
        assert!(matches!(
            reconcile_kv(&jetstream, &catalog, 64, 0).await.unwrap(),
            ResourceActual::Kv {
                history: 64,
                max_value_bytes: Some(512),
                ..
            }
        ));
        assert_eq!(
            kv.get("retained").await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"before increase")
        );
        assert!(matches!(
            reconcile_kv(&jetstream, &catalog, 2, 0).await,
            Err(ProvisionError::Incompatible(_))
        ));
        reconcile_kv(&jetstream, &catalog, 64, 0).await.unwrap();
        assert!(kv.get("retained").await.unwrap().is_some());
        assert!(matches!(
            reconcile_kv(&jetstream, &catalog, 2, 0).await,
            Err(ProvisionError::Incompatible(_))
        ));

        let mut finite_ttl = catalog.clone();
        finite_ttl.physical_id = "tr_kv_finite_ttl_test".to_owned();
        reconcile_kv(&jetstream, &finite_ttl, 1, 1_000)
            .await
            .unwrap();
        reconcile_kv(&jetstream, &finite_ttl, 1, 2_000)
            .await
            .unwrap();
        reconcile_kv(&jetstream, &finite_ttl, 1, 0).await.unwrap();
        assert!(matches!(
            reconcile_kv(&jetstream, &catalog, 2, 1_000).await,
            Err(ProvisionError::Incompatible(_))
        ));

        let mut foreign = catalog.clone();
        foreign.physical_id = "tr_kv_foreign_test".to_owned();
        let foreign_kv = jetstream
            .create_key_value(jetstream::kv::Config {
                bucket: foreign.physical_id.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        foreign_kv
            .put("retained", "foreign KV bytes".into())
            .await
            .unwrap();
        assert!(matches!(
            reconcile_kv(&jetstream, &foreign, 1, 0).await,
            Err(ProvisionError::Incompatible(_))
        ));
        let foreign_kv_id = foreign.resource_id.clone();
        let foreign_kv_physical_id = foreign.physical_id.clone();
        repository
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'kv', 'foreign-kv', '{}', ?2, NULL, 'detached', 'detached', 1, 1, 1, 1)",
                        rusqlite::params![foreign_kv_id, foreign_kv_physical_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        repository
            .begin_resource_destroy(
                foreign.resource_id.clone(),
                1,
                foreign.physical_id.clone(),
                2,
            )
            .await
            .unwrap();
        assert!(matches!(
            destroy_resource(
                &client,
                &repository,
                DestroyResourcePayload {
                    resource_id: foreign.resource_id.clone(),
                    catalog_revision: 2,
                },
            )
            .await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
        assert_eq!(
            foreign_kv.get("retained").await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"foreign KV bytes")
        );
        assert_eq!(
            repository
                .inspect_resource(foreign.resource_id.clone())
                .await
                .unwrap()["state"],
            "destroying"
        );
        assert_eq!(
            repository
                .run_read({
                    let foreign_resource_id = foreign.resource_id.clone();
                    move |connection| {
                        connection
                            .query_row(
                                "SELECT COUNT(*) FROM auth_resource_history WHERE resource_id = ?1",
                                [foreign_resource_id],
                                |row| row.get::<_, u64>(0),
                            )
                            .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
                    }
                })
                .await
                .unwrap(),
            1
        );

        let mut store = catalog;
        store.resource_kind = ParticipantResourceKind::Store;
        store.physical_id = "tr_store_provider_test".to_owned();
        assert!(matches!(
            reconcile_store(&jetstream, &store, 0).await.unwrap(),
            ResourceActual::Store {
                max_object_bytes: None,
                max_total_bytes: None,
                ..
            }
        ));
        let mut store_stream =
            get_required_stream(&jetstream, &format!("OBJ_{}", store.physical_id))
                .await
                .unwrap();
        let mut store_config = store_stream.info().await.unwrap().config.clone();
        store_config.max_bytes = 1_048_576;
        jetstream.update_stream(store_config).await.unwrap();
        assert!(matches!(
            reconcile_store(&jetstream, &store, 0).await.unwrap(),
            ResourceActual::Store {
                max_object_bytes: None,
                max_total_bytes: Some(1_048_576),
                ..
            }
        ));
        let object_store = jetstream
            .get_object_store(&store.physical_id)
            .await
            .unwrap();
        let mut object = &b"before increase"[..];
        object_store.put("retained", &mut object).await.unwrap();
        assert!(matches!(
            reconcile_store(&jetstream, &store, 0).await.unwrap(),
            ResourceActual::Store {
                max_object_bytes: None,
                max_total_bytes: Some(1_048_576),
                ..
            }
        ));
        assert_eq!(object_store.info("retained").await.unwrap().size, 15);
        assert!(matches!(
            reconcile_store(&jetstream, &store, 0).await,
            Ok(ResourceActual::Store { .. })
        ));
        let mut unlimited_store = store.clone();
        unlimited_store.physical_id = "tr_store_unlimited_test".to_owned();
        reconcile_store(&jetstream, &unlimited_store, 0)
            .await
            .unwrap();
        assert!(matches!(
            reconcile_store(&jetstream, &unlimited_store, 0).await,
            Ok(ResourceActual::Store { .. })
        ));

        let mut foreign_store = store;
        foreign_store.resource_id = "O".repeat(43);
        foreign_store.physical_id = "tr_store_foreign_test".to_owned();
        let foreign_object_store = jetstream
            .create_object_store(jetstream::object_store::Config {
                bucket: foreign_store.physical_id.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut foreign_object = &b"foreign Object Store bytes"[..];
        foreign_object_store
            .put("retained", &mut foreign_object)
            .await
            .unwrap();
        assert!(matches!(
            reconcile_store(&jetstream, &foreign_store, 0).await,
            Err(ProvisionError::Incompatible(_))
        ));
        let foreign_store_id = foreign_store.resource_id.clone();
        let foreign_store_physical_id = foreign_store.physical_id.clone();
        repository
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'store', 'foreign-store', '{}', ?2, NULL, 'detached', 'detached', 1, 1, 1, 1)",
                        rusqlite::params![foreign_store_id, foreign_store_physical_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        repository
            .begin_resource_destroy(
                foreign_store.resource_id.clone(),
                1,
                foreign_store.physical_id.clone(),
                2,
            )
            .await
            .unwrap();
        assert!(matches!(
            destroy_resource(
                &client,
                &repository,
                DestroyResourcePayload {
                    resource_id: foreign_store.resource_id.clone(),
                    catalog_revision: 2,
                },
            )
            .await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
        let mut retained_object = foreign_object_store.get("retained").await.unwrap();
        let mut retained_bytes = Vec::new();
        retained_object
            .read_to_end(&mut retained_bytes)
            .await
            .unwrap();
        assert_eq!(retained_bytes, b"foreign Object Store bytes");
        assert_eq!(
            repository
                .inspect_resource(foreign_store.resource_id.clone())
                .await
                .unwrap()["state"],
            "destroying"
        );

        let jobs_stream = jetstream
            .create_stream(stream::Config {
                name: "JOBS_WORK".to_owned(),
                subjects: vec!["trellis.work.>".to_owned()],
                ..Default::default()
            })
            .await
            .unwrap();
        let job_resource_id = "J".repeat(43);
        let other_job_resource_id = "K".repeat(43);
        let job = ResourceCatalogRecord {
            resource_id: job_resource_id.clone(),
            owner_kind: GrantOwnerKind::Deployment,
            owner_id: "deployment-1".to_owned(),
            participant_id: "participant-1".to_owned(),
            resource_kind: ParticipantResourceKind::JobQueue,
            local_name: "email".to_owned(),
            physical_id: "tr_job_destroy_test".to_owned(),
            commitment: Default::default(),
            actual: Some(ResourceActual::Job),
            state: ResourceCatalogState::Destroying,
            readiness_reason: None,
            revision: 2,
            binding_revision: 1,
            created_at: 1,
            updated_at: 2,
        };
        reconcile_job(&client, &job, None, &[]).await.unwrap();
        let namespace = job_namespace(&job);
        let job_keys = jetstream
            .get_key_value(format!("JOBS_KEYS_{namespace}"))
            .await
            .unwrap();
        let target_key = format!("{namespace}.email.target");
        let retained_key = format!("{namespace}.other.retained");
        job_keys.put(&target_key, "target".into()).await.unwrap();
        job_keys
            .put(&retained_key, "retained".into())
            .await
            .unwrap();
        let job_id = job_resource_id.clone();
        let other_job_id = other_job_resource_id.clone();
        repository
            .run(move |connection| {
                for (resource_id, local_name, physical_id) in [
                    (job_id, "email", "tr_job_destroy_test"),
                    (other_job_id, "other", "tr_job_other_test"),
                ] {
                    connection
                        .execute(
                            "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'job', ?2, '{}', ?3, '{\"kind\":\"job\"}', 'destroying', NULL, 1, 2, 1, 2)",
                            rusqlite::params![resource_id, local_name, physical_id],
                        )
                        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                }
                Ok(())
            })
            .await
            .unwrap();
        destroy_resource(
            &client,
            &repository,
            DestroyResourcePayload {
                resource_id: job_resource_id,
                catalog_revision: 2,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            jobs_stream.consumer_info("tr_job_destroy_test").await,
            Err(error) if error.kind() == jetstream::context::ConsumerInfoErrorKind::NotFound
        ));
        assert!(job_keys.get(&target_key).await.unwrap().is_none());
        assert_eq!(
            job_keys.get(&retained_key).await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"retained")
        );
        destroy_resource(
            &client,
            &repository,
            DestroyResourcePayload {
                resource_id: other_job_resource_id,
                catalog_revision: 2,
            },
        )
        .await
        .unwrap();
        assert!(
            get_stream_if_present(&jetstream, &format!("KV_JOBS_KEYS_{namespace}"))
                .await
                .unwrap()
                .is_none()
        );

        let event_stream = jetstream
            .create_stream(stream::Config {
                name: "trellis".to_owned(),
                subjects: vec!["trellis.events.>".to_owned()],
                ..Default::default()
            })
            .await
            .unwrap();
        let replay_stream = jetstream
            .create_stream(stream::Config {
                name: trellis_events_runtime::REPLAY_STREAM.to_owned(),
                subjects: vec!["trellis.replay.>".to_owned()],
                ..Default::default()
            })
            .await
            .unwrap();
        let event_resource_id = "E".repeat(43);
        for (stream, name) in [
            (&event_stream, "tr_cons_destroy_test"),
            (&replay_stream, "tr_cons_destroy_test_replay"),
        ] {
            stream
                .create_consumer(consumer::pull::Config {
                    durable_name: Some(name.to_owned()),
                    metadata: HashMap::from([(
                        "trellis.resource_id".to_owned(),
                        event_resource_id.clone(),
                    )]),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let catalog_event_id = event_resource_id.clone();
        repository
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'consumer', 'events', '{}', 'tr_cons_destroy_test', '{\"kind\":\"consumer\"}', 'destroying', NULL, 1, 2, 1, 2)",
                        [catalog_event_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        destroy_resource(
            &client,
            &repository,
            DestroyResourcePayload {
                resource_id: event_resource_id.clone(),
                catalog_revision: 2,
            },
        )
        .await
        .unwrap();
        for (stream, name) in [
            (&event_stream, "tr_cons_destroy_test"),
            (&replay_stream, "tr_cons_destroy_test_replay"),
        ] {
            assert!(matches!(
                stream.consumer_info(name).await,
                Err(error) if error.kind() == jetstream::context::ConsumerInfoErrorKind::NotFound
            ));
        }
        destroy_resource(
            &client,
            &repository,
            DestroyResourcePayload {
                resource_id: event_resource_id,
                catalog_revision: 2,
            },
        )
        .await
        .unwrap();

        let foreign_event_resource_id = "Q".repeat(43);
        event_stream
            .create_consumer(consumer::pull::Config {
                durable_name: Some("tr_cons_foreign_destroy_test".to_owned()),
                metadata: HashMap::from([(
                    "trellis.resource_id".to_owned(),
                    "foreign-resource".to_owned(),
                )]),
                ..Default::default()
            })
            .await
            .unwrap();
        let catalog_foreign_event_id = foreign_event_resource_id.clone();
        repository
            .run(move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_resources (resource_id, owner_kind, owner_id, participant_id, kind, local_name, commitment_json, physical_id, actual_json, state, readiness_reason, binding_revision, revision, created_at, updated_at) VALUES (?1, 'deployment', 'deployment-1', 'participant-1', 'consumer', 'foreign-events', '{}', 'tr_cons_foreign_destroy_test', '{\"kind\":\"consumer\"}', 'destroying', NULL, 1, 2, 1, 2)",
                        [catalog_foreign_event_id],
                    )
                    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            destroy_resource(
                &client,
                &repository,
                DestroyResourcePayload {
                    resource_id: foreign_event_resource_id.clone(),
                    catalog_revision: 2,
                },
            )
            .await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
        assert!(event_stream
            .consumer_info("tr_cons_foreign_destroy_test")
            .await
            .is_ok());
        assert_eq!(
            repository
                .inspect_resource(foreign_event_resource_id)
                .await
                .unwrap()["state"],
            "destroying"
        );

        let consumer_stream = jetstream
            .create_stream(stream::Config {
                name: "RESOURCE_PROVIDER_TEST".to_owned(),
                subjects: vec!["resource.provider.test".to_owned()],
                ..Default::default()
            })
            .await
            .unwrap();
        let consumer_config = consumer::pull::Config {
            durable_name: Some("tr_cons_provider_test".to_owned()),
            filter_subject: "resource.provider.test".to_owned(),
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(5),
            max_deliver: 6,
            backoff: vec![Duration::from_secs(5)],
            max_ack_pending: 1_000,
            metadata: HashMap::from([("trellis.resource_id".to_owned(), "A".repeat(43))]),
            ..Default::default()
        };
        ensure_pull_consumer(
            &consumer_stream,
            "tr_cons_provider_test",
            consumer_config.clone(),
        )
        .await
        .unwrap();
        let mut expanded_consumer = consumer_config;
        expanded_consumer.max_ack_pending = 1_500;
        assert!(matches!(
            ensure_pull_consumer(&consumer_stream, "tr_cons_provider_test", expanded_consumer)
                .await,
            Err(ProvisionError::Incompatible(_))
        ));
        assert_eq!(
            consumer_stream
                .consumer_info("tr_cons_provider_test")
                .await
                .unwrap()
                .config
                .max_ack_pending,
            1_000
        );

        for stream in [
            "KV_tr_kv_provider_test",
            "KV_tr_kv_finite_ttl_test",
            "OBJ_tr_store_provider_test",
            "OBJ_tr_store_unlimited_test",
            "RESOURCE_PROVIDER_TEST",
        ] {
            delete_stream_if_present(&jetstream, stream).await.unwrap();
            delete_stream_if_present(&jetstream, stream).await.unwrap();
            assert!(get_stream_if_present(&jetstream, stream)
                .await
                .unwrap()
                .is_none());
        }
    }
}
