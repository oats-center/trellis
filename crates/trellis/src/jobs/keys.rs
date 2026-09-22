use bytes::Bytes;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::jobs::bindings::{JobKeyStalePolicy, JobQueueWhenFull};
use crate::jobs::types::JobContext;

const JOBS_KEYS_BUCKET_PREFIX: &str = "JOBS_KEYS";
const KEY_STATE_VERSION: u32 = 1;
const CAS_RETRIES: usize = 8;

#[doc = concat!("Trellis API operation `", stringify!(job_key), "`.")]
pub fn job_key(service: &str, job_type: &str, job_id: &str) -> String {
    format!("{service}.{job_type}.{job_id}")
}

#[doc = concat!("Trellis API operation `", stringify!(worker_presence_key), "`.")]
pub fn worker_presence_key(service: &str, job_type: &str, instance_id: &str) -> String {
    format!("{service}.{job_type}.{instance_id}")
}

/// Derive a human-readable keyed-concurrency key from a validated JSON payload.
#[doc = concat!("Trellis API operation `", stringify!(derive_key), "`.")]
pub fn derive_key(payload: &Value, template: &[String]) -> Result<String, KeyDerivationError> {
    derive_job_key(payload, template).map(|derived| derived.key)
}

/// Derive the display key and canonical cross-runtime hash for a keyed job payload.
#[doc = concat!("Trellis API operation `", stringify!(derive_job_key), "`.")]
pub fn derive_job_key(
    payload: &Value,
    template: &[String],
) -> Result<DerivedJobKey, KeyDerivationError> {
    if template.is_empty() {
        return Err(KeyDerivationError::EmptyTemplate);
    }
    let segments = template
        .iter()
        .map(|segment| derive_key_segment(payload, segment))
        .collect::<Result<Vec<_>, _>>()?;
    let key = segments
        .iter()
        .map(display_key_segment)
        .collect::<Vec<_>>()
        .join(":");
    Ok(DerivedJobKey {
        key,
        key_hash: key_hash_for_segments(&segments),
    })
}

/// Return a stable SHA-256 hex hash for an already-canonicalized string.
#[doc = concat!("Trellis API operation `", stringify!(key_hash), "`.")]
pub fn key_hash(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Return the TS-compatible stable SHA-256 hash for structured key segments.
#[doc = concat!("Trellis API operation `", stringify!(key_hash_for_segments), "`.")]
pub fn key_hash_for_segments(segments: &[Value]) -> String {
    #[derive(Serialize)]
    struct CanonicalKeyHashPayload<'a> {
        version: u32,
        segments: &'a [Value],
    }

    let payload = CanonicalKeyHashPayload {
        version: 1,
        segments,
    };
    key_hash(&serde_json::to_string(&payload).expect("canonical key hash payload serializes"))
}

/// Build the NATS KV key for one service/job-type/key-hash tuple.
#[doc = concat!("Trellis API operation `", stringify!(coordination_key), "`.")]
pub fn coordination_key(service: &str, job_type: &str, key_hash: &str) -> String {
    format!("{service}.{job_type}.{key_hash}")
}

fn derive_key_segment(payload: &Value, segment: &str) -> Result<Value, KeyDerivationError> {
    if !segment.starts_with('/') {
        return Ok(Value::String(segment.to_string()));
    }
    let value = payload
        .pointer(segment)
        .ok_or_else(|| KeyDerivationError::MissingPointer {
            pointer: segment.to_string(),
        })?;
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(value.clone()),
        _ => Err(KeyDerivationError::NonScalarPointer {
            pointer: segment.to_string(),
        }),
    }
}

fn display_key_segment(segment: &Value) -> String {
    match segment {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => unreachable!("key derivation only produces scalar segments"),
    }
}

/// Derived display key and stable runtime hash for keyed concurrency.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(DerivedJobKey), "`.")]
pub struct DerivedJobKey {
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: String,
    #[doc = concat!("The `", stringify!(key_hash), "` value.")]
    pub key_hash: String,
}

