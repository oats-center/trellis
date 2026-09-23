use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream, AckKind};
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime};
use tracing::Instrument as _;
use ulid::Ulid;

use crate::jobs::active_job::ActiveJob;
use crate::jobs::bindings::{
    JobKeyConcurrencyBinding, JobQueueWhenFull, JobsQueueBinding, JobsRuntimeBinding,
};
use crate::jobs::job_key;
use crate::jobs::keys::{
    derive_job_key, new_key_state, release_active_slot, renew_active_slot, AcquireSlotInput,
    AcquireSlotOutcome, JobKeyActiveSlot, JobKeyCoordinator, JobKeyPolicy, LeaseMutationOutcome,
    NatsKeyCoordinator,
};
use crate::jobs::manager::{
    JobManager, JobMetaSource, JobProcessError, JobProcessOutcome, TerminalPublishDecision,
};
use crate::jobs::projection::job_from_work_event;
use crate::jobs::publisher::JobEventPublisher;
use crate::jobs::registry::{
    start_worker_heartbeat_loop, ActiveJobCancellationRegistry, ServiceRegistryError,
    WorkerHeartbeatHandle, WorkerHeartbeatOptions,
};
use crate::jobs::subjects::job_event_subject;
use crate::jobs::types::{Job, JobConcurrency, JobEvent, JobEventType};

const JOBS_STREAM: &str = "JOBS";

const CANCELLATION_NONE: u8 = 0;
const CANCELLATION_HOST_SHUTDOWN: u8 = 1;
const CANCELLATION_JOB: u8 = 2;
const CANCELLATION_LEASE_LOST: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerAckAction {
    Ack,
    Nak(Duration),
    AwaitMaxDeliver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectedWorkDecision {
    Process,
    SkipAck,
}

/// Cooperative cancellation token passed into worker handlers.
#[derive(Debug, Clone, Default)]
pub struct JobCancellationToken {
    cancelled: Arc<AtomicU8>,
    notify: Arc<tokio::sync::Notify>,
}

impl JobCancellationToken {
    /// Create a new uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the token as cancelled.
    pub fn cancel(&self) {
        let _ = self.cancelled.compare_exchange(
            CANCELLATION_NONE,
            CANCELLATION_JOB,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.notify.notify_waiters();
    }

    /// Mark the token as cancelled because the worker host is shutting down.
    pub fn cancel_for_shutdown(&self) {
        self.cancelled
            .store(CANCELLATION_HOST_SHUTDOWN, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn cancel_for_lease_loss(&self) {
        let _ = self.cancelled.compare_exchange(
            CANCELLATION_NONE,
            CANCELLATION_LEASE_LOST,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.notify.notify_waiters();
    }

    /// Return whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) != CANCELLATION_NONE
    }

    /// Return whether cancellation came from a business-level job cancel event.
    pub fn is_job_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) == CANCELLATION_JOB
    }

    /// Return whether cancellation came from worker-host shutdown.
    pub fn is_host_shutdown(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) == CANCELLATION_HOST_SHUTDOWN
    }

    pub(crate) fn is_lease_lost(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) == CANCELLATION_LEASE_LOST
    }

    pub(crate) fn is_same_token(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
            if self.is_cancelled() {
                return;
            }
        }
    }
}

/// Errors returned while consuming or acknowledging job work items.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeWorkerError {
    #[error("failed to open work stream '{stream}': {details}")]
    OpenStream { stream: String, details: String },
    #[error("worker queue binding '{queue_type}' is missing")]
    MissingQueueBinding { queue_type: String },
    #[error("failed to open worker consumer '{consumer}' for subject '{subject}': {details}")]
    Consumer {
        consumer: String,
        subject: String,
        details: String,
    },
    #[error("failed to read worker messages for consumer '{consumer}': {details}")]
    Messages { consumer: String, details: String },
    #[error("failed to process job payload: {0}")]
    Process(String),
    #[error("failed to acknowledge worker message: {0}")]
    Ack(String),
    #[error("failed to subscribe to cancellation subject '{subject}': {details}")]
    CancellationSubscription { subject: String, details: String },
    #[error("failed to open jobs lifecycle stream '{stream}': {details}")]
    LifecycleStream { stream: String, details: String },
    #[error("failed to read latest jobs lifecycle event for subject '{subject}' from stream '{stream}': {details}")]
    LifecycleRead {
        stream: String,
        subject: String,
        details: String,
    },
    #[error("failed to decode latest jobs lifecycle event for subject '{subject}' from stream '{stream}': {details}")]
    LifecycleDecode {
        stream: String,
        subject: String,
        details: String,
    },
    #[error("failed to coordinate keyed job concurrency: {0}")]
    KeyCoordinator(String),
}

#[derive(Debug, Clone)]
struct ActiveKeyLease {
    policy: JobKeyPolicy,
    slot: JobKeyActiveSlot,
    heartbeat_interval: Duration,
    heartbeat_ttl_ms: u64,
    stale_slots: Vec<JobKeyActiveSlot>,
    stale_takeover_count: u64,
}

/// Options controlling first-class worker-host startup from a resolved binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHostOptions {
    /// Optional subset of queue types to run. When omitted, all bound queues run.
    pub queue_types: Option<Vec<String>>,
    /// Local worker count by queue type. Selected queues not present here use one worker.
    pub queue_concurrency: BTreeMap<String, u32>,
    /// How often worker presence heartbeats should be published.
    pub heartbeat_interval: Duration,
    /// Optional service version to include in worker heartbeats.
    pub version: Option<String>,
}

impl Default for WorkerHostOptions {
    fn default() -> Self {
        Self {
            queue_types: None,
            queue_concurrency: BTreeMap::new(),
            heartbeat_interval: Duration::from_secs(30),
            version: None,
        }
    }
}

/// Errors returned while composing or stopping a worker host.
#[derive(Debug, thiserror::Error)]
pub enum WorkerHostError {
    #[error("requested worker queue binding '{queue_type}' is missing")]
    MissingQueueBinding { queue_type: String },
    #[error("worker queue '{queue_type}' has invalid concurrency {concurrency}; expected >= 1")]
    InvalidConcurrency {
        queue_type: String,
        concurrency: u32,
    },
    #[error("failed to start worker heartbeat lifecycle: {0}")]
    Heartbeat(#[from] ServiceRegistryError),
    #[error("failed to prepare worker queue '{queue_type}': {details}")]
    WorkerStartup { queue_type: String, details: String },
    #[error("worker task for queue '{queue_type}' slot {worker_index} failed: {details}")]
    WorkerTask {
        queue_type: String,
        worker_index: u32,
        details: String,
    },
}

struct WorkerTaskHandle {
    queue_type: String,
    worker_index: u32,
    task: tokio::task::JoinHandle<Result<(), RuntimeWorkerError>>,
}

type WorkerJoinResult = (
    String,
    u32,
    Result<Result<(), RuntimeWorkerError>, tokio::task::JoinError>,
);
type WorkerJoinFuture = BoxFuture<'static, WorkerJoinResult>;

/// Handle for a binding-driven worker host.
pub struct WorkerHostHandle {
    cancellation: JobCancellationToken,
    heartbeats: Vec<WorkerHeartbeatHandle>,
    workers: Vec<WorkerTaskHandle>,
}

impl WorkerHostHandle {
    /// Return the number of queue-worker tasks owned by this host.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Stop all worker tasks and then stop host heartbeats.
    pub async fn stop(self) -> Result<(), WorkerHostError> {
        self.cancellation.cancel_for_shutdown();

        let mut first_error = None;
        for worker in self.workers {
            match worker.task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    first_error.get_or_insert(WorkerHostError::WorkerTask {
                        queue_type: worker.queue_type,
                        worker_index: worker.worker_index,
                        details: error.to_string(),
                    });
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    first_error.get_or_insert(WorkerHostError::WorkerTask {
                        queue_type: worker.queue_type,
                        worker_index: worker.worker_index,
                        details: error.to_string(),
                    });
                }
            }
        }

