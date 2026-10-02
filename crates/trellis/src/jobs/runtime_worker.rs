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
use futures_util::{FutureExt, StreamExt};
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
    NatsKeyCoordinator, QueueMutationOutcome,
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
const CANCELLATION_DEADLINE: u8 = 4;
const CANCELLATION_RETRY_EXHAUSTED: u8 = 5;
const CANCELLATION_STALE_ATTEMPT: u8 = 6;

/// Why a job attempt must stop or reconcile previously started work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobCancellationReason {
    /// A business-level cancellation was requested.
    Requested,
    /// The worker host is stopping; work must remain redeliverable.
    HostShutdown,
    /// This attempt no longer owns its execution lease.
    LeaseLost,
    /// The absolute job deadline elapsed; reconcile before declaring Expired.
    DeadlineExceeded,
    /// Ordinary attempts were exhausted; reconcile before declaring Dead.
    RetryExhausted,
    /// Another job displaced the previous attempt; reconcile before Stale.
    StaleAttempt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerAckAction {
    Ack,
    Nak(Duration),
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
        let _ = self
            .cancelled
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |reason| {
                matches!(
                    reason,
                    CANCELLATION_NONE
                        | CANCELLATION_DEADLINE
                        | CANCELLATION_RETRY_EXHAUSTED
                        | CANCELLATION_STALE_ATTEMPT
                )
                .then_some(CANCELLATION_JOB)
            });
        self.notify.notify_waiters();
    }

    /// Mark the token as cancelled because the worker host is shutting down.
    pub fn cancel_for_shutdown(&self) {
        let _ = self
            .cancelled
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |reason| {
                (reason != CANCELLATION_LEASE_LOST).then_some(CANCELLATION_HOST_SHUTDOWN)
            });
        self.notify.notify_waiters();
    }

    fn cancel_for_lease_loss(&self) {
        self.cancelled
            .store(CANCELLATION_LEASE_LOST, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn cancel_for_deadline(&self) {
        let _ = self.cancelled.compare_exchange(
            CANCELLATION_NONE,
            CANCELLATION_DEADLINE,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.notify.notify_waiters();
    }

    pub(crate) fn cancel_for_retry_exhaustion(&self) {
        let _ = self.cancelled.compare_exchange(
            CANCELLATION_NONE,
            CANCELLATION_RETRY_EXHAUSTED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.notify.notify_waiters();
    }

    /// Request reconciliation of a displaced attempt under a recovered fence.
    pub(crate) fn cancel_for_stale_attempt(&self) {
        let _ = self.cancelled.compare_exchange(
            CANCELLATION_NONE,
            CANCELLATION_STALE_ATTEMPT,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.notify.notify_waiters();
    }

    /// Return the explicit cancellation reason, or None while execution is allowed.
    pub fn reason(&self) -> Option<JobCancellationReason> {
        match self.cancelled.load(Ordering::SeqCst) {
            CANCELLATION_JOB => Some(JobCancellationReason::Requested),
            CANCELLATION_HOST_SHUTDOWN => Some(JobCancellationReason::HostShutdown),
            CANCELLATION_LEASE_LOST => Some(JobCancellationReason::LeaseLost),
            CANCELLATION_DEADLINE => Some(JobCancellationReason::DeadlineExceeded),
            CANCELLATION_RETRY_EXHAUSTED => Some(JobCancellationReason::RetryExhausted),
            CANCELLATION_STALE_ATTEMPT => Some(JobCancellationReason::StaleAttempt),
            _ => None,
        }
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
    #[error(
        "failed to read latest jobs lifecycle event for subject '{subject}' from stream '{stream}': {details}"
    )]
    LifecycleRead {
        stream: String,
        subject: String,
        details: String,
    },
    #[error(
        "failed to decode latest jobs lifecycle event for subject '{subject}' from stream '{stream}': {details}"
    )]
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
    cleanup_required: bool,
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
        Ok(JobProcessOutcome::Expired { .. }) => "expired",
        Ok(JobProcessOutcome::Dead { .. }) => "dead",
        Ok(JobProcessOutcome::Stale { .. }) => "stale",
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
    /// A managed slot's receiving lease, held through bounded pull expiry or
    /// delivery processing and final disposition. Fixed workers need no lease.
    _lease: Option<crate::client::TransportLease>,
    /// Signals that this worker must stop accepting **new** intake. An accepted
    /// job always completes through its ack/nak before the worker exits. A fixed
    /// built-in worker has no retire watcher and never spins on a dropped sender.
    retire: Option<tokio::sync::watch::Receiver<bool>>,
    /// Managed slots return after one accounted receive, then select current transport.
    single_receive: bool,
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
        retire,
        single_receive,
    } = resources;
    let mut received = false;
    'work: loop {
        if single_receive && received {
            break;
        }
        // Retirement stops new intake: an accepted job below always completes
        // through its ack/nak before the loop exits and releases the lease.
        if cancellation.is_cancelled()
            || retire
                .as_ref()
                .is_some_and(|retire| *retire.borrow() || retire.has_changed().is_err())
        {
            break;
        }
        // Once issued, account for this bounded pull even if intake retires or
        // the host stops. This logical slot and its receiving lease stay occupied
        // through any returned message's final disposition; it cannot pull early.
        received = true;
        let mut messages = match consumer
            .batch()
            .max_messages(1)
            .expires(Duration::from_secs(1))
            .messages()
            .await
        {
            Ok(messages) => messages,
            Err(error) => {
                if retire
                    .as_ref()
                    .is_some_and(|retire| *retire.borrow() || retire.has_changed().is_err())
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
        let Some(message) = messages.next().await else {
            // Broker expiry is ordinary idle, including for fixed workers.
            continue;
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                // A retired generation's consumer stream ends with a connection
                // error as the physical attachment is retired; that is expected,
                // not a worker fault. A live worker's stream error stays fatal
                // and is surfaced through host supervision.
                if retire
                    .as_ref()
                    .is_some_and(|retire| *retire.borrow() || retire.has_changed().is_err())
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
        let _delivery_attempt = match message.info() {
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
        let job_cancellation = JobCancellationToken::new();
        if cancellation.is_host_shutdown() {
            job_cancellation.cancel_for_shutdown();
        } else if cancellation.is_job_cancelled() {
            job_cancellation.cancel();
        } else if cancellation.is_lease_lost() {
            job_cancellation.cancel_for_lease_loss();
        }
        let heartbeat_hook: Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync> = {
            let heartbeat_message = message.clone();
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
        // This delivery already occupies a slot. Maintain its broker reservation
        // before awaiting lifecycle reads or keyed admission, not just execution.
        let auto_heartbeat = {
            let heartbeat_interval = progress_ack_interval(&queue);
            let heartbeat_hook = Arc::clone(&heartbeat_hook);
            let job_cancellation = job_cancellation.clone();
            Some(AbortWorkerTask(tokio::spawn(async move {
                let mut interval = tokio::time::interval(heartbeat_interval);
                interval.tick().await;
                loop {
                    interval.tick().await;
                    if heartbeat_hook().await.is_err() {
                        job_cancellation.cancel_for_lease_loss();
                        break;
                    }
                }
            })))
        };
        let job_key = job_key(&parsed_job.service, &parsed_job.job_type, &parsed_job.id);
        if stream_work_decision(&lifecycle_stream, &queue.publish_prefix, &mut parsed_job).await?
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
        let _cancellation_guard =
            cancellation_registry.register(job_key.clone(), job_cancellation.clone());
        let handler = handler.clone();
        let active_key = loop {
            let active_key = acquire_key_slot_for_work(
                key_coordinator.as_ref(),
                &queue,
                manager.bindings().namespace.as_str(),
                &parsed_job,
                &manager.now_iso(),
                parsed_job.tries.saturating_add(1),
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
            if active_key.cleanup_required {
                job_cancellation.cancel_for_stale_attempt();
            }
        }
        let forward_cancellation = {
            let outer_cancellation = cancellation.clone();
            let job_cancellation = job_cancellation.clone();
            AbortWorkerTask(tokio::spawn(async move {
                outer_cancellation.cancelled().await;
                if outer_cancellation.is_host_shutdown() {
                    job_cancellation.cancel_for_shutdown();
                } else if outer_cancellation.is_job_cancelled() {
                    job_cancellation.cancel();
                } else if outer_cancellation.is_lease_lost() {
                    job_cancellation.cancel_for_lease_loss();
                }
            }))
        };
        let auto_key_heartbeat = active_key.as_ref().and_then(|active_key| {
            let coordinator = key_coordinator.clone()?;
            let heartbeat_interval = active_key.heartbeat_interval;
            let active_key = active_key.clone();
            let job_cancellation = job_cancellation.clone();
            Some(AbortWorkerTask(tokio::spawn(async move {
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
            })))
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
        if let Some(mut auto_heartbeat) = auto_heartbeat {
            auto_heartbeat.0.abort();
            let _ = (&mut auto_heartbeat.0).await;
        }
        if let Some(mut auto_key_heartbeat) = auto_key_heartbeat {
            auto_key_heartbeat.0.abort();
            let _ = (&mut auto_key_heartbeat.0).await;
        }
        let mut forward_cancellation = forward_cancellation;
        forward_cancellation.0.abort();
        let _ = (&mut forward_cancellation.0).await;
        let process_result = process_result?;
        if matches!(
            process_result,
            JobProcessOutcome::Expired { .. }
                | JobProcessOutcome::Dead { .. }
                | JobProcessOutcome::Stale { .. }
        ) {
            cleanup_queued_key_for_terminal(
                key_coordinator.as_ref(),
                &queue,
                manager.bindings().namespace.as_str(),
                &parsed_job,
                &manager.now_iso(),
            )
            .await?;
        }
        match ack_action_for_outcome(Some(&process_result), &queue.backoff_ms) {
            WorkerAckAction::Ack => message.ack().await.map_err(map_ack_error)?,
            WorkerAckAction::Nak(delay) => message
                .ack_with(AckKind::Nak(Some(delay)))
                .await
                .map_err(map_ack_error)?,
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
    // Managed single receives use their prepared source's persistent listeners.
    let cancellation_task = if resources.single_receive {
        None
    } else {
        Some(
            prepare_cancellation_listener(
                &nats,
                &resources.queue,
                resources.cancellation_registry.clone(),
                resources.cancellation.clone(),
            )
            .await?,
        )
    };

    resources.key_coordinator = key_coordinator_for_queue(
        nats.clone(),
        resources.manager.bindings().namespace.as_str(),
        &resources.queue,
    )
    .await?;
    // Keep the large generic loop off the surrounding worker poll frames.
    let result = Box::pin(run_prepared_queue_worker_loop(resources, handler)).await;
    drop(cancellation_task);
    result
}

async fn prepare_cancellation_listener(
    nats: &async_nats::Client,
    queue: &JobsQueueBinding,
    cancellation_registry: ActiveJobCancellationRegistry,
    host_cancellation: JobCancellationToken,
) -> Result<AbortWorkerTask, RuntimeWorkerError> {
    let cancellation_subject = format!("{}.*.cancelled", queue.publish_prefix);
    let mut cancellation_subscriber =
        nats.subscribe(cancellation_subject.clone())
            .await
            .map_err(|error| RuntimeWorkerError::CancellationSubscription {
                subject: cancellation_subject.clone(),
                details: error.to_string(),
            })?;
    Ok(AbortWorkerTask(tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                _ = host_cancellation.cancelled() => break,
                message = cancellation_subscriber.next() => message,
            };
            let Some(message) = message else { break };
            let Ok(event) = serde_json::from_slice::<JobEvent>(&message.payload) else {
                continue;
            };
            if event.event_type != JobEventType::Cancelled {
                continue;
            }
            let key = job_key(&event.service, &event.job_type, &event.job_id);
            cancellation_registry.cancel(&key);
        }
    })))
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
            stale_slots: _,
        } => Ok(Some(ActiveKeyLease {
            policy,
            slot: *slot,
            heartbeat_interval: Duration::from_millis(key_concurrency.heartbeat_interval_ms),
            heartbeat_ttl_ms: key_concurrency.heartbeat_ttl_ms,
            cleanup_required: state.cleanup_pending.contains(&job.id),
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
        .update_key(&policy, {
            let policy = policy.clone();
            let job_id = job.id.clone();
            let removed_at = removed_at.to_string();
            move |current| {
                let Some(mut state) = current else {
                    return QueueMutationOutcome::Missing {
                        state: new_key_state(&policy, &removed_at),
                    };
                };
                state.queued.retain(|entry| entry.job_id != job_id);
                state.active.retain(|slot| slot.job_id != job_id);
                state.cleanup_pending.retain(|id| id != &job_id);
                state.updated_at = removed_at.clone();
                QueueMutationOutcome::Removed { state }
            }
        })
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
                    single_receive: false,
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
    // Broker delivery must outlive application attempts until the execution owner
    // finishes reconciliation, including after repeated worker crashes.
    if config.max_deliver != -1 {
        return Some(format!(
            "stale consumer max deliver {}, expected unlimited (-1) for execution-owner reconciliation",
            config.max_deliver
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
    work: &mut Job,
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

    let decision = lifecycle_work_decision(Some(&latest), work);
    if latest.service == work.service
        && latest.job_type == work.job_type
        && latest.job_id == work.id
    {
        work.tries = latest.tries;
        work.state = latest.state;
    }
    Ok(decision)
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
    backoff_ms: &[u64],
) -> WorkerAckAction {
    match outcome {
        Some(JobProcessOutcome::Retry { tries, .. }) => {
            WorkerAckAction::Nak(Duration::from_millis(retry_delay_ms(*tries, backoff_ms)))
        }
        Some(JobProcessOutcome::Interrupted { .. })
        | Some(JobProcessOutcome::StaleCompletionIgnored { .. }) => {
            WorkerAckAction::Nak(Duration::from_secs(5))
        }
        Some(JobProcessOutcome::Completed { .. })
        | Some(JobProcessOutcome::Cancelled { .. })
        | Some(JobProcessOutcome::Failed { .. })
        | Some(JobProcessOutcome::Expired { .. })
        | Some(JobProcessOutcome::Dead { .. })
        | Some(JobProcessOutcome::Stale { .. })
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

/// Immutable broker sources prepared for one generation. No pull is opened by
/// preparation, and no lifetime lease is retained by the descriptor.
struct JobsWorkerIngress {
    generation_id: u64,
    retire: tokio::sync::watch::Sender<bool>,
    queues: BTreeMap<String, (JobsQueueBinding, stream::Stream<()>, consumer::PullConsumer)>,
    nats: async_nats::Client,
    // Raw subscriptions hold no generation lease and outlive individual receives.
    cancellation_listeners: std::sync::Mutex<Vec<AbortWorkerTask>>,
    reservations: std::sync::Mutex<usize>,
    drained: tokio::sync::Notify,
    forced: JobCancellationToken,
}

impl JobsWorkerIngress {
    fn retire(&self) {
        let _reservations = self.reservations.lock().unwrap_or_else(|e| e.into_inner());
        self.retire.send_replace(true);
    }

    fn is_finished(&self) -> bool {
        *self.reservations.lock().unwrap_or_else(|e| e.into_inner()) == 0
    }

    /// Stop new intake on every worker and await each accepted job through its
    /// final ack/nak before the leases release.
    async fn shutdown(&self) {
        self.retire();
        loop {
            let notified = self.drained.notified();
            if self.is_finished() {
                return;
            }
            notified.await;
        }
    }

    /// Dispose only this source; forced loss cancels its receiving work without
    /// aborting the persistent slots or borrowing another generation for ack.
    fn dispose(&self) {
        self.retire();
        self.forced.cancel_for_lease_loss();
        self.cancellation_listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// RAII handle the manager holds for one generation's jobs intake.
///
/// Dropping the handle disposes the ingress even while the owner keeps its own
/// `Arc` for bookkeeping and shutdown, so a failed or cancelled adoption always
/// stops the intake it started without relying on the final `Arc` drop.
struct JobsWorkerIngressHandle {
    ingress: Arc<JobsWorkerIngress>,
}

impl Drop for JobsWorkerIngressHandle {
    fn drop(&mut self) {
        self.ingress.dispose();
    }
}

impl GenerationIntakeHandle for JobsWorkerIngressHandle {
    fn retire(&self, _reason: GenerationIntakeRetireReason) {
        self.ingress.retire();
    }

    fn intake_stopped(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.ingress.shutdown().await;
        })
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Release the generation's worker intake without awaiting accepted
            // jobs; the graceful path only reaches here after `intake_stopped`.
            self.ingress.dispose();
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
    PF: Fn(async_nats::Client) -> P + Clone + Send + Sync + 'static,
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
    /// Fixed slots are the sole per-queue receive and processing budget.
    logical_workers: usize,
    installed: std::sync::Mutex<BTreeMap<u64, Arc<JobsWorkerIngress>>>,
    sources_changed: tokio::sync::Notify,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    manager: crate::client::TransportGenerationManager,
    /// Set once the host is stopped, so no later candidate can resurrect it.
    stopped: std::sync::atomic::AtomicBool,
    /// The first unexpected worker failure, surfaced to `join`.
    failure: Arc<tokio::sync::Notify>,
    failure_message: Arc<std::sync::Mutex<Option<String>>>,
}

impl<PF, P, MF, M, H, Fut, E> JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
where
    PF: Fn(async_nats::Client) -> P + Clone + Send + Sync + 'static,
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
        let mut queues = BTreeMap::new();
        let mut cancellation_listeners = Vec::new();
        for queue_type in &self.queue_types {
            let queue = self.binding.jobs.queues[queue_type].clone();
            let lifecycle = lifecycle_stream(&jetstream).await.map_err(|error| {
                WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                }
            })?;
            let consumer = ensure_worker_consumer(&jetstream, &self.binding.work_stream, &queue)
                .await
                .map_err(|error| WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                })?;
            cancellation_listeners.push(
                prepare_cancellation_listener(
                    &nats,
                    &queue,
                    self.cancellation_registry.clone(),
                    self.cancellation.clone(),
                )
                .await
                .map_err(|error| WorkerHostError::WorkerStartup {
                    queue_type: queue_type.clone(),
                    details: error.to_string(),
                })?,
            );
            queues.insert(queue_type.clone(), (queue, lifecycle, consumer));
        }
        // Every replacement listener is broker-ready before candidate adoption
        // completes. RAII aborts all listeners if preparation fails or is dropped.
        nats.flush()
            .await
            .map_err(|error| WorkerHostError::WorkerStartup {
                queue_type: "<all>".into(),
                details: error.to_string(),
            })?;
        let (retire, _) = tokio::sync::watch::channel(false);
        // Only preparation owns this base lease. The descriptor does not keep its
        // generation alive; each issued receive acquires its own published lease.
        Ok(Arc::new(JobsWorkerIngress {
            generation_id: lease.generation_id(),
            retire,
            queues,
            nats,
            cancellation_listeners: std::sync::Mutex::new(cancellation_listeners),
            reservations: std::sync::Mutex::new(0),
            drained: tokio::sync::Notify::new(),
            forced: JobCancellationToken::new(),
        }))
    }

    fn start_slots(self: &Arc<Self>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        for queue_type in &self.queue_types {
            for worker_index in 0..self.queue_concurrency[queue_type] {
                let owner = Arc::clone(self);
                let queue_type = queue_type.clone();
                // Metadata identity persists across receives and generations.
                let meta = Arc::new((self.meta_factory)(&queue_type, worker_index));
                tasks.push(tokio::spawn(async move {
                    let mut publication = owner.manager.subscribe_publication();
                    let mut availability = owner.manager.watch_own_availability();
                    loop {
                        let changed = owner.sources_changed.notified();
                        if owner.cancellation.is_cancelled() {
                            break;
                        }
                        let published = *publication.borrow_and_update();
                        // Never acquire manager locks with the source map locked.
                        let source = published.and_then(|id| {
                            owner
                                .installed
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&id)
                                .cloned()
                        });
                        let receiving = source.and_then(|source| {
                            let lease = owner.manager.acquire_application_published().ok()?;
                            if Some(lease.generation_id()) != published {
                                return None;
                            }
                            let mut reservations = source
                                .reservations
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            if *source.retire.borrow() || owner.cancellation.is_cancelled() {
                                return None;
                            }
                            *reservations += 1;
                            drop(reservations);
                            Some((JobsWorkerReservation(source), lease))
                        });
                        let Some((reservation, lease)) = receiving else {
                            tokio::select! {
                                _ = owner.cancellation.cancelled() => break,
                                _ = changed => {},
                                result = publication.changed() => {
                                    if result.is_err() { break; }
                                }
                                result = availability.changed() => {
                                    if result.is_err() { break; }
                                }
                            }
                            continue;
                        };
                        let source = &reservation.0;
                        // Lifecycle and update traffic stays on this receive's
                        // transport through final disposition, never current.
                        let manager = JobManager::new_with_shared_meta(
                            (owner.publisher_factory)(lease.nats().clone()),
                            owner.binding.jobs.clone(),
                            Arc::clone(&meta),
                        );
                        let (queue, lifecycle_stream, consumer) =
                            source.queues[&queue_type].clone();
                        let cancellation = JobCancellationToken::new();
                        let forward = {
                            let host = owner.cancellation.clone();
                            let forced = source.forced.clone();
                            let cancellation = cancellation.clone();
                            AbortWorkerTask(tokio::spawn(async move {
                                tokio::select! {
                                    _ = host.cancelled() => cancellation.cancel_for_shutdown(),
                                    _ = forced.cancelled() => cancellation.cancel_for_lease_loss(),
                                }
                            }))
                        };
                        let result = std::panic::AssertUnwindSafe(
                            run_prepared_queue_worker_with_cancellation(
                                source.nats.clone(),
                                WorkerLoopResources {
                                    consumer,
                                    lifecycle_stream,
                                    queue,
                                    manager,
                                    cancellation,
                                    cancellation_registry: owner.cancellation_registry.clone(),
                                    key_coordinator: None,
                                    _lease: Some(lease),
                                    retire: Some(source.retire.subscribe()),
                                    single_receive: true,
                                },
                                owner.handler.clone(),
                            ),
                        )
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| {
                            Err(RuntimeWorkerError::Process("worker panicked".into()))
                        });
                        drop(forward);
                        if let Err(error) = result {
                            // Physical loss belongs to the receiving work. It may
                            // fail its disposition, but cannot be rebound to current.
                            if !source.forced.is_lease_lost() && !owner.cancellation.is_cancelled()
                            {
                                let mut failure = owner
                                    .failure_message
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                if failure.is_none() {
                                    *failure = Some(format!(
                                        "queue {queue_type} slot {worker_index}: {error}"
                                    ));
                                }
                                owner.failure.notify_one();
                                break;
                            }
                        }
                    }
                }));
            }
        }
    }
}

struct JobsWorkerReservation(Arc<JobsWorkerIngress>);

impl Drop for JobsWorkerReservation {
    fn drop(&mut self) {
        let mut count = self
            .0
            .reservations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *count -= 1;
        self.0.drained.notify_waiters();
    }
}

struct AbortWorkerTask(tokio::task::JoinHandle<()>);

impl Drop for AbortWorkerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<PF, P, MF, M, H, Fut, E> GenerationIntake for JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
where
    PF: Fn(async_nats::Client) -> P + Clone + Send + Sync + 'static,
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
            let ingress = tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => {
                    return Err(TrellisClientError::TransportUnavailable(
                        "job worker host is stopped".into(),
                    ));
                }
                result = self.install(generation) => {
                    result.map_err(|error| TrellisClientError::TransportUnavailable(error.to_string()))?
                }
            };
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
                    ingress.dispose();
                    return Err(TrellisClientError::TransportUnavailable(
                        "job worker host is stopped".into(),
                    ));
                }
                installed
                    .retain(|_, existing| !*existing.retire.borrow() || !existing.is_finished());
                installed.insert(ingress.generation_id, Arc::clone(&ingress));
            }
            self.sources_changed.notify_waiters();
            Ok(Box::new(JobsWorkerIngressHandle { ingress }) as Box<dyn GenerationIntakeHandle>)
        })
    }
}

