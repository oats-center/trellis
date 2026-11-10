use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::client::{TransportGenerationManager, TransportLease};
use crate::jobs::active_job::ActiveJob as RuntimeActiveJob;
use crate::jobs::bindings::{JobsBinding, JobsQueueBinding};
use crate::jobs::manager::{JobManager, TrellisJobMetaSource};
use crate::jobs::projection::is_terminal;
use crate::jobs::runtime_ref::NatsJobWaiter;
use crate::jobs::types::{Job, JobContext, JobLogEntry, JobProgress, JobState, JobWaitEdge};
use crate::jobs::updates::{JobUpdate, JobUpdateSchema, NoJobUpdates};
use crate::jobs::TrellisJobEventPublisher;

pub(super) type RuntimeJob = RuntimeActiveJob<TrellisJobEventPublisher, TrellisJobMetaSource>;
/// Marks the payload, result and update types a handle is typed by.
type Typed<TPayload, TResult, TUpdate> = PhantomData<fn() -> (TPayload, TResult, TUpdate)>;

/// Errors returned by the typed jobs API.
#[derive(Debug, thiserror::Error)]
#[doc = concat!("Public Trellis value set `", stringify!(JobsError), "`.")]
pub enum JobsError {
    #[error("{message}")]
    Message { message: String },
    #[error("failed to decode job payload: {0}")]
    DecodePayload(serde_json::Error),
    #[error("failed to decode job result: {0}")]
    DecodeResult(serde_json::Error),
    #[error("failed to encode job payload: {0}")]
    EncodePayload(serde_json::Error),
    #[error("failed to encode job result: {0}")]
    EncodeResult(serde_json::Error),
    #[error(transparent)]
    NotEnqueued(#[from] JobNotEnqueued),
}

/// Reason a keyed job submission did not enqueue new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc = concat!("Public Trellis value set `", stringify!(JobNotEnqueuedReason), "`.")]
pub enum JobNotEnqueuedReason {
    ActiveLimit,
    QueueDepth,
    StaleBlocked,
    Coalesced,
}

/// Typed expected failure returned by strict keyed job creation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("job was not enqueued: {reason:?}")]
#[doc = concat!("Public Trellis data type `", stringify!(JobNotEnqueued), "`.")]
pub struct JobNotEnqueued {
    #[doc = concat!("The `", stringify!(reason), "` value.")]
    pub reason: JobNotEnqueuedReason,
    #[doc = concat!("The `", stringify!(key), "` value.")]
    pub key: String,
    #[doc = concat!("The `", stringify!(active), "` value.")]
    pub active: usize,
    #[doc = concat!("The `", stringify!(queued), "` value.")]
    pub queued: usize,
    #[doc = concat!("The `", stringify!(limit), "` value.")]
    pub limit: usize,
    #[doc = concat!("The `", stringify!(existing_job_id), "` value.")]
    pub existing_job_id: Option<String>,
}

/// Service-local jobs API entrypoint.
pub trait JobsService {
    type Facade: JobsFacade;

    fn jobs(&self) -> Self::Facade;
}

/// Typed service-local jobs facade.
pub trait JobsFacade {
    type WorkerHost: JobWorkerHost;

    fn start_workers(&self) -> impl Future<Output = Result<Self::WorkerHost, JobsError>> + Send;
}

/// Typed queue API for one job type.
pub trait JobQueue<TPayload, TResult> {
    fn create(
        &self,
        payload: TPayload,
    ) -> impl Future<Output = Result<JobRef<TPayload, TResult>, JobsError>> + Send;

    fn submit(
        &self,
        payload: TPayload,
    ) -> impl Future<Output = Result<JobSubmitOutcome<TPayload, TResult>, JobsError>> + Send;