        for heartbeat in self.heartbeats {
            heartbeat.stop().await.map_err(WorkerHostError::Heartbeat)?;
        }

        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    /// Supervise worker tasks until one exits, then stop the complete host.
    pub async fn join(self) -> Result<(), WorkerHostError> {
        let cancellation = self.cancellation;
        let _cancel_on_drop = CancelWorkersOnDrop(cancellation.clone());
        let mut workers: FuturesUnordered<WorkerJoinFuture> = self
            .workers
            .into_iter()
            .map(|worker| {
                Box::pin(async move { (worker.queue_type, worker.worker_index, worker.task.await) })
                    as BoxFuture<'static, _>
            })
            .collect::<FuturesUnordered<_>>();

        let first_error = match workers.next().await {
            Some((queue_type, worker_index, Ok(Ok(())))) => WorkerHostError::WorkerTask {
                queue_type,
                worker_index,
                details: "worker exited unexpectedly".to_string(),
            },
            Some((queue_type, worker_index, Ok(Err(error)))) => WorkerHostError::WorkerTask {
                queue_type,
                worker_index,
                details: error.to_string(),
            },
            Some((queue_type, worker_index, Err(error))) => WorkerHostError::WorkerTask {
                queue_type,
                worker_index,
                details: error.to_string(),
            },
            None => WorkerHostError::WorkerStartup {
                queue_type: "*".to_string(),
                details: "worker host has no tasks".to_string(),
            },
        };
        cancellation.cancel_for_shutdown();
        while workers.next().await.is_some() {}
        for heartbeat in self.heartbeats {
            heartbeat.stop().await.map_err(WorkerHostError::Heartbeat)?;
        }
        Err(first_error)
    }
}

struct CancelWorkersOnDrop(JobCancellationToken);

impl Drop for CancelWorkersOnDrop {
    fn drop(&mut self) {
        self.0.cancel_for_shutdown();
    }
}

/// Decode a work payload and run it through a manager with explicit runtime hooks.
pub async fn process_work_payload<P, M, HB, G, C, H, Fut, E>(
    manager: &JobManager<P, M>,
    payload: &[u8],
    cancellation: JobCancellationToken,
    heartbeat: HB,
    terminal_guard: G,
    terminal_cleanup: C,
    handler: H,
) -> Result<Option<JobProcessOutcome<Value>>, RuntimeWorkerError>
where
    P: JobEventPublisher,
    P::Error: std::fmt::Display,
    M: JobMetaSource,
    HB: Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static,
    G: Fn(String) -> BoxFuture<'static, Result<TerminalPublishDecision, String>> + Send + Sync,
    C: Fn(String) -> BoxFuture<'static, Result<(), String>> + Send + Sync,
    H: FnOnce(ActiveJob<P, M>) -> Fut,
    Fut: Future<Output = Result<Value, JobProcessError<E>>>,
    E: ToString,
{
    let event = match serde_json::from_slice::<JobEvent>(payload) {
        Ok(event) => event,
        Err(_) => return Ok(None),
    };
    let Some(job) = job_from_work_event(&event) else {
        return Ok(None);
    };

    process_job_with_context_heartbeat_and_terminal_hooks(
        manager,
        job,
        cancellation,
        heartbeat,
        terminal_guard,
        terminal_cleanup,
        handler,
    )
    .await
    .map(Some)
}

async fn process_job_with_context_heartbeat_and_terminal_hooks<P, M, HB, G, C, H, Fut, E>(
    manager: &JobManager<P, M>,
    job: Job,
    cancellation: JobCancellationToken,
    heartbeat: HB,
    terminal_guard: G,
    terminal_cleanup: C,
    handler: H,
) -> Result<JobProcessOutcome<Value>, RuntimeWorkerError>
where
    P: JobEventPublisher,
    P::Error: std::fmt::Display,
    M: JobMetaSource,
    HB: Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync + 'static,
    G: Fn(String) -> BoxFuture<'static, Result<TerminalPublishDecision, String>> + Send + Sync,
    C: Fn(String) -> BoxFuture<'static, Result<(), String>> + Send + Sync,
    H: FnOnce(ActiveJob<P, M>) -> Fut,
    Fut: Future<Output = Result<Value, JobProcessError<E>>>,
    E: ToString,
{
    let route = crate::telemetry::instruments::route_token(
        crate::telemetry::instruments::RouteFamily::Job,
        &job.job_type,
    );
    let observation = crate::telemetry::lifecycle::Observation::start(
        crate::telemetry::instruments::DurationFamily::JobAttempt,
        vec![crate::telemetry::KeyValue::new("trellis.route", route)],
        "interrupted",
    );
    // Each attempt roots locally and links back to the job's creation carrier
    // instead of pretending the original submission is still open. The span is
    // carried with the future; no thread-local guard is held across an await.
    let attempt_span = tracing::info_span!(
        "trellis.job.attempt",
        "trellis.route" = route,
        "trellis.outcome" = tracing::field::Empty,
    );
    crate::telemetry::propagation::link_span_to_carrier(
        &attempt_span,
        &job.context.traceparent,
        job.context.tracestate.as_deref(),
    );
    let result = manager
        .process_with_heartbeat_and_terminal_hooks(
            job,
            cancellation,
            heartbeat,
            terminal_guard,
            terminal_cleanup,
            handler,
        )
        .instrument(attempt_span.clone())
        .await
        .map_err(|error| RuntimeWorkerError::Process(error.to_string()));
    let outcome = match &result {
        Ok(JobProcessOutcome::Completed { .. }) => "completed",
        Ok(JobProcessOutcome::Retry { .. }) => "retry",
        Ok(JobProcessOutcome::Failed { .. }) => "failed",
        Ok(JobProcessOutcome::Cancelled { .. }) => "cancelled",
        Ok(JobProcessOutcome::Interrupted { .. }) => "interrupted",
        // A stale completion is an observed lease loss, not a completion.
        Ok(JobProcessOutcome::StaleCompletionIgnored { .. }) => "lease_lost",
        Err(_) => "error",
    };
    attempt_span.record("trellis.outcome", outcome);
    observation.finish(outcome);
    result
}

