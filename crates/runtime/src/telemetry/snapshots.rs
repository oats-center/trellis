//! Read-only telemetry snapshot samplers for platform components.
//!
//! Samplers poll existing authoritative state on a fixed interval and publish
//! immutable numeric snapshots to observable gauges. They never mutate
//! business state, never hold a lease, and never run exporter work inside a
//! gauge callback. A failed poll keeps the previous values and advances only
//! the error counter; genuine zeros are published as zeros.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use trellis_rs::telemetry::instruments::{self, ObservableFamily};
use trellis_rs::telemetry::KeyValue;

use crate::events::ConsumerTelemetryReader;
use crate::platform::auth::{
    AuthTelemetrySnapshot, AuthorizationStateError, SqliteAuthorizationStore,
};
use crate::shutdown::StopHandle;
use trellis_events_runtime::storage::EventsStore;
use trellis_jobs_runtime::storage::SqliteJobsStore;
use trellis_rs::service::EventsRuntime;

/// Snapshot poll interval.
const SAMPLER_INTERVAL: Duration = Duration::from_secs(15);
/// Total deadline for one snapshot read.
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(2);
/// Maximum concurrent telemetry snapshot reads per process.
const SNAPSHOT_READ_LIMIT: usize = 2;

/// Latest published values for one snapshot source.
#[derive(Default)]
struct SnapshotValues {
    observed_at: f64,
    gauges: Vec<(ObservableFamily, f64, Vec<KeyValue>)>,
}

/// One registered snapshot source with its gauge callbacks.
///
/// Dropping the source removes its registrations so a stopped sampler cannot
/// keep exporting its last values.
struct SnapshotSource {
    values: Arc<Mutex<SnapshotValues>>,
    _registrations: Vec<instruments::ObservationRegistration>,
}

impl SnapshotSource {
    /// Registers the observed-time gauge plus the named gauge families.
    ///
    /// `source` is `None` for sources outside the snapshot freshness contract
    /// (for example supervisor component state).
    fn register(source: Option<&'static str>, families: &[ObservableFamily]) -> Self {
        let values = Arc::new(Mutex::new(SnapshotValues::default()));
        let mut registrations = Vec::new();
        if let Some(source) = source {
            let observed = Arc::clone(&values);
            registrations.push(instruments::register_observable(
                ObservableFamily::SnapshotObservedTime,
                Arc::new(move || {
                    let Ok(guard) = observed.lock() else {
                        return Vec::new();
                    };
                    vec![(
                        guard.observed_at,
                        vec![KeyValue::new("trellis.source", source)],
                    )]
                }),
            ));
        }
        for family in families {
            let values = Arc::clone(&values);
            let family = *family;
            registrations.push(instruments::register_observable(
                family,
                Arc::new(move || {
                    let Ok(guard) = values.lock() else {
                        return Vec::new();
                    };
                    guard
                        .gauges
                        .iter()
                        .filter(|(registered, _, _)| *registered == family)
                        .map(|(_, value, attributes)| (*value, attributes.clone()))
                        .collect()
                }),
            ));
        }
        Self {
            values,
            _registrations: registrations,
        }
    }

    /// Replaces the gauge set and marks the snapshot observed.
    ///
    /// Callers publish only after every read assigned to this source succeeded,
    /// so a failed or partial poll never looks like fresh healthy data.
    fn publish(&self, gauges: Vec<(ObservableFamily, f64, Vec<KeyValue>)>) {
        let Ok(mut values) = self.values.lock() else {
            return;
        };
        values.gauges = gauges;
        values.observed_at = unix_seconds();
    }

    /// Records one failed poll: the previous complete values and observation
    /// time are retained, and only the bounded error counter advances.
    fn record_failure(&self, source: &'static str, reason: &'static str) {
        record_failure(source, reason);
    }
}

/// Sampler-private progress state for one broker consumer.
struct ConsumerProgress {
    /// Broker-created timestamp; a recreation resets the progress clock.
    generation: i64,
    /// Last observed ack-floor stream sequence.
    ack_floor: u64,
    /// Monotonic instant of the last observed progress.
    last_progress: Instant,
}

