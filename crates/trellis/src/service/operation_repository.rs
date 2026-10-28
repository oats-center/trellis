use std::future::Future;

use async_nats::jetstream::kv;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::Value;

use super::{OperationState, ServerError};

use trellis_protocol::operation_record::{
    decode_operation_record, encode_operation_record, OperationRecordError,
};
pub use trellis_protocol::operation_record::{
    DurableOperationRecord, DurableOperationSignal, OperationTraceCarrier,
    MAX_OPERATION_ERROR_BYTES, MAX_OPERATION_INPUT_BYTES, MAX_OPERATION_OUTPUT_BYTES,
    MAX_OPERATION_PROGRESS_BYTES, MAX_OPERATION_RECORD_BYTES, MAX_OPERATION_SIGNALS,
    MAX_OPERATION_SIGNAL_BYTES,
};

impl From<OperationRecordError> for ServerError {
    fn from(error: OperationRecordError) -> Self {
        match error {
            OperationRecordError::Capacity {
                field,
                actual_bytes,
                max_bytes,
            } => Self::OperationCapacityExceeded {
                field,
                actual_bytes,
                max_bytes,
            },
            other => Self::Nats(other.to_string()),
        }
    }
}

/// Compute the stable digest used to make operation acceptance idempotent.
pub fn operation_invocation_digest(
    api_id: &str,
    operation: &str,
    creator_principal_id: &str,
    creator_binding: &trellis_protocol::SessionBinding,
    input: &Value,
) -> Result<String, ServerError> {
    trellis_protocol::operation_record::operation_invocation_digest(
        api_id,
        operation,
        creator_principal_id,
        creator_binding,
        input,
    )
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

    /// Create or replay an invocation, monotonically merging requested cancellation.
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
#[derive(Clone)]
pub struct KvOperationRepository {
    transport: RepositoryTransport,
}

#[derive(Clone)]
enum RepositoryTransport {
    Fixed {
        store: Box<kv::Store>,
        lease: Option<crate::client::TransportLease>,
    },
    Managed {
        manager: crate::client::TransportGenerationManager,
        bucket: String,
        timeout_ms: u64,
    },
}

impl std::fmt::Debug for KvOperationRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KvOperationRepository")
            .finish_non_exhaustive()
    }
}

/// One finite repository invocation, or the lifetime of an escaping watch.
struct RepositoryStore {
    store: kv::Store,
    _lease: Option<crate::client::TransportLease>,
}

impl KvOperationRepository {
    /// Use a pre-provisioned deployment operation KV bucket.
    #[must_use]
    pub fn new(store: kv::Store) -> Self {
        Self {
            transport: RepositoryTransport::Fixed {
                store: Box::new(store),
                lease: None,
            },
        }
    }

    pub(crate) fn managed(
        manager: crate::client::TransportGenerationManager,
        bucket: String,
        timeout_ms: u64,
    ) -> Self {
        Self {
            transport: RepositoryTransport::Managed {
                manager,
                bucket,
                timeout_ms,
            },
        }
    }

    /// Bind accepted work to its receiving generation, never a later default.
    /// Explicitly unmanaged repositories retain their original store.
    pub(crate) async fn pinned(
        &self,
        lease: Option<&crate::client::TransportLease>,
    ) -> Result<Self, ServerError> {
        match &self.transport {
            RepositoryTransport::Fixed { .. } => Ok(self.clone()),
            RepositoryTransport::Managed { bucket, .. } => {
                let lease = lease.ok_or_else(|| {
                    ServerError::Nats(
                        "operation repository is missing its accepting transport generation"
                            .to_owned(),
                    )
                })?;
                let store = async_nats::jetstream::new(lease.nats().clone())
                    .get_key_value(bucket)
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                Ok(Self {
                    transport: RepositoryTransport::Fixed {
                        store: Box::new(store),
                        lease: Some(lease.clone()),
                    },
                })
            }
        }
    }

    async fn store(&self) -> Result<RepositoryStore, ServerError> {
        match &self.transport {
            RepositoryTransport::Fixed { store, lease } => Ok(RepositoryStore {
                store: (**store).clone(),
                _lease: lease.clone(),
            }),
            RepositoryTransport::Managed {
                manager,
                bucket,
                timeout_ms,
            } => {
                // Operation buckets are runtime-provisioned and authorized on
                // each application generation, like operation staging buckets.
                let lease = manager
                    .acquire_for(
                        &[],
                        &[],
                        std::time::Instant::now() + std::time::Duration::from_millis(*timeout_ms),
                    )
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                let store = async_nats::jetstream::new(lease.nats().clone())
                    .get_key_value(bucket)
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                Ok(RepositoryStore {
                    store,
                    _lease: Some(lease),
                })
            }
        }
    }