/// Errors returned when deriving a keyed-concurrency key from a payload.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis value set `", stringify!(KeyDerivationError), "`.")]
pub enum KeyDerivationError {
    #[error("key template must contain at least one segment")]
    EmptyTemplate,
    #[error("key pointer '{pointer}' did not resolve")]
    MissingPointer { pointer: String },
    #[error("key pointer '{pointer}' resolved to a non-scalar value")]
    NonScalarPointer { pointer: String },
}

/// Serialized keyed-concurrency coordination state stored in `JOBS_KEYS`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobKeyState), "`.")]
pub struct JobKeyState {
    #[doc = concat!("The `", stringify!(version), "` value.")]
    pub version: u32,
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: String,
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: String,
    #[doc = concat!("The `", stringify!(key_hash), "` value.")]
    pub key_hash: String,
    #[doc = concat!("The `", stringify!(max_active), "` value.")]
    pub max_active: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(max_queued_per_key), "` value.")]
    pub max_queued_per_key: Option<u64>,
    #[doc = concat!("The `", stringify!(active), "` value.")]
    pub active: Vec<JobKeyActiveSlot>,
    #[doc = concat!("The `", stringify!(queued), "` value.")]
    pub queued: Vec<JobKeyQueuedEntry>,
    #[doc = concat!("The `", stringify!(stale_takeover_count), "` value.")]
    pub stale_takeover_count: u64,
    #[doc = concat!("The `", stringify!(updated_at), "` value.")]
    pub updated_at: String,
}

/// Active key slot owned by a running worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobKeyActiveSlot), "`.")]
pub struct JobKeyActiveSlot {
    #[doc = concat!("The `", stringify!(job_id), "` value.")]
    pub job_id: String,
    #[doc = concat!("The `", stringify!(slot_token), "` value.")]
    pub slot_token: String,
    #[doc = concat!("The `", stringify!(instance_id), "` value.")]
    pub instance_id: String,
    #[doc = concat!("The `", stringify!(started_at), "` value.")]
    pub started_at: String,
    #[doc = concat!("The `", stringify!(heartbeat_at), "` value.")]
    pub heartbeat_at: String,
    #[doc = concat!("The `", stringify!(lease_expires_at), "` value.")]
    pub lease_expires_at: String,
    #[doc = concat!("The `", stringify!(tries), "` value.")]
    pub tries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: Option<JobContext>,
}

/// Queued job reservation for one keyed-concurrency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(JobKeyQueuedEntry), "`.")]
pub struct JobKeyQueuedEntry {
    #[doc = concat!("The `", stringify!(job_id), "` value.")]
    pub job_id: String,
    #[doc = concat!("The `", stringify!(created_at), "` value.")]
    pub created_at: String,
    #[doc = concat!("The `", stringify!(request_id), "` value.")]
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: Option<JobContext>,
}

/// Normalized keyed-concurrency policy used by reducer functions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(JobKeyPolicy), "`.")]
pub struct JobKeyPolicy {
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: String,
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: String,
    #[doc = concat!("The `", stringify!(key_hash), "` value.")]
    pub key_hash: String,
    #[doc = concat!("The `", stringify!(max_active), "` value.")]
    pub max_active: u32,
    #[doc = concat!("The `", stringify!(max_queued_per_key), "` value.")]
    pub max_queued_per_key: u64,
    #[doc = concat!("The `", stringify!(when_full), "` value.")]
    pub when_full: JobQueueWhenFull,
    #[doc = concat!("The `", stringify!(stale_policy), "` value.")]
    pub stale_policy: JobKeyStalePolicy,
}

impl JobKeyPolicy {
    /// Return the stable hash for this policy's human-readable key.
    #[doc = concat!("Trellis API operation `", stringify!(key_hash), "`.")]
    pub fn key_hash(&self) -> String {
        self.key_hash.clone()
    }
}