/// Updates one consumer's progress memory and returns its no-progress age.
///
/// Outstanding work includes pending plus ACK-pending, an idle consumer has a
/// zero progress age, actual ack-floor movement resets the clock, and a
/// recreated consumer (new broker generation) restarts from now.
fn observe_progress(
    progress: &mut HashMap<(String, String), ConsumerProgress>,
    key: (String, String),
    info: &async_nats::jetstream::consumer::Info,
    outstanding: u64,
    now: Instant,
) -> f64 {
    let generation = info.created.unix_timestamp_nanos() as i64;
    let floor = info.ack_floor.stream_sequence;
    let entry = progress.entry(key).or_insert(ConsumerProgress {
        generation,
        ack_floor: floor,
        last_progress: now,
    });
    if entry.generation != generation {
        entry.generation = generation;
        entry.ack_floor = floor;
        entry.last_progress = now;
    } else if outstanding == 0 || floor != entry.ack_floor {
        entry.ack_floor = floor;
        entry.last_progress = now;
    }
    if outstanding == 0 {
        0.0
    } else {
        (now - entry.last_progress).as_secs_f64()
    }
}

/// Owns the telemetry sampler tasks for one subsystem.
///
/// Samplers run as their own tasks and are never a branch of a business-loop
/// `select!`: a disabled, completed, or failed sampler must not terminate the
/// subsystem's real work. Dropping or stopping this owner aborts and joins its
/// tasks within the telemetry bounds.
pub(crate) struct SamplerOwner {
    joins: Vec<tokio::task::JoinHandle<()>>,
}

impl SamplerOwner {
    /// Starts the samplers whose futures are already created.
    ///
    /// A telemetry-disabled process starts no sampler work at all.
    pub(crate) fn start(
        samplers: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>,
    ) -> Self {
        if !metrics_enabled() {
            return Self { joins: Vec::new() };
        }
        Self {
            joins: samplers
                .into_iter()
                .map(|sampler| tokio::spawn(sampler))
                .collect(),
        }
    }

    /// Stops and joins every sampler task within the telemetry bounds.
    pub(crate) async fn stop(mut self) {
        let joins = std::mem::take(&mut self.joins);
        for join in &joins {
            join.abort();
        }
        for join in joins {
            let _ = join.await;
        }
    }
}

impl Drop for SamplerOwner {
    fn drop(&mut self) {
        // Abort-only fallback for cancellation/unwind paths: no blocking join,
        // and a dropped async sampler never releases a permit still held by its
        // own blocking SQL work.
        for join in self.joins.drain(..) {
            join.abort();
        }
    }
}

/// Current wall-clock time in Unix seconds.
fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

/// Records one failed snapshot attempt with a bounded reason.
fn record_failure(source: &'static str, reason: &'static str) {
    instruments::add_counter(
        instruments::CounterFamily::SnapshotErrors,
        1,
        &[
            KeyValue::new("trellis.source", source),
            KeyValue::new("trellis.reason", reason),
        ],
    );
}

