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

use crate::client::{
    GenerationIntake, GenerationIntakeHandle, GenerationIntakeRetireReason, TransportGeneration,
    TrellisClientError,
};
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
    intake: Option<Arc<dyn WorkerIntake>>,
    /// The manager-registered owner, unregistered on stop/drop so a stopped host
    /// is never resurrected by a later candidate adoption.
    intake_owner: Option<Arc<dyn GenerationIntake>>,
    manager: Option<crate::client::TransportGenerationManager>,
}

/// Shuts down a generation-following worker intake owner.
pub(crate) trait WorkerIntake: Send + Sync {
    /// Retire all installed intake, await accepted work, and release leases.
    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()>;
    /// Signal retirement without awaiting accepted work (bounded drop path).
    fn retire_now(&self);
    /// Resolve with the first unexpected worker failure; otherwise wait forever.
    fn failure<'a>(&'a self) -> BoxFuture<'a, Option<WorkerHostError>>;
    /// The logical worker count this host owns across every physical generation.
    fn worker_count(&self) -> usize;
}

impl WorkerHostHandle {
    /// Unregister the owner so no future generation can resurrect a stopped host.
    fn detach_owner(&mut self) {
        if let (Some(manager), Some(owner)) = (&self.manager, &self.intake_owner) {
            manager.detach_intake(owner);
        }
    }
}

impl Drop for WorkerHostHandle {
    fn drop(&mut self) {
        // Bounded cancellation: a host dropped without `stop` still stops
        // accepting new intake, unregisters its owner, and signals shutdown to
        // in-flight handlers.
        self.cancellation.cancel_for_shutdown();
        if let Some(intake) = &self.intake {
            intake.retire_now();
        }
        self.detach_owner();
    }
}

impl WorkerHostHandle {
    /// Return the number of logical queue workers owned by this host.
    pub fn worker_count(&self) -> usize {
        match &self.intake {
            Some(intake) => intake.worker_count(),
            None => self.workers.len(),
        }
    }