fn parse_work_payload_job(payload: &[u8]) -> Option<Job> {
    let event = serde_json::from_slice::<JobEvent>(payload).ok()?;
    job_from_work_event(&event)
}

struct WorkerLoopResources<P, M>
where
    P: JobEventPublisher,
    M: JobMetaSource,
{
    consumer: consumer::PullConsumer,
    lifecycle_stream: stream::Stream<()>,
    queue: JobsQueueBinding,
    manager: JobManager<P, M>,
    cancellation: JobCancellationToken,
    cancellation_registry: ActiveJobCancellationRegistry,
    key_coordinator: Option<NatsKeyCoordinator>,
}

async fn run_prepared_queue_worker_loop<P, M, H, Fut, E>(
    resources: WorkerLoopResources<P, M>,
    handler: H,
) -> Result<(), RuntimeWorkerError>
where
    P: JobEventPublisher + Send + Sync + 'static,
    P::Error: std::fmt::Display,
    M: JobMetaSource + Send + Sync + 'static,
    H: Fn(ActiveJob<P, M>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, JobProcessError<E>>> + Send,
    E: ToString + Send,
{
    let WorkerLoopResources {
        consumer,
        lifecycle_stream,
        queue,
        manager,
        cancellation,
        cancellation_registry,
        key_coordinator,
    } = resources;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| RuntimeWorkerError::Messages {
            consumer: queue.consumer_name.clone(),
            details: error.to_string(),
        })?;

    'work: loop {
        let next_message = tokio::select! {
            _ = cancellation.cancelled() => break,
            next_message = messages.next() => next_message,
        };
        let Some(message) = next_message else {
            break;
        };
        let message = message.map_err(|error| RuntimeWorkerError::Messages {
            consumer: queue.consumer_name.clone(),
            details: error.to_string(),
        })?;
        let delivery_attempt = match message.info() {
            Ok(info) => u64::try_from(info.delivered)
                .map_err(|error| RuntimeWorkerError::Messages {
                    consumer: queue.consumer_name.clone(),
                    details: format!("invalid delivery attempt metadata: {error}"),
                })?
                .max(1),
            Err(error) => {
                tracing::warn!(%error, subject = %message.subject, "retrying job with unavailable delivery metadata");
                message
                    .ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                    .await
                    .map_err(map_ack_error)?;
                continue;
            }
        };
        let payload = message.payload.clone();
        let Some(mut parsed_job) = parse_work_payload_job(&payload) else {
            message.ack().await.map_err(map_ack_error)?;
            continue;
        };
        parsed_job.tries = delivery_attempt.saturating_sub(1);
        let job_key = job_key(&parsed_job.service, &parsed_job.job_type, &parsed_job.id);
        if stream_work_decision(&lifecycle_stream, &queue.publish_prefix, &parsed_job).await?
            == ProjectedWorkDecision::SkipAck
        {
            cleanup_queued_key_for_terminal(
                key_coordinator.as_ref(),
                &queue,
                manager.bindings().namespace.as_str(),
                &parsed_job,
                &manager.now_iso(),
            )
            .await?;
            cancellation_registry.clear_pending(&job_key);
            message.ack().await.map_err(map_ack_error)?;
            continue;
        }
        let job_cancellation = JobCancellationToken::new();
        if cancellation.is_host_shutdown() {
            job_cancellation.cancel_for_shutdown();
        } else if cancellation.is_job_cancelled() {
            job_cancellation.cancel();
        }
        let _cancellation_guard =
            cancellation_registry.register(job_key.clone(), job_cancellation.clone());
        let handler = handler.clone();
        let heartbeat_message = message.clone();
        let active_key = loop {
            let active_key = acquire_key_slot_for_work(
                key_coordinator.as_ref(),
                &queue,
                manager.bindings().namespace.as_str(),
                &parsed_job,
                &manager.now_iso(),
                delivery_attempt,
            )
            .await?;
            if queue.key_concurrency.is_none() || active_key.is_some() {
                break active_key;
            }
            message
                .ack_with(AckKind::Progress)
                .await
                .map_err(map_ack_error)?;
            tokio::select! {
                _ = cancellation.cancelled() => {
                    cancellation_registry.clear_pending(&job_key);
                    message.ack_with(AckKind::Nak(Some(Duration::from_secs(5))))
                        .await.map_err(map_ack_error)?;
                    continue 'work;
                }
                _ = tokio::time::sleep(progress_ack_interval(&queue)) => {}
            }
        };
        if let Some(active_key) = active_key.as_ref() {
            for stale_slot in &active_key.stale_slots {
                manager
                    .emit_stale_slot(&queue.queue_type, stale_slot, "key lease expired")
                    .await
                    .map_err(|error| RuntimeWorkerError::Process(error.to_string()))?;
            }
        }
        let forward_cancellation = {
            let outer_cancellation = cancellation.clone();
            let job_cancellation = job_cancellation.clone();
            tokio::spawn(async move {
                outer_cancellation.cancelled().await;
                if outer_cancellation.is_host_shutdown() {
                    job_cancellation.cancel_for_shutdown();
                } else if outer_cancellation.is_job_cancelled() {
                    job_cancellation.cancel();
                }
            })
        };
        let heartbeat_hook: Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync> = {
            Arc::new(move || {
                let heartbeat_message = heartbeat_message.clone();
                Box::pin(async move {
                    heartbeat_message
                        .ack_with(AckKind::Progress)
                        .await
                        .map_err(|error| error.to_string())
                }) as BoxFuture<'static, Result<(), String>>
            })
        };
        let auto_heartbeat = {
            let heartbeat_interval = progress_ack_interval(&queue);
            let heartbeat_hook = Arc::clone(&heartbeat_hook);
            let job_cancellation = job_cancellation.clone();
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(heartbeat_interval);
                interval.tick().await;
                loop {
                    interval.tick().await;
                    if heartbeat_hook().await.is_err() {
                        job_cancellation.cancel_for_lease_loss();
                        break;
                    }
                }
            }))
        };
        let auto_key_heartbeat = active_key.as_ref().and_then(|active_key| {
            let coordinator = key_coordinator.clone()?;
            let heartbeat_interval = active_key.heartbeat_interval;
            let active_key = active_key.clone();
            let job_cancellation = job_cancellation.clone();
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(heartbeat_interval);
                interval.tick().await;
                loop {
                    interval.tick().await;
                    match renew_key_lease(&coordinator, &active_key).await {
                        Ok(LeaseMutationOutcome::Renewed { .. }) => {}
                        Ok(LeaseMutationOutcome::Lost { .. })
                        | Ok(LeaseMutationOutcome::Released { .. })
                        | Err(_) => {
                            job_cancellation.cancel_for_lease_loss();
                            break;
                        }
                    }
                }
            }))
        });
        let terminal_guard = {
            let active_key = active_key.clone();
            let key_coordinator = key_coordinator.clone();
            move |terminal_at: String| {
                let active_key = active_key.clone();
                let key_coordinator = key_coordinator.clone();
                Box::pin(async move {
                    let (Some(coordinator), Some(active_key)) = (key_coordinator, active_key)
                    else {
                        return Ok(TerminalPublishDecision::Publish);
                    };
                    match renew_key_lease_at(&coordinator, &active_key, &terminal_at)
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        LeaseMutationOutcome::Renewed { .. } => {
                            Ok(TerminalPublishDecision::Publish)
                        }
                        LeaseMutationOutcome::Lost { .. } => {
                            Ok(TerminalPublishDecision::StaleCompletionIgnored)
                        }
                        LeaseMutationOutcome::Released { .. } => {
                            Ok(TerminalPublishDecision::Publish)
                        }
                    }
                }) as BoxFuture<'static, Result<TerminalPublishDecision, String>>
            }
        };
        let terminal_cleanup = {
            let active_key = active_key.clone();
            let key_coordinator = key_coordinator.clone();
            move |released_at: String| {
                let active_key = active_key.clone();
                let key_coordinator = key_coordinator.clone();
                Box::pin(async move {
                    let (Some(coordinator), Some(active_key)) = (key_coordinator, active_key)
                    else {
                        return Ok(());
                    };
                    release_key_lease(&coordinator, &active_key, &released_at)
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                }) as BoxFuture<'static, Result<(), String>>
            }
        };
        let process_result = process_job_with_context_heartbeat_and_terminal_hooks(
            &manager,
            job_with_active_key_metadata(parsed_job.clone(), active_key.as_ref()),
            job_cancellation,
            {
                let heartbeat_hook = Arc::clone(&heartbeat_hook);
                move || heartbeat_hook()
            },
            terminal_guard,
            terminal_cleanup,
            handler.clone(),
        )
        .await;
        if let Some(auto_heartbeat) = auto_heartbeat {
            auto_heartbeat.abort();
            let _ = auto_heartbeat.await;
        }
        if let Some(auto_key_heartbeat) = auto_key_heartbeat {
            auto_key_heartbeat.abort();
            let _ = auto_key_heartbeat.await;
        }
        forward_cancellation.abort();
        let _ = forward_cancellation.await;
        let process_result = process_result?;
        match ack_action_for_outcome(
            Some(&process_result),
            parsed_job.max_tries,
            &queue.backoff_ms,
        ) {
            WorkerAckAction::Ack => message.ack().await.map_err(map_ack_error)?,
            WorkerAckAction::Nak(delay) => message
                .ack_with(AckKind::Nak(Some(delay)))
                .await
                .map_err(map_ack_error)?,
            WorkerAckAction::AwaitMaxDeliver => {}
        }
    }

    Ok(())
}