/// Runs the Jobs snapshot sampler until the runtime stops.
pub(crate) async fn run_jobs_sampler(
    store: SqliteJobsStore,
    nats: async_nats::Client,
    jobs_stream: String,
    stop: StopHandle,
) {
    if !metrics_enabled() {
        return;
    }

    let source = SnapshotSource::register(
        Some("jobs"),
        &[
            ObservableFamily::JobsReady,
            ObservableFamily::JobsOldestReadyAge,
            ObservableFamily::JobsDead,
            ObservableFamily::JobsWaitingRetry,
            ObservableFamily::JobsWorkerRegistrations,
            ObservableFamily::ProjectionPending,
            ObservableFamily::ProjectionProgressAge,
        ],
    );
    // Owner-established identities, read once from the live owners rather than
    // rediscovered by a mutating getter in a poll.
    let Some(jobs_projection_id) = established_projection_id(&store) else {
        source.record_failure("jobs", "invalid");
        return;
    };
    let jobs_consumer = trellis_jobs_runtime::projector_consumer_name(&jobs_projection_id);
    let mut projection_progress = std::collections::HashMap::new();
    // At most one outstanding read per source; a late result is discarded.
    let mut outstanding: Option<
        OutstandingRead<
            Result<
                trellis_jobs_runtime::telemetry::JobsTelemetrySnapshot,
                trellis_jobs_runtime::storage::SqliteJobsStoreError,
            >,
        >,
    > = None;
    loop {
        if let Some(read) = outstanding.as_ref() {
            if !read.is_finished() {
                tokio::select! {
                    _ = tokio::time::sleep(SAMPLER_INTERVAL) => continue,
                    _ = stop.stopped() => return,
                }
            }
        }
        if let Some(mut read) = outstanding.take() {
            let poll = SourcePoll::start();
            let _ = read_within_poll(&poll, &mut read).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(SAMPLER_INTERVAL) => {}
            _ = stop.stopped() => return,
        }
        // One deadline for the whole source read: SQL, projection inventory,
        // and every broker page.
        let poll = SourcePoll::start();
        let now = time::OffsetDateTime::now_utc();
        let now_nanos = now.unix_timestamp_nanos() as i64;
        let snapshot_store = store.clone();
        let Some(mut read) = start_bounded_read(&poll, move || {
            snapshot_store.telemetry_snapshot(now_nanos, now)
        })
        .await
        else {
            source.record_failure("jobs", "timeout");
            continue;
        };
        let snapshot = match read_within_poll(&poll, &mut read).await {
            Some(Ok(snapshot)) => snapshot,
            Some(Err(_)) => {
                source.record_failure("jobs", "io");
                continue;
            }
            None => {
                outstanding = Some(read);
                source.record_failure("jobs", "timeout");
                continue;
            }
        };
        let mut gauges = vec![
            (
                ObservableFamily::JobsReady,
                snapshot.ready as f64,
                Vec::new(),
            ),
            (
                ObservableFamily::JobsOldestReadyAge,
                snapshot.oldest_ready_age_seconds,
                Vec::new(),
            ),
            (ObservableFamily::JobsDead, snapshot.dead as f64, Vec::new()),
            (
                ObservableFamily::JobsWaitingRetry,
                snapshot.waiting_retry as f64,
                Vec::new(),
            ),
            (
                ObservableFamily::JobsWorkerRegistrations,
                snapshot.worker_registrations as f64,
                Vec::new(),
            ),
        ];
        // The projection inventory is part of the same source poll.
        let Some(projection) = poll
            .within(projection_consumer_gauges(
                &nats,
                &jobs_stream,
                &jobs_consumer,
                "jobs",
                &mut projection_progress,
            ))
            .await
        else {
            source.record_failure("jobs", "timeout");
            continue;
        };
        let projection_gauges = match projection {
            Ok(gauges) => gauges,
            Err(()) => {
                source.record_failure("jobs", "io");
                continue;
            }
        };
        if stop.is_stopped() {
            return;
        }
        gauges.extend(projection_gauges);
        source.publish(gauges);
    }
}

/// Runs the Events dead-letter snapshot sampler until the runtime stops.
pub(crate) async fn run_events_sampler(store: EventsStore, stop: StopHandle) {
    if !metrics_enabled() {
        return;
    }

    let source = SnapshotSource::register(Some("events_dlq"), &[ObservableFamily::DeadLetters]);
    // At most one outstanding read per source; a late result is discarded.
    let mut outstanding: Option<
        OutstandingRead<
            Result<
                trellis_events_runtime::telemetry::DlqTelemetrySnapshot,
                trellis_events_runtime::storage::EventsStoreError,
            >,
        >,
    > = None;
    loop {
        // While a read is still running, the source starts no replacement poll.
        if let Some(read) = outstanding.as_ref() {
            if !read.is_finished() {
                tokio::select! {
                    _ = tokio::time::sleep(SAMPLER_INTERVAL) => continue,
                    _ = stop.stopped() => return,
                }
            }
        }
        if let Some(mut read) = outstanding.take() {
            let poll = SourcePoll::start();
            let _ = read_within_poll(&poll, &mut read).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(SAMPLER_INTERVAL) => {}
            _ = stop.stopped() => return,
        }
        let poll = SourcePoll::start();
        let read_store = store.clone();
        let Some(mut read) =
            start_bounded_read(&poll, move || read_store.telemetry_snapshot()).await
        else {
            source.record_failure("events_dlq", "timeout");
            continue;
        };
        let result = read_within_poll(&poll, &mut read).await;
        // Stop before publication suppresses any part of this poll.
        if stop.is_stopped() {
            return;
        }
        match result {
            Some(Ok(snapshot)) => source.publish(vec![
                (
                    ObservableFamily::DeadLetters,
                    snapshot.open as f64,
                    vec![KeyValue::new("trellis.state", "open")],
                ),
                (
                    ObservableFamily::DeadLetters,
                    snapshot.replay_pending as f64,
                    vec![KeyValue::new("trellis.state", "replay_pending")],
                ),
                (
                    ObservableFamily::DeadLetters,
                    snapshot.replaying as f64,
                    vec![KeyValue::new("trellis.state", "replaying")],
                ),
            ]),
            Some(Err(_)) => source.record_failure("events_dlq", "io"),
            None => {
                // The read outlived the deadline: retain it and publish nothing.
                outstanding = Some(read);
                source.record_failure("events_dlq", "timeout");
            }
        }
    }
}

