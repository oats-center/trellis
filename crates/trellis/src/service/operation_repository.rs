use std::future::Future;

use async_nats::jetstream::kv;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{OperationSnapshot, OperationState, ServerError};

/// Maximum persisted operation input size.
pub const MAX_OPERATION_INPUT_BYTES: usize = 256 * 1024;
/// Maximum persisted operation progress size.
pub const MAX_OPERATION_PROGRESS_BYTES: usize = 64 * 1024;
/// Maximum persisted operation output size.
pub const MAX_OPERATION_OUTPUT_BYTES: usize = 512 * 1024;
/// Maximum persisted operation error size.
pub const MAX_OPERATION_ERROR_BYTES: usize = 32 * 1024;
/// Maximum accepted signals retained by one operation.
pub const MAX_OPERATION_SIGNALS: usize = 100;
/// Maximum encoded payload size for one operation signal.
pub const MAX_OPERATION_SIGNAL_BYTES: usize = 64 * 1024;
/// Maximum encoded size of one complete operation record.
pub const MAX_OPERATION_RECORD_BYTES: usize = 1024 * 1024;
/// Complete durable identity, ownership, and lifecycle record for one invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableOperationRecord {
    /// Caller-supplied or SDK-generated ULID used as the idempotency key.
    pub invocation_id: String,
    /// Digest binding the invocation identity, typed input, and stable creator.
    pub invocation_digest: String,
    /// Qualified API id owning the operation.
    pub api_id: String,
    /// Generated operation key.
    pub operation: String,
    /// Deployment that owns this operation store and execution.
    pub deployment_id: String,
    /// Stable principal that initiated the operation.
    pub creator_principal_id: String,
    /// Stable participant that initiated the operation.
    pub creator_participant_id: String,
    /// Session key that authenticated the accepted start request.
    pub caller_session_key: String,
    /// Verified caller projection restored when an expired invocation is reclaimed.
    pub caller: Option<crate::client::VerifiedCaller>,
    /// Exact accepted input.
    pub input: Value,
    /// Current durable lifecycle snapshot.
    pub snapshot: OperationSnapshot,
    /// Monotonic durable mutation revision.
    pub revision: u64,
    /// Process executor currently allowed to mutate the record.
    pub owner_executor_id: Option<String>,
    /// Signed logical connection that acquired the current owner fence.
    pub owner_connection_id: Option<String>,
    /// Monotonic fencing token for the current owner.
    pub owner_epoch: u64,
    /// Server-time lease expiry in milliseconds since the Unix epoch.
    pub lease_expires_at_ms: Option<i64>,
    /// Durable cancellation request time, when cancellation was accepted.
    pub cancellation_requested: bool,
    /// Next sequence assigned to a newly accepted signal.
    pub next_signal_sequence: u64,
    /// Accepted durable signals in sequence order.
    pub signals: Vec<DurableOperationSignal>,
    /// Runtime-owned transfer staging state, when this operation accepts bytes.
    pub transfer: Option<Value>,
    /// Diagnostic creation trace carrier, omitted when tracing is disabled.
    ///
    /// Never part of the invocation digest, authorization, record identity,
    /// lease calculation, or fence comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<OperationTraceCarrier>,
}

/// Diagnostic trace carrier retained with one durable operation record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationTraceCarrier {
    /// Validated W3C `traceparent` captured at creation.
    pub traceparent: String,
    /// Validated optional W3C `tracestate` captured at creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
}

/// One durable post-acceptance operation signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableOperationSignal {
    /// Stable order within the operation.
    pub sequence: u64,
    /// Caller-generated idempotency key.
    pub request_id: String,
    /// Declared signal name.
    pub name: String,
    /// Generated-codec payload bytes.
    pub payload: Vec<u8>,
    /// Whether the current fenced executor durably acknowledged processing.
    pub acknowledged: bool,
}