    /// Stop all worker tasks and then stop host heartbeats.
    pub async fn stop(mut self) -> Result<(), WorkerHostError> {
        self.cancellation.cancel_for_shutdown();
        self.detach_owner();
        let intake = self.intake.take();
        if let Some(intake) = &intake {
            intake.shutdown().await;
        }

        let mut first_error = None;
        for worker in std::mem::take(&mut self.workers) {
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

        for heartbeat in std::mem::take(&mut self.heartbeats) {
            heartbeat.stop().await.map_err(WorkerHostError::Heartbeat)?;
        }

        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    /// Supervise worker tasks until one exits, then stop the complete host.
    pub async fn join(mut self) -> Result<(), WorkerHostError> {
        if let Some(intake) = self.intake.take() {
            // A generation-following host is supervised across generations: it
            // stops on host cancellation or on the first unexpected worker
            // failure, and always awaits accepted work before returning.
            let cancellation = self.cancellation.clone();
            let _cancel_on_drop = CancelWorkersOnDrop(cancellation.clone());
            let failure = tokio::select! {
                _ = cancellation.cancelled() => None,
                failure = intake.failure() => failure,
            };
            intake.shutdown().await;
            for heartbeat in std::mem::take(&mut self.heartbeats) {
                heartbeat.stop().await.map_err(WorkerHostError::Heartbeat)?;
            }
            if let Some(error) = failure {
                return Err(error);
            }
            return Ok(());
        }
        let cancellation = self.cancellation.clone();
        let _cancel_on_drop = CancelWorkersOnDrop(cancellation.clone());
        let mut workers: FuturesUnordered<WorkerJoinFuture> = std::mem::take(&mut self.workers)
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
        for heartbeat in std::mem::take(&mut self.heartbeats) {
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
    /// The generation lease this worker's consumer was opened on. Held for the
    /// worker's whole life so a delivered job pins its receiving generation
    /// through its final ack/nak/term and handler completion.
    _lease: Option<crate::client::TransportLease>,
    /// Signals that this worker must stop accepting **new** intake. An accepted
    /// job always completes through its ack/nak before the worker exits. A fixed
    /// built-in worker has no retire watcher and never spins on a dropped sender.
    retire: Option<tokio::sync::watch::Receiver<bool>>,
    /// The one logical concurrency budget shared across every physical intake
    /// generation, so a cutover overlap never doubles running handlers.
    capacity: Option<Arc<tokio::sync::Semaphore>>,
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
        _lease,
        mut retire,
        capacity,
    } = resources;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| RuntimeWorkerError::Messages {
            consumer: queue.consumer_name.clone(),
            details: error.to_string(),
        })?;

    'work: loop {
        // Retirement stops new intake: an accepted job below always completes
        // through its ack/nak before the loop exits and releases the lease.
        if retire.as_ref().is_some_and(|retire| *retire.borrow()) {
            break;
        }
        // One logical concurrency budget is shared across every physical intake
        // generation, so a cutover overlap never doubles running handlers.
        let _permit = match &capacity {
            Some(semaphore) => semaphore.clone().acquire_owned().await.ok(),
            None => None,
        };
        let next_message = tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = wait_for_retire(&mut retire) => break,
            next_message = messages.next() => next_message,
        };
        let Some(message) = next_message else {
            break;
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                // A retired generation's consumer stream ends with a connection
                // error as the physical attachment is retired; that is expected,
                // not a worker fault. A live worker's stream error stays fatal
                // and is surfaced through host supervision.
                if retire.as_ref().is_some_and(|retire| *retire.borrow())
                    || cancellation.is_host_shutdown()
                {
                    break;
                }
                return Err(RuntimeWorkerError::Messages {
                    consumer: queue.consumer_name.clone(),
                    details: error.to_string(),
                });
            }
        };
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
    let prepared_workers =
        prepare_workers(&jetstream, &binding, &queue_types, &queue_concurrency).await?;

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
                    _lease: None,
                    retire: None,
                    capacity: None,
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
        intake: None,
        intake_owner: None,
        manager: None,
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

/// Wait until this worker's retire signal fires, or forever when there is none.
///
/// The fixed built-in path has no retire watcher, so it must never resolve on a
/// dropped sender (which would busy-spin or instantly terminate a valid worker).
async fn wait_for_retire(retire: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match retire {
        Some(receiver) => {
            // `Err` means the sender was dropped: treat it as retirement.
            let _ = receiver.changed().await;
        }
        None => std::future::pending().await,
    }
}

/// One prepared queue worker: its queue identity, lifecycle stream, and durable
/// consumer opened on the receiving generation.
type PreparedWorker = (
    String,
    u32,
    JobsQueueBinding,
    stream::Stream<()>,
    consumer::PullConsumer,
);

/// Open every durable worker consumer for `binding` on one generation's
/// connection. The durable consumer identity is unchanged; only the physical
/// connection it is opened on follows the generation.
async fn prepare_workers(
    jetstream: &jetstream::Context,
    binding: &JobsRuntimeBinding,
    queue_types: &[String],
    queue_concurrency: &BTreeMap<String, u32>,
) -> Result<Vec<PreparedWorker>, WorkerHostError> {
    let mut prepared = Vec::new();
    for queue_type in queue_types {
        let queue = binding.jobs.queues.get(queue_type).ok_or_else(|| {
            WorkerHostError::MissingQueueBinding {
                queue_type: queue_type.clone(),
            }
        })?;
        for worker_index in 0..queue_concurrency[queue_type] {
            let lifecycle_stream = lifecycle_stream(jetstream).await.map_err(|error| {
                WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                }
            })?;
            let consumer = ensure_worker_consumer(jetstream, &binding.work_stream, queue)
                .await
                .map_err(|error| WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                })?;
            prepared.push((
                queue_type.clone(),
                worker_index,
                queue.clone(),
                lifecycle_stream,
                consumer,
            ));
        }
    }
    Ok(prepared)
}

/// One generation's worker intake: the durable consumer tasks opened on that
/// generation, and the retire signal that stops new intake and drains accepted
/// jobs before each task releases its lease.
struct JobsWorkerIngress {
    retire: tokio::sync::watch::Sender<bool>,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<Result<(), RuntimeWorkerError>>>>,
}

impl JobsWorkerIngress {
    fn retire(&self) {
        let _ = self.retire.send(true);
    }

    fn is_finished(&self) -> bool {
        self.tasks
            .lock()
            .map(|tasks| tasks.iter().all(|task| task.is_finished()))
            .unwrap_or(true)
    }