/// Publishes selected component readiness and observation time.
///
/// Readiness is written by the supervisor from real lifecycle transitions:
/// a component is ready after its subsystem started and before it stops or
/// fails. The timer only exports the stored state and never manufactures
/// health from its own tick.
pub(crate) fn spawn_component_sampler(
    components: Vec<&'static str>,
    ready: Arc<ComponentReadiness>,
    stop: StopHandle,
) -> tokio::task::JoinHandle<()> {
    let source = SnapshotSource::register(
        None,
        &[
            ObservableFamily::ComponentReady,
            ObservableFamily::ComponentObservedTime,
        ],
    );
    tokio::spawn(async move {
        loop {
            let now = unix_seconds();
            let mut gauges = Vec::new();
            for component in &components {
                gauges.push((
                    ObservableFamily::ComponentReady,
                    ready.current(component),
                    vec![KeyValue::new("trellis.component", *component)],
                ));
                gauges.push((
                    ObservableFamily::ComponentObservedTime,
                    now,
                    vec![KeyValue::new("trellis.component", *component)],
                ));
            }
            source.publish(gauges);
            tokio::select! {
                _ = tokio::time::sleep(SAMPLER_INTERVAL) => {}
                _ = stop.stopped() => return,
            }
        }
    })
}

/// Per-component readiness written by the supervisor lifecycle.
#[derive(Default)]
pub(crate) struct ComponentReadiness {
    states: std::sync::Mutex<std::collections::BTreeMap<&'static str, f64>>,
}

impl ComponentReadiness {
    /// Records one component transition: 1 when eligible to serve, else 0.
    pub(crate) fn set(&self, component: &'static str, ready: f64) {
        if let Ok(mut states) = self.states.lock() {
            states.insert(component, ready);
        }
    }

    /// Current stored readiness for one component.
    fn current(&self, component: &str) -> f64 {
        self.states
            .lock()
            .ok()
            .and_then(|states| states.get(component).copied())
            .unwrap_or(0.0)
    }
}