/// Input for admitting a new job into a per-key queue.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(AdmitJobInput), "`.")]
pub struct AdmitJobInput {
    #[doc = concat!("The `", stringify!(job_id), "` value.")]
    pub job_id: String,
    #[doc = concat!("The `", stringify!(request_id), "` value.")]
    pub request_id: String,
    #[doc = concat!("The `", stringify!(created_at), "` value.")]
    pub created_at: String,
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: JobContext,
}

/// Outcome of keyed admission before lifecycle creation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis value set `", stringify!(AdmitJobOutcome), "`.")]
pub enum AdmitJobOutcome {
    Accepted {
        state: JobKeyState,
    },
    Rejected {
        reason: KeyRejectReason,
        active: usize,
        queued: usize,
        limit: usize,
    },
    Coalesced {
        existing_job_id: String,
    },
    Replaced {
        state: JobKeyState,
        replaced: JobKeyQueuedEntry,
    },
}

/// Reason keyed admission or acquisition could not proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc = concat!("Public Trellis value set `", stringify!(KeyRejectReason), "`.")]
pub enum KeyRejectReason {
    ActiveLimit,
    QueueDepth,
    StaleBlocked,
}

/// Input for acquiring an active keyed slot before handler execution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(AcquireSlotInput), "`.")]
pub struct AcquireSlotInput {
    #[doc = concat!("The `", stringify!(job_id), "` value.")]
    pub job_id: String,
    #[doc = concat!("The `", stringify!(slot_token), "` value.")]
    pub slot_token: String,
    #[doc = concat!("The `", stringify!(instance_id), "` value.")]
    pub instance_id: String,
    #[doc = concat!("The `", stringify!(started_at), "` value.")]
    pub started_at: String,
    #[doc = concat!("The `", stringify!(lease_expires_at), "` value.")]
    pub lease_expires_at: String,
    #[doc = concat!("The `", stringify!(tries), "` value.")]
    pub tries: u64,
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: JobContext,
}

/// Outcome of attempting to acquire an active keyed slot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis value set `", stringify!(AcquireSlotOutcome), "`.")]
pub enum AcquireSlotOutcome {
    Acquired {
        state: JobKeyState,
        slot: Box<JobKeyActiveSlot>,
        stale_slots: Vec<JobKeyActiveSlot>,
    },
    Blocked {
        state: JobKeyState,
        reason: KeyRejectReason,
    },
}

/// Outcome of renewing or releasing an active keyed slot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis value set `", stringify!(LeaseMutationOutcome), "`.")]
pub enum LeaseMutationOutcome {
    Renewed { state: JobKeyState },
    Released { state: JobKeyState },
    Lost { state: JobKeyState },
}

/// Outcome of removing a queued keyed reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis value set `", stringify!(QueueMutationOutcome), "`.")]
pub enum QueueMutationOutcome {
    Removed { state: JobKeyState },
    Restored { state: JobKeyState },
    Missing { state: JobKeyState },
}