    fn push_task(&self, task: tokio::task::JoinHandle<Result<(), RuntimeWorkerError>>) {
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.push(task);
        } else {
            task.abort();
        }
    }

    /// Stop new intake on every worker and await each accepted job through its
    /// final ack/nak before the leases release.
    async fn shutdown(&self) {
        self.retire();
        let tasks = self
            .tasks
            .lock()
            .map(|mut tasks| std::mem::take(&mut *tasks))
            .unwrap_or_default();
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Abandon this generation's intake without waiting for accepted jobs: stop
    /// new intake and abort any task still running. The graceful disposal path
    /// only reaches here after [`Self::shutdown`], so it is a no-op there; a
    /// forced termination abandons accepted jobs rather than blocking on them.
    fn abort(&self) {
        self.retire();
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

impl Drop for JobsWorkerIngress {
    fn drop(&mut self) {
        // RAII: a rolled-back or cancelled adoption and a reaped generation both
        // stop new intake here. Tasks that have not already drained are aborted
        // so a dropped ingress cannot leave broker subscriptions or leases
        // behind; a normally retired ingress has already drained, so this is a
        // no-op.
        let _ = self.retire.send(true);
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

/// RAII handle the manager holds for one generation's jobs intake.
///
/// Dropping the handle retires the ingress even while the owner keeps its own
/// `Arc` for bookkeeping and shutdown, so a failed or cancelled adoption always
/// stops the intake it started without relying on the final `Arc` drop.
struct JobsWorkerIngressHandle {
    ingress: Arc<JobsWorkerIngress>,
}

impl Drop for JobsWorkerIngressHandle {
    fn drop(&mut self) {
        self.ingress.retire();
    }
}

impl GenerationIntakeHandle for JobsWorkerIngressHandle {
    fn retire(&self, _reason: GenerationIntakeRetireReason) {
        self.ingress.retire();
    }

    fn intake_stopped(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Each worker task pins its generation lease until every accepted
            // job drains, so this only needs to observe task completion. The
            // ingress owns its tasks; `dispose` awaits them.
            loop {
                if self.ingress.is_finished() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Release the generation's worker intake without awaiting accepted
            // jobs; the graceful path only reaches here after `intake_stopped`.
            self.ingress.abort();
        })
    }
}

/// The logical service job worker host's generation-following intake owner.
///
/// One owner spans the host's whole life: it installs broker-ready durable
/// consumer intake on each adopted generation and retires the previous
/// generation's intake when that generation is superseded, so there is exactly
/// one logical worker host and no per-generation registration of it.
struct JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
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
    binding: JobsRuntimeBinding,
    queue_types: Vec<String>,
    queue_concurrency: BTreeMap<String, u32>,
    publisher_factory: PF,
    meta_factory: MF,
    handler: H,
    cancellation: JobCancellationToken,
    cancellation_registry: ActiveJobCancellationRegistry,
    /// One logical concurrency budget **per queue**, shared across every
    /// physical intake generation, so a cutover overlap never doubles running
    /// handlers and one queue's idle permits never starve another queue.
    capacity: BTreeMap<String, Arc<tokio::sync::Semaphore>>,
    /// The total logical worker count, reported honestly for managed hosts.
    logical_workers: usize,
    installed: std::sync::Mutex<Vec<Arc<JobsWorkerIngress>>>,
    /// Set once the host is stopped, so no later candidate can resurrect it.
    stopped: std::sync::atomic::AtomicBool,
    /// The first unexpected worker failure, surfaced to `join`.
    failure: Arc<tokio::sync::Notify>,
    failure_message: Arc<std::sync::Mutex<Option<String>>>,
}

impl<PF, P, MF, M, H, Fut, E> JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
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
    async fn install(
        &self,
        generation: Arc<TransportGeneration>,
    ) -> Result<Arc<JobsWorkerIngress>, WorkerHostError> {
        let lease = generation
            .lease()
            .ok_or_else(|| WorkerHostError::WorkerStartup {
                queue_type: "<all>".to_string(),
                details: "transport generation is closed; jobs intake was not installed"
                    .to_string(),
            })?;
        let nats = lease.nats().clone();
        let jetstream = jetstream::new(nats.clone());
        let prepared = prepare_workers(
            &jetstream,
            &self.binding,
            &self.queue_types,
            &self.queue_concurrency,
        )
        .await?;
        let (retire, retire_rx) = tokio::sync::watch::channel(false);
        // Built incrementally and wrapped in an RAII ingress: cancelling this
        // future drops the ingress, whose `Drop` retires and aborts any partial
        // installation instead of leaking broker subscriptions.
        let ingress = Arc::new(JobsWorkerIngress {
            retire,
            tasks: std::sync::Mutex::new(Vec::new()),
        });
        for (queue_type, worker_index, queue, lifecycle_stream, consumer) in prepared {
            let worker_nats = nats.clone();
            let publisher = (self.publisher_factory)();
            let meta = (self.meta_factory)(&queue_type, worker_index);
            let handler = self.handler.clone();
            let cancellation = self.cancellation.clone();
            let registry = self.cancellation_registry.clone();
            let jobs = self.binding.jobs.clone();
            let worker_retire = retire_rx.clone();
            let worker_lease = Some(lease.clone());
            let capacity = Arc::clone(&self.capacity[&queue_type]);
            let failure = Arc::clone(&self.failure);
            let failure_message = Arc::clone(&self.failure_message);
            let task = tokio::spawn(async move {
                let manager = JobManager::new(publisher, jobs, meta);
                let result = run_prepared_queue_worker_with_cancellation(
                    worker_nats,
                    WorkerLoopResources {
                        consumer,
                        lifecycle_stream,
                        queue,
                        manager,
                        cancellation,
                        cancellation_registry: registry,
                        key_coordinator: None,
                        _lease: worker_lease,
                        retire: Some(worker_retire),
                        capacity: Some(capacity),
                    },
                    handler,
                )
                .await;
                if let Err(error) = &result {
                    if let Ok(mut message) = failure_message.lock() {
                        if message.is_none() {
                            *message = Some(error.to_string());
                        }
                    }
                    failure.notify_one();
                }
                result
            });
            ingress.push_task(task);
        }
        Ok(ingress)
    }
}

impl<PF, P, MF, M, H, Fut, E> GenerationIntake for JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
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
    fn adopt<'a>(
        &'a self,
        generation: Arc<TransportGeneration>,
    ) -> BoxFuture<'a, Result<Box<dyn GenerationIntakeHandle>, TrellisClientError>> {
        Box::pin(async move {
            if self.stopped.load(Ordering::Acquire) {
                return Err(TrellisClientError::TransportUnavailable(
                    "job worker host is stopped".into(),
                ));
            }
            let ingress = self
                .install(generation)
                .await
                .map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?;
            // The stopped check and the registration share the `installed` lock
            // with `shutdown`/`retire_now`, so a host stopped under an in-flight
            // adoption can never end up with an undrained late ingress: either it
            // is registered before the drain takes the list, or the check sees
            // the stop and this ingress is retired instead of registered.
            {
                let mut installed = self
                    .installed
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if self.stopped.load(Ordering::Acquire) {
                    drop(installed);
                    ingress.retire();
                    return Err(TrellisClientError::TransportUnavailable(
                        "job worker host is stopped".into(),
                    ));
                }
                installed.retain(|existing| !existing.is_finished());
                installed.push(Arc::clone(&ingress));
            }
            Ok(Box::new(JobsWorkerIngressHandle { ingress }) as Box<dyn GenerationIntakeHandle>)
        })
    }
}