impl<PF, P, MF, M, H, Fut, E> WorkerIntake for JobsWorkerIntake<PF, P, MF, M, H, Fut, E>
where
    PF: Fn(async_nats::Client) -> P + Clone + Send + Sync + 'static,
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
            self.retire_now();
            let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
            for task in tasks {
                let _ = task.await;
            }
            self.installed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        })
    }

    fn retire_now(&self) {
        let mut installed = self
            .installed
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.stopped.store(true, Ordering::Release);
        self.cancellation.cancel_for_shutdown();
        // Completed generations are dropped so bookkeeping cannot accumulate
        // indefinitely across repeated growth.
        installed.retain(|_, ingress| !ingress.is_finished());
        for ingress in installed.values() {
            ingress.retire();
            ingress
                .cancellation_listeners
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
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
        // After this guard tears down registration, its heartbeat handles drop
        // and abort themselves: failed startup leaves no publishing task behind.
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
/// accepted jobs finish through their final ack. Each queue has exactly its
/// declared number of persistent logical slots. Slots choose the published,
/// safe consumer source only between bounded receives and final dispositions.
/// The publisher factory receives that selected transport; each receive gets an
/// immutable publisher while its slot retains the same metadata source.
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
    PF: Fn(async_nats::Client) -> P + Clone + Send + Sync + 'static,
    P: JobEventPublisher + Send + Sync + 'static,
    P::Error: std::fmt::Display,
    MF: Fn(&str, u32) -> M + Clone + Send + Sync + 'static,
    M: JobMetaSource + Send + Sync + 'static,
    H: Fn(ActiveJob<P, M>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, JobProcessError<E>>> + Send + 'static,
    E: ToString + Send + 'static,
{
    let mut queue_types = selected_queue_types(&binding, options.queue_types.as_deref())?;
    queue_types.sort();
    queue_types.dedup();
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
    let owner = Arc::new(JobsWorkerIntake {
        binding,
        queue_types: queue_types.clone(),
        queue_concurrency,
        publisher_factory,
        meta_factory,
        handler,
        cancellation: cancellation.clone(),
        cancellation_registry: ActiveJobCancellationRegistry::new(),
        logical_workers,
        installed: std::sync::Mutex::new(BTreeMap::new()),
        sources_changed: tokio::sync::Notify::new(),
        tasks: std::sync::Mutex::new(Vec::new()),
        manager: client.transport_generations(),
        stopped: std::sync::atomic::AtomicBool::new(false),
        failure: Arc::new(tokio::sync::Notify::new()),
        failure_message: Arc::new(std::sync::Mutex::new(None)),
    });
    let generation_owner: Arc<dyn GenerationIntake> =
        Arc::clone(&owner) as Arc<dyn GenerationIntake>;
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
    owner.start_slots();
    client
        .transport_generations()
        .attach_intake(Arc::clone(&generation_owner))
        .await
        .map_err(|error| WorkerHostError::WorkerStartup {
            queue_type: "<all>".to_string(),
            details: error.to_string(),
        })?;
    for queue_type in &queue_types {
        let heartbeat = super::registry::start_managed_worker_heartbeat_loop(
            client.transport_generations(),
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::Value;

    use super::JobCancellationToken;

    use super::{
        ack_action_for_outcome, lifecycle_work_decision, progress_ack_interval,
        ProjectedWorkDecision, WorkerAckAction,
    };
    use crate::jobs::bindings::JobsQueueBinding;
    use crate::jobs::events::{cancelled, completed, created, started, EventMeta};
    use crate::jobs::manager::JobProcessOutcome;
    use crate::jobs::types::{Job, JobContext, JobState};

    #[tokio::test]
    async fn stale_completion_redelivers_and_retries_cleanup_through_the_durable_consumer() {
        use crate::jobs::bindings::{JobKeyConcurrencyBinding, JobKeyStalePolicy, JobsBinding};
        use crate::jobs::keys::NatsKeyCoordinator;
        use crate::jobs::{
            JobEventHeaders, JobEventPublisher, TrellisJobEventPublisher, TrellisJobMetaSource,
        };
        use async_nats::jetstream::{consumer, stream};
        use futures_util::StreamExt;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/trellis-test-cache"),
            )
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
        let js = async_nats::jetstream::new(client.clone());
        js.create_stream(stream::Config {
            name: "JOBS".into(),
            subjects: vec!["jobs.>".into()],
            allow_direct: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let lifecycle = js.get_stream_no_info("JOBS").await.unwrap();
        let kv = js
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: "JOBS_KEYS_documents".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let coordinator = Arc::new(
            NatsKeyCoordinator::open_for_service(client.clone(), "documents")
                .await
                .unwrap(),
        );
        let queue = JobsQueueBinding {
            queue_type: "work".into(),
            publish_prefix: "jobs.work".into(),
            updates_prefix: None,
            work_subject: "jobs.work.*.created".into(),
            consumer_name: "a".into(),
            max_deliver: 2,
            backoff_ms: vec![100],
            ack_wait_ms: 1_000,
            default_deadline_ms: None,
            update: None,
            key_concurrency: Some(JobKeyConcurrencyBinding {
                key: vec!["/key".into()],
                max_active: 1,
                heartbeat_interval_ms: 30_000,
                heartbeat_ttl_ms: 5_000,
                stale_policy: JobKeyStalePolicy::FailStale,
            }),
            queue: Some(crate::jobs::bindings::JobQueueDepthBinding {
                max_queued_per_key: 1,
                when_full: crate::jobs::bindings::JobQueueWhenFull::Reject,
            }),
        };
        let publisher = TrellisJobEventPublisher::new(client.clone());
        let manager = crate::jobs::manager::JobManager::new_with_key_coordinator(
            publisher.clone(),
            JobsBinding {
                service_name: "documents".into(),
                namespace: "documents".into(),
                queues: [("work".into(), queue.clone())].into(),
            },
            TrellisJobMetaSource,
            coordinator.clone(),
        );
        let a = manager
            .create("work", serde_json::json!({"key":"shared"}))
            .await
            .unwrap();
        let headers = JobEventHeaders::from(&a.context);
        // Lifecycle publication must report missing durable storage; live updates
        // must still reach subscribers without a stream covering their subject.
        assert!(publisher
            .publish("unpersisted.lifecycle".into(), headers.clone(), Vec::new())
            .await
            .is_err());
        let mut updates = client.subscribe("typed_updates.work").await.unwrap();
        publisher
            .publish_update("typed_updates.work".into(), headers, b"live".to_vec())
            .await
            .unwrap();
        let update = tokio::time::timeout(Duration::from_secs(5), updates.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(update.payload.as_ref(), b"live");
        updates.unsubscribe().await.unwrap();
        let a_started = Arc::new(tokio::sync::Notify::new());
        let b_started = Arc::new(tokio::sync::Notify::new());
        let finish_a = Arc::new(tokio::sync::Notify::new());
        let finish_b = Arc::new(tokio::sync::Notify::new());
        let cleanups = Arc::new(AtomicUsize::new(0));
        let mut consumers = Vec::new();
        let mut tasks = Vec::new();
        let cancellation = JobCancellationToken::new();
        let key_hash = crate::jobs::keys::derive_job_key(&a.payload, &["/key".into()])
            .unwrap()
            .key_hash;
        let key = crate::jobs::keys::coordination_key("documents", "work", &key_hash);
        // Separate real durable subscriptions make the takeover order explicit;
        // both run the unchanged production worker and key-coordination adapter.
        for is_a in [true, false] {
            if !is_a {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let entry = kv.entry(key.clone()).await.unwrap().unwrap();
                        let key_state: crate::jobs::keys::JobKeyState =
                            serde_json::from_slice(&entry.value).unwrap();
                        let expiry = time::OffsetDateTime::parse(
                            &key_state.active[0].lease_expires_at,
                            &time::format_description::well_known::Rfc3339,
                        )
                        .unwrap();
                        if time::OffsetDateTime::now_utc() >= expiry {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
            }
            let job = if is_a {
                a.clone()
            } else {
                manager
                    .create("work", serde_json::json!({"key":"shared"}))
                    .await
                    .unwrap()
            };
            let mut binding = queue.clone();
            binding.consumer_name = if is_a { "a" } else { "b" }.into();
            binding.work_subject = format!("jobs.work.{}.created", job.id);
            let consumer = lifecycle
                .create_consumer(consumer::pull::Config {
                    durable_name: Some(binding.consumer_name.clone()),
                    filter_subject: binding.work_subject.clone(),
                    ack_policy: consumer::AckPolicy::Explicit,
                    max_deliver: -1,
                    ack_wait: Duration::from_secs(1),
                    ..Default::default()
                })
                .await
                .unwrap();
            consumers.push(consumer.clone());
            let entered = if is_a {
                a_started.clone()
            } else {
                b_started.clone()
            };
            let finished = if is_a {
                finish_a.clone()
            } else {
                finish_b.clone()
            };
            let cleanup = cleanups.clone();
            tasks.push(tokio::spawn(super::run_prepared_queue_worker_loop(
                super::WorkerLoopResources {
                    consumer,
                    lifecycle_stream: js.get_stream_no_info("JOBS").await.unwrap(),
                    queue: binding,
                    manager: manager.clone(),
                    cancellation: cancellation.clone(),
                    cancellation_registry: Default::default(),
                    key_coordinator: Some(coordinator.as_ref().clone()),
                    _lease: None,
                    retire: None,
                    single_receive: false,
                },
                move |active| {
                    let entered = entered.clone();
                    let finished = finished.clone();
                    let cleanup = cleanup.clone();
                    async move {
                        if active.cancellation_token().reason()
                            == Some(super::JobCancellationReason::StaleAttempt)
                        {
                            if cleanup.fetch_add(1, Ordering::SeqCst) == 0 {
                                return Err(
                                    crate::jobs::manager::JobProcessError::<String>::retryable(
                                        "cleanup interrupted".into(),
                                    ),
                                );
                            }
                        } else {
                            entered.notify_one();
                            finished.notified().await;
                        }
                        Ok(serde_json::json!({"key":"shared"}))
                    }
                },
            )));
            tokio::time::timeout(
                Duration::from_secs(5),
                if is_a {
                    a_started.notified()
                } else {
                    b_started.notified()
                },
            )
            .await
            .unwrap();
        }
        let taken: crate::jobs::keys::JobKeyState =
            serde_json::from_slice(&kv.entry(key.clone()).await.unwrap().unwrap().value).unwrap();
        let b_token = taken.active[0].slot_token.clone();
        finish_a.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event =
                    super::latest_lifecycle_message(&lifecycle, &format!("jobs.work.{}.>", a.id))
                        .await
                        .unwrap()
                        .unwrap();
                let event: crate::jobs::JobEvent = serde_json::from_slice(&event.payload).unwrap();
                if event.event_type == crate::jobs::JobEventType::StaleCompletionIgnored {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let unchanged: crate::jobs::keys::JobKeyState =
            serde_json::from_slice(&kv.entry(key).await.unwrap().unwrap().value).unwrap();
        assert_eq!(unchanged.active[0].slot_token, b_token);
        finish_b.notify_one();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let event =
                    super::latest_lifecycle_message(&lifecycle, &format!("jobs.work.{}.>", a.id))
                        .await
                        .unwrap()
                        .unwrap();
                let event: crate::jobs::JobEvent = serde_json::from_slice(&event.payload).unwrap();
                if event.event_type == crate::jobs::JobEventType::Stale
                    && consumers[0].info().await.unwrap().num_ack_pending == 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(cleanups.load(Ordering::SeqCst), 2);
        cancellation.cancel_for_shutdown();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        nats.stop().unwrap();
    }

    #[tokio::test]
    async fn retired_pending_pull_finishes_one_delivery_without_buffering_backlog() {
        use std::sync::Arc;

        use crate::jobs::bindings::JobsBinding;
        use crate::jobs::{TrellisJobEventPublisher, TrellisJobMetaSource};
        use async_nats::jetstream::{consumer, stream};

        let source = tempfile::tempdir().unwrap();
        trellis_bootstrap::generate_nats_bootstrap(&trellis_bootstrap::NatsBootstrapOptions::new(
            source.path(),
        ))
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut nats = trellis_local_nats::LocalNats::builder()
            .binary(trellis_local_nats::NatsBinarySource::DownloadPinned)
            .cache_dir(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/trellis-test-cache"),
            )
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
        let lifecycle_stream = jetstream
            .create_stream(stream::Config {
                name: "JOBS".into(),
                subjects: vec!["jobs.>".into()],
                allow_direct: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut consumer = lifecycle_stream
            .create_consumer(consumer::pull::Config {
                durable_name: Some("work".into()),
                filter_subject: "jobs.work.run".into(),
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await
            .unwrap();
        let queue = JobsQueueBinding {
            queue_type: "document-process".into(),
            publish_prefix: "jobs.work".into(),
            updates_prefix: None,
            work_subject: "jobs.work.run".into(),
            consumer_name: "work".into(),
            max_deliver: 3,
            backoff_ms: vec![],
            ack_wait_ms: 30_000,
            default_deadline_ms: None,
            update: None,
            key_concurrency: None,
            queue: None,
        };
        let manager = crate::jobs::manager::JobManager::new(
            TrellisJobEventPublisher::new(client),
            JobsBinding {
                service_name: "documents".into(),
                namespace: "documents".into(),
                queues: [(queue.queue_type.clone(), queue.clone())].into(),
            },
            TrellisJobMetaSource,
        );
        let (retire, retire_rx) = tokio::sync::watch::channel(false);
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let worker = tokio::spawn(super::run_prepared_queue_worker_loop(
            super::WorkerLoopResources {
                consumer: consumer.clone(),
                lifecycle_stream: jetstream.get_stream_no_info("JOBS").await.unwrap(),
                queue: queue.clone(),
                manager: manager.clone(),
                cancellation: JobCancellationToken::new(),
                cancellation_registry: Default::default(),
                key_coordinator: None,
                _lease: None,
                retire: Some(retire_rx),
                single_receive: false,
            },
            {
                let started = Arc::clone(&started);
                let release = Arc::clone(&release);
                move |_job| {
                    let started = Arc::clone(&started);
                    let release = Arc::clone(&release);
                    async move {
                        started.notify_one();
                        release.notified().await;
                        Ok::<_, crate::jobs::manager::JobProcessError<String>>(
                            serde_json::json!({"ok": true}),
                        )
                    }
                }
            },
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while consumer.info().await.unwrap().num_waiting != 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        retire.send(true).unwrap();
        for id in ["received", "queued-1", "queued-2"] {
            let mut job = sample_job(JobState::Pending, 0);
            job.id = id.into();
            let event = created(
                EventMeta {
                    service: &job.service,
                    job_type: &job.job_type,
                    job_id: &job.id,
                    context: &job.context,
                    timestamp: &job.created_at,
                },
                job.payload,
                job.max_tries,
                None,
            );
            jetstream
                .publish("jobs.work.run", serde_json::to_vec(&event).unwrap().into())
                .await
                .unwrap()
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .expect("a pre-retirement pull must still process its delivery");
        let info = consumer.info().await.unwrap();
        assert_eq!(
            info.num_ack_pending, 1,
            "only the held delivery is in flight"
        );
        assert_eq!(info.num_pending, 2, "backlog must remain on the broker");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while consumer.info().await.unwrap().num_ack_pending != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(consumer.info().await.unwrap().num_pending, 2);
        // A persistent slot must return to source selection after each final
        // disposition, even when its source has not retired. Exercise backlog
        // and then idle expiry without starting another worker population.
        for pending in [1, 0, 0] {
            tokio::time::timeout(
                Duration::from_secs(5),
                super::run_prepared_queue_worker_loop(
                    super::WorkerLoopResources {
                        consumer: consumer.clone(),
                        lifecycle_stream: jetstream.get_stream_no_info("JOBS").await.unwrap(),
                        queue: queue.clone(),
                        manager: manager.clone(),
                        cancellation: JobCancellationToken::new(),
                        cancellation_registry: Default::default(),
                        key_coordinator: None,
                        _lease: None,
                        retire: None,
                        single_receive: true,
                    },
                    |_job| async {
                        Ok::<_, crate::jobs::manager::JobProcessError<String>>(
                            serde_json::json!({"ok": true}),
                        )
                    },
                ),
            )
            .await
            .expect("one reservation must finish after delivery or bounded idle expiry")
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let info = consumer.info().await.unwrap();
                    if info.num_ack_pending == 0 && info.num_pending == pending {
                        assert_eq!(info.num_waiting, 0, "a completed slot cannot leave a pull");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        nats.stop().unwrap();
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
                &[5_000],
            ),
            WorkerAckAction::Nak(Duration::from_secs(5))
        );
    }

    #[test]
    fn final_retry_is_redelivered_for_execution_owned_reconciliation() {
        assert_eq!(
            ack_action_for_outcome(
                Some(&JobProcessOutcome::<Value>::Retry {
                    tries: 2,
                    error: "retry requested".to_string(),
                }),
                &[5_000],
            ),
            WorkerAckAction::Nak(Duration::from_secs(5))
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