impl DurableOperationRecord {
    fn validate(&self) -> Result<(), ServerError> {
        self.invocation_id
            .parse::<ulid::Ulid>()
            .map_err(|_| ServerError::Nats("operation invocation id must be a ULID".to_owned()))?;
        if self.invocation_digest
            != operation_invocation_digest(
                &self.api_id,
                &self.operation,
                &self.creator_principal_id,
                &self.creator_participant_id,
                &self.input,
            )?
        {
            return Err(ServerError::Nats(
                "operation invocation digest does not match its identity and input".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&self.input)?.len();
        if input_bytes > MAX_OPERATION_INPUT_BYTES {
            return Err(ServerError::OperationCapacityExceeded {
                field: "input",
                actual_bytes: input_bytes,
                max_bytes: MAX_OPERATION_INPUT_BYTES,
            });
        }
        if self.snapshot.progress.as_ref().is_some_and(|value| {
            serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() > MAX_OPERATION_PROGRESS_BYTES)
        }) {
            return Err(ServerError::OperationCapacityExceeded {
                field: "progress",
                actual_bytes: serde_json::to_vec(self.snapshot.progress.as_ref().unwrap())?.len(),
                max_bytes: MAX_OPERATION_PROGRESS_BYTES,
            });
        }
        if self.snapshot.output.as_ref().is_some_and(|value| {
            serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() > MAX_OPERATION_OUTPUT_BYTES)
        }) {
            return Err(ServerError::Nats(
                "operation output exceeds 512 KiB".to_owned(),
            ));
        }
        if self.signals.len() > MAX_OPERATION_SIGNALS {
            return Err(ServerError::Nats(
                "operation signal limit exceeded".to_owned(),
            ));
        }
        for (index, signal) in self.signals.iter().enumerate() {
            if signal.payload.len() > MAX_OPERATION_SIGNAL_BYTES {
                return Err(ServerError::OperationCapacityExceeded {
                    field: "signal",
                    actual_bytes: signal.payload.len(),
                    max_bytes: MAX_OPERATION_SIGNAL_BYTES,
                });
            }
            if signal.sequence != index as u64 + 1
                || self.signals[..index]
                    .iter()
                    .any(|accepted| accepted.request_id == signal.request_id)
            {
                return Err(ServerError::Nats(
                    "operation signals must have unique request ids and contiguous sequences"
                        .to_owned(),
                ));
            }
        }
        if self.next_signal_sequence != self.signals.len() as u64 + 1 {
            return Err(ServerError::Nats(
                "operation next signal sequence is inconsistent".to_owned(),
            ));
        }
        if self.snapshot.id.as_deref() != Some(&self.invocation_id)
            || self.snapshot.operation.as_deref() != Some(&self.operation)
        {
            return Err(ServerError::Nats(
                "operation snapshot identity does not match its durable record".to_owned(),
            ));
        }
        if self.snapshot.revision != self.revision {
            return Err(ServerError::Nats(
                "operation snapshot revision does not match its durable record".to_owned(),
            ));
        }
        if self.snapshot.error.as_ref().is_some_and(|error| {
            serde_json::to_vec(error).is_ok_and(|bytes| bytes.len() > MAX_OPERATION_ERROR_BYTES)
        }) {
            return Err(ServerError::Nats(
                "operation error exceeds 32 KiB".to_owned(),
            ));
        }
        let parse_time = |value: &str| {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .map_err(|error| ServerError::Nats(format!("invalid operation timestamp: {error}")))
        };
        let created_at =
            self.snapshot.created_at.as_deref().ok_or_else(|| {
                ServerError::Nats("operation record is missing createdAt".to_owned())
            })?;
        let updated_at =
            self.snapshot.updated_at.as_deref().ok_or_else(|| {
                ServerError::Nats("operation record is missing updatedAt".to_owned())
            })?;
        parse_time(created_at)?;
        parse_time(updated_at)?;
        let record_bytes = serde_json::to_vec(self)?.len();
        if record_bytes > MAX_OPERATION_RECORD_BYTES {
            return Err(ServerError::OperationCapacityExceeded {
                field: "record",
                actual_bytes: record_bytes,
                max_bytes: MAX_OPERATION_RECORD_BYTES,
            });
        }
        Ok(())
    }

    fn same_invocation(&self, other: &Self) -> bool {
        self.invocation_id == other.invocation_id
            && self.invocation_digest == other.invocation_digest
    }
}

/// Compute the stable digest used to make operation acceptance idempotent.
pub fn operation_invocation_digest(
    api_id: &str,
    operation: &str,
    creator_principal_id: &str,
    creator_participant_id: &str,
    input: &Value,
) -> Result<String, ServerError> {
    trellis_protocol::digest_json(&serde_json::json!({
        "apiId": api_id,
        "operation": operation,
        "creatorPrincipalId": creator_principal_id,
        "creatorParticipantId": creator_participant_id,
        "input": input,
    }))
    .map_err(|error| ServerError::Nats(error.to_string()))
}

/// One operation record paired with its authoritative KV revision.
#[derive(Debug, Clone, PartialEq)]
pub struct RevisionedOperationRecord {
    /// Durable record value.
    pub record: DurableOperationRecord,
    /// KV revision required by the next compare-and-swap.
    pub revision: u64,
}