/// Apply keyed admission policy to a candidate job and return the next state.
#[doc = concat!("Trellis API operation `", stringify!(admit_job), "`.")]
pub fn admit_job(
    current: Option<JobKeyState>,
    policy: &JobKeyPolicy,
    input: AdmitJobInput,
) -> AdmitJobOutcome {
    let mut state = current.unwrap_or_else(|| new_key_state(policy, &input.created_at));
    state.max_active = policy.max_active;
    state.max_queued_per_key = Some(policy.max_queued_per_key);

    if state.active.iter().any(|slot| slot.job_id == input.job_id)
        || state
            .queued
            .iter()
            .any(|entry| entry.job_id == input.job_id)
    {
        return AdmitJobOutcome::Accepted { state };
    }

    let active_capacity = (policy.max_active as usize).saturating_sub(state.active.len());
    let queue_limit = (policy.max_queued_per_key as usize).saturating_add(active_capacity);
    let queue_full = state.queued.len() >= queue_limit;
    if queue_full {
        return match policy.when_full {
            JobQueueWhenFull::Reject => AdmitJobOutcome::Rejected {
                reason: admission_reject_reason(&state, policy),
                active: state.active.len(),
                queued: state.queued.len(),
                limit: admission_reject_limit(&state, policy, queue_limit),
            },
            JobQueueWhenFull::Coalesce => AdmitJobOutcome::Coalesced {
                existing_job_id: state
                    .queued
                    .first()
                    .map(|entry| entry.job_id.clone())
                    .or_else(|| state.active.first().map(|slot| slot.job_id.clone()))
                    .unwrap_or_default(),
            },
            JobQueueWhenFull::ReplaceOldest if !state.queued.is_empty() => {
                let replaced = state.queued.remove(0);
                state.queued.push(JobKeyQueuedEntry {
                    job_id: input.job_id,
                    created_at: input.created_at.clone(),
                    request_id: input.request_id,
                    context: Some(input.context),
                });
                state.updated_at = input.created_at;
                AdmitJobOutcome::Replaced { state, replaced }
            }
            JobQueueWhenFull::ReplaceOldest => AdmitJobOutcome::Rejected {
                reason: admission_reject_reason(&state, policy),
                active: state.active.len(),
                queued: state.queued.len(),
                limit: admission_reject_limit(&state, policy, queue_limit),
            },
        };
    }

    state.queued.push(JobKeyQueuedEntry {
        job_id: input.job_id,
        created_at: input.created_at.clone(),
        request_id: input.request_id,
        context: Some(input.context),
    });
    state.updated_at = input.created_at;
    AdmitJobOutcome::Accepted { state }
}

fn admission_reject_reason(state: &JobKeyState, policy: &JobKeyPolicy) -> KeyRejectReason {
    if state.active.len() >= policy.max_active as usize {
        KeyRejectReason::ActiveLimit
    } else {
        KeyRejectReason::QueueDepth
    }
}

fn admission_reject_limit(state: &JobKeyState, policy: &JobKeyPolicy, queue_limit: usize) -> usize {
    if state.active.len() >= policy.max_active as usize {
        policy.max_active as usize
    } else {
        queue_limit
    }
}

/// Acquire an active keyed slot from existing key state.
#[doc = concat!("Trellis API operation `", stringify!(acquire_active_slot), "`.")]
pub fn acquire_active_slot(
    mut state: JobKeyState,
    policy: &JobKeyPolicy,
    input: AcquireSlotInput,
) -> AcquireSlotOutcome {
    state.max_active = policy.max_active;
    state.queued.retain(|entry| entry.job_id != input.job_id);

    if let Some(existing) = state
        .active
        .iter()
        .find(|slot| slot.job_id == input.job_id && slot.slot_token == input.slot_token)
        .cloned()
    {
        return AcquireSlotOutcome::Acquired {
            state,
            slot: Box::new(existing),
            stale_slots: Vec::new(),
        };
    }

    let mut stale_slots = Vec::new();
    if state.active.len() >= policy.max_active as usize {
        let expired_index = state
            .active
            .iter()
            .position(|slot| slot.lease_expires_at <= input.started_at);
        match (expired_index, &policy.stale_policy) {
            (Some(index), JobKeyStalePolicy::FailStale) => {
                stale_slots.push(state.active.remove(index));
                state.stale_takeover_count = state.stale_takeover_count.saturating_add(1);
            }
            (Some(_), JobKeyStalePolicy::Block) => {
                return AcquireSlotOutcome::Blocked {
                    state,
                    reason: KeyRejectReason::StaleBlocked,
                };
            }
            (None, _) => {
                return AcquireSlotOutcome::Blocked {
                    state,
                    reason: KeyRejectReason::ActiveLimit,
                };
            }
        }
    }

    let slot = JobKeyActiveSlot {
        job_id: input.job_id,
        slot_token: input.slot_token,
        instance_id: input.instance_id,
        started_at: input.started_at.clone(),
        heartbeat_at: input.started_at.clone(),
        lease_expires_at: input.lease_expires_at,
        tries: input.tries,
        context: Some(input.context),
    };
    state.active.push(slot.clone());
    state.updated_at = input.started_at;
    AcquireSlotOutcome::Acquired {
        state,
        slot: Box::new(slot),
        stale_slots,
    }
}