    fn handle<H, Fut>(&self, handler: H) -> impl Future<Output = Result<(), JobsError>> + Send
    where
        H: Fn(ActiveJob<TPayload, TResult>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<TResult, JobsError>> + Send;
}

impl From<crate::jobs::manager::JobNotEnqueued> for JobNotEnqueued {
    fn from(value: crate::jobs::manager::JobNotEnqueued) -> Self {
        use crate::jobs::manager::JobNotEnqueuedReason as Reason;
        Self {
            reason: match value.reason {
                Reason::ActiveLimit => JobNotEnqueuedReason::ActiveLimit,
                Reason::QueueDepth => JobNotEnqueuedReason::QueueDepth,
                Reason::StaleBlocked => JobNotEnqueuedReason::StaleBlocked,
                Reason::Coalesced => JobNotEnqueuedReason::Coalesced,
            },
            key: value.key,
            active: value.active,
            queued: value.queued,
            limit: value.limit,
            existing_job_id: value.existing_job_id,
        }
    }
}

/// Policy-aware keyed job submission outcome.
#[derive(Debug)]
pub enum JobSubmitOutcome<TPayload, TResult, TUpdate = NoJobUpdates> {
    Accepted {
        job_ref: JobRef<TPayload, TResult, TUpdate>,
        key: Option<String>,
    },
    Rejected(JobNotEnqueued),
    Coalesced {
        key: String,
        existing: JobIdentity,
        reason: String,
    },
    Replaced {
        key: String,
        replaced: JobIdentity,
        job_ref: JobRef<TPayload, TResult, TUpdate>,
    },
}

/// Handle for a created job.
///
/// Each read, wait, or cancellation acquires the currently published transport
/// and retains that generation until the invocation completes or is dropped.
pub struct JobRef<TPayload, TResult, TUpdate = NoJobUpdates> {
    identity: JobIdentity,
    seed: Job,
    client: std::sync::Arc<crate::client::TrellisClient>,
    generations: TransportGenerationManager,
    binding: JobsBinding,
    queue: JobsQueueBinding,
    update: Option<JobUpdateSchema>,
    _types: Typed<TPayload, TResult, TUpdate>,
}

impl<TPayload, TResult, TUpdate> std::fmt::Debug for JobRef<TPayload, TResult, TUpdate> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobRef")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl<TPayload, TResult, TUpdate> Clone for JobRef<TPayload, TResult, TUpdate> {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            seed: self.seed.clone(),
            client: self.client.clone(),
            generations: self.generations.clone(),
            binding: self.binding.clone(),
            queue: self.queue.clone(),
            update: self.update,
            _types: PhantomData,
        }
    }
}

impl<TPayload, TResult, TUpdate> JobRef<TPayload, TResult, TUpdate>
where
    TPayload: DeserializeOwned + Clone + Send + Sync + 'static,
    TResult: DeserializeOwned + Clone + Send + Sync + 'static,
    TUpdate: DeserializeOwned + Send + 'static,
{
    pub(crate) fn from_runtime(
        seed: Job,
        client: std::sync::Arc<crate::client::TrellisClient>,
        binding: JobsBinding,
        queue: JobsQueueBinding,
        update: Option<JobUpdateSchema>,
    ) -> Self {
        Self {
            identity: JobIdentity::from(&seed),
            seed,
            generations: client.transport_generations(),
            client,
            binding,
            queue,
            update,
            _types: PhantomData,
        }
    }

    /// Subscribe to the job's live-only updates.
    ///
    /// The stream yields each validated update in order, one attempt after
    /// another, and ends once the job reaches a terminal state. Updates
    /// published before the subscription opened are not replayed; read
    /// [`JobRef::get`] for the durable state.
    pub async fn updates(
        &self,
    ) -> Result<
        futures_util::stream::BoxStream<'static, Result<JobUpdate<TUpdate>, JobsError>>,
        JobsError,
    > {
        let schema = self.update.ok_or_else(|| {
            jobs_message(format!(
                "jobs queue '{}' does not declare updates",
                self.queue.queue_type
            ))
        })?;
        let (lease, waiter) = self.published_waiter()?;
        let updates = waiter.updates(self.identity.id.clone(), schema).await?;
        // The stream keeps the transport it subscribed on.
        Ok(Box::pin(futures_util::StreamExt::map(
            updates,
            move |update| {
                let _ = &lease;
                update
            },
        )))
    }

    fn published_waiter(&self) -> Result<(TransportLease, NatsJobWaiter), JobsError> {
        let lease = self
            .generations
            .acquire_application_published()
            .map_err(jobs_message)?;
        let waiter = NatsJobWaiter::new(
            self.client.clone(),
            lease.nats().clone(),
            self.queue.clone(),
            Duration::from_secs(30),
        );
        Ok((lease, waiter))
    }

    #[doc = concat!("Trellis API operation `", stringify!(identity), "`.")]
    pub fn identity(&self) -> &JobIdentity {
        &self.identity
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(get), "`.")]
    pub async fn get(&self) -> Result<JobSnapshot<TPayload, TResult>, JobsError> {
        let (_lease, waiter) = self.published_waiter()?;
        JobSnapshot::try_from(waiter.get(self.seed.clone()).await?)
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(wait), "`.")]
    pub async fn wait(&self) -> Result<TerminalJob<TPayload, TResult>, JobsError> {
        let (_lease, waiter) = self.published_waiter()?;
        waiter.wait_for_terminal(self.seed.clone()).await?;
        JobSnapshot::try_from(waiter.get(self.seed.clone()).await?)
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(cancel), "`.")]
    pub async fn cancel(&self) -> Result<JobSnapshot<TPayload, TResult>, JobsError> {
        let (lease, waiter) = self.published_waiter()?;
        let current = waiter.get(self.seed.clone()).await?;
        if is_terminal(current.state) {
            return JobSnapshot::try_from(current);
        }
        let publisher = TrellisJobEventPublisher::new(lease.nats().clone());
        let manager = if self.queue.key_concurrency.is_some() {
            let coordinator = crate::jobs::keys::NatsKeyCoordinator::open_for_service(
                lease.nats().clone(),
                &self.binding.namespace,
            )
            .await
            .map_err(jobs_message)?;
            JobManager::new_with_key_coordinator(
                publisher,
                self.binding.clone(),
                TrellisJobMetaSource,
                Arc::new(coordinator),
            )
        } else {
            JobManager::new(publisher, self.binding.clone(), TrellisJobMetaSource)
        };
        manager.cancel(&current).await.map_err(jobs_message)?;
        waiter.wait_for_terminal(current).await?;
        JobSnapshot::try_from(waiter.get(self.seed.clone()).await?)
    }
}