async fn run_prepared_queue_worker_with_cancellation<P, M, H, Fut, E>(
    nats: async_nats::Client,
    mut resources: WorkerLoopResources<P, M>,
    handler: H,
) -> Result<(), RuntimeWorkerError>
where
    P: JobEventPublisher + Send + Sync + 'static,
    P::Error: std::fmt::Display,
    M: JobMetaSource + Send + Sync + 'static,
    H: Fn(ActiveJob<P, M>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, JobProcessError<E>>> + Send,
    E: ToString + Send,
{
    let cancellation_subject = format!("{}.*.cancelled", resources.queue.publish_prefix);
    let mut cancellation_subscriber =
        nats.subscribe(cancellation_subject.clone())
            .await
            .map_err(|error| RuntimeWorkerError::CancellationSubscription {
                subject: cancellation_subject.clone(),
                details: error.to_string(),
            })?;
    let cancellation_task = {
        let cancellation_registry = resources.cancellation_registry.clone();
        tokio::spawn(async move {
            while let Some(message) = cancellation_subscriber.next().await {
                let Ok(event) = serde_json::from_slice::<JobEvent>(&message.payload) else {
                    continue;
                };
                if event.event_type != JobEventType::Cancelled {
                    continue;
                }
                let key = job_key(&event.service, &event.job_type, &event.job_id);
                cancellation_registry.cancel(&key);
            }
        })
    };

    resources.key_coordinator = key_coordinator_for_queue(
        nats.clone(),
        resources.manager.bindings().namespace.as_str(),
        &resources.queue,
    )
    .await?;
    let result = run_prepared_queue_worker_loop(resources, handler).await;
    cancellation_task.abort();
    let _ = cancellation_task.await;
    result
}

async fn key_coordinator_for_queue(
    nats: async_nats::Client,
    namespace: &str,
    queue: &JobsQueueBinding,
) -> Result<Option<NatsKeyCoordinator>, RuntimeWorkerError> {
    if queue.key_concurrency.is_none() {
        return Ok(None);
    }
    NatsKeyCoordinator::open_for_service(nats, namespace)
        .await
        .map(Some)
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))
}

async fn acquire_key_slot_for_work(
    coordinator: Option<&NatsKeyCoordinator>,
    queue: &JobsQueueBinding,
    namespace: &str,
    job: &Job,
    started_at: &str,
    tries: u64,
) -> Result<Option<ActiveKeyLease>, RuntimeWorkerError> {
    let Some(coordinator) = coordinator else {
        return Ok(None);
    };
    let Some(key_concurrency) = queue.key_concurrency.as_ref() else {
        return Ok(None);
    };
    let policy = key_policy_for_job(queue, key_concurrency, namespace, job)?;
    let lease_expires_at = add_millis(started_at, key_concurrency.heartbeat_ttl_ms)
        .map_err(RuntimeWorkerError::KeyCoordinator)?;
    let input = AcquireSlotInput {
        job_id: job.id.clone(),
        slot_token: Ulid::new().to_string(),
        instance_id: "rust-worker".to_string(),
        started_at: started_at.to_string(),
        lease_expires_at,
        tries,
        context: job.context.clone(),
    };
    let outcome = coordinator
        .acquire(policy.clone(), input)
        .await
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))?;
    record_lease_event(
        "acquire",
        match &outcome {
            AcquireSlotOutcome::Acquired { .. } => "ok",
            AcquireSlotOutcome::Blocked { .. } => "conflict",
        },
    );
    match outcome {
        AcquireSlotOutcome::Acquired {
            state,
            slot,
            stale_slots,
        } => Ok(Some(ActiveKeyLease {
            policy,
            slot: *slot,
            heartbeat_interval: Duration::from_millis(key_concurrency.heartbeat_interval_ms),
            heartbeat_ttl_ms: key_concurrency.heartbeat_ttl_ms,
            stale_slots,
            stale_takeover_count: state.stale_takeover_count,
        })),
        AcquireSlotOutcome::Blocked { .. } => Ok(None),
    }
}