    async fn load(
        store: &kv::Store,
        invocation_id: &str,
    ) -> Result<Option<RevisionedOperationRecord>, ServerError> {
        let Some(entry) = store
            .entry(invocation_id)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(RevisionedOperationRecord {
            record: decode_operation_record(&entry.value)?,
            revision: entry.revision,
        }))
    }
}

impl OperationRepository for KvOperationRepository {
    async fn get(
        &self,
        invocation_id: &str,
    ) -> Result<Option<RevisionedOperationRecord>, ServerError> {
        let guard = self.store().await?;
        Self::load(&guard.store, invocation_id).await
    }

    async fn create(
        &self,
        record: DurableOperationRecord,
    ) -> Result<RevisionedOperationRecord, ServerError> {
        record.validate()?;
        let guard = self.store().await?;
        let bytes = encode_operation_record(&record)?.into();
        match guard
            .store
            .create(record.invocation_id.clone(), bytes)
            .await
        {
            Ok(revision) => Ok(RevisionedOperationRecord { record, revision }),
            Err(_) => {
                for _ in 0..8 {
                    let existing = Self::load(&guard.store, &record.invocation_id)
                        .await?
                        .ok_or_else(|| {
                            ServerError::Nats(
                                "operation acceptance lost its KV create race".to_owned(),
                            )
                        })?;
                    if !existing.record.same_invocation(&record) {
                        return Err(ServerError::OperationIdempotencyConflict {
                            kind: "invocation",
                            request_id: record.invocation_id,
                        });
                    }
                    if !record.cancellation_requested
                        || existing.record.cancellation_requested
                        || existing.record.snapshot.state.is_terminal()
                    {
                        return Ok(existing);
                    }
                    let mut cancelled = existing.record;
                    cancelled.revision += 1;
                    cancelled.cancellation_requested = true;
                    cancelled.cancellation_requested_at = record.cancellation_requested_at.clone();
                    cancelled.snapshot.updated_at = record.snapshot.updated_at.clone();
                    if let Ok(updated) = Self::new(guard.store.clone())
                        .compare_record_exchange(existing.revision, cancelled)
                        .await
                    {
                        return Ok(updated);
                    }
                }
                Err(ServerError::Nats(
                    "cancelled admission exceeded CAS retry limit".to_owned(),
                ))
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
        let guard = self.store().await?;
        for _ in 0..8 {
            let current = Self::load(&guard.store, invocation_id)
                .await?
                .ok_or_else(|| {
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
            let bytes = encode_operation_record(&record)?.into();
            if let Ok(revision) = guard
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
        let guard = self.store().await?;
        for _ in 0..8 {
            let current = Self::load(&guard.store, invocation_id)
                .await?
                .ok_or_else(|| {
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
            if let Ok(revision) = guard
                .store
                .update(
                    invocation_id,
                    encode_operation_record(&record)?.into(),
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
        let guard = self.store().await?;
        let current = Self::load(&guard.store, &record.invocation_id)
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
            || record.creator_binding != current.record.creator_binding
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
        let revision = guard
            .store
            .update(
                record.invocation_id.clone(),
                encode_operation_record(&record)?.into(),
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
        let guard = self.store().await?;
        let current = Self::load(&guard.store, &record.invocation_id)
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
            || record.creator_binding != current.record.creator_binding
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
        let revision = guard
            .store
            .update(
                record.invocation_id.clone(),
                encode_operation_record(&record)?.into(),
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
        let guard = self.store().await?;
        let watcher = guard
            .store
            .watch(invocation_id)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let snapshot = Self::load(&guard.store, invocation_id)
            .await?
            .ok_or_else(|| ServerError::Nats("operation invocation was not found".to_owned()))?;
        let snapshot_revision = snapshot.revision;
        let stream = Box::pin(
            futures_util::stream::once(async move { Ok(snapshot) }).chain(watcher.filter_map(
                move |entry| async move {
                    match entry {
                        Ok(entry) if entry.revision > snapshot_revision => Some(
                            decode_operation_record(&entry.value)
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
        );
        // Carry the selected generation through the snapshot and all watch
        // items. True EOF or dropping the stream releases it; an Err item does not.
        Ok(
            futures_util::stream::unfold((stream, guard), |(mut stream, guard)| async move {
                stream.next().await.map(|item| (item, (stream, guard)))
            })
            .boxed(),
        )
    }

    async fn list_nonterminal(&self) -> Result<Vec<RevisionedOperationRecord>, ServerError> {
        let guard = self.store().await?;
        let mut keys = guard
            .store
            .keys()
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        let mut records = Vec::new();
        while let Some(key) = keys.next().await {
            let key = key.map_err(|error| ServerError::Nats(error.to_string()))?;
            if let Some(record) = Self::load(&guard.store, &key).await? {
                if !record.record.snapshot.state.is_terminal() {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }
}