/// Typed snapshot of one job.
#[derive(Debug, Clone, PartialEq)]
pub struct JobSnapshot<TPayload, TResult> {
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: String,
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: JobContext,
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(r#type), "` value.")]
    pub r#type: String,
    #[doc = concat!("The `", stringify!(state), "` value.")]
    pub state: JobState,
    #[doc = concat!("The `", stringify!(payload), "` value.")]
    pub payload: TPayload,
    #[doc = concat!("The `", stringify!(result), "` value.")]
    pub result: Option<TResult>,
    #[doc = concat!("The `", stringify!(created_at), "` value.")]
    pub created_at: String,
    #[doc = concat!("The `", stringify!(updated_at), "` value.")]
    pub updated_at: String,
    #[doc = concat!("The `", stringify!(started_at), "` value.")]
    pub started_at: Option<String>,
    #[doc = concat!("The `", stringify!(completed_at), "` value.")]
    pub completed_at: Option<String>,
    #[doc = concat!("The `", stringify!(tries), "` value.")]
    pub tries: u64,
    #[doc = concat!("The `", stringify!(max_tries), "` value.")]
    pub max_tries: u64,
    #[doc = concat!("The `", stringify!(last_error), "` value.")]
    pub last_error: Option<String>,
    #[doc = concat!("The `", stringify!(progress), "` value.")]
    pub progress: Option<JobProgress>,
    #[doc = concat!("The `", stringify!(logs), "` value.")]
    pub logs: Vec<JobLogEntry>,
}

impl<TPayload, TResult> TryFrom<Job> for JobSnapshot<TPayload, TResult>
where
    TPayload: DeserializeOwned,
    TResult: DeserializeOwned,
{
    type Error = JobsError;

    fn try_from(job: Job) -> Result<Self, Self::Error> {
        let payload = serde_json::from_value(job.payload).map_err(JobsError::DecodePayload)?;
        let result = job
            .result
            .map(|value| serde_json::from_value(value).map_err(JobsError::DecodeResult))
            .transpose()?;

        Ok(Self {
            id: job.id,
            context: job.context,
            service: job.service,
            r#type: job.job_type,
            state: job.state,
            payload,
            result,
            created_at: job.created_at,
            updated_at: job.updated_at,
            started_at: job.started_at,
            completed_at: job.completed_at,
            tries: job.tries,
            max_tries: job.max_tries,
            last_error: job.last_error,
            progress: job.progress,
            logs: job.logs.unwrap_or_default(),
        })
    }
}

/// Terminal snapshot of one job.
pub type TerminalJob<TPayload, TResult> = JobSnapshot<TPayload, TResult>;

/// Typed active-job handle.
pub struct ActiveJob<TPayload, TResult, TUpdate = NoJobUpdates> {
    payload: TPayload,
    runtime: RuntimeJob,
    update: Option<JobUpdateSchema>,
    _types: Typed<(), TResult, TUpdate>,
}

impl<TPayload, TResult, TUpdate> std::fmt::Debug for ActiveJob<TPayload, TResult, TUpdate> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveJob")
            .field("context", self.runtime.context())
            .field("state", &self.runtime.job().state)
            .field("tries", &self.runtime.job().tries)
            .finish_non_exhaustive()
    }
}