async fn renew_key_lease(
    coordinator: &NatsKeyCoordinator,
    active_key: &ActiveKeyLease,
) -> Result<LeaseMutationOutcome, RuntimeWorkerError> {
    let heartbeat_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))?;
    let outcome = renew_key_lease_at(coordinator, active_key, &heartbeat_at).await?;
    record_lease_event(
        "renew",
        match &outcome {
            LeaseMutationOutcome::Renewed { .. } => "ok",
            LeaseMutationOutcome::Lost { .. } => "lost",
            LeaseMutationOutcome::Released { .. } => "ok",
        },
    );
    Ok(outcome)
}

async fn renew_key_lease_at(
    coordinator: &NatsKeyCoordinator,
    active_key: &ActiveKeyLease,
    heartbeat_at: &str,
) -> Result<LeaseMutationOutcome, RuntimeWorkerError> {
    let lease_expires_at = add_millis(heartbeat_at, active_key.heartbeat_ttl_ms)
        .map_err(RuntimeWorkerError::KeyCoordinator)?;
    coordinator
        .update_key(&active_key.policy, {
            let active_key = active_key.clone();
            let heartbeat_at = heartbeat_at.to_string();
            move |current| match current {
                Some(state) => renew_active_slot(
                    state,
                    &active_key.slot.job_id,
                    &active_key.slot.slot_token,
                    &heartbeat_at,
                    &lease_expires_at,
                ),
                None => LeaseMutationOutcome::Lost {
                    state: new_key_state(&active_key.policy, &heartbeat_at),
                },
            }
        })
        .await
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))
}

async fn cleanup_queued_key_for_terminal(
    coordinator: Option<&NatsKeyCoordinator>,
    queue: &JobsQueueBinding,
    namespace: &str,
    job: &Job,
    removed_at: &str,
) -> Result<(), RuntimeWorkerError> {
    let Some(coordinator) = coordinator else {
        return Ok(());
    };
    let Some(key_concurrency) = queue.key_concurrency.as_ref() else {
        return Ok(());
    };
    let policy = key_policy_for_job(queue, key_concurrency, namespace, job)?;
    coordinator
        .remove_queued(policy, job.id.clone(), removed_at.to_string())
        .await
        .map(|_| ())
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))
}

fn job_with_active_key_metadata(mut job: Job, active_key: Option<&ActiveKeyLease>) -> Job {
    let Some(active_key) = active_key else {
        return job;
    };
    job.concurrency = Some(JobConcurrency {
        key: active_key.policy.key.clone(),
        key_hash: active_key.policy.key_hash(),
        instance_id: Some(active_key.slot.instance_id.clone()),
        slot_token: Some(active_key.slot.slot_token.clone()),
        heartbeat_at: Some(active_key.slot.heartbeat_at.clone()),
        lease_expires_at: Some(active_key.slot.lease_expires_at.clone()),
        stale_takeover_count: Some(active_key.stale_takeover_count),
    });
    job
}

async fn release_key_lease(
    coordinator: &NatsKeyCoordinator,
    active_key: &ActiveKeyLease,
    released_at: &str,
) -> Result<LeaseMutationOutcome, RuntimeWorkerError> {
    let outcome = coordinator
        .update_key(&active_key.policy, {
            let active_key = active_key.clone();
            let released_at = released_at.to_string();
            move |current| match current {
                Some(state) => release_active_slot(
                    state,
                    &active_key.slot.job_id,
                    &active_key.slot.slot_token,
                    &released_at,
                ),
                None => LeaseMutationOutcome::Lost {
                    state: new_key_state(&active_key.policy, &released_at),
                },
            }
        })
        .await
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))?;
    record_lease_event(
        "release",
        match &outcome {
            LeaseMutationOutcome::Released { .. } => "ok",
            LeaseMutationOutcome::Lost { .. } => "lost",
            LeaseMutationOutcome::Renewed { .. } => "ok",
        },
    );
    Ok(outcome)
}

/// Records one completed keyed job lease action with its bounded outcome.
fn record_lease_event(action: &'static str, outcome: &'static str) {
    crate::telemetry::instruments::add_counter(
        crate::telemetry::instruments::CounterFamily::JobLeaseEvents,
        1,
        &[
            crate::telemetry::KeyValue::new("trellis.action", action),
            crate::telemetry::KeyValue::new("trellis.outcome", outcome),
        ],
    );
}

fn key_policy_for_job(
    queue: &JobsQueueBinding,
    key_concurrency: &JobKeyConcurrencyBinding,
    namespace: &str,
    job: &Job,
) -> Result<JobKeyPolicy, RuntimeWorkerError> {
    let derived = derive_job_key(&job.payload, &key_concurrency.key)
        .map_err(|error| RuntimeWorkerError::KeyCoordinator(error.to_string()))?;
    let queue_depth = queue.queue.as_ref();
    Ok(JobKeyPolicy {
        service: namespace.to_string(),
        job_type: job.job_type.clone(),
        key: derived.key,
        key_hash: derived.key_hash,
        max_active: key_concurrency.max_active,
        max_queued_per_key: queue_depth.map_or(0, |queue| queue.max_queued_per_key),
        when_full: queue_depth.map_or(JobQueueWhenFull::Reject, |queue| queue.when_full.clone()),
        stale_policy: key_concurrency.stale_policy.clone(),
    })
}

fn add_millis(timestamp: &str, millis: u64) -> Result<String, String> {
    let parsed = OffsetDateTime::parse(timestamp, &Rfc3339).map_err(|error| error.to_string())?;
    let offset = TimeDuration::milliseconds(i64::try_from(millis).unwrap_or(i64::MAX));
    (parsed + offset)
        .format(&Rfc3339)
        .map_err(|error| error.to_string())
}