/// Acquire an active keyed slot, creating empty key state when needed.
#[doc = concat!("Trellis API operation `", stringify!(acquire_active_slot_from_state), "`.")]
pub fn acquire_active_slot_from_state(
    current: Option<JobKeyState>,
    policy: &JobKeyPolicy,
    input: AcquireSlotInput,
) -> AcquireSlotOutcome {
    let state = current.unwrap_or_else(|| new_key_state(policy, &input.started_at));
    acquire_active_slot(state, policy, input)
}

/// Renew the heartbeat and lease expiry for a matching active slot token.
#[doc = concat!("Trellis API operation `", stringify!(renew_active_slot), "`.")]
pub fn renew_active_slot(
    mut state: JobKeyState,
    job_id: &str,
    slot_token: &str,
    heartbeat_at: &str,
    lease_expires_at: &str,
) -> LeaseMutationOutcome {
    let Some(slot) = state
        .active
        .iter_mut()
        .find(|slot| slot.job_id == job_id && slot.slot_token == slot_token)
    else {
        return LeaseMutationOutcome::Lost { state };
    };
    slot.heartbeat_at = heartbeat_at.to_string();
    slot.lease_expires_at = lease_expires_at.to_string();
    state.updated_at = heartbeat_at.to_string();
    LeaseMutationOutcome::Renewed { state }
}

/// Release a matching active slot token from key state.
#[doc = concat!("Trellis API operation `", stringify!(release_active_slot), "`.")]
pub fn release_active_slot(
    mut state: JobKeyState,
    job_id: &str,
    slot_token: &str,
    released_at: &str,
) -> LeaseMutationOutcome {
    let Some(index) = state
        .active
        .iter()
        .position(|slot| slot.job_id == job_id && slot.slot_token == slot_token)
    else {
        return LeaseMutationOutcome::Lost { state };
    };
    state.active.remove(index);
    state.updated_at = released_at.to_string();
    LeaseMutationOutcome::Released { state }
}

/// Remove a queued keyed reservation for work that became terminal before acquisition.
#[doc = concat!("Trellis API operation `", stringify!(remove_queued_entry), "`.")]
pub fn remove_queued_entry(
    mut state: JobKeyState,
    job_id: &str,
    removed_at: &str,
) -> QueueMutationOutcome {
    let Some(index) = state.queued.iter().position(|entry| entry.job_id == job_id) else {
        return QueueMutationOutcome::Missing { state };
    };
    state.queued.remove(index);
    state.updated_at = removed_at.to_string();
    QueueMutationOutcome::Removed { state }
}

/// Restore a queued job that was replaced before the replacement lifecycle publish succeeded.
#[doc = concat!("Trellis API operation `", stringify!(restore_replaced_queued_entry), "`.")]
pub fn restore_replaced_queued_entry(
    mut state: JobKeyState,
    replaced: JobKeyQueuedEntry,
    replacement_job_id: &str,
    restored_at: &str,
) -> QueueMutationOutcome {
    state
        .queued
        .retain(|entry| entry.job_id != replacement_job_id && entry.job_id != replaced.job_id);
    state.queued.insert(0, replaced);
    state.updated_at = restored_at.to_string();
    QueueMutationOutcome::Restored { state }
}