impl<TPayload, TResult, TUpdate> ActiveJob<TPayload, TResult, TUpdate>
where
    TPayload: Send + Sync + 'static,
    TResult: Send + Sync + 'static,
    TUpdate: Serialize + Send + Sync + 'static,
{
    pub(super) fn from_runtime(
        payload: TPayload,
        runtime: RuntimeJob,
        update: Option<JobUpdateSchema>,
    ) -> Self {
        Self {
            payload,
            runtime,
            update,
            _types: PhantomData,
        }
    }

    #[doc = concat!("Trellis API operation `", stringify!(payload), "`.")]
    pub fn payload(&self) -> &TPayload {
        &self.payload
    }

    #[doc = concat!("Trellis API operation `", stringify!(context), "`.")]
    pub fn context(&self) -> &JobContext {
        self.runtime.context()
    }

    #[doc = concat!("Trellis API operation `", stringify!(state), "`.")]
    pub fn state(&self) -> JobState {
        self.runtime.job().state
    }

    #[doc = concat!("Trellis API operation `", stringify!(tries), "`.")]
    pub fn tries(&self) -> u64 {
        self.runtime.job().tries
    }

    /// Return the ordinary execution-attempt budget; cleanup may outlive it.
    pub fn max_tries(&self) -> u64 {
        self.runtime.job().max_tries
    }

    /// Observe cancellation and its reason for this execution-owned delivery.
    pub fn cancellation_token(&self) -> crate::jobs::JobCancellationToken {
        self.runtime.cancellation_token()
    }

    #[doc = concat!("Trellis API operation `", stringify!(redelivery_count), "`.")]
    pub fn redelivery_count(&self) -> u64 {
        self.tries().saturating_sub(1)
    }

    #[doc = concat!("Trellis API operation `", stringify!(is_redelivery), "`.")]
    pub fn is_redelivery(&self) -> bool {
        self.redelivery_count() > 0
    }

    #[doc = concat!("Trellis API operation `", stringify!(is_cancelled), "`.")]
    pub fn is_cancelled(&self) -> bool {
        self.runtime.is_cancelled()
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(heartbeat), "`.")]
    pub async fn heartbeat(&self) -> Result<(), JobsError> {
        self.runtime.heartbeat().await.map_err(jobs_message)
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(progress), "`.")]
    pub async fn progress(&self, value: JobProgress) -> Result<(), JobsError> {
        self.runtime
            .update_progress(
                value.current.unwrap_or_default(),
                value.total.unwrap_or_default(),
                value.message,
            )
            .await
            .map_err(jobs_message)
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(log), "`.")]
    pub async fn log(&self, entry: JobLogEntry) -> Result<(), JobsError> {
        self.runtime
            .log(entry.level, entry.message)
            .await
            .map_err(jobs_message)
    }

    /// Publish a live-only update to the job's subscribers.
    ///
    /// Updates are not stored: only subscribers already listening receive one,
    /// and the job's durable state is unchanged. Each update is validated
    /// against the queue's declared update schema, and sequence numbers order
    /// the updates of one attempt.
    pub async fn emit_update(&self, update: TUpdate) -> Result<JobUpdate<TUpdate>, JobsError> {
        let schema = self.update.ok_or_else(|| {
            jobs_message(format!(
                "jobs queue '{}' does not declare updates",
                self.runtime.job().job_type
            ))
        })?;
        self.runtime
            .emit_update(schema, update)
            .await
            .map_err(jobs_message)
    }

    /// Record that this job is waiting on `wait_edge` while `future` runs, so
    /// its state shows what it waits for.
    pub async fn wait_for<T, Fut>(&self, wait_edge: JobWaitEdge, future: Fut) -> T
    where
        Fut: Future<Output = T>,
    {
        self.runtime.wait_for(wait_edge, future).await
    }
}

fn jobs_message(error: impl ToString) -> JobsError {
    JobsError::Message {
        message: error.to_string(),
    }
}

/// Job identity fields used by service-local and admin APIs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[doc = concat!("Public Trellis data type `", stringify!(JobIdentity), "`.")]
pub struct JobIdentity {
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: String,
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: String,
    #[doc = concat!("The `", stringify!(id), "` value.")]
    pub id: String,
}

/// Filter used by admin query helpers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[doc = concat!("Public Trellis data type `", stringify!(JobFilter), "`.")]
pub struct JobFilter {
    #[doc = concat!("The `", stringify!(service), "` value.")]
    pub service: Option<String>,
    #[doc = concat!("The `", stringify!(job_type), "` value.")]
    pub job_type: Option<String>,
    #[doc = concat!("The `", stringify!(state), "` value.")]
    pub state: Option<JobState>,
}

impl From<&Job> for JobIdentity {
    fn from(job: &Job) -> Self {
        Self {
            service: job.service.clone(),
            job_type: job.job_type.clone(),
            id: job.id.clone(),
        }
    }
}

/// Typed worker-host abstraction.
pub trait JobWorkerHost {
    fn stop(self) -> impl Future<Output = Result<(), JobsError>> + Send;
    fn join(self) -> impl Future<Output = Result<(), JobsError>> + Send;
}

/// Convert a typed payload into a JSON value for raw wire-model helpers.
pub fn to_value<T: Serialize>(value: T) -> Result<Value, JobsError> {
    serde_json::to_value(value).map_err(JobsError::EncodePayload)
}
