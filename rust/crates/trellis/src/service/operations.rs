use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::{self, BoxStream};
use futures_util::{Stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use trellis_protocol::{ApiSurfaceKind, PermissionAction};

use super::{
    operation_invocation_digest, DurableOperationRecord, DurableOperationSignal, FileTransferInfo,
    KvOperationRepository, OperationRepository, RequestContext, RequestValidator,
    RevisionedOperationRecord, ServerError, UploadTransferGrant, UploadTransferGrantPlan,
    UploadTransferSession,
};

use super::resources::backend::BoundStoreResourceClient;
use super::resources::StoreResourceClient;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[doc = concat!("Public Trellis value set `", stringify!(OperationState), "`.")]
pub enum OperationState {
    /// Accepted but not yet running.
    Pending,
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Terminated with an error.
    Failed,
    /// Canceled before successful completion.
    Cancelled,
}

impl OperationState {
    /// Return whether this state ends an operation lifecycle.
    #[doc = concat!("Trellis API operation `", stringify!(is_terminal), "`.")]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OperationState::Completed | OperationState::Failed | OperationState::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(OperationRefData), "`.")]
pub struct OperationRefData {
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: String,
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(operation), "` value.")]
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// Current persisted state of an operation.
pub struct OperationSnapshot<TProgress = Value, TOutput = Value> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(operation), "` value.")]
    pub operation: Option<String>,
    #[doc = concat!("The `", stringify!(revision), "` value.")]
    pub revision: u64,
    #[doc = concat!("The `", stringify!(state), "` value.")]
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(created_at), "` value.")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(updated_at), "` value.")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(completed_at), "` value.")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(progress), "` value.")]
    pub progress: Option<TProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(transfer), "` value.")]
    pub transfer: Option<OperationTransferProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(output), "` value.")]
    pub output: Option<TOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(error), "` value.")]
    pub error: Option<OperationError>,
}

impl<TProgress, TOutput> Default for OperationSnapshot<TProgress, TOutput> {
    fn default() -> Self {
        Self {
            id: None,
            service: None,
            operation: None,
            revision: 0,
            state: OperationState::Pending,
            created_at: None,
            updated_at: None,
            completed_at: None,
            progress: None,
            transfer: None,
            output: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(OperationError), "`.")]
pub struct OperationError {
    #[serde(rename = "type")]
    #[doc = concat!("The `", stringify!(error_type), "` value.")]
    pub error_type: String,
    #[doc = concat!("The `", stringify!(message), "` value.")]
    pub message: String,
}

/// A typed operation failure payload that can be serialized into the operation snapshot.
pub trait OperationFailureLike: Send + 'static {
    /// The wire error type discriminator (e.g. "NotFoundError").
    fn error_type(&self) -> &str;
    /// Human-facing error message.
    fn message(&self) -> String;
    /// Additional structured fields for the error payload.
    fn fields(&self) -> serde_json::Map<String, Value>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(OperationFailure), "`.")]
pub struct OperationFailure {
    #[doc = concat!("The `", stringify!(message), "` value.")]
    pub message: String,
}

impl OperationFailureLike for OperationFailure {
    fn error_type(&self) -> &str {
        "OperationFailure"
    }
    fn message(&self) -> String {
        self.message.clone()
    }
    fn fields(&self) -> serde_json::Map<String, Value> {
        serde_json::Map::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(OperationTransferProgress), "`.")]
pub struct OperationTransferProgress {
    /// Zero-based transfer chunk index.
    #[doc = concat!("The `", stringify!(chunk_index), "` value.")]
    pub chunk_index: u64,
    /// Number of bytes carried by this chunk.
    #[doc = concat!("The `", stringify!(chunk_bytes), "` value.")]
    pub chunk_bytes: u64,
    /// Total number of bytes transferred after this chunk.
    #[doc = concat!("The `", stringify!(transferred_bytes), "` value.")]
    pub transferred_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[doc(hidden)]
pub struct AcceptedOperation<TProgress = Value, TOutput = Value> {
    #[doc = concat!("The `", stringify!(kind), "` value.")]
    pub kind: String,
    #[serde(rename = "ref")]
    #[doc = concat!("The `", stringify!(operation_ref), "` value.")]
    pub operation_ref: OperationRefData,
    #[doc = concat!("The `", stringify!(snapshot), "` value.")]
    pub snapshot: OperationSnapshot<TProgress, TOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(transfer), "` value.")]
    pub transfer: Option<UploadTransferGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[doc(hidden)]
pub struct OperationSnapshotFrame<TProgress = Value, TOutput = Value> {
    #[doc = concat!("The `", stringify!(kind), "` value.")]
    pub kind: String,
    #[doc = concat!("The `", stringify!(snapshot), "` value.")]
    pub snapshot: OperationSnapshot<TProgress, TOutput>,
}

/// Signal accepted for delivery to an operation provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(OperationSignal), "`.")]
pub struct OperationSignal {
    #[doc = concat!("The `", stringify!(operation_id), "` value.")]
    pub operation_id: String,
    #[doc = concat!("The `", stringify!(signal), "` value.")]
    pub signal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(input), "` value.")]
    pub input: Option<Value>,
    #[doc = concat!("The `", stringify!(signal_sequence), "` value.")]
    pub signal_sequence: u64,
    #[doc = concat!("The `", stringify!(accepted_at), "` value.")]
    pub accepted_at: String,
}

/// Acknowledgement frame returned to callers after a signal is accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OperationSignalAccepted<TProgress = Value, TOutput = Value> {
    #[doc = concat!("The `", stringify!(kind), "` value.")]
    pub kind: String,
    #[doc = concat!("The `", stringify!(operation_id), "` value.")]
    pub operation_id: String,
    #[doc = concat!("The `", stringify!(signal), "` value.")]
    pub signal: String,
    #[doc = concat!("The `", stringify!(signal_sequence), "` value.")]
    pub signal_sequence: u64,
    #[doc = concat!("The `", stringify!(accepted_at), "` value.")]
    pub accepted_at: String,
    #[doc = concat!("The `", stringify!(snapshot), "` value.")]
    pub snapshot: OperationSnapshot<TProgress, TOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(OperationControlRequest), "`.")]
pub struct OperationControlRequest {
    #[doc = concat!("The `", stringify!(action), "` value.")]
    pub action: String,
    #[doc = concat!("The `", stringify!(operation_id), "` value.")]
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(signal), "` value.")]
    pub signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(input), "` value.")]
    pub input: Option<Value>,
    /// Whether this watch request opts in to live-only updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[doc = concat!("The `", stringify!(include_updates), "` value.")]
    pub include_updates: Option<bool>,
}

/// One item in an opt-in operation stream that includes live updates.
#[derive(Debug, Clone, PartialEq)]
pub enum OperationLiveEvent<TProgress = Value, TUpdate = Value, TOutput = Value> {
    /// Durable lifecycle snapshot.
    Snapshot(OperationSnapshot<TProgress, TOutput>),
    /// Live-only update that never mutates the durable snapshot.
    Update(crate::client::OperationUpdateEvent<TUpdate>),
}

#[doc(hidden)]
pub trait OperationDescriptor {
    type Input: Serialize + DeserializeOwned + Send + 'static;
    type Progress: Serialize + Send + 'static;
    type Output: Serialize + Send + 'static;
    type Update: Serialize + DeserializeOwned + Send + 'static;
    type UpdateEvidence: crate::client::OperationUpdateEvidence;
    type Error: OperationFailureLike + Send + 'static;

    const API_ID: &'static str;
    const KEY: &'static str;
    const SUBJECT: &'static str;
    const CALLER_CAPABILITIES: &'static [&'static str] = &[];
    const OBSERVE_CAPABILITIES: &'static [&'static str] = Self::CALLER_CAPABILITIES;
    const CANCEL_CAPABILITIES: &'static [&'static str] = &[];
    const CONTROL_CAPABILITIES: &'static [&'static str] = &[];
    const CANCELABLE: bool;
    const UPLOAD: bool = false;
    const ERRORS: &'static [&'static str] = &[];
    const INPUT_SCHEMA_JSON: &'static str;
    const PROGRESS_SCHEMA_JSON: Option<&'static str>;
    const OUTPUT_SCHEMA_JSON: &'static str;
    const UPDATE_SCHEMA_JSON: Option<&'static str>;
    const SIGNAL_INPUT_SCHEMAS_JSON: &'static str;
}

impl<D> OperationDescriptor for D
where
    D: crate::client::OperationDescriptor,
    D::Input: DeserializeOwned + Send + 'static,
    D::Progress: Serialize + Send + 'static,
    D::Output: Serialize + Send + 'static,
    D::Error: OperationFailureLike,
{
    type Input = D::Input;
    type Progress = D::Progress;
    type Output = D::Output;
    type Update = D::Update;
    type UpdateEvidence = D::UpdateEvidence;
    type Error = D::Error;

    const API_ID: &'static str = D::API_ID;
    const KEY: &'static str = D::KEY;
    const SUBJECT: &'static str = D::SUBJECT;
    const CALLER_CAPABILITIES: &'static [&'static str] = D::CALLER_CAPABILITIES;
    const OBSERVE_CAPABILITIES: &'static [&'static str] = D::OBSERVE_CAPABILITIES;
    const CANCEL_CAPABILITIES: &'static [&'static str] = D::CANCEL_CAPABILITIES;
    const CONTROL_CAPABILITIES: &'static [&'static str] = D::CONTROL_CAPABILITIES;
    const CANCELABLE: bool = D::CANCELABLE;
    const UPLOAD: bool = <D as crate::client::OperationDescriptor>::UPLOAD;
    const ERRORS: &'static [&'static str] = D::ERRORS;
    const INPUT_SCHEMA_JSON: &'static str = D::INPUT_SCHEMA_JSON;
    const PROGRESS_SCHEMA_JSON: Option<&'static str> = D::PROGRESS_SCHEMA_JSON;
    const OUTPUT_SCHEMA_JSON: &'static str = D::OUTPUT_SCHEMA_JSON;
    const UPDATE_SCHEMA_JSON: Option<&'static str> = D::UPDATE_SCHEMA_JSON;
    const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = D::SIGNAL_INPUT_SCHEMAS_JSON;
}

type AcceptedOperationFuture<D> = BoxFuture<
    'static,
    Result<
        AcceptedOperation<<D as OperationDescriptor>::Progress, <D as OperationDescriptor>::Output>,
        ServerError,
    >,
>;
type OperationSnapshotFuture<D> = BoxFuture<
    'static,
    Result<
        OperationSnapshot<<D as OperationDescriptor>::Progress, <D as OperationDescriptor>::Output>,
        ServerError,
    >,
>;
/// Stream returned by operation providers for snapshots and live updates.
pub type OperationLiveWatch<TProgress, TUpdate, TOutput> = Pin<
    Box<
        dyn Stream<Item = Result<OperationLiveEvent<TProgress, TUpdate, TOutput>, ServerError>>
            + Send,
    >,
>;
type OperationSignalFuture<D> = BoxFuture<
    'static,
    Result<
        OperationSignalAccepted<
            <D as OperationDescriptor>::Progress,
            <D as OperationDescriptor>::Output,
        >,
        ServerError,
    >,
>;

/// Descriptor-backed operation handler registered by owning services.
pub(crate) trait ServiceOperationProvider<D>: Send + Sync + 'static
where
    D: OperationDescriptor,
{
    /// Reclaim and resume expired non-terminal invocations during service startup.
    fn recover(&self) -> BoxFuture<'static, Result<(), ServerError>> {
        Box::pin(async { Ok(()) })
    }
    /// Start a new operation instance from the decoded input.
    fn start(&self, context: RequestContext, input: D::Input) -> AcceptedOperationFuture<D>;

    /// Start or replay one caller-selected idempotent invocation id.
    fn start_invocation(
        &self,
        context: RequestContext,
        _invocation_id: String,
        input: D::Input,
    ) -> AcceptedOperationFuture<D> {
        self.start(context, input)
    }

    /// Return the current snapshot for an operation id.
    fn get(&self, context: RequestContext, operation_id: String) -> OperationSnapshotFuture<D>;

    /// Wait until the operation reaches a terminal snapshot.
    fn wait(&self, context: RequestContext, operation_id: String) -> OperationSnapshotFuture<D>;

    /// Stream operation snapshots and optional live updates.
    ///
    /// The default preserves one-shot wait behavior for providers without a live stream.
    fn watch(
        &self,
        context: RequestContext,
        operation_id: String,
    ) -> OperationLiveWatch<D::Progress, D::Update, D::Output> {
        Box::pin(
            futures_util::stream::once(self.wait(context, operation_id))
                .map(|snapshot| snapshot.map(OperationLiveEvent::Snapshot)),
        )
    }

    /// Cancel an operation id and return the resulting snapshot.
    fn cancel(
        &self,
        _context: RequestContext,
        _operation_id: String,
    ) -> OperationSnapshotFuture<D> {
        Box::pin(async {
            Err(ServerError::InvalidOperationControlAction {
                subject: D::SUBJECT.to_owned(),
                action: "cancel".to_owned(),
            })
        })
    }

    /// Apply a named operation signal.
    ///
    /// Providers without signal support return `InvalidOperationControlAction`.
    fn signal(
        &self,
        _context: RequestContext,
        _operation_id: String,
        _signal: String,
        _input: Option<Value>,
    ) -> OperationSignalFuture<D> {
        Box::pin(async {
            Err(ServerError::InvalidOperationControlAction {
                subject: D::SUBJECT.to_owned(),
                action: "signal".to_owned(),
            })
        })
    }
}