/// Create an empty key state value for a policy at the given timestamp.
#[doc = concat!("Trellis API operation `", stringify!(new_key_state), "`.")]
pub fn new_key_state(policy: &JobKeyPolicy, timestamp: &str) -> JobKeyState {
    JobKeyState {
        version: KEY_STATE_VERSION,
        service: policy.service.clone(),
        job_type: policy.job_type.clone(),
        key: policy.key.clone(),
        key_hash: policy.key_hash(),
        max_active: policy.max_active,
        max_queued_per_key: Some(policy.max_queued_per_key),
        active: Vec::new(),
        queued: Vec::new(),
        stale_takeover_count: 0,
        updated_at: timestamp.to_string(),
    }
}

/// Errors returned by the NATS KV-backed keyed coordinator.
#[doc(hidden)]
#[derive(Debug, thiserror::Error)]
#[doc = concat!("Public Trellis value set `", stringify!(NatsKeyCoordinatorError), "`.")]
pub enum NatsKeyCoordinatorError {
    #[error("failed to open keyed jobs KV bucket: {0}")]
    Open(String),
    #[error("failed to read key state '{key}': {details}")]
    Read { key: String, details: String },
    #[error("failed to decode key state '{key}': {details}")]
    Decode { key: String, details: String },
    #[error("failed to encode key state '{key}': {details}")]
    Encode { key: String, details: String },
    #[error("failed to write key state '{key}' after compare-and-set retries")]
    Conflict { key: String },
    #[error("failed to write key state '{key}': {details}")]
    Write { key: String, details: String },
}

/// NATS KV-backed keyed-concurrency coordinator using compare-and-set writes.
#[derive(Clone)]
#[doc = concat!("Public Trellis data type `", stringify!(NatsKeyCoordinator), "`.")]
pub struct NatsKeyCoordinator {
    store: async_nats::jetstream::kv::Store,
}

/// Keyed-concurrency coordinator used by managers and workers.
#[doc(hidden)]
pub trait JobKeyCoordinator: std::fmt::Debug + Send + Sync {
    /// Apply admission with compare-and-set semantics.
    fn admit(
        &self,
        policy: JobKeyPolicy,
        input: AdmitJobInput,
    ) -> BoxFuture<'static, Result<AdmitJobOutcome, NatsKeyCoordinatorError>>;

    /// Acquire an active slot with compare-and-set semantics.
    fn acquire(
        &self,
        policy: JobKeyPolicy,
        input: AcquireSlotInput,
    ) -> BoxFuture<'static, Result<AcquireSlotOutcome, NatsKeyCoordinatorError>>;

    /// Release an active slot with compare-and-set semantics.
    fn release(
        &self,
        policy: JobKeyPolicy,
        job_id: String,
        slot_token: String,
        released_at: String,
    ) -> BoxFuture<'static, Result<LeaseMutationOutcome, NatsKeyCoordinatorError>>;

    /// Remove a queued reservation with compare-and-set semantics.
    fn remove_queued(
        &self,
        policy: JobKeyPolicy,
        job_id: String,
        removed_at: String,
    ) -> BoxFuture<'static, Result<QueueMutationOutcome, NatsKeyCoordinatorError>>;

    /// Restore a replaced queued reservation after replacement publish rollback.
    fn restore_replaced(
        &self,
        policy: JobKeyPolicy,
        replaced: JobKeyQueuedEntry,
        replacement_job_id: String,
        restored_at: String,
    ) -> BoxFuture<'static, Result<QueueMutationOutcome, NatsKeyCoordinatorError>>;
}

impl std::fmt::Debug for NatsKeyCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NatsKeyCoordinator")
            .finish_non_exhaustive()
    }
}