/// Start a first-class worker host from a resolved runtime binding.
pub async fn start_worker_host_from_binding<PF, P, MF, M, H, Fut, E>(
    nats: async_nats::Client,
    binding: JobsRuntimeBinding,
    instance_id: String,
    publisher_factory: PF,
    meta_factory: MF,
    handler: H,
    options: WorkerHostOptions,
) -> Result<WorkerHostHandle, WorkerHostError>
where
    PF: Fn() -> P + Clone + Send + Sync + 'static,
    P: JobEventPublisher + Send + Sync + 'static,
    P::Error: std::fmt::Display,
    MF: Fn(&str, u32) -> M + Clone + Send + Sync + 'static,
    M: JobMetaSource + Send + Sync + 'static,
    H: Fn(ActiveJob<P, M>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, JobProcessError<E>>> + Send + 'static,
    E: ToString + Send + 'static,
{
    let queue_types = selected_queue_types(&binding, options.queue_types.as_deref())?;
    let queue_concurrency = queue_types
        .iter()
        .map(|queue_type| {
            let concurrency = options
                .queue_concurrency
                .get(queue_type)
                .copied()
                .unwrap_or(1);
            if concurrency == 0 {
                Err(WorkerHostError::InvalidConcurrency {
                    queue_type: queue_type.clone(),
                    concurrency,
                })
            } else {
                Ok((queue_type.clone(), concurrency))
            }
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    for queue_type in &queue_types {
        binding.jobs.queues.get(queue_type).ok_or_else(|| {
            WorkerHostError::MissingQueueBinding {
                queue_type: queue_type.clone(),
            }
        })?;
    }

    let jetstream = jetstream::new(nats.clone());
    let mut prepared_workers = Vec::new();
    for queue_type in &queue_types {
        let queue = binding.jobs.queues.get(queue_type).ok_or_else(|| {
            WorkerHostError::MissingQueueBinding {
                queue_type: queue_type.clone(),
            }
        })?;
        for worker_index in 0..queue_concurrency[queue_type] {
            let lifecycle_stream = lifecycle_stream(&jetstream).await.map_err(|error| {
                WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                }
            })?;
            let consumer = ensure_worker_consumer(&jetstream, &binding.work_stream, queue)
                .await
                .map_err(|error| WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                })?;
            prepared_workers.push((
                queue_type.clone(),
                worker_index,
                queue.clone(),
                lifecycle_stream,
                consumer,
            ));
        }
    }

    let cancellation = JobCancellationToken::new();
    let mut heartbeats = Vec::new();
    for queue_type in &queue_types {
        heartbeats.push(
            start_worker_heartbeat_loop(
                nats.clone(),
                WorkerHeartbeatOptions {
                    service: binding.jobs.service_name.clone(),
                    subject_service: binding.jobs.namespace.clone(),
                    job_type: queue_type.clone(),
                    instance_id: instance_id.clone(),
                    concurrency: Some(queue_concurrency[queue_type]),
                    version: options.version.clone(),
                    interval: options.heartbeat_interval,
                },
            )
            .await?,
        );
    }
    let mut workers = Vec::new();
    let cancellation_registry = ActiveJobCancellationRegistry::new();
    for (queue_type, worker_index, queue, lifecycle_stream, consumer) in prepared_workers {
        let worker_nats = nats.clone();
        let worker_publisher = publisher_factory.clone()();
        let worker_meta = meta_factory.clone()(&queue_type, worker_index);
        let worker_handler = handler.clone();
        let worker_cancellation = cancellation.clone();
        let worker_cancellation_registry = cancellation_registry.clone();
        let worker_jobs = binding.jobs.clone();
        let task = tokio::spawn(async move {
            let manager = JobManager::new(worker_publisher, worker_jobs, worker_meta);
            run_prepared_queue_worker_with_cancellation(
                worker_nats,
                WorkerLoopResources {
                    consumer,
                    lifecycle_stream,
                    queue,
                    manager,
                    cancellation: worker_cancellation,
                    cancellation_registry: worker_cancellation_registry,
                    key_coordinator: None,
                },
                worker_handler,
            )
            .await
        });
        workers.push(WorkerTaskHandle {
            queue_type,
            worker_index,
            task,
        });
    }

    Ok(WorkerHostHandle {
        cancellation,
        heartbeats,
        workers,
    })
}

fn selected_queue_types(
    binding: &JobsRuntimeBinding,
    requested: Option<&[String]>,
) -> Result<Vec<String>, WorkerHostError> {
    match requested {
        Some(queue_types) => queue_types
            .iter()
            .map(|queue_type| {
                if binding.jobs.queues.contains_key(queue_type) {
                    Ok(queue_type.clone())
                } else {
                    Err(WorkerHostError::MissingQueueBinding {
                        queue_type: queue_type.clone(),
                    })
                }
            })
            .collect(),
        None => {
            let mut queue_types = binding.jobs.queues.keys().cloned().collect::<Vec<_>>();
            queue_types.sort();
            Ok(queue_types)
        }
    }
}

fn map_ack_error(error: async_nats::Error) -> RuntimeWorkerError {
    RuntimeWorkerError::Ack(error.to_string())
}

async fn ensure_worker_consumer(
    jetstream: &jetstream::Context,
    work_stream: &str,
    queue: &JobsQueueBinding,
) -> Result<consumer::PullConsumer, RuntimeWorkerError> {
    let consumer = jetstream
        .get_consumer_from_stream(&queue.consumer_name, work_stream)
        .await
        .map_err(|error| RuntimeWorkerError::Consumer {
            consumer: queue.consumer_name.clone(),
            subject: queue.work_subject.clone(),
            details: error.to_string(),
        })?;
    if let Some(details) = worker_consumer_mismatch(consumer.cached_info(), queue) {
        return Err(RuntimeWorkerError::Consumer {
            consumer: queue.consumer_name.clone(),
            subject: queue.work_subject.clone(),
            details,
        });
    }
    Ok(consumer)
}

fn worker_consumer_mismatch(info: &consumer::Info, queue: &JobsQueueBinding) -> Option<String> {
    let config = &info.config;
    if config.durable_name.as_deref() != Some(queue.consumer_name.as_str()) {
        return Some(format!(
            "stale consumer durable {:?}, expected {:?}",
            config.durable_name, queue.consumer_name
        ));
    }
    if config.filter_subject != queue.work_subject {
        return Some(format!(
            "stale consumer filter subject {:?}, expected {:?}",
            config.filter_subject, queue.work_subject
        ));
    }
    if config.ack_policy != consumer::AckPolicy::Explicit {
        return Some(format!(
            "stale consumer ack policy {:?}, expected explicit",
            config.ack_policy
        ));
    }
    let expected_ack_wait = expected_consumer_ack_wait(queue);
    if config.ack_wait != expected_ack_wait {
        return Some(format!(
            "stale consumer ack wait {:?}, expected {:?}",
            config.ack_wait, expected_ack_wait
        ));
    }
    let expected_max_deliver = i64::try_from(queue.max_deliver).unwrap_or(i64::MAX);
    if config.max_deliver != expected_max_deliver {
        return Some(format!(
            "stale consumer max deliver {}, expected {}",
            config.max_deliver, expected_max_deliver
        ));
    }
    let expected_backoff = queue
        .backoff_ms
        .iter()
        .copied()
        .map(Duration::from_millis)
        .collect::<Vec<_>>();
    if config.backoff != expected_backoff {
        return Some(format!(
            "stale consumer backoff {:?}, expected {:?}",
            config.backoff, expected_backoff
        ));
    }
    None
}

fn expected_consumer_ack_wait(queue: &JobsQueueBinding) -> Duration {
    Duration::from_millis(
        queue
            .backoff_ms
            .iter()
            .copied()
            .min()
            .unwrap_or(queue.ack_wait_ms),
    )
}

fn progress_ack_interval(queue: &JobsQueueBinding) -> Duration {
    Duration::from_millis((expected_consumer_ack_wait(queue).as_millis() as u64 / 3).max(1))
}

async fn lifecycle_stream(
    jetstream: &jetstream::Context,
) -> Result<stream::Stream<()>, RuntimeWorkerError> {
    jetstream
        .get_stream_no_info(JOBS_STREAM)
        .await
        .map_err(|error| RuntimeWorkerError::LifecycleStream {
            stream: JOBS_STREAM.to_string(),
            details: error.to_string(),
        })
}

async fn stream_work_decision(
    lifecycle_stream: &stream::Stream<()>,
    publish_prefix: &str,
    work: &Job,
) -> Result<ProjectedWorkDecision, RuntimeWorkerError> {
    if exact_terminal_lifecycle_event_exists(lifecycle_stream, publish_prefix, work).await? {
        return Ok(ProjectedWorkDecision::SkipAck);
    }

    let subject = format!("{publish_prefix}.{}.*", work.id);
    let latest = match latest_lifecycle_message(lifecycle_stream, &subject).await {
        Ok(Some(message)) => message,
        Ok(None) => return Ok(ProjectedWorkDecision::Process),
        Err(error) => {
            return Err(RuntimeWorkerError::LifecycleRead {
                stream: JOBS_STREAM.to_string(),
                subject,
                details: error,
            });
        }
    };

    let latest = serde_json::from_slice::<JobEvent>(&latest.payload).map_err(|error| {
        RuntimeWorkerError::LifecycleDecode {
            stream: JOBS_STREAM.to_string(),
            subject: subject.clone(),
            details: error.to_string(),
        }
    })?;

    Ok(lifecycle_work_decision(Some(&latest), work))
}

async fn exact_terminal_lifecycle_event_exists(
    lifecycle_stream: &stream::Stream<()>,
    publish_prefix: &str,
    work: &Job,
) -> Result<bool, RuntimeWorkerError> {
    for event_type in [
        JobEventType::Completed,
        JobEventType::Failed,
        JobEventType::Cancelled,
        JobEventType::Expired,
        JobEventType::Skipped,
        JobEventType::Stale,
        JobEventType::Dead,
        JobEventType::Dismissed,
    ] {
        let bound_subject = format!("{publish_prefix}.{}.{}", work.id, event_type.as_token());
        let canonical_subject =
            job_event_subject(&work.service, &work.job_type, &work.id, event_type);
        for subject in [bound_subject, canonical_subject] {
            let Some(message) = latest_lifecycle_message(lifecycle_stream, &subject)
                .await
                .map_err(|error| RuntimeWorkerError::LifecycleRead {
                    stream: JOBS_STREAM.to_string(),
                    subject: subject.clone(),
                    details: error,
                })?
            else {
                continue;
            };
            let event = serde_json::from_slice::<JobEvent>(&message.payload).map_err(|error| {
                RuntimeWorkerError::LifecycleDecode {
                    stream: JOBS_STREAM.to_string(),
                    subject: subject.clone(),
                    details: error.to_string(),
                }
            })?;
            if event.service == work.service
                && event.job_type == work.job_type
                && event.job_id == work.id
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn lifecycle_work_decision(latest: Option<&JobEvent>, work: &Job) -> ProjectedWorkDecision {
    let Some(latest) = latest else {
        return ProjectedWorkDecision::Process;
    };

    if latest.service != work.service
        || latest.job_type != work.job_type
        || latest.job_id != work.id
    {
        return ProjectedWorkDecision::Process;
    }

    if is_terminal_lifecycle_event(latest.event_type) {
        return ProjectedWorkDecision::SkipAck;
    }
    ProjectedWorkDecision::Process
}

fn is_terminal_lifecycle_event(event_type: JobEventType) -> bool {
    matches!(
        event_type,
        JobEventType::Completed
            | JobEventType::Failed
            | JobEventType::Cancelled
            | JobEventType::Expired
            | JobEventType::Skipped
            | JobEventType::Stale
            | JobEventType::Dead
            | JobEventType::Dismissed
    )
}

fn ack_action_for_outcome<TResult>(
    outcome: Option<&JobProcessOutcome<TResult>>,
    max_tries: u64,
    backoff_ms: &[u64],
) -> WorkerAckAction {
    match outcome {
        Some(JobProcessOutcome::Retry { tries, .. }) if *tries >= max_tries => {
            WorkerAckAction::AwaitMaxDeliver
        }
        Some(JobProcessOutcome::Retry { tries, .. }) => {
            WorkerAckAction::Nak(Duration::from_millis(retry_delay_ms(*tries, backoff_ms)))
        }
        Some(JobProcessOutcome::Interrupted { .. }) => WorkerAckAction::Nak(Duration::from_secs(5)),
        Some(JobProcessOutcome::Completed { .. })
        | Some(JobProcessOutcome::Cancelled { .. })
        | Some(JobProcessOutcome::Failed { .. })
        | Some(JobProcessOutcome::StaleCompletionIgnored { .. })
        | None => WorkerAckAction::Ack,
    }
}

fn retry_delay_ms(delivery: u64, backoff_ms: &[u64]) -> u64 {
    const DEFAULT_BACKOFF_MS: [u64; 4] = [5_000, 30_000, 120_000, 600_000];
    let schedule = if backoff_ms.is_empty() {
        DEFAULT_BACKOFF_MS.as_slice()
    } else {
        backoff_ms
    };
    let index = usize::try_from(delivery.saturating_sub(1)).unwrap_or(usize::MAX);
    schedule
        .get(index)
        .copied()
        .or_else(|| schedule.last().copied())
        .unwrap_or(5_000)
}

async fn latest_lifecycle_message(
    lifecycle_stream: &stream::Stream<()>,
    subject: &str,
) -> Result<Option<async_nats::jetstream::message::StreamMessage>, String> {
    match lifecycle_stream.direct_get_last_for_subject(subject).await {
        Ok(message) => return Ok(Some(message)),
        Err(error) if matches!(error.kind(), stream::DirectGetErrorKind::NotFound) => {}
        Err(direct_error) => match lifecycle_stream
            .get_last_raw_message_by_subject(subject)
            .await
        {
            Ok(message) => return Ok(Some(message)),
            Err(error)
                if matches!(
                    error.kind(),
                    stream::LastRawMessageErrorKind::NoMessageFound
                ) => {}
            Err(error) => {
                return Err(format!(
                    "direct get failed: {direct_error}; raw get failed: {error}"
                ));
            }
        },
    }

    match lifecycle_stream
        .get_last_raw_message_by_subject(subject)
        .await
    {
        Ok(message) => Ok(Some(message)),
        Err(error)
            if matches!(
                error.kind(),
                stream::LastRawMessageErrorKind::NoMessageFound
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(format!("raw get failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use serde_json::Value;

    use super::JobCancellationToken;

    use super::{
        ack_action_for_outcome, lifecycle_work_decision, progress_ack_interval,
        ProjectedWorkDecision, WorkerAckAction, WorkerHostOptions,
    };
    use crate::jobs::bindings::JobsQueueBinding;
    use crate::jobs::events::{cancelled, completed, created, started, EventMeta};
    use crate::jobs::manager::JobProcessOutcome;
    use crate::jobs::types::{Job, JobContext, JobState};

    #[test]
    fn worker_host_options_keep_concurrency_local() {
        let options = WorkerHostOptions::default();
        assert!(options.queue_concurrency.is_empty());

        let options = WorkerHostOptions {
            queue_concurrency: BTreeMap::from([("documents".to_string(), 4)]),
            ..WorkerHostOptions::default()
        };
        assert_eq!(options.queue_concurrency["documents"], 4);
    }

    #[test]
    fn progress_ack_interval_uses_shortest_retry_window() {
        let queue = JobsQueueBinding {
            queue_type: "work".to_owned(),
            publish_prefix: "jobs.work".to_owned(),
            updates_prefix: None,
            work_subject: "jobs.work.run".to_owned(),
            consumer_name: "work".to_owned(),
            max_deliver: 3,
            backoff_ms: vec![30_000, 3_000, 10_000],
            ack_wait_ms: 60_000,
            default_deadline_ms: None,
            update: None,
            key_concurrency: None,
            queue: None,
        };
        assert_eq!(progress_ack_interval(&queue), Duration::from_millis(1_000));

        let mut queue = queue;
        queue.backoff_ms = vec![1];
        assert_eq!(progress_ack_interval(&queue), Duration::from_millis(1));
        queue.backoff_ms = vec![2];
        assert_eq!(progress_ack_interval(&queue), Duration::from_millis(1));
        queue.backoff_ms = vec![5];
        assert_eq!(progress_ack_interval(&queue), Duration::from_millis(1));
    }

    fn sample_context() -> JobContext {
        JobContext {
            request_id: "request-1".to_string(),
            trace_id: "0123456789abcdef0123456789abcdef".to_string(),
            traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
            tracestate: None,
        }
    }

    fn sample_job(state: JobState, tries: u64) -> Job {
        Job {
            id: "job-1".to_string(),
            context: sample_context(),
            service: "documents".to_string(),
            job_type: "document-process".to_string(),
            state,
            payload: serde_json::json!({ "documentId": "doc-1" }),
            result: None,
            created_at: "2026-03-28T11:59:00.000Z".to_string(),
            updated_at: "2026-03-28T11:59:00.000Z".to_string(),
            started_at: None,
            completed_at: None,
            tries,
            max_tries: 2,
            last_error: None,
            error_detail: None,
            deadline: None,
            progress: None,
            logs: None,
            concurrency: None,
            queue_policy: None,
            trigger: None,
            lineage: None,
            waiting_on: None,
        }
    }

    #[test]
    fn interrupted_outcomes_use_nak_instead_of_ack() {
        assert_eq!(
            ack_action_for_outcome(
                Some(&JobProcessOutcome::<Value>::Interrupted { tries: 1 }),
                2,
                &[5_000],
            ),
            WorkerAckAction::Nak(Duration::from_secs(5))
        );
    }

    #[test]
    fn final_retry_waits_for_max_deliver_advisory() {
        assert_eq!(
            ack_action_for_outcome(
                Some(&JobProcessOutcome::<Value>::Retry {
                    tries: 2,
                    error: "retry requested".to_string(),
                }),
                2,
                &[5_000],
            ),
            WorkerAckAction::AwaitMaxDeliver
        );
    }

    #[test]
    fn retry_uses_the_declared_delivery_interval() {
        assert_eq!(
            ack_action_for_outcome(
                Some(&JobProcessOutcome::<Value>::Retry {
                    tries: 1,
                    error: "retry requested".to_string(),
                }),
                3,
                &[17, 29],
            ),
            WorkerAckAction::Nak(Duration::from_millis(17))
        );
    }

    #[test]
    fn lifecycle_work_decision_allows_when_latest_event_is_created() {
        let work = sample_job(JobState::Pending, 0);
        let latest = created(
            EventMeta {
                service: &work.service,
                job_type: &work.job_type,
                job_id: &work.id,
                context: &work.context,
                timestamp: &work.created_at,
            },
            work.payload.clone(),
            work.max_tries,
            None,
        );

        assert_eq!(
            lifecycle_work_decision(Some(&latest), &work),
            ProjectedWorkDecision::Process
        );
    }

    #[test]
    fn lifecycle_work_decision_skips_when_latest_event_is_cancelled() {
        let work = sample_job(JobState::Pending, 0);
        let latest = cancelled(
            EventMeta {
                service: &work.service,
                job_type: &work.job_type,
                job_id: &work.id,
                context: &work.context,
                timestamp: &work.updated_at,
            },
            work.tries,
            JobState::Pending,
        );

        assert_eq!(
            lifecycle_work_decision(Some(&latest), &work),
            ProjectedWorkDecision::SkipAck
        );
    }

    #[test]
    fn lifecycle_work_decision_processes_when_latest_event_is_started_for_created_work() {
        let work = sample_job(JobState::Pending, 0);
        let latest = started(
            EventMeta {
                service: &work.service,
                job_type: &work.job_type,
                job_id: &work.id,
                context: &work.context,
                timestamp: &work.updated_at,
            },
            1,
            JobState::Pending,
        );

        assert_eq!(
            lifecycle_work_decision(Some(&latest), &work),
            ProjectedWorkDecision::Process
        );
    }

    #[test]
    fn lifecycle_work_decision_skips_when_latest_event_is_terminal() {
        let work = sample_job(JobState::Retry, 0);
        let latest = completed(
            EventMeta {
                service: &work.service,
                job_type: &work.job_type,
                job_id: &work.id,
                context: &work.context,
                timestamp: &work.updated_at,
            },
            1,
            serde_json::json!({ "ok": true }),
        );

        assert_eq!(
            lifecycle_work_decision(Some(&latest), &work),
            ProjectedWorkDecision::SkipAck
        );
    }

    #[tokio::test]
    async fn job_cancellation_token_cancelled_returns_after_prior_cancel() {
        let token = JobCancellationToken::new();
        token.cancel();

        let result = tokio::time::timeout(Duration::from_millis(50), token.cancelled()).await;

        assert!(
            result.is_ok(),
            "cancelled should complete after prior cancel"
        );
    }

    #[test]
    fn job_cancellation_token_shutdown_wins_if_shutdown_happens_first() {
        let token = JobCancellationToken::new();

        token.cancel_for_shutdown();
        token.cancel();

        assert!(token.is_host_shutdown());
        assert!(!token.is_job_cancelled());
    }

    #[test]
    fn job_cancellation_token_shutdown_wins_if_job_cancel_happens_first() {
        let token = JobCancellationToken::new();

        token.cancel();
        token.cancel_for_shutdown();

        assert!(token.is_host_shutdown());
        assert!(!token.is_job_cancelled());
    }
}
