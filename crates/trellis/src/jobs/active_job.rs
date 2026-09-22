//! Runtime-facing active job handle.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::jobs::manager::{JobManager, JobManagerError, JobMetaSource};
use crate::jobs::publisher::JobEventPublisher;
use crate::jobs::runtime_worker::JobCancellationToken;
use crate::jobs::types::{Job, JobContext, JobLogEntry, JobLogLevel, JobProgress, JobWaitEdge};
use crate::jobs::updates::{JobUpdate, JobUpdateDescriptor};

type HeartbeatHook = Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Errors returned by [`ActiveJob`] runtime operations.
#[derive(Debug, thiserror::Error)]
#[doc = concat!("Public Trellis value set `", stringify!(ActiveJobRuntimeError), "`.")]
pub enum ActiveJobRuntimeError {
    #[error("failed to send worker heartbeat: {0}")]
    Heartbeat(String),
}

/// Handler-facing runtime handle for an in-flight job.
///
/// This type wraps the projected [`Job`] snapshot together with the runtime
/// helpers needed while a worker is actively processing that job.
#[derive(Clone)]
pub struct ActiveJob<P, M> {
    manager: JobManager<P, M>,
    job: Job,
    cancellation: JobCancellationToken,
    heartbeat: HeartbeatHook,
    update_sequence: Arc<AtomicU64>,
    update_gate: Arc<tokio::sync::Mutex<bool>>,
}

impl<P, M> ActiveJob<P, M> {
    #[doc = concat!("Trellis API operation `", stringify!(new), "`.")]
    pub fn new(
        manager: JobManager<P, M>,
        job: Job,
        cancellation: JobCancellationToken,
        heartbeat: HeartbeatHook,
    ) -> Self {
        Self {
            manager,
            job,
            cancellation,
            heartbeat,
            update_sequence: Arc::new(AtomicU64::new(0)),
            update_gate: Arc::new(tokio::sync::Mutex::new(true)),
        }
    }

    /// Return the current in-memory job snapshot for this handler invocation.
    #[doc = concat!("Trellis API operation `", stringify!(job), "`.")]
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// Return the request and trace context carried by this job.
    #[doc = concat!("Trellis API operation `", stringify!(context), "`.")]
    pub fn context(&self) -> &JobContext {
        &self.job.context
    }

    /// Return whether cooperative cancellation has been requested.
    #[doc = concat!("Trellis API operation `", stringify!(is_cancelled), "`.")]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Clone the underlying cooperative cancellation token.
    #[doc = concat!("Trellis API operation `", stringify!(cancellation_token), "`.")]
    pub fn cancellation_token(&self) -> JobCancellationToken {
        self.cancellation.clone()
    }

    /// Extend the JetStream ack deadline for a long-running active job.
    ///
    /// This is only available when the job is running under a queue-worker path
    /// that provides a runtime heartbeat hook.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(heartbeat), "`.")]
    pub async fn heartbeat(&self) -> Result<(), ActiveJobRuntimeError> {
        (self.heartbeat)()
            .await
            .map_err(ActiveJobRuntimeError::Heartbeat)
    }
}

impl<P, M> ActiveJob<P, M>
where
    P: JobEventPublisher,
    M: JobMetaSource,
{
    /// Publish a progress update for this active job.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(update_progress), "`.")]
    pub async fn update_progress(
        &self,
        current: u64,
        total: u64,
        message: Option<String>,
    ) -> Result<(), JobManagerError<P::Error>> {
        self.manager
            .emit_progress(
                &self.job,
                JobProgress {
                    step: None,
                    message,
                    current: Some(current),
                    total: Some(total),
                },
            )
            .await
    }

    /// Emit one contract-typed live-only update for this active attempt.
    pub async fn emit_update<D>(
        &self,
        update: D::Update,
    ) -> Result<JobUpdate<D::Update>, JobManagerError<P::Error>>
    where
        D: JobUpdateDescriptor,
        D::Update: Clone,
    {
        let gate = self.update_gate.lock().await;
        if !*gate {
            return Err(JobManagerError::UpdatesClosed {
                job_id: self.job.id.clone(),
            });
        }
        let sequence = self.update_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let result = self
            .manager
            .emit_update::<D>(&self.job, sequence, update)
            .await;
        drop(gate);
        result
    }

    #[doc = concat!("Trellis API operation `", stringify!(update_gate), "`.")]
    pub fn update_gate(&self) -> Arc<tokio::sync::Mutex<bool>> {
        Arc::clone(&self.update_gate)
    }

    /// Publish a log entry for this active job.
    #[doc = concat!("Asynchronous Trellis API operation `", stringify!(log), "`.")]
    pub async fn log(
        &self,
        level: JobLogLevel,
        message: impl Into<String>,
    ) -> Result<(), JobManagerError<P::Error>> {
        self.manager
            .emit_log(
                &self.job,
                JobLogEntry {
                    timestamp: self.manager.now_iso(),
                    level,
                    message: message.into(),
                },
            )
            .await
    }

    /// Record that this active job is waiting while a future runs.
    pub async fn wait_for<T, Fut>(&self, wait_edge: JobWaitEdge, future: Fut) -> T
    where
        Fut: Future<Output = T>,
    {
        let _ = self
            .manager
            .emit_waiting(&self.job, wait_edge.clone())
            .await;
        let output = future.await;
        let _ = self.manager.emit_resumed(&self.job, wait_edge).await;
        output
    }
}