impl JobKeyCoordinator for NatsKeyCoordinator {
    fn admit(
        &self,
        policy: JobKeyPolicy,
        input: AdmitJobInput,
    ) -> BoxFuture<'static, Result<AdmitJobOutcome, NatsKeyCoordinatorError>> {
        let coordinator = self.clone();
        Box::pin(async move {
            coordinator
                .update_key(&policy, {
                    let policy = policy.clone();
                    let input = input.clone();
                    move |current| admit_job(current, &policy, input.clone())
                })
                .await
        })
    }

    fn acquire(
        &self,
        policy: JobKeyPolicy,
        input: AcquireSlotInput,
    ) -> BoxFuture<'static, Result<AcquireSlotOutcome, NatsKeyCoordinatorError>> {
        let coordinator = self.clone();
        Box::pin(async move {
            coordinator
                .update_key(&policy, {
                    let policy = policy.clone();
                    let input = input.clone();
                    move |current| acquire_active_slot_from_state(current, &policy, input.clone())
                })
                .await
        })
    }

    fn release(
        &self,
        policy: JobKeyPolicy,
        job_id: String,
        slot_token: String,
        released_at: String,
    ) -> BoxFuture<'static, Result<LeaseMutationOutcome, NatsKeyCoordinatorError>> {
        let coordinator = self.clone();
        Box::pin(async move {
            coordinator
                .update_key(&policy, {
                    let policy_for_missing = policy.clone();
                    move |current| match current {
                        Some(state) => {
                            release_active_slot(state, &job_id, &slot_token, &released_at)
                        }
                        None => LeaseMutationOutcome::Lost {
                            state: new_key_state(&policy_for_missing, &released_at),
                        },
                    }
                })
                .await
        })
    }

    fn remove_queued(
        &self,
        policy: JobKeyPolicy,
        job_id: String,
        removed_at: String,
    ) -> BoxFuture<'static, Result<QueueMutationOutcome, NatsKeyCoordinatorError>> {
        let coordinator = self.clone();
        Box::pin(async move {
            coordinator
                .update_key(&policy, {
                    let policy_for_missing = policy.clone();
                    move |current| match current {
                        Some(state) => remove_queued_entry(state, &job_id, &removed_at),
                        None => QueueMutationOutcome::Missing {
                            state: new_key_state(&policy_for_missing, &removed_at),
                        },
                    }
                })
                .await
        })
    }

    fn restore_replaced(
        &self,
        policy: JobKeyPolicy,
        replaced: JobKeyQueuedEntry,
        replacement_job_id: String,
        restored_at: String,
    ) -> BoxFuture<'static, Result<QueueMutationOutcome, NatsKeyCoordinatorError>> {
        let coordinator = self.clone();
        Box::pin(async move {
            coordinator
                .update_key(&policy, {
                    let policy_for_missing = policy.clone();
                    move |current| match current {
                        Some(state) => restore_replaced_queued_entry(
                            state,
                            replaced.clone(),
                            &replacement_job_id,
                            &restored_at,
                        ),
                        None => {
                            let mut state = new_key_state(&policy_for_missing, &restored_at);
                            state.queued.push(replaced.clone());
                            QueueMutationOutcome::Restored { state }
                        }
                    }
                })
                .await
        })
    }
}

impl NatsKeyCoordinator {
    /// Open the service-scoped Trellis keyed jobs KV bucket.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(open_for_service), "`.")]
    pub async fn open_for_service(
        nats: async_nats::Client,
        service: &str,
    ) -> Result<Self, NatsKeyCoordinatorError> {
        let jetstream = async_nats::jetstream::new(nats);
        let store = jetstream
            .get_key_value(format!("{JOBS_KEYS_BUCKET_PREFIX}_{service}"))
            .await
            .map_err(|error| NatsKeyCoordinatorError::Open(error.to_string()))?;
        Ok(Self { store })
    }