/// Runs the declared-Consumer inventory sampler until the runtime stops.
pub(crate) async fn run_consumer_sampler(
    runtime: EventsRuntime,
    reader: ConsumerTelemetryReader,
    store: EventsStore,
    stop: StopHandle,
) {
    if !metrics_enabled() {
        return;
    }

    let source = SnapshotSource::register(
        Some("events"),
        &[
            ObservableFamily::ConsumerPending,
            ObservableFamily::ConsumerAckPending,
            ObservableFamily::ConsumerMissing,
            ObservableFamily::ConsumerProgressAge,
            ObservableFamily::ProjectionPending,
            ObservableFamily::ProjectionProgressAge,
        ],
    );
    // Exact current projector identities from the live owner. A failure here is
    // not a configured absence: the sampler records it and publishes nothing.
    let Ok(projection_id) = store.projection_id() else {
        source.record_failure("events", "invalid");
        return;
    };
    let events_consumer = trellis_events_runtime::projector_consumer_name(&projection_id);
    let dlq_consumer =
        trellis_events_runtime::dead_letters::dead_letters_projector_consumer_name(&projection_id);
    // Sampler-private liveness memory; no metric label carries a consumer name.
    // The stored generation is the broker-created timestamp, so a recreated
    // consumer restarts its progress clock instead of inheriting the old one.
    let mut progress: HashMap<(String, String), ConsumerProgress> = HashMap::new();
    let mut outstanding: Option<
        OutstandingRead<Result<Vec<trellis_events_runtime::ConsumerBinding>, String>>,
    > = None;
    loop {
        if let Some(read) = outstanding.as_ref() {
            if !read.is_finished() {
                tokio::select! {
                    _ = tokio::time::sleep(SAMPLER_INTERVAL) => continue,
                    _ = stop.stopped() => return,
                }
            }
        }
        if let Some(mut read) = outstanding.take() {
            let poll = SourcePoll::start();
            let _ = read_within_poll(&poll, &mut read).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(SAMPLER_INTERVAL) => {}
            _ = stop.stopped() => return,
        }
        // One deadline for the whole source poll: broker inventory, DLQ
        // inventory, and SQLite inventory share it.
        let poll = SourcePoll::start();
        // Every read assigned to this source must succeed before the snapshot is
        // published with a fresh timestamp. A failed read retains the previous
        // complete values and only advances the bounded error counter.
        let Some(live_result) = poll.within(runtime.consumers()).await else {
            source.record_failure("events", "timeout");
            continue;
        };
        let live = match live_result {
            Ok(live) => live,
            Err(_) => {
                source.record_failure("events", "io");
                continue;
            }
        };
        let Some(dlq_result) = poll.within(runtime.dlq_consumers()).await else {
            source.record_failure("events", "timeout");
            continue;
        };
        let dlq_live = match dlq_result {
            Ok(live) => live,
            Err(_) => {
                source.record_failure("events", "io");
                continue;
            }
        };
        let read_reader = reader.clone();
        let Some(mut read) = start_bounded_read(&poll, move || read_reader.read()).await else {
            source.record_failure("events", "timeout");
            continue;
        };
        let bindings_result = read_within_poll(&poll, &mut read).await;
        let Some(bindings_result) = bindings_result else {
            outstanding = Some(read);
            source.record_failure("events", "timeout");
            continue;
        };
        let bindings = match bindings_result {
            Ok(bindings) => bindings,
            Err(_) => {
                source.record_failure("events", "invalid");
                continue;
            }
        };
        let now = Instant::now();
        let mut pending = 0u64;
        let mut ack_pending = 0u64;
        let mut missing = 0u64;
        let mut max_progress_age = 0.0f64;
        let mut projection_outstanding = 0u64;
        let mut max_projection_age = 0.0f64;
        let mut dlq_outstanding = 0u64;
        let mut max_dlq_age = 0.0f64;
        let mut seen: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();

        // Only the exact current Events projector is a projection source. A
        // missing required projector fails this source's snapshot instead of
        // reporting fresh zero backlog.
        let mut events_projection_seen = false;
        if let Some(info) = live.iter().find(|info| info.name == events_consumer) {
            events_projection_seen = true;
            let key = (info.stream_name.clone(), info.name.clone());
            seen.insert(key.clone());
            let outstanding = info.num_pending + info.num_ack_pending as u64;
            projection_outstanding += outstanding;
            max_projection_age = max_projection_age.max(observe_progress(
                &mut progress,
                key,
                info,
                outstanding,
                now,
            ));
        }

        let mut dlq_seen = false;
        if let Some(info) = dlq_live.iter().find(|info| info.name == dlq_consumer) {
            dlq_seen = true;
            let key = (info.stream_name.clone(), info.name.clone());
            seen.insert(key.clone());
            let outstanding = info.num_pending + info.num_ack_pending as u64;
            dlq_outstanding += outstanding;
            max_dlq_age =
                max_dlq_age.max(observe_progress(&mut progress, key, info, outstanding, now));
        }

        for binding in &bindings {
            let Some(info) = live.iter().find(|info| {
                info.stream_name == binding.stream && info.name == binding.consumer_name
            }) else {
                missing += 1;
                continue;
            };
            let key = (info.stream_name.clone(), info.name.clone());
            seen.insert(key.clone());
            pending += info.num_pending;
            ack_pending += info.num_ack_pending as u64;
            let outstanding = info.num_pending + info.num_ack_pending as u64;
            max_progress_age =
                max_progress_age.max(observe_progress(&mut progress, key, info, outstanding, now));
        }

        // Prune memory for consumers that no longer exist so a later recreation
        // cannot inherit stale progress.
        progress.retain(|key, _| seen.contains(key));

        // A missing required projector is not an idle projector: the source
        // keeps its previous values and old observation time.
        if !events_projection_seen {
            source.record_failure("events", "invalid");
            continue;
        }
        if !dlq_seen {
            source.record_failure("events_dlq", "invalid");
            continue;
        }

        source.publish(vec![
            (
                ObservableFamily::ConsumerPending,
                pending as f64,
                Vec::new(),
            ),
            (
                ObservableFamily::ConsumerAckPending,
                ack_pending as f64,
                Vec::new(),
            ),
            (
                ObservableFamily::ConsumerMissing,
                missing as f64,
                Vec::new(),
            ),
            (
                ObservableFamily::ConsumerProgressAge,
                max_progress_age,
                Vec::new(),
            ),
            (
                ObservableFamily::ProjectionPending,
                projection_outstanding as f64,
                vec![KeyValue::new("trellis.component", "events")],
            ),
            (
                ObservableFamily::ProjectionProgressAge,
                max_projection_age,
                vec![KeyValue::new("trellis.component", "events")],
            ),
            (
                ObservableFamily::ProjectionPending,
                dlq_outstanding as f64,
                vec![KeyValue::new("trellis.component", "events_dlq")],
            ),
            (
                ObservableFamily::ProjectionProgressAge,
                max_dlq_age,
                vec![KeyValue::new("trellis.component", "events_dlq")],
            ),
        ]);
    }
}