impl<PF, P, MF, M, H, Fut, E> WorkerIntake for JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
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
    fn shutdown<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let ingresses = {
                let mut installed = self
                    .installed
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                self.stopped.store(true, Ordering::Release);
                std::mem::take(&mut *installed)
            };
            for ingress in ingresses {
                ingress.shutdown().await;
            }
        })
    }

    fn retire_now(&self) {
        let mut installed = self
            .installed
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.stopped.store(true, Ordering::Release);
        // Completed generations are dropped so bookkeeping cannot accumulate
        // indefinitely across repeated growth.
        installed.retain(|ingress| !ingress.is_finished());
        for ingress in installed.iter() {
            ingress.retire();
        }
    }

    fn failure<'a>(&'a self) -> BoxFuture<'a, Option<WorkerHostError>> {
        Box::pin(async move {
            self.failure.notified().await;
            self.failure_message
                .lock()
                .ok()
                .and_then(|message| message.clone())
                .map(|details| WorkerHostError::WorkerTask {
                    queue_type: "*".to_string(),
                    worker_index: 0,
                    details,
                })
        })
    }

    fn worker_count(&self) -> usize {
        self.logical_workers
    }
}

/// RAII guard for worker-host startup.
///
/// A startup that fails or whose future is dropped before the host is returned
/// detaches the owner and retires the intake it installed, so a failed or
/// cancelled start never leaves worker intake or a registration behind.
struct WorkerHostStartupGuard {
    intake: Arc<dyn WorkerIntake>,
    owner: Arc<dyn GenerationIntake>,
    manager: crate::client::TransportGenerationManager,
    /// Every heartbeat started so far. Dropped (aborting each task) if startup
    /// fails or is cancelled; handed to the host only on successful return.
    heartbeats: Vec<WorkerHeartbeatHandle>,
    armed: bool,
}