    /// Read-modify-write one key state value with NATS KV compare-and-set retries.
    pub(crate) async fn update_key<T>(
        &self,
        policy: &JobKeyPolicy,
        mut update: impl FnMut(Option<JobKeyState>) -> T,
    ) -> Result<T, NatsKeyCoordinatorError>
    where
        T: KeyStateUpdate,
    {
        let key = coordination_key(&policy.service, &policy.job_type, &policy.key_hash());
        for _ in 0..CAS_RETRIES {
            let entry = self.store.entry(key.clone()).await.map_err(|error| {
                NatsKeyCoordinatorError::Read {
                    key: key.clone(),
                    details: error.to_string(),
                }
            })?;
            let current = entry
                .as_ref()
                .map(|entry| serde_json::from_slice::<JobKeyState>(&entry.value))
                .transpose()
                .map_err(|error| NatsKeyCoordinatorError::Decode {
                    key: key.clone(),
                    details: error.to_string(),
                })?;
            if let Some(state) = current.as_ref() {
                validate_loaded_state(&key, policy, state)?;
            }
            let output = update(current);
            let Some(next) = output.next_state() else {
                return Ok(output);
            };
            let value =
                serde_json::to_vec(next).map_err(|error| NatsKeyCoordinatorError::Encode {
                    key: key.clone(),
                    details: error.to_string(),
                })?;
            let written: Result<u64, String> = match entry {
                Some(entry) => self
                    .store
                    .update(key.clone(), Bytes::from(value), entry.revision)
                    .await
                    .map_err(|error| error.to_string()),
                None => self
                    .store
                    .create(key.clone(), Bytes::from(value))
                    .await
                    .map_err(|error| error.to_string()),
            };
            match written {
                Ok(_) => return Ok(output),
                Err(error) if is_revision_mismatch(&error) || already_exists(&error) => continue,
                Err(error) => {
                    return Err(NatsKeyCoordinatorError::Write {
                        key: key.clone(),
                        details: error,
                    })
                }
            }
        }
        Err(NatsKeyCoordinatorError::Conflict { key })
    }
}

/// Reducer output that can expose the next key state for CAS persistence.
pub(crate) trait KeyStateUpdate {
    fn next_state(&self) -> Option<&JobKeyState>;
}

impl KeyStateUpdate for AdmitJobOutcome {
    fn next_state(&self) -> Option<&JobKeyState> {
        match self {
            Self::Accepted { state } | Self::Replaced { state, .. } => Some(state),
            Self::Rejected { .. } | Self::Coalesced { .. } => None,
        }
    }
}

impl KeyStateUpdate for AcquireSlotOutcome {
    fn next_state(&self) -> Option<&JobKeyState> {
        match self {
            Self::Acquired { state, .. } => Some(state),
            Self::Blocked { .. } => None,
        }
    }
}

impl KeyStateUpdate for LeaseMutationOutcome {
    fn next_state(&self) -> Option<&JobKeyState> {
        match self {
            Self::Renewed { state } | Self::Released { state } => Some(state),
            Self::Lost { .. } => None,
        }
    }
}

impl KeyStateUpdate for QueueMutationOutcome {
    fn next_state(&self) -> Option<&JobKeyState> {
        match self {
            Self::Removed { state } | Self::Restored { state } => Some(state),
            Self::Missing { .. } => None,
        }
    }
}

fn validate_loaded_state(
    key: &str,
    policy: &JobKeyPolicy,
    state: &JobKeyState,
) -> Result<(), NatsKeyCoordinatorError> {
    let expected_hash = policy.key_hash();
    if state.version != KEY_STATE_VERSION
        || state.service != policy.service
        || state.job_type != policy.job_type
        || state.key != policy.key
        || state.key_hash != expected_hash
    {
        return Err(NatsKeyCoordinatorError::Decode {
            key: key.to_string(),
            details: format!(
                "loaded key state does not match expected service/job/key/hash: expected {}/{}/{}/{}, found version={} {}/{}/{}/{}",
                policy.service,
                policy.job_type,
                policy.key,
                expected_hash,
                state.version,
                state.service,
                state.job_type,
                state.key,
                state.key_hash
            ),
        });
    }
    Ok(())
}

fn is_revision_mismatch(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("wrong last sequence")
        || message.contains("wrong last revision")
        || message.contains("revision mismatch")
        || message.contains("sequence mismatch")
}

fn already_exists(error: &impl std::fmt::Display) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("already exists")
}