/// Runs the platform Auth snapshot sampler until the runtime stops.
pub(crate) async fn run_auth_sampler(
    store: SqliteAuthorizationStore,
    stop: StopHandle,
) -> Result<(), AuthorizationStateError> {
    if !metrics_enabled() {
        return Ok(());
    }

    let source = SnapshotSource::register(
        Some("auth"),
        &[
            ObservableFamily::AuthPostCommitPending,
            ObservableFamily::AuthPostCommitOldestAge,
            ObservableFamily::ResourceBindings,
        ],
    );
    // At most one outstanding read per source; a late result is discarded.
    let mut outstanding: Option<
        OutstandingRead<Result<AuthTelemetrySnapshot, AuthorizationStateError>>,
    > = None;
    loop {
        if let Some(read) = outstanding.as_ref() {
            if !read.is_finished() {
                tokio::select! {
                    _ = tokio::time::sleep(SAMPLER_INTERVAL) => continue,
                    _ = stop.stopped() => return Ok(()),
                }
            }
        }
        if let Some(mut read) = outstanding.take() {
            let poll = SourcePoll::start();
            let _ = read_within_poll(&poll, &mut read).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(SAMPLER_INTERVAL) => {}
            _ = stop.stopped() => return Ok(()),
        }
        let poll = SourcePoll::start();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as i64);
        let Some(mut read) = store.start_telemetry_snapshot(&poll, now_ms).await else {
            source.record_failure("auth", "timeout");
            continue;
        };
        let result = read_within_poll(&poll, &mut read).await;
        if stop.is_stopped() {
            return Ok(());
        }
        match result {
            Some(Ok(snapshot)) => {
                let mut gauges = vec![
                    (
                        ObservableFamily::AuthPostCommitPending,
                        snapshot.post_commit_pending as f64,
                        Vec::new(),
                    ),
                    (
                        ObservableFamily::AuthPostCommitOldestAge,
                        snapshot.post_commit_oldest_age_seconds,
                        Vec::new(),
                    ),
                ];
                for (kind, state, count) in &snapshot.resources {
                    gauges.push((
                        ObservableFamily::ResourceBindings,
                        *count as f64,
                        vec![
                            KeyValue::new("trellis.kind", kind.clone()),
                            KeyValue::new("trellis.state", *state),
                        ],
                    ));
                }
                source.publish(gauges);
            }
            Some(Err(_)) => {
                source.record_failure("auth", "io");
            }
            None => {
                outstanding = Some(read);
                source.record_failure("auth", "timeout");
            }
        }
    }
}