impl WorkerHostStartupGuard {
    fn push_heartbeat(&mut self, heartbeat: WorkerHeartbeatHandle) {
        self.heartbeats.push(heartbeat);
    }

    /// Disarm and release the started heartbeats to the completed host.
    fn finish(mut self) -> Vec<WorkerHeartbeatHandle> {
        self.armed = false;
        std::mem::take(&mut self.heartbeats)
    }
}

impl Drop for WorkerHostStartupGuard {
    fn drop(&mut self) {
        // `heartbeats` drop first and abort themselves, so a failed or cancelled
        // start leaves no publishing heartbeat zombie.
        if self.armed {
            self.intake.retire_now();
            self.manager.detach_intake(&self.owner);
        }
    }
}

/// Start a first-class worker host whose work intake follows the logical
/// connection's current transport generation.
///
/// The host registers one generation-following intake owner: the manager
/// installs broker-ready durable consumer intake on a candidate **before** it
/// becomes the default, and retires the superseded generation's intake so its
/// accepted jobs finish through their final ack. There is one logical worker
/// host; only the physical receive loops move.
pub async fn start_worker_host_generation_following<PF, P, MF, M, H, Fut, E>(
    client: Arc<crate::client::TrellisClient>,
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

    let cancellation = JobCancellationToken::new();
    let logical_workers: usize = queue_concurrency
        .values()
        .map(|count| *count as usize)
        .sum();
    // One semaphore per queue so each queue enforces its own declared
    // concurrency and a busy queue never blocks an idle one.
    let capacity = queue_concurrency
        .iter()
        .map(|(queue_type, concurrency)| {
            (
                queue_type.clone(),
                Arc::new(tokio::sync::Semaphore::new(*concurrency as usize)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let owner = Arc::new(JobsWorkerIntake {
        binding,
        queue_types: queue_types.clone(),
        queue_concurrency,
        publisher_factory,
        meta_factory,
        handler,
        cancellation: cancellation.clone(),
        cancellation_registry: ActiveJobCancellationRegistry::new(),
        capacity,
        logical_workers,
        installed: std::sync::Mutex::new(Vec::new()),
        stopped: std::sync::atomic::AtomicBool::new(false),
        failure: Arc::new(tokio::sync::Notify::new()),
        failure_message: Arc::new(std::sync::Mutex::new(None)),
    });
    let generation_owner: Arc<dyn GenerationIntake> =
        Arc::clone(&owner) as Arc<dyn GenerationIntake>;
    // Attach intake before starting heartbeats, so a failed registration cannot
    // leave detached heartbeat loops behind, and a failed heartbeat start drains
    // the intake it just installed.
    client
        .transport_generations()
        .attach_intake(Arc::clone(&generation_owner))
        .await
        .map_err(|error| WorkerHostError::WorkerStartup {
            queue_type: "<all>".to_string(),
            details: error.to_string(),
        })?;
    let intake: Arc<dyn WorkerIntake> = Arc::clone(&owner) as Arc<dyn WorkerIntake>;
    // Cancellation-safe startup: if this future is dropped or a heartbeat fails
    // before the host is returned, the guard detaches the owner and retires the
    // intake it installed.
    let mut startup = WorkerHostStartupGuard {
        intake: Arc::clone(&intake),
        owner: Arc::clone(&generation_owner),
        manager: client.transport_generations(),
        heartbeats: Vec::new(),
        armed: true,
    };
    for queue_type in &queue_types {
        let heartbeat = start_worker_heartbeat_loop(
            client.nats().clone(),
            WorkerHeartbeatOptions {
                service: owner.binding.jobs.service_name.clone(),
                subject_service: owner.binding.jobs.namespace.clone(),
                job_type: queue_type.clone(),
                instance_id: instance_id.clone(),
                concurrency: Some(owner.queue_concurrency[queue_type]),
                version: options.version.clone(),
                interval: options.heartbeat_interval,
            },
        )
        .await?;
        startup.push_heartbeat(heartbeat);
    }
    let heartbeats = startup.finish();

    Ok(WorkerHostHandle {
        cancellation,
        heartbeats,
        workers: Vec::new(),
        intake: Some(intake),
        intake_owner: Some(generation_owner),
        manager: Some(client.transport_generations()),
    })
}
