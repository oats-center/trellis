use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::future::Future;
use std::marker::PhantomData;

use crate::jobs::active_job::ActiveJob as RuntimeActiveJob;
use crate::jobs::manager::{JobManager, TrellisJobMetaSource};
use crate::jobs::projection::is_terminal;
use crate::jobs::runtime_ref::NatsJobWaiter;
use crate::jobs::types::{Job, JobContext, JobLogEntry, JobProgress, JobState};
use crate::jobs::TrellisJobEventPublisher;

pub(super) type RuntimeJob = RuntimeActiveJob<TrellisJobEventPublisher, TrellisJobMetaSource>;
type RuntimeJobManager = JobManager<TrellisJobEventPublisher, TrellisJobMetaSource>;

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

/// Policy-aware keyed job submission outcome.
#[derive(Debug)]
pub enum JobSubmitOutcome<TPayload, TResult> {
    Accepted {
        job_ref: JobRef<TPayload, TResult>,
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
        job_ref: JobRef<TPayload, TResult>,
    },
}

/// Handle for a created job.
pub struct JobRef<TPayload, TResult> {
    identity: JobIdentity,
    seed: Job,
    waiter: NatsJobWaiter,
    manager: RuntimeJobManager,
    _types: PhantomData<fn() -> (TPayload, TResult)>,
}

impl<TPayload, TResult> std::fmt::Debug for JobRef<TPayload, TResult> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobRef")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl<TPayload, TResult> Clone for JobRef<TPayload, TResult> {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            seed: self.seed.clone(),
            waiter: self.waiter.clone(),
            manager: self.manager.clone(),
            _types: PhantomData,
        }
    }
}

impl<TPayload, TResult> JobRef<TPayload, TResult>
where
    TPayload: DeserializeOwned + Clone + Send + Sync + 'static,
    TResult: DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub(crate) fn from_runtime(
        seed: Job,
        waiter: NatsJobWaiter,
        manager: RuntimeJobManager,
    ) -> Self {
        Self {
            identity: JobIdentity::from(&seed),
            seed,
            waiter,
            manager,
            _types: PhantomData,
        }
    }

    #[doc = concat!("Trellis API operation `", stringify!(identity), "`.")]
    pub fn identity(&self) -> &JobIdentity {
        &self.identity
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(get), "`.")]
    pub async fn get(&self) -> Result<JobSnapshot<TPayload, TResult>, JobsError> {
        JobSnapshot::try_from(self.waiter.get(self.seed.clone()).await?)
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(wait), "`.")]
    pub async fn wait(&self) -> Result<TerminalJob<TPayload, TResult>, JobsError> {
        self.waiter.wait_for_terminal(self.seed.clone()).await?;
        self.get().await
    }

    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(cancel), "`.")]
    pub async fn cancel(&self) -> Result<JobSnapshot<TPayload, TResult>, JobsError> {
        let current = self.waiter.get(self.seed.clone()).await?;
        if is_terminal(current.state) {
            return JobSnapshot::try_from(current);
        }
        self.manager.cancel(&current).await.map_err(jobs_message)?;
        self.waiter.wait_for_terminal(current).await?;
        self.get().await
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
pub struct ActiveJob<TPayload, TResult> {
    payload: TPayload,
    runtime: RuntimeJob,
    _result: PhantomData<TResult>,
}

impl<TPayload, TResult> std::fmt::Debug for ActiveJob<TPayload, TResult> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveJob")
            .field("context", self.runtime.context())
            .field("state", &self.runtime.job().state)
            .field("tries", &self.runtime.job().tries)
            .finish_non_exhaustive()
    }
}

impl<TPayload, TResult> ActiveJob<TPayload, TResult>
where
    TPayload: Send + Sync + 'static,
    TResult: Send + Sync + 'static,
{
    pub(super) fn from_runtime(payload: TPayload, runtime: RuntimeJob) -> Self {
        Self {
            payload,
            runtime,
            _result: PhantomData,
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