pub(crate) async fn run_health_sampler(
    nats: async_nats::Client,
    stream_name: String,
    projection_id: String,
    stop: StopHandle,
) {
    if !metrics_enabled() {
        return;
    }

    let source = SnapshotSource::register(
        Some("health"),
        &[
            ObservableFamily::ProjectionPending,
            ObservableFamily::ProjectionProgressAge,
        ],
    );
    let mut progress = std::collections::HashMap::new();
    // Exact current projector identity from the live owner; no re-resolution.
    let consumer_name = crate::telemetry::snapshots::health_projector_consumer_name(&projection_id);
    loop {
        tokio::select! {
            _ = tokio::time::sleep(SAMPLER_INTERVAL) => {}
            _ = stop.stopped() => return,
        }
        // One deadline for the whole health projection poll.
        let poll = SourcePoll::start();
        let polled = poll
            .within(projection_consumer_gauges(
                &nats,
                &stream_name,
                &consumer_name,
                "health",
                &mut progress,
            ))
            .await;
        match polled {
            Some(Ok(gauges)) => {
                if stop.is_stopped() {
                    return;
                }
                source.publish(gauges)
            }
            Some(Err(())) => source.record_failure("health", "io"),
            None => source.record_failure("health", "timeout"),
        }
    }
}

/// Exact durable consumer name for one Health projection identity.
fn health_projector_consumer_name(projection_id: &str) -> String {
    format!("health-projector-{}", projection_id).to_lowercase()
}

/// Samples one internal projection consumer from a live broker consumer list.
///
/// Returns the catalog projection gauges for the component, using the same
/// ack-floor progress tracking as the application-consumer sampler. A broker
/// read failure yields no gauges, leaving the previous sample visible.
async fn projection_consumer_gauges(
    nats: &async_nats::Client,
    stream_name: &str,
    consumer_name: &str,
    component: &'static str,
    progress: &mut std::collections::HashMap<(String, String), ConsumerProgress>,
) -> Result<Vec<(ObservableFamily, f64, Vec<KeyValue>)>, ()> {
    // The exact current projector identity is required: a missing required
    // consumer is not an idle current consumer and must not publish a fresh
    // zero. Historical prefixed identities are never enumerated.
    let info = async_nats::jetstream::new(nats.clone())
        .get_stream(stream_name)
        .await
        .map_err(|_| ())?
        .consumer_info(consumer_name)
        .await
        .map_err(|_| ())?;
    let now = Instant::now();
    let outstanding = info.num_pending + info.num_ack_pending as u64;
    let key = (info.stream_name.clone(), info.name.clone());
    let max_age = observe_progress(progress, key, &info, outstanding, now);
    Ok(vec![
        (
            ObservableFamily::ProjectionPending,
            outstanding as f64,
            vec![KeyValue::new("trellis.component", component)],
        ),
        (
            ObservableFamily::ProjectionProgressAge,
            max_age,
            vec![KeyValue::new("trellis.component", component)],
        ),
    ])
}

/// Whether process telemetry metrics are enabled for sampler polling.
///
/// Disabled telemetry performs no sampler database or broker work.
fn metrics_enabled() -> bool {
    trellis_rs::telemetry::process_metrics_enabled()
}

/// One source poll with a single monotonic deadline.
///
/// The deadline covers telemetry-capacity wait, store/pool wait, SQL execution,
/// resolver work, and every broker read for that source. Suboperations must not
/// restart it.
pub(crate) struct SourcePoll {
    deadline: tokio::time::Instant,
}

impl SourcePoll {
    /// Starts one poll whose whole read must finish within the source deadline.
    pub(crate) fn start() -> Self {
        Self {
            deadline: tokio::time::Instant::now() + SNAPSHOT_DEADLINE,
        }
    }

    /// Remaining time before this poll's deadline.
    fn remaining(&self) -> Option<Duration> {
        self.deadline
            .checked_duration_since(tokio::time::Instant::now())
    }

    /// Runs one suboperation under the remaining poll budget.
    pub(crate) async fn within<T>(
        &self,
        future: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let remaining = self.remaining()?;
        tokio::time::timeout(remaining, future).await.ok()
    }
}

/// One process-wide telemetry capacity permit.
///
/// The permit is moved into the blocking read and held until that work really
/// finishes, so a poll that times out cannot release capacity and let the next
/// poll multiply concurrent telemetry reads.
static READ_PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

/// Returns the process telemetry read-capacity semaphore.
fn read_permits() -> &'static Arc<tokio::sync::Semaphore> {
    READ_PERMITS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(SNAPSHOT_READ_LIMIT)))
}