pub(crate) struct RuntimeOperationProvider<D: OperationDescriptor, F, V> {
    service: String,
    provider_participant_id: String,
    deployment_id: String,
    executor_id: String,
    connection_id: String,
    repository: KvOperationRepository,
    mutation_gate: Arc<Mutex<()>>,
    next_update_sequence: Arc<AtomicU64>,
    handler: Arc<F>,
    nats: async_nats::Client,
    service_session_key: String,
    staging: BoundStoreResourceClient,
    validator: V,
    publisher: Option<Arc<crate::client::TrellisClient>>,
    _descriptor: PhantomData<fn() -> D>,
}

#[doc(hidden)]
pub struct OperationHandlerRuntime<V> {
    pub service: String,
    pub deployment_id: String,
    pub executor_id: String,
    pub connection_id: String,
    pub repository: KvOperationRepository,
    pub nats: async_nats::Client,
    pub service_session_key: String,
    pub staging: BoundStoreResourceClient,
    pub validator: V,
}

impl<D, F, V> RuntimeOperationProvider<D, F, V>
where
    D: OperationDescriptor + 'static,
{
    #[cfg(any(feature = "runtime-internals", test))]
    pub(crate) fn new(runtime: OperationHandlerRuntime<V>, handler: F) -> Self {
        Self::new_authenticated(runtime, handler, String::new(), None)
    }

    pub(crate) fn new_authenticated(
        runtime: OperationHandlerRuntime<V>,
        handler: F,
        provider_participant_id: String,
        publisher: Option<Arc<crate::client::TrellisClient>>,
    ) -> Self {
        Self {
            service: runtime.service,
            provider_participant_id,
            deployment_id: runtime.deployment_id,
            executor_id: runtime.executor_id,
            connection_id: runtime.connection_id,
            repository: runtime.repository,
            mutation_gate: Arc::new(Mutex::new(())),
            next_update_sequence: Arc::new(AtomicU64::new(1)),
            handler: Arc::new(handler),
            nats: runtime.nats,
            service_session_key: runtime.service_session_key,
            staging: runtime.staging,
            validator: runtime.validator,
            publisher,
            _descriptor: PhantomData,
        }
    }

    fn authorize_record(
        deployment_id: &str,
        context: &RequestContext,
        record: &DurableOperationRecord,
        action: PermissionAction,
    ) -> Result<(), ServerError> {
        let caller = context
            .caller
            .as_ref()
            .ok_or_else(|| ServerError::RequestDenied {
                subject: context.subject.clone(),
                session_key: context.session_key.clone().unwrap_or_default(),
            })?;
        let name = D::KEY.split_once('.').map_or(D::KEY, |(_, name)| name);
        let permission_matches = context
            .required_permission
            .as_ref()
            .is_some_and(|permission| {
                permission.api == D::API_ID
                    && permission.surface == ApiSurfaceKind::Operation
                    && permission.name == name
                    && permission.action == action
            });
        if record.api_id == D::API_ID
            && record.operation == D::KEY
            && record.deployment_id == deployment_id
            && !record.creator_principal_id.is_empty()
            && !record.creator_participant_id.is_empty()
            && caller.principal_id == record.creator_principal_id
            && caller.participant_id == record.creator_participant_id
            && permission_matches
        {
            Ok(())
        } else {
            Err(ServerError::RequestDenied {
                subject: context.subject.clone(),
                session_key: caller.session_key.clone(),
            })
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "resumption restores one runtime-owned handle"
    )]
    async fn resume<Fut>(
        service: String,
        deployment_id: String,
        fence: OwnerFence,
        repository: KvOperationRepository,
        mutation_gate: Arc<Mutex<()>>,
        handler: Arc<F>,
        nats: async_nats::Client,
        publisher: Option<Arc<crate::client::TrellisClient>>,
        update_subject: String,
        next_update_sequence: Arc<AtomicU64>,
        claimed: super::RevisionedOperationRecord,
    ) -> Result<(), ServerError>
    where
        D::Input: DeserializeOwned,
        D::Progress: DeserializeOwned,
        D::Output: DeserializeOwned,
        F: Fn(RequestContext, D::Input, OperationControl<D>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    {
        let claimed = admit_operation_execution::<D>(
            &repository,
            &mutation_gate,
            &fence,
            &deployment_id,
            &claimed.record.invocation_id,
        )
        .await?
        .ok_or_else(|| ServerError::Nats("operation owner fence is stale".to_owned()))?;
        if claimed.record.cancellation_requested {
            finalize_cancellation::<D>(
                &repository,
                &fence,
                &mutation_gate,
                &claimed.record.invocation_id,
            )
            .await?;
            return Ok(());
        }
        let mut cancellation = repository.watch(&claimed.record.invocation_id).await?;
        let input = serde_json::from_value(claimed.record.input.clone())?;
        tokio::spawn(async move {
            // Final entry admission after asynchronous setup: the captured fence,
            // identity, lease, and nonterminal state must still hold immediately
            // before user code runs. The mutation gate only fences this check.
            let admitted = admit_operation_execution::<D>(
                &repository,
                &mutation_gate,
                &fence,
                &deployment_id,
                &claimed.record.invocation_id,
            )
            .await
            .ok()
            .flatten();
            let Some(admitted) = admitted else {
                return;
            };
            if admitted.record.cancellation_requested {
                let _ = finalize_cancellation::<D>(
                    &repository,
                    &fence,
                    &mutation_gate,
                    &claimed.record.invocation_id,
                )
                .await;
                return;
            }
            let context = RequestContext {
                resuming: admitted.record.owner_epoch > 1,
                operation_progress: admitted.record.snapshot.progress.clone(),
                session_key: Some(admitted.record.caller_session_key),
                caller: admitted.record.caller,
                ..RequestContext::default()
            };
            let control = OperationControl {
                operation_ref: OperationRefData {
                    id: claimed.record.invocation_id.clone(),
                    service,
                    operation: D::KEY.to_owned(),
                },
                durable: DurableOperationControl {
                    repository: repository.clone(),
                    fence: fence.clone(),
                    mutation_gate: Arc::clone(&mutation_gate),
                },
                nats,
                publisher,
                update_subject,
                next_update_sequence,
                _descriptor: PhantomData,
            };
            let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
            heartbeat.tick().await;
            let mut execution = Box::pin(handler(context, input, control));
            loop {
                tokio::select! {
                    result = &mut execution => {
                        drop(execution);
                        let cancellation_requested = repository
                            .get(&claimed.record.invocation_id)
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|current| current.record.cancellation_requested);
                        if cancellation_requested {
                            let _ = finalize_cancellation::<D>(
                                &repository,
                                &fence,
                                &mutation_gate,
                                &claimed.record.invocation_id,
                            ).await;
                        } else if let Err(error) = result {
                            tracing::error!(%error, "operation handler failed");
                            let _ = durable_snapshot_update::<D>(
                                &repository,
                                &fence,
                                &mutation_gate,
                                &claimed.record.invocation_id,
                                SnapshotUpdate {
                                    state: OperationState::Failed,
                                    progress: None,
                                    output: None,
                                    error: Some(OperationError {
                                        error_type: "OperationHandlerError".to_owned(),
                                        message: error.to_string(),
                                    }),
                                    cancellation_requested: false,
                                },
                            ).await;
                        }
                        break;
                    }
                    changed = cancellation.next() => {
                        match changed {
                            Some(Ok(current)) if current.record.cancellation_requested => {
                                drop(execution);
                                let _ = finalize_cancellation::<D>(
                                    &repository,
                                    &fence,
                                    &mutation_gate,
                                    &claimed.record.invocation_id,
                                ).await;
                                break;
                            }
                            Some(Ok(current)) if fence.matches(&current.record) => {}
                            Some(Ok(_)) => break,
                            Some(Err(error)) => {
                                tracing::warn!(%error, "operation cancellation watch failed");
                                break;
                            }
                            None => break,
                        }
                    }
                    _ = heartbeat.tick() => {
                        let _guard = mutation_gate.lock().await;
                        let now = now_ms();
                        if repository.renew(&claimed.record.invocation_id, &fence.executor_id, fence.owner_epoch, now, now + 30_000).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(())
    }
}

impl<D, F, Fut, V> ServiceOperationProvider<D> for RuntimeOperationProvider<D, F, V>
where
    D: OperationDescriptor + 'static,
    D::Progress: DeserializeOwned,
    D::Output: DeserializeOwned,
    F: Fn(RequestContext, D::Input, OperationControl<D>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), ServerError>> + Send + 'static,
    V: RequestValidator + Clone + 'static,
{
    fn recover(&self) -> BoxFuture<'static, Result<(), ServerError>> {
        let service = self.service.clone();
        let deployment_id = self.deployment_id.clone();
        let executor_id = self.executor_id.clone();
        let connection_id = self.connection_id.clone();
        let repository = self.repository.clone();
        let mutation_gate = Arc::clone(&self.mutation_gate);
        let handler = Arc::clone(&self.handler);
        let staging = self.staging.clone();
        let nats = self.nats.clone();
        let publisher = self.publisher.clone();
        let next_update_sequence = Arc::clone(&self.next_update_sequence);
        Box::pin(async move {
            let mut records = repository.list_nonterminal().await?;
            tokio::spawn(async move {
                loop {
                    for record in records {
                        if record.record.deployment_id != deployment_id
                            || record.record.api_id != D::API_ID
                            || record.record.operation != D::KEY
                            || record.record.creator_principal_id.is_empty()
                            || record.record.creator_participant_id.is_empty()
                            || record.record.caller.as_ref().is_none_or(|caller| {
                                caller.principal_id != record.record.creator_principal_id
                                    || caller.participant_id != record.record.creator_participant_id
                            })
                            || record
                                .record
                                .lease_expires_at_ms
                                .is_some_and(|expiry| expiry > now_ms())
                        {
                            continue;
                        }
                        if D::UPLOAD {
                            let Some(upload) = record.record.transfer.as_ref().and_then(|value| {
                                serde_json::from_value::<DurableOperationUpload>(value.clone()).ok()
                            }) else {
                                continue;
                            };
                            if upload.state != "committed"
                                && OffsetDateTime::parse(&upload.expires_at, &Rfc3339)
                                    .ok()
                                    .map(|expiry| {
                                        expiry
                                            + time::Duration::hours(23)
                                            + time::Duration::minutes(45)
                                    })
                                    .is_none_or(|cleanup| cleanup > OffsetDateTime::now_utc())
                            {
                                continue;
                            }
                        }
                        let operation_id = record.record.invocation_id;
                        let repository = repository.clone();
                        let deployment_id_for_resume = deployment_id.clone();
                        let executor_id = executor_id.clone();
                        let connection_id = connection_id.clone();
                        let service = service.clone();
                        let mutation_gate = Arc::clone(&mutation_gate);
                        let handler = Arc::clone(&handler);
                        let nats = nats.clone();
                        let publisher = publisher.clone();
                        let update_subject =
                            operation_update_subject::<D>(&deployment_id, &operation_id);
                        let next_update_sequence = Arc::clone(&next_update_sequence);
                        let staging = staging.clone();
                        tokio::spawn(async move {
                            let now = now_ms();
                            if let Ok(claimed) = repository
                                .claim_for_connection(
                                    &operation_id,
                                    &executor_id,
                                    &connection_id,
                                    now,
                                    now + 30_000,
                                )
                                .await
                            {
                                let reconciled = async {
                                    if !D::UPLOAD {
                                        return Ok(claimed);
                                    }
                                    let mut claimed = claimed;
                                    let mut upload: DurableOperationUpload =
                                        serde_json::from_value(
                                            claimed.record.transfer.clone().ok_or_else(|| {
                                                ServerError::Nats(
                                                    "operation upload staging state is missing"
                                                        .to_owned(),
                                                )
                                            })?,
                                        )?;
                                    if upload.state == "committed" {
                                        let objects = staging.list_objects().await?;
                                        if objects.into_iter().any(|object| {
                                             object.key == upload.staging_key
                                                 && Some(object.size) == upload.size
                                                 && object.digest.as_deref().is_some_and(|digest| {
                                                     upload.digest.as_deref().is_some_and(|expected| {
                                                         super::transfer::transfer_digests_match(
                                                             digest, expected,
                                                         )
                                                     })
                                                 })
                                        }) {
                                            return Ok(claimed);
                                        }
                                        return Err(ServerError::Nats(
                                            "committed operation upload metadata does not match staging"
                                                .to_owned(),
                                        ));
                                    }
                                    let cleanup_at =
                                        OffsetDateTime::parse(&upload.expires_at, &Rfc3339)
                                            .ok()
                                            .map(|expiry| {
                                                expiry
                                                    + time::Duration::hours(23)
                                                    + time::Duration::minutes(45)
                                            });
                                    if cleanup_at
                                        .is_none_or(|cleanup| cleanup > OffsetDateTime::now_utc())
                                    {
                                        return Err(ServerError::Nats(
                                            "operation upload is not committed".to_owned(),
                                        ));
                                    }
                                    upload.state = "invalid".to_owned();
                                    upload.size = None;
                                    upload.digest = None;
                                    upload.updated_at = None;
                                    let staging_key = upload.staging_key.clone();
                                    let fence = OwnerFence::from(&claimed.record)?;
                                    claimed.record.revision += 1;
                                    claimed.record.lease_expires_at_ms = Some(now_ms());
                                    claimed.record.transfer = Some(serde_json::to_value(upload)?);
                                    repository
                                        .compare_exchange(
                                            claimed.revision,
                                            &fence.executor_id,
                                            fence.owner_epoch,
                                            claimed.record,
                                        )
                                        .await?;
                                    let _ = staging.delete(&staging_key).await;
                                    Err(ServerError::Nats(
                                        "operation upload is not committed".to_owned(),
                                    ))
                                }
                                .await;
                                let claimed = match reconciled {
                                    Ok(claimed) => claimed,
                                    Err(error) => {
                                        tracing::error!(%error, %operation_id, "operation upload recovery failed");
                                        return;
                                    }
                                };
                                let fence = match OwnerFence::from(&claimed.record) {
                                    Ok(fence) => fence,
                                    Err(error) => {
                                        tracing::error!(%error, %operation_id, "operation upload recovery fence is invalid");
                                        return;
                                    }
                                };
                                if let Err(error) = Self::resume(
                                    service,
                                    deployment_id_for_resume,
                                    fence,
                                    repository,
                                    mutation_gate,
                                    handler,
                                    nats,
                                    publisher,
                                    update_subject,
                                    next_update_sequence,
                                    claimed,
                                )
                                .await
                                {
                                    tracing::error!(%error, %operation_id, "operation recovery failed");
                                }
                            }
                        });
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    records = match repository.list_nonterminal().await {
                        Ok(records) => records,
                        Err(error) => {
                            tracing::warn!(%error, "operation survivor discovery failed");
                            Vec::new()
                        }
                    };
                }
            });
            Ok(())
        })
    }

    fn start(&self, context: RequestContext, input: D::Input) -> AcceptedOperationFuture<D> {
        self.start_invocation(context, ulid::Ulid::new().to_string(), input)
    }

    fn start_invocation(
        &self,
        context: RequestContext,
        invocation_id: String,
        input: D::Input,
    ) -> AcceptedOperationFuture<D> {
        let service = self.service.clone();
        let deployment_id = self.deployment_id.clone();
        let executor_id = self.executor_id.clone();
        let connection_id = self.connection_id.clone();
        let repository = self.repository.clone();
        let mutation_gate = Arc::clone(&self.mutation_gate);
        let handler = Arc::clone(&self.handler);
        let nats = self.nats.clone();
        let publisher = self.publisher.clone();
        let service_session_key = self.service_session_key.clone();
        let staging = self.staging.clone();
        let validator = self.validator.clone();
        let next_update_sequence = Arc::clone(&self.next_update_sequence);
        let update_subject = operation_update_subject::<D>(&deployment_id, &invocation_id);
        Box::pin(async move {
            let caller = context.caller.as_ref().ok_or_else(|| {
                ServerError::Nats("operation start is missing verified caller".to_owned())
            })?;
            let input_value = serde_json::to_value(&input)?;
            let digest = operation_invocation_digest(
                D::API_ID,
                D::KEY,
                &caller.principal_id,
                &caller.participant_id,
                &input_value,
            )?;
            let timestamp = now_timestamp();
            let upload = if D::UPLOAD {
                let transfer_id = ulid::Ulid::new().to_string();
                let expires_at = (OffsetDateTime::now_utc() + time::Duration::minutes(15))
                    .format(&Rfc3339)
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                Some(DurableOperationUpload {
                    staging_key: format!("operation-{invocation_id}"),
                    state: "pending".to_owned(),
                    subject: super::transfer::transfer_subject(
                        super::transfer::upload_subject_prefix(),
                        &service_session_key,
                        &transfer_id,
                    ),
                    transfer_id,
                    expires_at,
                    size: None,
                    digest: None,
                    updated_at: None,
                    content_type: None,
                })
            } else {
                None
            };
            let created = repository
                .create(DurableOperationRecord {
                    invocation_id: invocation_id.clone(),
                    invocation_digest: digest,
                    api_id: D::API_ID.to_owned(),
                    operation: D::KEY.to_owned(),
                    deployment_id: deployment_id.clone(),
                    creator_principal_id: caller.principal_id.clone(),
                    creator_participant_id: caller.participant_id.clone(),
                    caller_session_key: caller.session_key.clone(),
                    caller: Some(caller.clone()),
                    input: input_value,
                    snapshot: OperationSnapshot {
                        id: Some(invocation_id.clone()),
                        service: Some(service.clone()),
                        operation: Some(D::KEY.to_owned()),
                        revision: 1,
                        state: OperationState::Pending,
                        created_at: Some(timestamp.clone()),
                        updated_at: Some(timestamp),
                        ..OperationSnapshot::default()
                    },
                    revision: 1,
                    owner_executor_id: None,
                    owner_connection_id: None,
                    owner_epoch: 0,
                    lease_expires_at_ms: None,
                    cancellation_requested: false,
                    next_signal_sequence: 1,
                    signals: Vec::new(),
                    transfer: upload.as_ref().map(serde_json::to_value).transpose()?,
                })
                .await?;
            if created.record.snapshot.state.is_terminal() {
                return Ok(AcceptedOperation {
                    kind: "accepted".to_owned(),
                    operation_ref: OperationRefData {
                        id: invocation_id,
                        service,
                        operation: D::KEY.to_owned(),
                    },
                    snapshot: typed_snapshot(created.record.snapshot)?,
                    transfer: None,
                });
            }
            let now = now_ms();
            if created
                .record
                .lease_expires_at_ms
                .is_some_and(|expires| expires > now)
            {
                let transfer = created
                    .record
                    .transfer
                    .clone()
                    .map(serde_json::from_value::<DurableOperationUpload>)
                    .transpose()?
                    .filter(|upload| upload.state != "committed")
                    .map(|upload| operation_upload_grant(&service, &caller.session_key, &upload));
                return Ok(AcceptedOperation {
                    kind: "accepted".to_owned(),
                    operation_ref: OperationRefData {
                        id: invocation_id,
                        service,
                        operation: D::KEY.to_owned(),
                    },
                    snapshot: typed_snapshot(created.record.snapshot)?,
                    transfer,
                });
            }
            let claimed = repository
                .claim_for_connection(
                    &invocation_id,
                    &executor_id,
                    &connection_id,
                    now,
                    now + 30_000,
                )
                .await?;
            let fence = OwnerFence::from(&claimed.record)?;
            if D::UPLOAD {
                let mut upload: DurableOperationUpload =
                    serde_json::from_value(claimed.record.transfer.clone().ok_or_else(|| {
                        ServerError::Nats("operation upload staging state is missing".to_owned())
                    })?)?;
                if upload.state == "committed" {
                    durable_snapshot_update::<D>(
                        &repository,
                        &fence,
                        &mutation_gate,
                        &invocation_id,
                        SnapshotUpdate {
                            state: OperationState::Running,
                            progress: None,
                            output: None,
                            error: None,
                            cancellation_requested: false,
                        },
                    )
                    .await?;
                    let claimed = repository.get(&invocation_id).await?.ok_or_else(|| {
                        ServerError::OperationNotFound {
                            operation_id: invocation_id.clone(),
                        }
                    })?;
                    let accepted = AcceptedOperation {
                        kind: "accepted".to_owned(),
                        operation_ref: OperationRefData {
                            id: invocation_id,
                            service: service.clone(),
                            operation: D::KEY.to_owned(),
                        },
                        snapshot: typed_snapshot(claimed.record.snapshot.clone())?,
                        transfer: None,
                    };
                    Self::resume(
                        service,
                        deployment_id,
                        fence,
                        repository,
                        mutation_gate,
                        handler,
                        nats.clone(),
                        publisher.clone(),
                        update_subject.clone(),
                        Arc::clone(&next_update_sequence),
                        claimed,
                    )
                    .await?;
                    return Ok(accepted);
                }

                if upload.state != "pending" {
                    upload.transfer_id = ulid::Ulid::new().to_string();
                    upload.subject = super::transfer::transfer_subject(
                        super::transfer::upload_subject_prefix(),
                        &service_session_key,
                        &upload.transfer_id,
                    );
                    upload.expires_at = (OffsetDateTime::now_utc() + time::Duration::minutes(15))
                        .format(&Rfc3339)
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    upload.size = None;
                    upload.digest = None;
                    upload.updated_at = None;
                }
                upload.state = "uploading".to_owned();
                let mut staged = claimed.record;
                staged.revision += 1;
                staged.transfer = Some(serde_json::to_value(&upload)?);
                let fence = OwnerFence::from(&staged)?;
                let staged = repository
                    .compare_exchange(
                        claimed.revision,
                        &fence.executor_id,
                        fence.owner_epoch,
                        staged,
                    )
                    .await?;
                let _ = staging.delete(&upload.staging_key).await;
                let grant = operation_upload_grant(&service, &caller.session_key, &upload);
                let plan = UploadTransferGrantPlan {
                    grant: grant.clone(),
                    store_alias: "__operations".to_owned(),
                    store: format!("trellis_operation_staging_{}", staged.record.deployment_id),
                    key: upload.staging_key.clone(),
                };
                let progress_repository = repository.clone();
                let progress_fence = fence.clone();
                let progress_gate = Arc::clone(&mutation_gate);
                let progress_operation_id = invocation_id.clone();
                let completion =
                    super::transfer::spawn_upload_transfer_endpoint_with_progress_and_completion(
                        nats.clone(),
                        UploadTransferSession::new(plan, now_timestamp()),
                        staging.clone(),
                        validator,
                        move |progress| {
                            let repository = progress_repository.clone();
                            let fence = progress_fence.clone();
                            let gate = Arc::clone(&progress_gate);
                            let operation_id = progress_operation_id.clone();
                            tokio::spawn(async move {
                                let _guard = gate.lock().await;
                                let Ok(Some(current)) = repository.get(&operation_id).await else {
                                    return;
                                };
                                if !fence.matches(&current.record)
                                    || current.record.snapshot.state.is_terminal()
                                    || current.record.cancellation_requested
                                    || current
                                        .record
                                        .lease_expires_at_ms
                                        .is_none_or(|expiry| expiry <= now_ms())
                                {
                                    return;
                                }
                                let mut record = current.record;
                                let Ok(mut upload) =
                                    record.transfer.clone().ok_or(()).and_then(|value| {
                                        serde_json::from_value::<DurableOperationUpload>(value)
                                            .map_err(|_| ())
                                    })
                                else {
                                    return;
                                };
                                upload.size = Some(progress.transferred_bytes);
                                record.revision += 1;
                                record.snapshot.revision += 1;
                                record.snapshot.transfer = Some(progress);
                                record.transfer = serde_json::to_value(upload).ok();
                                let _ = repository
                                    .compare_exchange(
                                        current.revision,
                                        &fence.executor_id,
                                        fence.owner_epoch,
                                        record,
                                    )
                                    .await;
                            });
                        },
                    )
                    .await?;
                let operation_id = invocation_id.clone();
                let service_for_resume = service.clone();
                let deployment_id_for_resume = deployment_id.clone();
                let repository_for_completion = repository.clone();
                let fence_for_completion = fence.clone();
                let gate_for_completion = Arc::clone(&mutation_gate);
                let heartbeat_repository = repository.clone();
                let heartbeat_fence = fence.clone();
                let heartbeat_operation_id = operation_id.clone();
                let heartbeat_gate = Arc::clone(&mutation_gate);
                let heartbeat = tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
                    interval.tick().await;
                    loop {
                        interval.tick().await;
                        let _guard = heartbeat_gate.lock().await;
                        let now = now_ms();
                        if heartbeat_repository
                            .renew(
                                &heartbeat_operation_id,
                                &heartbeat_fence.executor_id,
                                heartbeat_fence.owner_epoch,
                                now,
                                now + 30_000,
                            )
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
                let acknowledgement_repository = repository_for_completion.clone();
                let acknowledgement_fence = fence_for_completion.clone();
                let acknowledgement_gate = Arc::clone(&gate_for_completion);
                let acknowledgement_operation_id = operation_id.clone();
                tokio::spawn(async move {
                    match completion
                        .completed_after(move |info| {
                            persist_operation_upload(
                                acknowledgement_repository,
                                acknowledgement_fence,
                                acknowledgement_gate,
                                acknowledgement_operation_id,
                                info,
                            )
                        })
                        .await
                    {
                        Ok(info) => {
                            let _guard = gate_for_completion.lock().await;
                            let result = async {
                                let current = repository_for_completion
                                    .get(&operation_id)
                                    .await?
                                    .ok_or_else(|| ServerError::OperationNotFound {
                                    operation_id: operation_id.clone(),
                                })?;
                                if !fence_for_completion.matches(&current.record)
                                    || current.record.snapshot.state.is_terminal()
                                    || current.record.cancellation_requested
                                    || current
                                        .record
                                        .lease_expires_at_ms
                                        .is_none_or(|expiry| expiry <= now_ms())
                                {
                                    return Err(ServerError::Nats(
                                        "operation owner fence is stale".to_owned(),
                                    ));
                                }
                                let mut record = current.record;
                                let mut upload: DurableOperationUpload = serde_json::from_value(
                                    record.transfer.clone().ok_or_else(|| {
                                        ServerError::Nats(
                                            "operation upload staging state is missing".to_owned(),
                                        )
                                    })?,
                                )?;
                                upload.state = "committed".to_owned();
                                upload.size = Some(info.size);
                                upload.digest = Some(info.digest);
                                upload.updated_at = Some(info.updated_at);
                                upload.content_type = info.content_type;
                                record.revision += 1;
                                record.transfer = Some(serde_json::to_value(upload)?);
                                repository_for_completion
                                    .compare_exchange(
                                        current.revision,
                                        &fence_for_completion.executor_id,
                                        fence_for_completion.owner_epoch,
                                        record,
                                    )
                                    .await
                            }
                            .await;
                            drop(_guard);
                            match result {
                                Ok(_) => {
                                    if durable_snapshot_update::<D>(
                                        &repository_for_completion,
                                        &fence_for_completion,
                                        &gate_for_completion,
                                        &operation_id,
                                        SnapshotUpdate {
                                            state: OperationState::Running,
                                            progress: None,
                                            output: None,
                                            error: None,
                                            cancellation_requested: false,
                                        },
                                    )
                                    .await
                                    .is_ok()
                                    {
                                        if let Ok(Some(claimed)) =
                                            repository_for_completion.get(&operation_id).await
                                        {
                                            let _ = Self::resume(
                                                service_for_resume,
                                                deployment_id_for_resume,
                                                fence_for_completion,
                                                repository_for_completion,
                                                gate_for_completion,
                                                handler,
                                                nats.clone(),
                                                publisher.clone(),
                                                update_subject.clone(),
                                                Arc::clone(&next_update_sequence),
                                                claimed,
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(%error, %operation_id, "operation upload commit was fenced")
                                }
                            }
                        }
                        Err(error) => {
                            let _guard = gate_for_completion.lock().await;
                            if let Ok(Some(current)) =
                                repository_for_completion.get(&operation_id).await
                            {
                                let mut record = current.record;
                                if let Ok(mut persisted) =
                                    record.transfer.clone().ok_or(()).and_then(|value| {
                                        serde_json::from_value::<DurableOperationUpload>(value)
                                            .map_err(|_| ())
                                    })
                                {
                                    persisted.state = "invalid".to_owned();
                                    persisted.size = None;
                                    persisted.digest = None;
                                    record.revision += 1;
                                    record.lease_expires_at_ms = Some(now_ms());
                                    record.transfer = serde_json::to_value(persisted).ok();
                                    let _ = repository_for_completion
                                        .compare_exchange(
                                            current.revision,
                                            &fence_for_completion.executor_id,
                                            fence_for_completion.owner_epoch,
                                            record,
                                        )
                                        .await;
                                }
                            }
                            tracing::warn!(%error, %operation_id, "operation upload staging invalidated");
                        }
                    }
                    heartbeat.abort();
                });
                return Ok(AcceptedOperation {
                    kind: "accepted".to_owned(),
                    operation_ref: OperationRefData {
                        id: invocation_id,
                        service,
                        operation: D::KEY.to_owned(),
                    },
                    snapshot: typed_snapshot(staged.record.snapshot)?,
                    transfer: Some(grant),
                });
            }
            durable_snapshot_update::<D>(
                &repository,
                &fence,
                &mutation_gate,
                &invocation_id,
                SnapshotUpdate {
                    state: OperationState::Running,
                    progress: None,
                    output: None,
                    error: None,
                    cancellation_requested: false,
                },
            )
            .await?;
            let claimed = repository.get(&invocation_id).await?.ok_or_else(|| {
                ServerError::OperationNotFound {
                    operation_id: invocation_id.clone(),
                }
            })?;
            let accepted = AcceptedOperation {
                kind: "accepted".to_owned(),
                operation_ref: OperationRefData {
                    id: invocation_id,
                    service: service.clone(),
                    operation: D::KEY.to_owned(),
                },
                snapshot: typed_snapshot(claimed.record.snapshot.clone())?,
                transfer: None,
            };
            Self::resume(
                service,
                deployment_id,
                fence,
                repository,
                mutation_gate,
                handler,
                nats,
                publisher,
                update_subject,
                next_update_sequence,
                claimed,
            )
            .await?;
            Ok(accepted)
        })
    }

    fn get(&self, context: RequestContext, operation_id: String) -> OperationSnapshotFuture<D> {
        let repository = self.repository.clone();
        let deployment_id = self.deployment_id.clone();
        Box::pin(async move {
            let record = repository
                .get(&operation_id)
                .await?
                .ok_or(ServerError::OperationNotFound { operation_id })?;
            Self::authorize_record(
                &deployment_id,
                &context,
                &record.record,
                PermissionAction::Observe,
            )?;
            typed_snapshot(record.record.snapshot)
        })
    }

    fn wait(&self, context: RequestContext, operation_id: String) -> OperationSnapshotFuture<D> {
        let repository = self.repository.clone();
        let deployment_id = self.deployment_id.clone();
        Box::pin(async move {
            let mut watch = repository.watch(&operation_id).await?;
            while let Some(record) = watch.next().await {
                let record = record?;
                Self::authorize_record(
                    &deployment_id,
                    &context,
                    &record.record,
                    PermissionAction::Observe,
                )?;
                let snapshot = record.record.snapshot;
                if snapshot.state.is_terminal() {
                    return typed_snapshot(snapshot);
                }
            }
            Err(ServerError::Nats(
                "operation watch closed before terminal state".to_owned(),
            ))
        })
    }

    fn watch(
        &self,
        context: RequestContext,
        operation_id: String,
    ) -> OperationLiveWatch<D::Progress, D::Update, D::Output> {
        let repository = self.repository.clone();
        let nats = self.nats.clone();
        let deployment_id = self.deployment_id.clone();
        let validator = self.validator.clone();
        let provider_participant_id = self.provider_participant_id.clone();
        let update_subject = operation_update_subject::<D>(&deployment_id, &operation_id);
        Box::pin(
            stream::once(async move {
                let updates = nats
                    .subscribe(update_subject)
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                nats.flush()
                    .await
                    .map_err(|error| ServerError::Nats(error.to_string()))?;
                let record = repository.get(&operation_id).await?.ok_or_else(|| {
                    ServerError::OperationNotFound {
                        operation_id: operation_id.clone(),
                    }
                })?;
                Self::authorize_record(
                    &deployment_id,
                    &context,
                    &record.record,
                    PermissionAction::Observe,
                )?;
                Ok((
                    repository.watch(&operation_id).await?,
                    updates,
                    operation_id,
                    deployment_id,
                    context,
                    repository,
                    validator,
                    provider_participant_id,
                ))
            })
            .flat_map(|result| match result {
                Ok((
                    durable,
                    updates,
                    update_operation_id,
                    deployment_id,
                    context,
                    repository,
                    validator,
                    provider_participant_id,
                )) => {
                    let update_deployment_id = deployment_id.clone();
                    let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
                    let mut terminal_tx = Some(terminal_tx);
                    stream::select(
                        durable.scan(false, move |stopped, record| {
                            let result = if *stopped {
                                return futures_util::future::ready(None);
                            } else {
                                if record.is_err() {
                                    *stopped = true;
                                    if let Some(terminal_tx) = terminal_tx.take() {
                                        let _ = terminal_tx.send(());
                                    }
                                }
                                record
                                    .and_then(|record| {
                                        *stopped = record.record.snapshot.state.is_terminal();
                                        if *stopped {
                                            if let Some(terminal_tx) = terminal_tx.take() {
                                                let _ = terminal_tx.send(());
                                            }
                                        }
                                        Self::authorize_record(
                                            &deployment_id,
                                            &context,
                                            &record.record,
                                            PermissionAction::Observe,
                                        )?;
                                        typed_snapshot(record.record.snapshot)
                                    })
                                    .map(OperationLiveEvent::Snapshot)
                            };
                            futures_util::future::ready(Some(result))
                        }),
                        stream::unfold(
                            (
                                updates,
                                update_operation_id,
                                repository,
                                update_deployment_id,
                                validator,
                                provider_participant_id,
                            ),
                            move |(
                                mut updates,
                                update_operation_id,
                                repository,
                                update_deployment_id,
                                validator,
                                provider_participant_id,
                            )| async move {
                                loop {
                                    let message = updates.next().await?;
                                    let Some(reply_to) =
                                        message.reply.as_ref().map(ToString::to_string)
                                    else {
                                        continue;
                                    };
                                    let request = RequestContext {
                                        subject: message.subject.to_string(),
                                        reply_to: Some(reply_to),
                                        session_key: message
                                            .headers
                                            .as_ref()
                                            .and_then(|headers| headers.get("session-key"))
                                            .map(|value| value.as_str().to_owned()),
                                        proof: message
                                            .headers
                                            .as_ref()
                                            .and_then(|headers| headers.get("proof"))
                                            .map(|value| value.as_str().to_owned()),
                                        authorization_context: message
                                            .headers
                                            .as_ref()
                                            .and_then(|headers| {
                                                headers.get("authorization-context")
                                            })
                                            .map(|value| value.as_str().to_owned()),
                                        iat: message
                                            .headers
                                            .as_ref()
                                            .and_then(|headers| headers.get("iat"))
                                            .and_then(|value| value.as_str().parse().ok()),
                                        request_id: message
                                            .headers
                                            .as_ref()
                                            .and_then(|headers| headers.get("request-id"))
                                            .map(|value| value.as_str().to_owned()),
                                        ..RequestContext::default()
                                    };
                                    let Ok(validation) = validator
                                        .validate_possession(
                                            &request.subject,
                                            &message.payload,
                                            &request,
                                        )
                                        .await
                                    else {
                                        continue;
                                    };
                                    let Some(caller) =
                                        validation.caller.filter(|_| validation.allowed)
                                    else {
                                        continue;
                                    };
                                    if !matches!(
                                        caller.principal_kind,
                                        trellis_protocol::AuthorizationPrincipalKind::Service
                                            | trellis_protocol::AuthorizationPrincipalKind::Device
                                    ) || caller.participant_id != provider_participant_id
                                        || caller.deployment_id.as_deref()
                                            != Some(&update_deployment_id)
                                    {
                                        continue;
                                    }
                                    let Ok(envelope) =
                                        serde_json::from_slice::<OperationUpdateEnvelope>(
                                            &message.payload,
                                        )
                                    else {
                                        continue;
                                    };
                                    let Ok(epoch) = envelope.owner_epoch.parse::<u64>() else {
                                        continue;
                                    };
                                    let Ok(sequence) = envelope.sequence.parse::<u64>() else {
                                        continue;
                                    };
                                    if epoch == 0
                                        || sequence == 0
                                        || epoch.to_string() != envelope.owner_epoch
                                        || sequence.to_string() != envelope.sequence
                                    {
                                        continue;
                                    }
                                    let Ok(Some(current)) =
                                        repository.get(&update_operation_id).await
                                    else {
                                        continue;
                                    };
                                    if envelope.operation_id != update_operation_id
                                        || envelope.api_id != D::API_ID
                                        || envelope.operation != D::KEY
                                        || envelope.deployment_id != update_deployment_id
                                        || current.record.owner_epoch != epoch
                                        || current.record.owner_executor_id.as_deref()
                                            != Some(&envelope.owner_executor_id)
                                        || current.record.owner_connection_id.as_deref()
                                            != Some(&envelope.owner_connection_id)
                                        || caller.connection_id != envelope.owner_connection_id
                                        || current
                                            .record
                                            .lease_expires_at_ms
                                            .is_none_or(|expiry| expiry <= now_ms())
                                        || current.record.snapshot.state.is_terminal()
                                        || current.record.cancellation_requested
                                        || current.record.api_id != D::API_ID
                                        || current.record.operation != D::KEY
                                        || current.record.deployment_id != update_deployment_id
                                    {
                                        continue;
                                    }
                                    if D::UPDATE_SCHEMA_JSON.is_none_or(|schema| {
                                        super::validate_input_schema(schema, &envelope.update)
                                            .is_err()
                                    }) {
                                        continue;
                                    }
                                    let result = serde_json::from_value(envelope.update)
                                        .map(|update| {
                                            OperationLiveEvent::Update(
                                                crate::client::OperationUpdateEvent {
                                                    operation_id: update_operation_id.clone(),
                                                    sequence,
                                                    timestamp: envelope.occurred_at,
                                                    update,
                                                },
                                            )
                                        })
                                        .map_err(ServerError::from);
                                    return Some((
                                        result,
                                        (
                                            updates,
                                            update_operation_id,
                                            repository,
                                            update_deployment_id,
                                            validator,
                                            provider_participant_id,
                                        ),
                                    ));
                                }
                            },
                        )
                        .take_until(terminal_rx),
                    )
                    .boxed()
                }
                Err(error) => stream::once(async move { Err(error) }).boxed(),
            }),
        )
    }

    fn cancel(&self, context: RequestContext, operation_id: String) -> OperationSnapshotFuture<D> {
        let repository = self.repository.clone();
        let gate = Arc::clone(&self.mutation_gate);
        let deployment_id = self.deployment_id.clone();
        Box::pin(async move {
            let _guard = gate.lock().await;
            let current = repository.get(&operation_id).await?.ok_or_else(|| {
                ServerError::OperationNotFound {
                    operation_id: operation_id.clone(),
                }
            })?;
            Self::authorize_record(
                &deployment_id,
                &context,
                &current.record,
                PermissionAction::Cancel,
            )?;
            if current.record.snapshot.state.is_terminal() {
                return Err(operation_terminal_error(&current.record));
            }
            let mut watch = repository.watch(&operation_id).await?;
            let mut record = current.record;
            if !record.cancellation_requested {
                record.revision += 1;
                record.cancellation_requested = true;
                record.snapshot.updated_at = Some(now_timestamp());
                repository
                    .compare_record_exchange(current.revision, record)
                    .await?;
            }
            drop(_guard);
            while let Some(current) = watch.next().await {
                let snapshot = current?.record.snapshot;
                if snapshot.state.is_terminal() {
                    return typed_snapshot(snapshot);
                }
            }
            Err(ServerError::Nats(
                "operation cancellation watch closed before terminal state".to_owned(),
            ))
        })
    }

    fn signal(
        &self,
        context: RequestContext,
        operation_id: String,
        signal: String,
        input: Option<Value>,
    ) -> OperationSignalFuture<D> {
        let repository = self.repository.clone();
        let gate = Arc::clone(&self.mutation_gate);
        let deployment_id = self.deployment_id.clone();
        Box::pin(async move {
            let _guard = gate.lock().await;
            let current = repository.get(&operation_id).await?.ok_or_else(|| {
                ServerError::OperationNotFound {
                    operation_id: operation_id.clone(),
                }
            })?;
            Self::authorize_record(
                &deployment_id,
                &context,
                &current.record,
                PermissionAction::Control,
            )?;
            if context
                .required_permission
                .as_ref()
                .and_then(|permission| permission.signal.as_deref())
                != Some(&signal)
            {
                return Err(ServerError::RequestDenied {
                    subject: context.subject.clone(),
                    session_key: context.session_key.clone().unwrap_or_default(),
                });
            }
            if current.record.snapshot.state.is_terminal() {
                return Err(operation_terminal_error(&current.record));
            }
            let mut record = current.record;
            let request_id = context
                .request_id
                .unwrap_or_else(|| ulid::Ulid::new().to_string());
            if let Some(existing) = record
                .signals
                .iter()
                .find(|accepted| accepted.request_id == request_id)
            {
                let payload = serde_json::to_vec(&input)?;
                if existing.name != signal || existing.payload != payload {
                    return Err(ServerError::OperationIdempotencyConflict {
                        kind: "signal",
                        request_id,
                    });
                }
                return Ok(OperationSignalAccepted {
                    kind: "signal-accepted".to_owned(),
                    operation_id,
                    signal: existing.name.clone(),
                    signal_sequence: existing.sequence,
                    accepted_at: now_timestamp(),
                    snapshot: typed_snapshot(record.snapshot)?,
                });
            }
            let sequence = record.next_signal_sequence;
            record.next_signal_sequence += 1;
            record.revision += 1;
            record.signals.push(DurableOperationSignal {
                sequence,
                request_id,
                name: signal.clone(),
                payload: serde_json::to_vec(&input)?,
                acknowledged: false,
            });
            let updated = repository
                .compare_record_exchange(current.revision, record)
                .await?;
            Ok(OperationSignalAccepted {
                kind: "signal-accepted".to_owned(),
                operation_id,
                signal,
                signal_sequence: sequence,
                accepted_at: now_timestamp(),
                snapshot: typed_snapshot(updated.record.snapshot)?,
            })
        })
    }
}

#[doc = concat!("Trellis API operation `", stringify!(control_subject), "`.")]
pub fn control_subject(subject: &str) -> String {
    format!("{subject}.control")
}

fn operation_update_subject<D: OperationDescriptor>(
    deployment_id: &str,
    operation_id: &str,
) -> String {
    let action = D::KEY.split_once('.').map_or(D::KEY, |(_, action)| action);
    let start = trellis_protocol::derive_bound_operation_subject(D::API_ID, deployment_id, action)
        .expect("generated operation metadata must form a valid bound subject");
    format!("{start}.updates.{operation_id}")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OperationUpdateEnvelope {
    operation_id: String,
    api_id: String,
    operation: String,
    deployment_id: String,
    owner_executor_id: String,
    owner_connection_id: String,
    owner_epoch: String,
    sequence: String,
    occurred_at: String,
    update: Value,
}

/// Typed service-owned operation lifecycle control handle.
pub struct OperationControl<D>
where
    D: OperationDescriptor,
{
    operation_ref: OperationRefData,
    durable: DurableOperationControl,
    nats: async_nats::Client,
    publisher: Option<Arc<crate::client::TrellisClient>>,
    update_subject: String,
    next_update_sequence: Arc<AtomicU64>,
    _descriptor: PhantomData<fn() -> D>,
}

impl<D: OperationDescriptor> std::fmt::Debug for OperationControl<D> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationControl")
            .field("operation_ref", &self.operation_ref)
            .field("durable", &self.durable)
            .finish_non_exhaustive()
    }
}

/// The exact execution-entry admission for one acquired Operation fence.
///
/// Runs under the provider mutation gate and is called both before setup and
/// again immediately before user code so a delayed execution cannot enter after
/// a successor acquired the operation.
async fn admit_operation_execution<D>(
    repository: &KvOperationRepository,
    mutation_gate: &Arc<Mutex<()>>,
    fence: &OwnerFence,
    deployment_id: &str,
    invocation_id: &str,
) -> Result<Option<RevisionedOperationRecord>, ServerError>
where
    D: OperationDescriptor,
{
    let _guard = mutation_gate.lock().await;
    Ok(repository.get(invocation_id).await?.filter(|current| {
        fence.matches(&current.record)
            && current.record.api_id == D::API_ID
            && current.record.operation == D::KEY
            && current.record.deployment_id == deployment_id
            && !current.record.snapshot.state.is_terminal()
            && current
                .record
                .lease_expires_at_ms
                .is_some_and(|expiry| expiry > now_ms())
            && !current.record.creator_principal_id.is_empty()
            && !current.record.creator_participant_id.is_empty()
            && current.record.caller.as_ref().is_some_and(|caller| {
                caller.principal_id == current.record.creator_principal_id
                    && caller.participant_id == current.record.creator_participant_id
            })
    }))
}

#[derive(Debug, Clone)]
struct DurableOperationControl {
    repository: KvOperationRepository,
    fence: OwnerFence,
    mutation_gate: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerFence {
    executor_id: String,
    connection_id: String,
    owner_epoch: u64,
}

impl OwnerFence {
    fn from(record: &DurableOperationRecord) -> Result<Self, ServerError> {
        Ok(Self {
            executor_id: record
                .owner_executor_id
                .clone()
                .ok_or_else(|| ServerError::Nats("operation has no active owner".to_owned()))?,
            connection_id: record.owner_connection_id.clone().ok_or_else(|| {
                ServerError::Nats("operation has no owning connection".to_owned())
            })?,
            owner_epoch: record.owner_epoch,
        })
    }

    fn matches(&self, record: &DurableOperationRecord) -> bool {
        record.owner_executor_id.as_deref() == Some(&self.executor_id)
            && record.owner_connection_id.as_deref() == Some(&self.connection_id)
            && record.owner_epoch == self.owner_epoch
            && record
                .lease_expires_at_ms
                .is_some_and(|expiry| expiry > now_ms())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DurableOperationUpload {
    staging_key: String,
    state: String,
    transfer_id: String,
    subject: String,
    expires_at: String,
    size: Option<u64>,
    digest: Option<String>,
    updated_at: Option<String>,
    content_type: Option<String>,
}

fn operation_upload_grant(
    service: &str,
    session_key: &str,
    upload: &DurableOperationUpload,
) -> UploadTransferGrant {
    UploadTransferGrant {
        type_name: "TransferGrant".to_owned(),
        direction: "send".to_owned(),
        service: service.to_owned(),
        session_key: session_key.to_owned(),
        transfer_id: upload.transfer_id.clone(),
        subject: upload.subject.clone(),
        expires_at: upload.expires_at.clone(),
        chunk_bytes: 64 * 1024,
        max_bytes: Some(512 * 1024 * 1024),
        content_type: upload.content_type.clone(),
        metadata: Default::default(),
    }
}

async fn persist_operation_upload(
    repository: KvOperationRepository,
    fence: OwnerFence,
    gate: Arc<Mutex<()>>,
    operation_id: String,
    info: FileTransferInfo,
) -> Result<(), ServerError> {
    let _guard = gate.lock().await;
    let current =
        repository
            .get(&operation_id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: operation_id.clone(),
            })?;
    let mut record = current.record;
    let mut upload: DurableOperationUpload =
        serde_json::from_value(record.transfer.clone().ok_or_else(|| {
            ServerError::Nats("operation upload staging state is missing".to_owned())
        })?)?;
    upload.state = "committed".to_owned();
    upload.size = Some(info.size);
    upload.digest = Some(info.digest);
    upload.updated_at = Some(info.updated_at);
    upload.content_type = info.content_type;
    record.revision += 1;
    record.transfer = Some(serde_json::to_value(upload)?);
    repository
        .compare_exchange(
            current.revision,
            &fence.executor_id,
            fence.owner_epoch,
            record,
        )
        .await?;
    Ok(())
}

impl<D> OperationControl<D>
where
    D: OperationDescriptor,
    D::Progress: Serialize + DeserializeOwned + Send + 'static,
    D::Output: Serialize + DeserializeOwned + Send + 'static,
{
    /// Return the runtime-owned committed upload associated with this invocation.
    pub async fn upload(&self) -> Result<Option<FileTransferInfo>, ServerError> {
        let record = self
            .durable
            .repository
            .get(&self.operation_ref.id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: self.operation_ref.id.clone(),
            })?
            .record;
        if !self.durable.fence.matches(&record) {
            return Err(ServerError::Nats(
                "operation owner fence is stale".to_owned(),
            ));
        }
        let Some(upload) = record.transfer else {
            return Ok(None);
        };
        let upload: DurableOperationUpload = serde_json::from_value(upload)?;
        if upload.state != "committed" {
            return Err(ServerError::Nats(
                "operation upload is not committed".to_owned(),
            ));
        }
        Ok(Some(FileTransferInfo {
            key: upload.staging_key,
            size: upload
                .size
                .ok_or_else(|| ServerError::Nats("committed upload size is missing".to_owned()))?,
            updated_at: upload.updated_at.ok_or_else(|| {
                ServerError::Nats("committed upload timestamp is missing".to_owned())
            })?,
            digest: upload.digest.ok_or_else(|| {
                ServerError::Nats("committed upload digest is missing".to_owned())
            })?,
            content_type: upload.content_type,
            metadata: Default::default(),
        }))
    }
    /// Mark the operation as started/running.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(started), "`.")]
    pub async fn started(&self) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError> {
        self.update(OperationState::Running, None, None, None).await
    }

    /// Publish typed operation progress and mark the operation as running.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(progress), "`.")]
    pub async fn progress(
        &self,
        progress: D::Progress,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError> {
        self.update(
            OperationState::Running,
            Some(serde_json::to_value(progress)?),
            None,
            None,
        )
        .await
    }

    /// Emit a validated live-only update without changing the durable snapshot.
    pub async fn emit_update(
        &self,
        update: D::Update,
    ) -> Result<crate::client::OperationUpdateEvent<D::Update>, ServerError>
    where
        D::UpdateEvidence: crate::client::HasOperationUpdates,
        D::Update: Clone,
    {
        let update_schema =
            D::UPDATE_SCHEMA_JSON.ok_or_else(|| ServerError::InvalidOperationControlAction {
                subject: D::SUBJECT.to_owned(),
                action: "update".to_owned(),
            })?;
        let update_value = serde_json::to_value(&update)?;
        super::validate_input_schema(update_schema, &update_value)?;
        let durable = &self.durable;
        let _guard = durable.mutation_gate.lock().await;
        let current = durable
            .repository
            .get(&self.operation_ref.id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: self.operation_ref.id.clone(),
            })?;
        if current.record.snapshot.state.is_terminal() {
            return Err(operation_terminal_error(&current.record));
        }
        if !durable.fence.matches(&current.record) {
            return Err(ServerError::Nats(
                "operation owner fence is stale".to_owned(),
            ));
        }
        let sequence = self.next_update_sequence.fetch_add(1, Ordering::Relaxed);
        let timestamp = now_timestamp();
        let publisher = self.publisher.as_ref().ok_or_else(|| {
            ServerError::Nats("operation update publisher authentication is unavailable".to_owned())
        })?;
        let reply = self.nats.new_inbox();
        let owner_connection_id =
            current.record.owner_connection_id.clone().ok_or_else(|| {
                ServerError::Nats("operation owner connection is missing".to_owned())
            })?;
        let payload = serde_json::to_vec(&OperationUpdateEnvelope {
            operation_id: self.operation_ref.id.clone(),
            api_id: D::API_ID.to_owned(),
            operation: D::KEY.to_owned(),
            deployment_id: current.record.deployment_id.clone(),
            owner_executor_id: durable.fence.executor_id.clone(),
            owner_connection_id,
            owner_epoch: durable.fence.owner_epoch.to_string(),
            sequence: sequence.to_string(),
            occurred_at: timestamp.clone(),
            update: update_value,
        })?;
        let headers = publisher
            .signed_headers(&self.update_subject, &reply, &payload)
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        self.nats
            .publish_with_reply_and_headers(
                self.update_subject.clone(),
                reply,
                headers,
                payload.into(),
            )
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        self.nats
            .flush()
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        Ok(crate::client::OperationUpdateEvent {
            operation_id: self.operation_ref.id.clone(),
            sequence,
            timestamp,
            update,
        })
    }

    /// Complete the operation with typed output.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(complete), "`.")]
    pub async fn complete(
        &self,
        output: D::Output,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError> {
        self.update(
            OperationState::Completed,
            None,
            Some(serde_json::to_value(output)?),
            None,
        )
        .await
    }

    /// Fail the operation with a typed failure payload.
    pub async fn fail(
        &self,
        error: D::Error,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError>
    where
        D::Error: OperationFailureLike,
    {
        self.update(
            OperationState::Failed,
            None,
            None,
            Some(OperationError {
                error_type: error.error_type().to_string(),
                message: error.message(),
            }),
        )
        .await
    }

    /// Attach operation completion to a service-owned async task.
    ///
    /// The attached task is expected to drive operation lifecycle updates through
    /// this control handle or another service-owned control path. After the task
    /// returns, the operation must have a terminal snapshot.
    pub async fn attach<Fut, E>(
        &self,
        task: Fut,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError>
    where
        Fut: Future<Output = Result<(), E>>,
        E: std::fmt::Display,
    {
        task.await.map_err(|error| {
            ServerError::Nats(format!("attached operation task failed: {error}"))
        })?;
        let record = self
            .durable
            .repository
            .get(&self.operation_ref.id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: self.operation_ref.id.clone(),
            })?;
        if !self.durable.fence.matches(&record.record) {
            return Err(ServerError::Nats(
                "operation owner fence is stale".to_owned(),
            ));
        }
        let snapshot = typed_snapshot(record.record.snapshot)?;
        if snapshot.state.is_terminal() {
            Ok(snapshot)
        } else {
            Err(ServerError::Nats(
                "attached operation task completed without terminal operation state".to_string(),
            ))
        }
    }

    /// Cancel the operation.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(cancel), "`.")]
    pub async fn cancel(&self) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError> {
        if !D::CANCELABLE {
            return Err(ServerError::OperationUnsupportedControl {
                operation: D::KEY.to_string(),
                action: "cancel".to_string(),
            });
        }
        let durable = &self.durable;
        let _guard = durable.mutation_gate.lock().await;
        let current = durable
            .repository
            .get(&self.operation_ref.id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: self.operation_ref.id.clone(),
            })?;
        if current.record.snapshot.state.is_terminal() {
            return Err(operation_terminal_error(&current.record));
        }
        if current.record.cancellation_requested {
            return typed_snapshot(current.record.snapshot);
        }
        let mut record = current.record;
        record.revision += 1;
        record.cancellation_requested = true;
        record.snapshot.updated_at = Some(now_timestamp());
        let updated = durable
            .repository
            .compare_exchange(
                current.revision,
                &durable.fence.executor_id,
                durable.fence.owner_epoch,
                record,
            )
            .await?;
        typed_snapshot(updated.record.snapshot)
    }

    /// Iterate accepted signals for this operation from this subscription onward.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(signals), "`.")]
    pub async fn signals(
        &self,
    ) -> Result<BoxStream<'static, Result<OperationSignal, ServerError>>, ServerError> {
        let durable = &self.durable;
        let state = (
            durable.repository.clone(),
            durable.fence.clone(),
            Arc::clone(&durable.mutation_gate),
            self.operation_ref.id.clone(),
            0_u64,
        );
        Ok(Box::pin(stream::unfold(
            state,
            |(repository, fence, gate, id, mut sequence)| async move {
                loop {
                    let _guard = gate.lock().await;
                    let current = match repository.get(&id).await {
                        Ok(Some(current)) => current,
                        Ok(None) => {
                            return Some((
                                Err(ServerError::OperationNotFound {
                                    operation_id: id.clone(),
                                }),
                                (repository, fence, Arc::clone(&gate), id, sequence),
                            ))
                        }
                        Err(error) => {
                            return Some((
                                Err(error),
                                (repository, fence, Arc::clone(&gate), id, sequence),
                            ))
                        }
                    };
                    if !fence.matches(&current.record) {
                        return Some((
                            Err(ServerError::Nats(
                                "operation owner fence is stale".to_owned(),
                            )),
                            (repository, fence, Arc::clone(&gate), id, sequence),
                        ));
                    }
                    if let Some(signal) = current
                        .record
                        .signals
                        .iter()
                        .find(|signal| !signal.acknowledged && signal.sequence > sequence)
                        .cloned()
                    {
                        sequence = signal.sequence;
                        let input = serde_json::from_slice(&signal.payload).unwrap_or(None);
                        return Some((
                            Ok(OperationSignal {
                                operation_id: id.clone(),
                                signal_sequence: sequence,
                                signal: signal.name,
                                input,
                                accepted_at: now_timestamp(),
                            }),
                            (repository, fence, Arc::clone(&gate), id, sequence),
                        ));
                    }
                    drop(_guard);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
        )))
    }

    /// Durably acknowledge a signal after the handler has accepted its side effects.
    pub async fn acknowledge_signal(&self, sequence: u64) -> Result<(), ServerError> {
        let durable = &self.durable;
        let _guard = durable.mutation_gate.lock().await;
        let current = durable
            .repository
            .get(&self.operation_ref.id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: self.operation_ref.id.clone(),
            })?;
        if current.record.snapshot.state.is_terminal() {
            return Err(operation_terminal_error(&current.record));
        }
        let mut record = current.record;
        let signal = record
            .signals
            .iter_mut()
            .find(|signal| signal.sequence == sequence)
            .ok_or_else(|| {
                ServerError::Nats(format!("operation signal {sequence} was not found"))
            })?;
        if signal.acknowledged {
            return Ok(());
        }
        signal.acknowledged = true;
        record.revision += 1;
        durable
            .repository
            .compare_exchange(
                current.revision,
                &durable.fence.executor_id,
                durable.fence.owner_epoch,
                record,
            )
            .await?;
        Ok(())
    }

    async fn update(
        &self,
        state: OperationState,
        progress: Option<Value>,
        output: Option<Value>,
        error: Option<OperationError>,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError> {
        let durable = &self.durable;
        durable_snapshot_update::<D>(
            &durable.repository,
            &durable.fence,
            &durable.mutation_gate,
            &self.operation_ref.id,
            SnapshotUpdate {
                state: state.clone(),
                progress,
                output,
                error,
                cancellation_requested: state == OperationState::Cancelled,
            },
        )
        .await
    }
}

fn typed_snapshot<TProgress, TOutput>(
    snapshot: OperationSnapshot<Value, Value>,
) -> Result<OperationSnapshot<TProgress, TOutput>, ServerError>
where
    TProgress: DeserializeOwned,
    TOutput: DeserializeOwned,
{
    Ok(OperationSnapshot {
        id: snapshot.id,
        service: snapshot.service,
        operation: snapshot.operation,
        revision: snapshot.revision,
        state: snapshot.state,
        created_at: snapshot.created_at,
        updated_at: snapshot.updated_at,
        completed_at: snapshot.completed_at,
        progress: snapshot.progress.map(serde_json::from_value).transpose()?,
        transfer: snapshot.transfer,
        output: snapshot.output.map(serde_json::from_value).transpose()?,
        error: snapshot.error,
    })
}

fn now_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn now_ms() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000
}

struct SnapshotUpdate {
    state: OperationState,
    progress: Option<Value>,
    output: Option<Value>,
    error: Option<OperationError>,
    cancellation_requested: bool,
}

async fn durable_snapshot_update<D: OperationDescriptor>(
    repository: &KvOperationRepository,
    fence: &OwnerFence,
    gate: &Mutex<()>,
    operation_id: &str,
    update: SnapshotUpdate,
) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError>
where
    D::Progress: DeserializeOwned,
    D::Output: DeserializeOwned,
{
    let SnapshotUpdate {
        state,
        progress,
        output,
        error,
        cancellation_requested,
    } = update;
    let _guard = gate.lock().await;
    let current =
        repository
            .get(operation_id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: operation_id.to_owned(),
            })?;
    let mut record = current.record;
    if record.snapshot.state.is_terminal() || record.cancellation_requested {
        return Err(operation_terminal_error(&record));
    }
    if !fence.matches(&record) {
        return Err(ServerError::Nats(
            "operation owner fence is stale".to_owned(),
        ));
    }
    record.revision += 1;
    record.cancellation_requested |= cancellation_requested;
    record.snapshot.revision += 1;
    record.snapshot.state = state.clone();
    record.snapshot.updated_at = Some(now_timestamp());
    if state.is_terminal() {
        record.snapshot.completed_at = record.snapshot.updated_at.clone();
    }
    if progress.is_some() {
        record.snapshot.progress = progress;
    }
    if output.is_some() {
        record.snapshot.output = output;
    }
    if error.is_some() {
        record.snapshot.error = error;
    }
    let updated = repository
        .compare_exchange(
            current.revision,
            &fence.executor_id,
            fence.owner_epoch,
            record,
        )
        .await?;
    typed_snapshot(updated.record.snapshot)
}

async fn finalize_cancellation<D: OperationDescriptor>(
    repository: &KvOperationRepository,
    fence: &OwnerFence,
    gate: &Mutex<()>,
    operation_id: &str,
) -> Result<OperationSnapshot<D::Progress, D::Output>, ServerError>
where
    D::Progress: DeserializeOwned,
    D::Output: DeserializeOwned,
{
    let _guard = gate.lock().await;
    let current =
        repository
            .get(operation_id)
            .await?
            .ok_or_else(|| ServerError::OperationNotFound {
                operation_id: operation_id.to_owned(),
            })?;
    if current.record.snapshot.state.is_terminal() {
        return typed_snapshot(current.record.snapshot);
    }
    if !current.record.cancellation_requested {
        return Err(ServerError::Nats(
            "operation cancellation was not requested".to_owned(),
        ));
    }
    if !fence.matches(&current.record) {
        return Err(ServerError::Nats(
            "operation owner fence is stale".to_owned(),
        ));
    }
    let mut record = current.record;
    record.revision += 1;
    record.snapshot.state = OperationState::Cancelled;
    record.snapshot.updated_at = Some(now_timestamp());
    record.snapshot.completed_at = record.snapshot.updated_at.clone();
    let updated = repository
        .compare_exchange(
            current.revision,
            &fence.executor_id,
            fence.owner_epoch,
            record,
        )
        .await?;
    typed_snapshot(updated.record.snapshot)
}

fn operation_terminal_error(record: &DurableOperationRecord) -> ServerError {
    ServerError::OperationAlreadyTerminal {
        operation_id: record.invocation_id.clone(),
        state: if record.cancellation_requested {
            "cancelled".to_owned()
        } else {
            match record.snapshot.state {
                OperationState::Pending => "pending",
                OperationState::Running => "running",
                OperationState::Completed => "completed",
                OperationState::Failed => "failed",
                OperationState::Cancelled => "cancelled",
            }
            .to_owned()
        },
    }
}

#[cfg(all(test, feature = "live-integration"))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use bytes::Bytes;
    use futures_util::future::BoxFuture;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::client::DeclaredOperationUpdates;
    use crate::service::{RequestValidation, VerifiedCaller};

    #[derive(Clone)]
    struct Allow;

    impl RequestValidator for Allow {
        fn validate<'a>(
            &'a self,
            _subject: &'a str,
            _payload: &'a Bytes,
            _context: &'a RequestContext,
        ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
            Box::pin(async { Ok(RequestValidation::default()) })
        }

        fn revalidate_current<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> BoxFuture<'a, Result<bool, ServerError>> {
            Box::pin(async { Ok(true) })
        }
    }

    struct TestOperation;

    impl OperationDescriptor for TestOperation {
        type Input = Value;
        type Progress = Value;
        type Output = Value;
        type Update = Value;
        type UpdateEvidence = DeclaredOperationUpdates;
        type Error = OperationFailure;

        const API_ID: &'static str = "test.operations@v1";
        const KEY: &'static str = "run";
        const SUBJECT: &'static str = "operations.v1.test.run";
        const CANCELABLE: bool = true;
        const INPUT_SCHEMA_JSON: &'static str = "{}";
        const PROGRESS_SCHEMA_JSON: Option<&'static str> = Some("{}");
        const OUTPUT_SCHEMA_JSON: &'static str = "{}";
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some("{}");
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
    }

    struct UploadOperation;

    impl OperationDescriptor for UploadOperation {
        type Input = Value;
        type Progress = Value;
        type Output = Value;
        type Update = Value;
        type UpdateEvidence = DeclaredOperationUpdates;
        type Error = OperationFailure;

        const API_ID: &'static str = "test.uploads@v1";
        const KEY: &'static str = "upload";
        const SUBJECT: &'static str = "operations.v1.test.upload";
        const CANCELABLE: bool = true;
        const UPLOAD: bool = true;
        const INPUT_SCHEMA_JSON: &'static str = "{}";
        const PROGRESS_SCHEMA_JSON: Option<&'static str> = Some("{}");
        const OUTPUT_SCHEMA_JSON: &'static str = "{}";
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some("{}");
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
    }

    struct HandlerDrop(Arc<AtomicBool>);

    impl Drop for HandlerDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn operation(id: String, api_id: &str, name: &str) -> DurableOperationRecord {
        let input = json!({"value": 1});
        DurableOperationRecord {
            invocation_digest: operation_invocation_digest(
                api_id,
                name,
                "principal",
                "participant",
                &input,
            )
            .unwrap(),
            invocation_id: id.clone(),
            api_id: api_id.to_owned(),
            operation: name.to_owned(),
            deployment_id: "deployment".to_owned(),
            creator_principal_id: "principal".to_owned(),
            creator_participant_id: "participant".to_owned(),
            caller_session_key: "session".to_owned(),
            caller: Some(VerifiedCaller {
                session_key: "session".to_owned(),
                inbox_prefix: "_INBOX".to_owned(),
                context_digest: "digest".to_owned(),
                connection_id: "connection".to_owned(),
                login_session_id: None,
                principal_id: "principal".to_owned(),
                principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
                participant_id: "participant".to_owned(),
                platform_privileges: Vec::new(),
                deployment_id: None,
                instance_id: None,
            }),
            input,
            snapshot: OperationSnapshot {
                id: Some(id),
                service: Some("service".to_owned()),
                operation: Some(name.to_owned()),
                created_at: Some("2026-09-10T00:00:00Z".to_owned()),
                updated_at: Some("2026-09-10T00:00:00Z".to_owned()),
                state: OperationState::Pending,
                revision: 1,
                ..OperationSnapshot::default()
            },
            revision: 1,
            owner_executor_id: None,
            owner_connection_id: None,
            owner_epoch: 0,
            lease_expires_at_ms: None,
            cancellation_requested: false,
            next_signal_sequence: 1,
            signals: Vec::new(),
            transfer: None,
        }
    }

    async fn control(
        repository: KvOperationRepository,
        nats: async_nats::Client,
        executor_id: &str,
        id: &str,
    ) -> OperationControl<TestOperation> {
        let record = repository.get(id).await.unwrap().unwrap();
        OperationControl {
            operation_ref: OperationRefData {
                id: id.to_owned(),
                service: "service".to_owned(),
                operation: TestOperation::KEY.to_owned(),
            },
            durable: DurableOperationControl {
                repository,
                fence: OwnerFence {
                    executor_id: executor_id.to_owned(),
                    connection_id: record
                        .record
                        .owner_connection_id
                        .clone()
                        .unwrap_or_else(|| executor_id.to_owned()),
                    owner_epoch: record.record.owner_epoch,
                },
                mutation_gate: Arc::new(Mutex::new(())),
            },
            nats,
            publisher: None,
            update_subject: operation_update_subject::<TestOperation>("deployment", id),
            next_update_sequence: Arc::new(AtomicU64::new(1)),
            _descriptor: PhantomData,
        }
    }

    async fn put(store: &BoundStoreResourceClient, key: &str) -> (u64, Option<String>) {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer.write_all(b"retained").await.unwrap();
        writer.shutdown().await.unwrap();
        let info = store.write_from(key, &mut reader).await.unwrap();
        (info.size, info.digest)
    }

    #[tokio::test]
    async fn durable_operation_recovery_and_controls_use_real_kv() {
        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(state.path().join("cache"))
            .source(source.path())
            .temporary_state()
            .ephemeral_ports()
            .output(trellis_local_nats::NatsOutput::Log {
                path: state.path().join("nats.log"),
                mirror: false,
            })
            .start()
            .unwrap();
        let client = async_nats::ConnectOptions::new()
            .credentials_file(source.path().join("creds/trellis-auth.creds"))
            .await
            .unwrap()
            .connect(nats.nats_url())
            .await
            .unwrap();
        let jetstream = async_nats::jetstream::new(client.clone());
        let bucket = format!("trellis_test_operation_controls_{}", ulid::Ulid::new());
        let repository = KvOperationRepository::new(
            jetstream
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: bucket.clone(),
                    history: 10,
                    ..Default::default()
                })
                .await
                .unwrap(),
        );
        let object_bucket = format!("trellis_test_operation_staging_{}", ulid::Ulid::new());
        let staging = BoundStoreResourceClient::new(
            jetstream
                .create_object_store(async_nats::jetstream::object_store::Config {
                    bucket: object_bucket.clone(),
                    ..Default::default()
                })
                .await
                .unwrap(),
        );

        let old_key = "abandoned";
        let committed_key = "committed";
        let interrupted_commit_key = "interrupted-commit";
        let _ = put(&staging, old_key).await;
        let (committed_size, committed_digest) = put(&staging, committed_key).await;
        let _ = put(&staging, interrupted_commit_key).await;
        let expired = (OffsetDateTime::now_utc() - time::Duration::hours(25))
            .format(&Rfc3339)
            .unwrap();
        let mut abandoned = operation(
            ulid::Ulid::new().to_string(),
            UploadOperation::API_ID,
            UploadOperation::KEY,
        );
        abandoned.transfer = Some(json!({
            "stagingKey": old_key, "state": "uploading", "transferId": ulid::Ulid::new().to_string(),
            "subject": "_INBOX.upload", "expiresAt": expired, "size": null, "digest": null,
            "updatedAt": null, "contentType": null
        }));
        repository.create(abandoned).await.unwrap();
        let mut committed = operation(
            ulid::Ulid::new().to_string(),
            UploadOperation::API_ID,
            UploadOperation::KEY,
        );
        committed.transfer = Some(json!({
            "stagingKey": committed_key, "state": "committed", "transferId": ulid::Ulid::new().to_string(),
            "subject": "_INBOX.upload", "expiresAt": expired, "size": committed_size,
            "digest": committed_digest,
            "updatedAt": "2026-09-10T00:00:00Z", "contentType": null
        }));
        repository.create(committed).await.unwrap();
        let mismatched_key = "mismatched-committed";
        let _ = put(&staging, mismatched_key).await;
        let mut mismatched = operation(
            ulid::Ulid::new().to_string(),
            UploadOperation::API_ID,
            UploadOperation::KEY,
        );
        mismatched.transfer = Some(json!({
            "stagingKey": mismatched_key, "state": "committed", "transferId": ulid::Ulid::new().to_string(),
            "subject": "_INBOX.upload", "expiresAt": expired, "size": 7, "digest": "SHA-256=wrong",
            "updatedAt": "2026-09-10T00:00:00Z", "contentType": null
        }));
        repository.create(mismatched).await.unwrap();
        let interrupted_commit_id = ulid::Ulid::new().to_string();
        let mut interrupted_commit = operation(
            interrupted_commit_id.clone(),
            UploadOperation::API_ID,
            UploadOperation::KEY,
        );
        interrupted_commit.transfer = Some(json!({
            "stagingKey": interrupted_commit_key, "state": "uploading",
            "transferId": ulid::Ulid::new().to_string(), "subject": "_INBOX.upload",
            "expiresAt": expired, "size": 8, "digest": null, "updatedAt": null,
            "contentType": null
        }));
        repository.create(interrupted_commit).await.unwrap();
        let upload_runs = Arc::new(AtomicUsize::new(0));
        let runs = Arc::clone(&upload_runs);
        let upload_provider = RuntimeOperationProvider::<UploadOperation, _, _>::new(
            OperationHandlerRuntime {
                service: "service".to_owned(),
                deployment_id: "deployment".to_owned(),
                executor_id: "upload-executor".to_owned(),
                connection_id: "upload-executor".to_owned(),
                repository: repository.clone(),
                nats: client.clone(),
                service_session_key: "session".to_owned(),
                staging: staging.clone(),
                validator: Allow,
            },
            move |_context, _input, _control| {
                runs.fetch_add(1, Ordering::Relaxed);
                async { std::future::pending::<Result<(), ServerError>>().await }
            },
        );
        upload_provider.recover().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while staging.list().await.unwrap().contains(&old_key.to_owned()) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(staging
            .list()
            .await
            .unwrap()
            .contains(&committed_key.to_owned()));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let recovered = repository
                    .get(&interrupted_commit_id)
                    .await
                    .unwrap()
                    .unwrap();
                let upload: DurableOperationUpload =
                    serde_json::from_value(recovered.record.transfer.clone().unwrap()).unwrap();
                if upload.state == "invalid" {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!staging
            .list()
            .await
            .unwrap()
            .contains(&interrupted_commit_key.to_owned()));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while upload_runs.load(Ordering::Relaxed) != 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(staging
            .list()
            .await
            .unwrap()
            .contains(&mismatched_key.to_owned()));

        let id = ulid::Ulid::new().to_string();
        let created = repository
            .create(operation(
                id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        let now = now_ms();
        repository
            .claim(&id, "executor-a", now, now + 30_000)
            .await
            .unwrap();
        let provider = RuntimeOperationProvider::<TestOperation, _, _>::new(
            OperationHandlerRuntime {
                service: "service".to_owned(),
                deployment_id: "deployment".to_owned(),
                executor_id: "executor-a".to_owned(),
                connection_id: "executor-a".to_owned(),
                repository: repository.clone(),
                nats: client.clone(),
                service_session_key: "session".to_owned(),
                staging: staging.clone(),
                validator: Allow,
            },
            |_context, _input, _control| async { Ok(()) },
        );
        provider.recover().await.unwrap();
        let survivor_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                survivor_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        let now = now_ms();
        repository
            .claim(&survivor_id, "expired-executor", now, now + 100)
            .await
            .unwrap();
        let survived = tokio::time::timeout(std::time::Duration::from_secs(12), async {
            loop {
                let current = repository.get(&survivor_id).await.unwrap().unwrap();
                if current.record.owner_executor_id.as_deref() == Some("executor-a") {
                    break current;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(survived.record.owner_epoch, 2);
        let context =
            |request_id: &str, action: PermissionAction, signal: Option<&str>| RequestContext {
                request_id: Some(request_id.to_owned()),
                required_permission: Some(super::super::RoutePermission {
                    api: TestOperation::API_ID.to_owned(),
                    surface: ApiSurfaceKind::Operation,
                    name: TestOperation::KEY.to_owned(),
                    action,
                    signal: signal.map(str::to_owned),
                }),
                caller: Some(VerifiedCaller {
                    session_key: "session".to_owned(),
                    inbox_prefix: "_INBOX".to_owned(),
                    context_digest: "digest".to_owned(),
                    connection_id: "connection".to_owned(),
                    login_session_id: None,
                    principal_id: "principal".to_owned(),
                    principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
                    participant_id: "participant".to_owned(),
                    platform_privileges: Vec::new(),
                    deployment_id: None,
                    instance_id: None,
                }),
                ..RequestContext::default()
            };

        let other_api_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                other_api_id.clone(),
                "test.other-operations@v1",
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        let other_before = repository.get(&other_api_id).await.unwrap().unwrap();
        assert!(matches!(
            provider
                .get(
                    context("cross-api-get", PermissionAction::Observe, None),
                    other_api_id.clone(),
                )
                .await,
            Err(ServerError::RequestDenied { .. })
        ));
        assert_eq!(
            repository.get(&other_api_id).await.unwrap().unwrap(),
            other_before
        );

        let lease_revision_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                lease_revision_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        let mut lease_watch = repository.watch(&lease_revision_id).await.unwrap();
        assert_eq!(
            lease_watch
                .next()
                .await
                .unwrap()
                .unwrap()
                .record
                .snapshot
                .revision,
            1
        );
        let lease = repository
            .claim(&lease_revision_id, "lease-a", 0, 1)
            .await
            .unwrap();
        repository
            .renew(
                &lease_revision_id,
                "lease-a",
                lease.record.owner_epoch,
                0,
                2,
            )
            .await
            .unwrap();
        repository
            .claim(&lease_revision_id, "lease-b", 2, 3)
            .await
            .unwrap();
        for expected in 2..=4 {
            let revision = lease_watch.next().await.unwrap().unwrap().record;
            assert_eq!(revision.revision, expected);
            assert_eq!(revision.snapshot.revision, expected);
        }

        let long_running_id = ulid::Ulid::new().to_string();
        let mut long_running = operation(
            long_running_id.clone(),
            TestOperation::API_ID,
            TestOperation::KEY,
        );
        long_running.snapshot.created_at = Some("2026-08-01T00:00:00Z".to_owned());
        long_running.snapshot.updated_at = Some("2026-09-01T00:00:00Z".to_owned());
        assert_eq!(
            repository
                .create(long_running)
                .await
                .unwrap()
                .record
                .invocation_id,
            long_running_id
        );

        let expired_signal_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                expired_signal_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        let past = now_ms() - 2_000;
        repository
            .claim(
                &expired_signal_id,
                "expired-signal-owner",
                past,
                past + 1_000,
            )
            .await
            .unwrap();
        provider
            .signal(
                context(
                    "expired-owner-signal",
                    PermissionAction::Control,
                    Some("resume"),
                ),
                expired_signal_id.clone(),
                "resume".to_owned(),
                None,
            )
            .await
            .unwrap();
        let expired_signal = repository.get(&expired_signal_id).await.unwrap().unwrap();
        assert_eq!(
            expired_signal.record.owner_executor_id.as_deref(),
            Some("expired-signal-owner")
        );
        assert_eq!(expired_signal.record.owner_epoch, 1);
        assert_eq!(expired_signal.record.signals.len(), 1);

        let expired_cancel_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                expired_cancel_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        repository
            .claim(
                &expired_cancel_id,
                "expired-cancel-owner",
                past,
                past + 1_000,
            )
            .await
            .unwrap();
        let cancel_result = provider.cancel(
            context("expired-owner-cancel", PermissionAction::Cancel, None),
            expired_cancel_id.clone(),
        );
        let cancel_result =
            tokio::time::timeout(std::time::Duration::from_millis(100), cancel_result).await;
        assert!(cancel_result.is_err());
        let expired_cancel = repository.get(&expired_cancel_id).await.unwrap().unwrap();
        assert_eq!(
            expired_cancel.record.owner_executor_id.as_deref(),
            Some("expired-cancel-owner")
        );
        assert_eq!(expired_cancel.record.owner_epoch, 1);
        assert!(expired_cancel.record.cancellation_requested);

        let handler_started = Arc::new(tokio::sync::Notify::new());
        let handler_stopped = Arc::new(AtomicBool::new(false));
        let started = Arc::clone(&handler_started);
        let stopped = Arc::clone(&handler_stopped);
        let cancellation_provider = RuntimeOperationProvider::<TestOperation, _, _>::new(
            OperationHandlerRuntime {
                service: "service".to_owned(),
                deployment_id: "deployment".to_owned(),
                executor_id: "cancellation-executor".to_owned(),
                connection_id: "cancellation-executor".to_owned(),
                repository: repository.clone(),
                nats: client.clone(),
                service_session_key: "session".to_owned(),
                staging: staging.clone(),
                validator: Allow,
            },
            move |_context, _input, _control| {
                let started = Arc::clone(&started);
                let stopped = Arc::clone(&stopped);
                async move {
                    let _drop = HandlerDrop(stopped);
                    started.notify_one();
                    std::future::pending::<Result<(), ServerError>>().await
                }
            },
        );
        let handler_cancel_id = ulid::Ulid::new().to_string();
        cancellation_provider
            .start_invocation(
                context("start-cancellable-handler", PermissionAction::Invoke, None),
                handler_cancel_id.clone(),
                json!({"value": 1}),
            )
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handler_started.notified(),
        )
        .await
        .unwrap();
        let mut cancellation_watch = repository.watch(&handler_cancel_id).await.unwrap();
        cancellation_watch.next().await.unwrap().unwrap();
        let observe_cancellation = async {
            let mut requested_revision = None;
            loop {
                let current = cancellation_watch.next().await.unwrap().unwrap().record;
                assert_eq!(current.snapshot.revision, current.revision);
                if current.cancellation_requested && !current.snapshot.state.is_terminal() {
                    requested_revision = Some(current.snapshot.revision);
                }
                if current.snapshot.state == OperationState::Cancelled {
                    assert!(handler_stopped.load(Ordering::SeqCst));
                    assert!(requested_revision.is_some_and(|revision| revision < current.revision));
                    break;
                }
            }
        };
        let (cancelled, ()) = tokio::join!(
            cancellation_provider.cancel(
                context("cancel-running-handler", PermissionAction::Cancel, None),
                handler_cancel_id.clone(),
            ),
            observe_cancellation,
        );
        assert_eq!(cancelled.unwrap().state, OperationState::Cancelled);
        assert!(handler_stopped.load(Ordering::SeqCst));

        let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
        let ownership_stopped = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&ownership_stopped);
        let ownership_provider = RuntimeOperationProvider::<TestOperation, _, _>::new(
            OperationHandlerRuntime {
                service: "service".to_owned(),
                deployment_id: "deployment".to_owned(),
                executor_id: "ownership-a".to_owned(),
                connection_id: "ownership-a".to_owned(),
                repository: repository.clone(),
                nats: client.clone(),
                service_session_key: "session".to_owned(),
                staging: staging.clone(),
                validator: Allow,
            },
            move |_context, _input, control| {
                control_tx.send(control).unwrap();
                let stopped = Arc::clone(&stopped);
                async move {
                    let _drop = HandlerDrop(stopped);
                    std::future::pending::<Result<(), ServerError>>().await
                }
            },
        );
        let ownership_id = ulid::Ulid::new().to_string();
        ownership_provider
            .start_invocation(
                context("start-owned-handler", PermissionAction::Invoke, None),
                ownership_id.clone(),
                json!({"value": 1}),
            )
            .await
            .unwrap();
        let stale_handler_control = control_rx.recv().await.unwrap();
        let current = repository.get(&ownership_id).await.unwrap().unwrap();
        let mut expired = current.record.clone();
        expired.revision += 1;
        expired.lease_expires_at_ms = Some(0);
        repository
            .compare_exchange(
                current.revision,
                "ownership-a",
                current.record.owner_epoch,
                expired,
            )
            .await
            .unwrap();
        repository
            .claim(&ownership_id, "ownership-b", now_ms(), now_ms() + 30_000)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !ownership_stopped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(stale_handler_control
            .progress(json!({"stale": true}))
            .await
            .is_err());

        let first = provider
            .signal(
                context("request-1", PermissionAction::Control, Some("resume")),
                id.clone(),
                "resume".to_owned(),
                Some(json!({"choice": 1})),
            )
            .await
            .unwrap();
        let replay = provider
            .signal(
                context("request-1", PermissionAction::Control, Some("resume")),
                id.clone(),
                "resume".to_owned(),
                Some(json!({"choice": 1})),
            )
            .await
            .unwrap();
        assert_eq!(replay.signal_sequence, first.signal_sequence);
        assert!(matches!(
            provider
                .signal(
                    context("request-1", PermissionAction::Control, Some("resume")),
                    id.clone(),
                    "resume".to_owned(),
                    Some(json!({"choice": 9})),
                )
                .await,
            Err(ServerError::OperationIdempotencyConflict { kind: "signal", .. })
        ));
        assert!(matches!(
            provider
                .signal(
                    context("request-1", PermissionAction::Control, Some("different")),
                    id.clone(),
                    "different".to_owned(),
                    Some(json!({"choice": 1})),
                )
                .await,
            Err(ServerError::OperationIdempotencyConflict { kind: "signal", .. })
        ));
        provider
            .signal(
                context("request-2", PermissionAction::Control, Some("resume")),
                id.clone(),
                "resume".to_owned(),
                Some(json!({"choice": 2})),
            )
            .await
            .unwrap();
        let signalled = repository.get(&id).await.unwrap().unwrap();
        assert_eq!(
            signalled
                .record
                .signals
                .iter()
                .map(|signal| signal.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(signalled.record.signals.len(), 2);

        let first_control = control(repository.clone(), client.clone(), "executor-a", &id).await;
        let mut signals = first_control.signals().await.unwrap();
        assert_eq!(signals.next().await.unwrap().unwrap().signal_sequence, 1);
        drop(signals);
        let mut expired_owner = repository.get(&id).await.unwrap().unwrap().record;
        expired_owner.revision += 1;
        expired_owner.lease_expires_at_ms = Some(0);
        repository
            .compare_exchange(
                signalled.revision,
                "executor-a",
                signalled.record.owner_epoch,
                expired_owner,
            )
            .await
            .unwrap();
        let same_executor = repository
            .claim(&id, "executor-a", now_ms(), now_ms() + 30_000)
            .await
            .unwrap();
        assert_eq!(
            same_executor.record.owner_epoch,
            signalled.record.owner_epoch + 1
        );
        assert!(first_control
            .progress(json!({"stale": true}))
            .await
            .is_err());
        let mut expired_same_executor = same_executor.record.clone();
        expired_same_executor.revision += 1;
        expired_same_executor.lease_expires_at_ms = Some(0);
        repository
            .compare_exchange(
                same_executor.revision,
                "executor-a",
                same_executor.record.owner_epoch,
                expired_same_executor,
            )
            .await
            .unwrap();
        let recovered = repository
            .claim(&id, "executor-b", now_ms(), now_ms() + 30_000)
            .await
            .unwrap();
        let second_control = control(repository.clone(), client.clone(), "executor-b", &id).await;
        let mut redelivered = second_control.signals().await.unwrap();
        assert_eq!(
            redelivered.next().await.unwrap().unwrap().signal_sequence,
            1
        );
        assert_eq!(
            redelivered.next().await.unwrap().unwrap().signal_sequence,
            2
        );
        assert!(first_control.acknowledge_signal(1).await.is_err());
        assert!(first_control
            .complete(json!({"stale": true}))
            .await
            .is_err());
        assert!(first_control
            .emit_update(json!({"stale": true}))
            .await
            .is_err());
        assert!(first_control.upload().await.is_err());
        second_control.acknowledge_signal(1).await.unwrap();
        assert!(repository.get(&id).await.unwrap().unwrap().record.signals[0].acknowledged);

        // A delayed E1 execution cannot enter after E2 acquired the operation,
        // and the current acquisition is still admitted by the same boundary.
        let stale_fence = OwnerFence {
            executor_id: "executor-a".to_owned(),
            connection_id: "executor-a".to_owned(),
            owner_epoch: signalled.record.owner_epoch,
        };
        assert!(admit_operation_execution::<TestOperation>(
            &repository,
            &provider.mutation_gate,
            &stale_fence,
            "deployment",
            &id,
        )
        .await
        .unwrap()
        .is_none());
        let current_fence = OwnerFence {
            executor_id: "executor-b".to_owned(),
            connection_id: "executor-b".to_owned(),
            owner_epoch: recovered.record.owner_epoch,
        };
        assert!(admit_operation_execution::<TestOperation>(
            &repository,
            &provider.mutation_gate,
            &current_fence,
            "deployment",
            &id,
        )
        .await
        .unwrap()
        .is_some());

        let mut live = provider.watch(
            context("watch-replica", PermissionAction::Observe, None),
            id.clone(),
        );
        assert!(matches!(
            live.next().await.unwrap().unwrap(),
            OperationLiveEvent::Snapshot(_)
        ));
        second_control
            .progress(json!({"durable": true}))
            .await
            .unwrap();
        assert!(matches!(
            live.next().await.unwrap().unwrap(),
            OperationLiveEvent::Snapshot(_)
        ));
        let current = repository.get(&id).await.unwrap().unwrap();
        let payload = Bytes::from_static(br#"{"forged":true}"#);
        let mut forged = async_nats::HeaderMap::new();
        forged.insert("trellis-owner-executor", "executor-b");
        forged.insert(
            "trellis-owner-epoch",
            current.record.owner_epoch.to_string().as_str(),
        );
        forged.insert("trellis-update-sequence", "1");
        forged.insert("trellis-update-time", "2026-09-13T00:00:00Z");
        client
            .publish_with_reply_and_headers(
                operation_update_subject::<TestOperation>("deployment", &id),
                client.new_inbox(),
                forged,
                payload,
            )
            .await
            .unwrap();
        client.flush().await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), live.next())
                .await
                .is_err()
        );
        drop(live);

        let cancel_id = ulid::Ulid::new().to_string();
        let pending = repository
            .create(operation(
                cancel_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        assert!(pending.record.owner_executor_id.is_none());
        let pending_cancel = provider.cancel(
            context("cancel-request", PermissionAction::Cancel, None),
            cancel_id.clone(),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), pending_cancel)
                .await
                .is_err()
        );
        let cancelled = repository.get(&cancel_id).await.unwrap().unwrap();
        assert!(cancelled.record.cancellation_requested);
        assert_eq!(cancelled.record.snapshot.state, OperationState::Pending);

        let progress_race_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                progress_race_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        let past = now_ms() - 2_000;
        repository
            .claim(&progress_race_id, "executor-a", past, past + 1_000)
            .await
            .unwrap();
        let mut progress_control = control(
            repository.clone(),
            client.clone(),
            "executor-a",
            &progress_race_id,
        )
        .await;
        progress_control.durable.mutation_gate = Arc::clone(&provider.mutation_gate);
        let (cancel_result, progress_result) = tokio::join!(
            cancellation_provider.cancel(
                context("cancel-progress-race", PermissionAction::Cancel, None),
                progress_race_id.clone(),
            ),
            progress_control.progress(json!({"value": "racing"})),
        );
        cancel_result.unwrap();
        assert!(progress_result.is_err());
        let progress_race = repository.get(&progress_race_id).await.unwrap().unwrap();
        assert_eq!(
            progress_race.record.snapshot.state,
            OperationState::Cancelled
        );
        assert!(progress_race.record.cancellation_requested);

        let completion_race_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                completion_race_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        repository
            .claim(
                &completion_race_id,
                "executor-a",
                now_ms(),
                now_ms() + 30_000,
            )
            .await
            .unwrap();
        let mut completion_control = control(
            repository.clone(),
            client.clone(),
            "executor-a",
            &completion_race_id,
        )
        .await;
        completion_control.durable.mutation_gate = Arc::clone(&provider.mutation_gate);
        let (completion_result, cancel_result) = tokio::join!(
            completion_control.complete(json!({"value": "completed"})),
            cancellation_provider.cancel(
                context("cancel-complete-race", PermissionAction::Cancel, None),
                completion_race_id.clone(),
            ),
        );
        completion_result.unwrap();
        assert!(cancel_result.is_err());
        let completion_race = repository.get(&completion_race_id).await.unwrap().unwrap();
        assert_eq!(
            completion_race.record.snapshot.state,
            OperationState::Completed
        );
        assert_eq!(
            completion_race.record.snapshot.output,
            Some(json!({"value": "completed"}))
        );
        assert!(!completion_race.record.cancellation_requested);

        let completed_id = ulid::Ulid::new().to_string();
        repository
            .create(operation(
                completed_id.clone(),
                TestOperation::API_ID,
                TestOperation::KEY,
            ))
            .await
            .unwrap();
        repository
            .claim(&completed_id, "executor-a", now_ms(), now_ms() + 30_000)
            .await
            .unwrap();
        let completed_control = control(
            repository.clone(),
            client.clone(),
            "executor-a",
            &completed_id,
        )
        .await;
        completed_control
            .complete(json!({"value": "final"}))
            .await
            .unwrap();
        let completed = repository.get(&completed_id).await.unwrap().unwrap();
        assert!(matches!(
            provider
                .cancel(
                    context("post-complete-cancel", PermissionAction::Cancel, None),
                    completed_id.clone(),
                )
                .await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert!(matches!(
            completed_control.progress(json!({"value": "late"})).await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert!(matches!(
            completed_control.complete(json!({"value": "late"})).await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert!(matches!(
            completed_control
                .fail(OperationFailure {
                    message: "late".to_owned(),
                })
                .await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert!(matches!(
            completed_control.emit_update(json!({"value": "late"})).await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert!(matches!(
            completed_control.acknowledge_signal(1).await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert!(matches!(
            provider
                .signal(
                    context(
                        "post-complete-signal",
                        PermissionAction::Control,
                        Some("resume"),
                    ),
                    completed_id.clone(),
                    "resume".to_owned(),
                    None,
                )
                .await,
            Err(ServerError::OperationAlreadyTerminal { state, .. }) if state == "completed"
        ));
        assert_eq!(
            repository.get(&completed_id).await.unwrap().unwrap(),
            completed
        );

        assert_eq!(created.record.revision, 1);
        jetstream.delete_key_value(bucket).await.unwrap();
        jetstream.delete_object_store(object_bucket).await.unwrap();
        nats.stop().unwrap();
        drop(recovered);
    }
}