/// Durable operation persistence with idempotent acceptance and fenced CAS updates.
pub trait OperationRepository: Send + Sync {
    /// Load one invocation.
    fn get(
        &self,
        invocation_id: &str,
    ) -> impl Future<Output = Result<Option<RevisionedOperationRecord>, ServerError>> + Send;

    /// Create an invocation, or return the exact existing invocation on replay.
    fn create(
        &self,
        record: DurableOperationRecord,
    ) -> impl Future<Output = Result<RevisionedOperationRecord, ServerError>> + Send;

    /// Acquire ownership, reclaiming only an expired non-terminal lease.
    fn claim(
        &self,
        invocation_id: &str,
        owner_executor_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> impl Future<Output = Result<RevisionedOperationRecord, ServerError>> + Send {
        self.claim_for_connection(
            invocation_id,
            owner_executor_id,
            owner_executor_id,
            now_ms,
            lease_expires_at_ms,
        )
    }

    /// Acquire ownership for one exact process and signed connection.
    fn claim_for_connection(
        &self,
        invocation_id: &str,
        owner_executor_id: &str,
        owner_connection_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> impl Future<Output = Result<RevisionedOperationRecord, ServerError>> + Send;

    /// Renew an unexpired lease held by one exact owner fence.
    fn renew(
        &self,
        invocation_id: &str,
        owner_executor_id: &str,
        owner_epoch: u64,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> impl Future<Output = Result<RevisionedOperationRecord, ServerError>> + Send;

    /// Replace caller-owned fields by record revision without claiming execution.
    fn compare_record_exchange(
        &self,
        expected_revision: u64,
        record: DurableOperationRecord,
    ) -> impl Future<Output = Result<RevisionedOperationRecord, ServerError>> + Send;

    /// Replace one record only when both KV revision and owner fencing token match.
    fn compare_exchange(
        &self,
        expected_revision: u64,
        expected_owner_executor_id: &str,
        expected_owner_epoch: u64,
        record: DurableOperationRecord,
    ) -> impl Future<Output = Result<RevisionedOperationRecord, ServerError>> + Send;

    /// Watch authoritative record revisions for one invocation.
    fn watch(
        &self,
        invocation_id: &str,
    ) -> impl Future<
        Output = Result<
            BoxStream<'static, Result<RevisionedOperationRecord, ServerError>>,
            ServerError,
        >,
    > + Send;

    /// List every persisted operation that has not reached a terminal state.
    fn list_nonterminal(
        &self,
    ) -> impl Future<Output = Result<Vec<RevisionedOperationRecord>, ServerError>> + Send;
}

/// Production JetStream KV implementation of [`OperationRepository`].
#[derive(Debug, Clone)]
pub struct KvOperationRepository {
    store: kv::Store,
}

impl KvOperationRepository {
    /// Use a pre-provisioned deployment operation KV bucket.
    #[must_use]
    pub fn new(store: kv::Store) -> Self {
        Self { store }
    }

    async fn load(
        &self,
        invocation_id: &str,
    ) -> Result<Option<RevisionedOperationRecord>, ServerError> {
        let Some(entry) = self
            .store
            .entry(invocation_id)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(RevisionedOperationRecord {
            record: serde_json::from_slice(&entry.value)?,
            revision: entry.revision,
        }))
    }
}

impl OperationRepository for KvOperationRepository {
    async fn get(
        &self,
        invocation_id: &str,
    ) -> Result<Option<RevisionedOperationRecord>, ServerError> {
        self.load(invocation_id).await
    }

    async fn create(
        &self,
        record: DurableOperationRecord,
    ) -> Result<RevisionedOperationRecord, ServerError> {
        record.validate()?;
        let bytes = serde_json::to_vec(&record)?.into();
        match self.store.create(record.invocation_id.clone(), bytes).await {
            Ok(revision) => Ok(RevisionedOperationRecord { record, revision }),
            Err(_) => {
                let existing = self.load(&record.invocation_id).await?.ok_or_else(|| {
                    ServerError::Nats("operation acceptance lost its KV create race".to_owned())
                })?;
                if existing.record.same_invocation(&record) {
                    Ok(existing)
                } else {
                    Err(ServerError::OperationIdempotencyConflict {
                        kind: "invocation",
                        request_id: record.invocation_id,
                    })
                }
            }
        }
    }

    async fn claim_for_connection(
        &self,
        invocation_id: &str,
        owner_executor_id: &str,
        owner_connection_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<RevisionedOperationRecord, ServerError> {
        if lease_expires_at_ms <= now_ms {
            return Err(ServerError::Nats(
                "operation lease must expire in the future".to_owned(),
            ));
        }
        for _ in 0..8 {
            let current = self.load(invocation_id).await?.ok_or_else(|| {
                ServerError::Nats("operation invocation was not found".to_owned())
            })?;
            if matches!(
                current.record.snapshot.state,
                OperationState::Completed | OperationState::Failed | OperationState::Cancelled
            ) {
                return Err(ServerError::Nats(
                    "terminal operation cannot be claimed".to_owned(),
                ));
            }
            if current
                .record
                .lease_expires_at_ms
                .is_some_and(|expiry| expiry > now_ms)
            {
                return Err(ServerError::Nats(
                    "operation already has an active lease".to_owned(),
                ));
            }
            let mut record = current.record;
            record.owner_executor_id = Some(owner_executor_id.to_owned());
            record.owner_connection_id = Some(owner_connection_id.to_owned());
            record.owner_epoch = record
                .owner_epoch
                .checked_add(1)
                .ok_or_else(|| ServerError::Nats("operation owner epoch overflow".to_owned()))?;
            record.lease_expires_at_ms = Some(lease_expires_at_ms);
            record.revision = record
                .revision
                .checked_add(1)
                .ok_or_else(|| ServerError::Nats("operation revision overflow".to_owned()))?;
            record.snapshot.revision = record.revision;
            record.validate()?;
            let bytes = serde_json::to_vec(&record)?.into();
            if let Ok(revision) = self
                .store
                .update(invocation_id, bytes, current.revision)
                .await
            {
                return Ok(RevisionedOperationRecord { record, revision });
            }
        }
        Err(ServerError::Nats(
            "operation claim exceeded CAS retry limit".to_owned(),
        ))
    }

    async fn renew(
        &self,
        invocation_id: &str,
        owner_executor_id: &str,
        owner_epoch: u64,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<RevisionedOperationRecord, ServerError> {
        if lease_expires_at_ms <= now_ms {
            return Err(ServerError::Nats(
                "operation lease must expire in the future".to_owned(),
            ));
        }
        for _ in 0..8 {
            let current = self.load(invocation_id).await?.ok_or_else(|| {
                ServerError::Nats("operation invocation was not found".to_owned())
            })?;
            if current.record.owner_executor_id.as_deref() != Some(owner_executor_id)
                || current.record.owner_epoch != owner_epoch
                || current
                    .record
                    .lease_expires_at_ms
                    .is_none_or(|expiry| expiry <= now_ms)
                || current.record.snapshot.state.is_terminal()
            {
                return Err(ServerError::Nats(
                    "operation owner fence is stale".to_owned(),
                ));
            }
            let mut record = current.record;
            record.lease_expires_at_ms = Some(lease_expires_at_ms);
            record.revision += 1;
            record.snapshot.revision = record.revision;
            record.validate()?;
            if let Ok(revision) = self
                .store
                .update(
                    invocation_id,
                    serde_json::to_vec(&record)?.into(),
                    current.revision,
                )
                .await
            {
                return Ok(RevisionedOperationRecord { record, revision });
            }
        }
        Err(ServerError::Nats(
            "operation renewal exceeded CAS retry limit".to_owned(),
        ))
    }

    async fn compare_record_exchange(
        &self,
        expected_revision: u64,
        mut record: DurableOperationRecord,
    ) -> Result<RevisionedOperationRecord, ServerError> {
        let current = self
            .load(&record.invocation_id)
            .await?
            .ok_or_else(|| ServerError::Nats("operation invocation was not found".to_owned()))?;
        if current.revision != expected_revision
            || record.owner_executor_id != current.record.owner_executor_id
            || record.owner_epoch != current.record.owner_epoch
            || record.lease_expires_at_ms != current.record.lease_expires_at_ms
            || record.api_id != current.record.api_id
            || record.operation != current.record.operation
            || record.deployment_id != current.record.deployment_id
            || record.creator_principal_id != current.record.creator_principal_id
            || record.creator_participant_id != current.record.creator_participant_id
            || record.caller != current.record.caller
            || record.input != current.record.input
        {
            return Err(ServerError::Nats(
                "operation record CAS is stale".to_owned(),
            ));
        }
        if record.revision != current.record.revision.saturating_add(1) {
            return Err(ServerError::Nats(
                "operation durable revision must increment exactly once".to_owned(),
            ));
        }
        record.snapshot.revision = record.revision;
        record.validate()?;
        let revision = self
            .store
            .update(
                record.invocation_id.clone(),
                serde_json::to_vec(&record)?.into(),
                expected_revision,
            )
            .await
            .map_err(|error| ServerError::Nats(format!("operation CAS failed: {error}")))?;
        Ok(RevisionedOperationRecord { record, revision })
    }

    async fn compare_exchange(
        &self,
        expected_revision: u64,
        expected_owner_executor_id: &str,
        expected_owner_epoch: u64,
        mut record: DurableOperationRecord,
    ) -> Result<RevisionedOperationRecord, ServerError> {
        if record.owner_executor_id.as_deref() != Some(expected_owner_executor_id)
            || record.owner_epoch != expected_owner_epoch
        {
            return Err(ServerError::Nats(
                "operation owner fence rejected update".to_owned(),
            ));
        }
        let current = self
            .load(&record.invocation_id)
            .await?
            .ok_or_else(|| ServerError::Nats("operation invocation was not found".to_owned()))?;
        if current.revision != expected_revision
            || current.record.owner_executor_id.as_deref() != Some(expected_owner_executor_id)
            || current.record.owner_epoch != expected_owner_epoch
            || current.record.lease_expires_at_ms.is_none_or(|expiry| {
                expiry <= time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000
            })
        {
            return Err(ServerError::Nats(
                "operation owner fence is stale".to_owned(),
            ));
        }
        if record.api_id != current.record.api_id
            || record.operation != current.record.operation
            || record.deployment_id != current.record.deployment_id
            || record.creator_principal_id != current.record.creator_principal_id
            || record.creator_participant_id != current.record.creator_participant_id
            || record.caller != current.record.caller
            || record.input != current.record.input
        {
            return Err(ServerError::Nats(
                "operation immutable identity cannot change".to_owned(),
            ));
        }
        if record.revision != current.record.revision.saturating_add(1) {
            return Err(ServerError::Nats(
                "operation durable revision must increment exactly once".to_owned(),
            ));
        }
        record.snapshot.revision = record.revision;
        record.validate()?;
        if current.record.cancellation_requested
            && !matches!(record.snapshot.state, OperationState::Cancelled)
        {
            return Err(ServerError::OperationAlreadyTerminal {
                operation_id: current.record.invocation_id,
                state: "cancelled".to_owned(),
            });
        }
        if matches!(
            current.record.snapshot.state,
            OperationState::Completed | OperationState::Failed | OperationState::Cancelled
        ) {
            return Err(ServerError::OperationAlreadyTerminal {
                operation_id: current.record.invocation_id,
                state: match current.record.snapshot.state {
                    OperationState::Pending => "pending",
                    OperationState::Running => "running",
                    OperationState::Completed => "completed",
                    OperationState::Failed => "failed",
                    OperationState::Cancelled => "cancelled",
                }
                .to_owned(),
            });
        }
        let revision = self
            .store
            .update(
                record.invocation_id.clone(),
                serde_json::to_vec(&record)?.into(),
                expected_revision,
            )
            .await
            .map_err(|error| ServerError::Nats(format!("operation CAS failed: {error}")))?;
        Ok(RevisionedOperationRecord { record, revision })
    }

    async fn watch(
        &self,
        invocation_id: &str,
    ) -> Result<BoxStream<'static, Result<RevisionedOperationRecord, ServerError>>, ServerError>
    {
        let watcher = self
            .store
            .watch(invocation_id)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let snapshot = self
            .load(invocation_id)
            .await?
            .ok_or_else(|| ServerError::Nats("operation invocation was not found".to_owned()))?;
        let snapshot_revision = snapshot.revision;
        Ok(Box::pin(
            futures_util::stream::once(async move { Ok(snapshot) }).chain(watcher.filter_map(
                move |entry| async move {
                    match entry {
                        Ok(entry) if entry.revision > snapshot_revision => Some(
                            serde_json::from_slice(&entry.value)
                                .map(|record| RevisionedOperationRecord {
                                    record,
                                    revision: entry.revision,
                                })
                                .map_err(ServerError::from),
                        ),
                        Ok(_) => None,
                        Err(error) => Some(Err(ServerError::Nats(error.to_string()))),
                    }
                },
            )),
        ))
    }

    async fn list_nonterminal(&self) -> Result<Vec<RevisionedOperationRecord>, ServerError> {
        let mut keys = self
            .store
            .keys()
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let mut records = Vec::new();
        while let Some(key) = keys.next().await {
            let key = key.map_err(|error| ServerError::Nats(error.to_string()))?;
            if let Some(record) = self.load(&key).await? {
                if !record.record.snapshot.state.is_terminal() {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }
}