/// In-flight blocking read for one source.
///
/// While a read is still running the source starts no replacement poll; when it
/// finishes late its result is discarded and never published.
pub(crate) struct OutstandingRead<T> {
    join: tokio::task::JoinHandle<T>,
}

impl<T> OutstandingRead<T> {
    /// Whether the blocking work has finished.
    pub(crate) fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
}

/// Starts one synchronous telemetry read as bounded blocking work under the
/// poll's remaining budget.
///
/// Returns `None` when capacity or the deadline is unavailable; in that case no
/// blocking work was started.
pub(crate) async fn start_bounded_read<T, F>(
    poll: &SourcePoll,
    read: F,
) -> Option<OutstandingRead<T>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let remaining = poll.remaining()?;
    let permit = match tokio::time::timeout(remaining, read_permits().clone().acquire_owned()).await
    {
        Ok(Ok(permit)) => permit,
        _ => return None,
    };
    let join = tokio::task::spawn_blocking(move || {
        let result = read();
        // Held until the blocking work really finishes.
        drop(permit);
        result
    });
    Some(OutstandingRead { join })
}

/// Awaits one in-flight read under the poll's remaining budget.
pub(crate) async fn read_within_poll<T>(
    poll: &SourcePoll,
    read: &mut OutstandingRead<T>,
) -> Option<T>
where
    T: Send + 'static,
{
    let remaining = poll.remaining()?;
    match tokio::time::timeout(remaining, &mut read.join).await {
        Ok(Ok(value)) => Some(value),
        // A timed-out join detaches nothing: the blocking work keeps its permit
        // and the source retains the handle until it actually completes.
        _ => None,
    }
}

/// Reads the already-established projection identity from the live owner.
///
/// Identity comes from the owner's existing state, never from a mutating
/// getter inside a poll.
fn established_projection_id(store: &SqliteJobsStore) -> Option<String> {
    store.established_projection_id().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteStore;
    use crate::{SqliteStorageConfig, SubsystemName};

    #[tokio::test]
    async fn late_inventory_read_keeps_observation_time_and_finishes_before_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("platform.sqlite");
        SqliteStore::new(
            SubsystemName::Platform,
            SqliteStorageConfig {
                path: path.clone(),
                journal_mode: Some("delete".to_owned()),
                busy_timeout_ms: Some(2_500),
                single_writer: Some(true),
            },
        )
        .migrate()
        .unwrap();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let reader = ConsumerTelemetryReader::new(path);
        let source = SnapshotSource::register(Some("events"), &[ObservableFamily::ConsumerMissing]);
        source.publish(vec![(ObservableFamily::ConsumerMissing, 3.0, Vec::new())]);
        let observed = source.values.lock().unwrap().observed_at;
        let poll = SourcePoll::start();
        let mut read = start_bounded_read(&poll, move || reader.read())
            .await
            .unwrap();
        assert!(read_within_poll(&poll, &mut read).await.is_none());
        source.record_failure("events", "timeout");
        assert_eq!(source.values.lock().unwrap().observed_at, observed);
        assert_eq!(source.values.lock().unwrap().gauges[0].1, 3.0);
        assert!(!read.is_finished());
        blocker.execute_batch("ROLLBACK").unwrap();
        assert!(read.join.await.unwrap().unwrap().is_empty());
    }

    #[tokio::test]
    async fn disabled_metrics_owner_starts_no_sampler_work() {
        // The default process has no claimed telemetry mode, so metrics are
        // disabled and the owner must not start sampler tasks.
        assert!(!metrics_enabled());
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&started);
        let owner = SamplerOwner::start(vec![Box::pin(async move {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })]);
        tokio::task::yield_now().await;
        assert_eq!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "disabled metrics must not start sampler work"
        );
        owner.stop().await;
    }

    #[tokio::test]
    async fn sampler_owner_stop_aborts_a_running_sampler_promptly() {
        // A sampler that never finishes must not keep the subsystem stop path
        // waiting; stopping aborts and joins it within the poll budget.
        let owner = SamplerOwner {
            joins: vec![tokio::spawn(async {
                std::future::pending::<()>().await;
            })],
        };
        let stopped = tokio::time::timeout(Duration::from_secs(1), owner.stop()).await;
        assert!(stopped.is_ok(), "sampler stop must return promptly");
    }
}
