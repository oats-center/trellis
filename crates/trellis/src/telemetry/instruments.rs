//! Bounded OpenTelemetry instruments for Trellis.
//!
//! Metric names, units, dimensions, and histogram boundaries are fixed here so
//! call sites cannot invent high-cardinality telemetry. Identifiers such as
//! principal IDs, participant IDs, deployment IDs, digests, subjects, and URLs
//! are never metric attributes. One instrument handle exists per meter/name.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram, UpDownCounter};
use opentelemetry::{global, KeyValue};

use super::INSTRUMENTATION_SCOPE;

/// Widest shared duration boundaries in seconds.
const DURATION_BOUNDARIES: [f64; 13] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Longer boundaries for attempt/execution/transfer/cli durations in seconds.
const LONG_DURATION_BOUNDARIES: [f64; 15] = [
    0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 900.0, 3600.0,
];

/// Maximum distinct route tokens per registration family.
pub const ROUTE_LIMIT_PER_FAMILY: usize = 128;
/// Maximum route token length in UTF-8 bytes.
pub const ROUTE_TOKEN_MAX_BYTES: usize = 128;

/// Accepted duration families owned by the runtime Auth loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthDurationMetric {
    /// Authorization HTTP flow boundaries.
    AuthFlow,
    /// NATS auth callout boundaries.
    AuthCallout,
    /// Contract compilation and compatibility analysis.
    ContractAnalysis,
}

/// Outcome dimension for the accepted Auth/contract duration families.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The measured operation succeeded.
    Ok,
    /// The measured operation returned an error.
    Error,
    /// The measured operation was denied by authorization.
    Denied,
    /// The measured operation was rejected by rate limiting.
    RateLimited,
}

impl Outcome {
    /// Stable catalog value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::RateLimited => "rate_limited",
        }
    }
}

/// Cache family for request counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheKind {
    /// Compiled installed-evidence graphs.
    Evidence,
    /// Selected-surface compatibility reports.
    Compatibility,
}

impl CacheKind {
    /// Stable catalog value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Compatibility => "compatibility",
        }
    }
}

/// Final lookup role for a cache request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheResult {
    /// A resident ready value was returned.
    Hit,
    /// An in-flight owner's completion was awaited.
    Wait,
    /// This caller became the computation owner.
    Miss,
}

impl CacheResult {
    /// Stable catalog value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Wait => "wait",
        }
    }
}

/// Duration families beyond the accepted Auth/contract families.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurationFamily {
    /// Public SDK connect through usable installation.
    Connect,
    /// One logical RPC client call including retries.
    RpcClient,
    /// One unary server RPC dispatch.
    RpcServer,
    /// One HTTP response through response-header construction.
    HttpServer,
    /// One authorization verification result.
    AuthVerification,
    /// One claimed Auth post-commit action execution.
    AuthPostCommit,
    /// One logical job submission.
    JobSubmission,
    /// One local job attempt.
    JobAttempt,
    /// One acquired operation execution lifetime.
    OperationExecution,
    /// Time from observing cancellation until the owned handler has finished cleanup.
    OperationCancellationCleanup,
    /// One durable event publication.
    EventPublish,
    /// One consumer delivery attempt.
    EventProcess,
    /// One projection apply/batch.
    Projection,
    /// One storage operation.
    Storage,
    /// One transfer session.
    Transfer,
    /// One CLI command.
    Cli,
    /// One built-in browser navigation milestone.
    BrowserNavigation,
    /// One authorization context refresh flow.
    AuthFlow,
    /// One authorization callout flow.
    AuthCallout,
    /// One contract analysis flow.
    ContractAnalysis,
    /// One live observation activation handshake.
    LiveHandshake,
}

impl DurationFamily {
    /// Catalog metric name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Connect => "trellis.connect.duration",
            Self::RpcClient => "trellis.rpc.client.duration",
            Self::RpcServer => "trellis.rpc.server.duration",
            Self::HttpServer => "trellis.http.server.duration",
            Self::AuthVerification => "trellis.auth.verification.duration",
            Self::AuthPostCommit => "trellis.auth.post_commit.duration",
            Self::JobSubmission => "trellis.job.submission.duration",
            Self::JobAttempt => "trellis.job.attempt.duration",
            Self::OperationExecution => "trellis.operation.execution.duration",
            Self::OperationCancellationCleanup => "trellis.operation.cancellation.cleanup.duration",
            Self::EventPublish => "trellis.event.publish.duration",
            Self::EventProcess => "trellis.event.process.duration",
            Self::Projection => "trellis.projection.duration",
            Self::Storage => "trellis.storage.duration",
            Self::Transfer => "trellis.transfer.duration",
            Self::Cli => "trellis.cli.duration",
            Self::BrowserNavigation => "trellis.browser.navigation.duration",
            Self::AuthFlow => "trellis.auth.flow.duration",
            Self::AuthCallout => "trellis.auth.callout.duration",
            Self::ContractAnalysis => "trellis.contract.analysis.duration",
            Self::LiveHandshake => "trellis.live.handshake.duration",
        }
    }

    /// Histogram boundaries in seconds for this family.
    fn boundaries(self) -> &'static [f64] {
        match self {
            Self::JobAttempt
            | Self::OperationExecution
            | Self::OperationCancellationCleanup
            | Self::EventProcess
            | Self::Transfer
            | Self::Cli
            | Self::LiveHandshake => &LONG_DURATION_BOUNDARIES,
            _ => &DURATION_BOUNDARIES,
        }
    }
}

/// Counter families in the catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterFamily {
    /// Actual RPC transport attempts.
    RpcClientAttempts,
    /// Completed connection state changes.
    ConnectionTransitions,
    /// Actual credential-bound refresh attempts.
    AuthRefreshAttempts,
    /// Completed keyed job lease actions.
    JobLeaseEvents,
    /// Observed operation ownership results.
    OperationOwnershipEvents,
    /// Observed final delivery disposition handoffs.
    DeliveryDispositions,
    /// Authoritative dead-letter transition operations.
    DeadLetterTransitions,
    /// Live observation session local terminal commits.
    LiveEnds,
    /// Admitted outgoing and verified incoming live session frames.
    LiveFrames,
    /// Rejected live session messages by fixed reason category.
    LiveRejections,
    /// Actual transmitted/received transfer payload bytes.
    TransferWireBytes,
    /// Existing singleton lease actions.
    RuntimeLeaseEvents,
    /// Failed telemetry snapshot polling attempts.
    SnapshotErrors,
    /// Route registrations that exceeded the bounded catalog.
    RouteOverflow,
}

impl CounterFamily {
    /// Catalog metric name.
    pub fn name(self) -> &'static str {
        match self {
            Self::RpcClientAttempts => "trellis.rpc.client.attempts",
            Self::ConnectionTransitions => "trellis.connection.transitions",
            Self::AuthRefreshAttempts => "trellis.auth.refresh.attempts",
            Self::JobLeaseEvents => "trellis.job.lease.events",
            Self::OperationOwnershipEvents => "trellis.operation.ownership.events",
            Self::DeliveryDispositions => "trellis.delivery.dispositions",
            Self::DeadLetterTransitions => "trellis.dlq.transitions",
            Self::LiveEnds => "trellis.live.ends",
            Self::LiveFrames => "trellis.live.frames",
            Self::LiveRejections => "trellis.live.rejections",
            Self::TransferWireBytes => "trellis.transfer.wire.bytes",
            Self::RuntimeLeaseEvents => "trellis.runtime.lease.events",
            Self::SnapshotErrors => "trellis.snapshot.errors",
            Self::RouteOverflow => "trellis.telemetry.route_overflow",
        }
    }

    /// Catalog unit.
    fn unit(self) -> &'static str {
        match self {
            Self::DeliveryDispositions => "{delivery}",
            Self::DeadLetterTransitions => "{transition}",
            Self::ConnectionTransitions => "{transition}",
            Self::JobLeaseEvents | Self::RuntimeLeaseEvents => "{event}",
            Self::OperationOwnershipEvents => "{event}",
            Self::LiveEnds => "{session}",
            Self::LiveFrames => "{frame}",
            Self::LiveRejections => "{message}",
            Self::TransferWireBytes => "By",
            Self::RpcClientAttempts => "{attempt}",
            Self::AuthRefreshAttempts => "{attempt}",
            Self::SnapshotErrors => "{error}",
            Self::RouteOverflow => "{registration}",
        }
    }
}

/// Up/down counter families in the catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpDownFamily {
    /// Unary server requests currently admitted.
    RpcServerInflight,
    /// Locally owned operation executions.
    OperationActive,
    /// Live observation sessions by current phase.
    LiveSessions,
    /// Retained live serialized payload bytes.
    LiveBufferedBytes,
    /// Owned live cleanup that exceeded the shared grace.
    LiveCleanupPending,
}

impl UpDownFamily {
    /// Catalog metric name.
    pub fn name(self) -> &'static str {
        match self {
            Self::RpcServerInflight => "trellis.rpc.server.inflight",
            Self::OperationActive => "trellis.operation.active",
            Self::LiveSessions => "trellis.live.sessions",
            Self::LiveBufferedBytes => "trellis.live.buffered.bytes",
            Self::LiveCleanupPending => "trellis.live.cleanup.pending",
        }
    }

    /// Catalog unit.
    fn unit(self) -> &'static str {
        match self {
            Self::RpcServerInflight => "{request}",
            Self::OperationActive => "{execution}",
            Self::LiveSessions => "{session}",
            Self::LiveBufferedBytes => "By",
            Self::LiveCleanupPending => "{source}",
        }
    }
}

/// Observable gauge families in the catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservableFamily {
    /// Current local connection counts.
    ConnectionCount,
    /// Aggregate live retained authorization coverage.
    AuthCoverageCount,
    /// Outstanding Auth post-commit actions.
    AuthPostCommitPending,
    /// Age of the oldest outstanding Auth post-commit action.
    AuthPostCommitOldestAge,
    /// Pending jobs satisfying the ready predicate.
    JobsReady,
    /// Age of the oldest ready job.
    JobsOldestReadyAge,
    /// Undismissed dead jobs.
    JobsDead,
    /// Fresh queue-worker registrations.
    JobsWorkerRegistrations,
    /// Retry-state jobs without a persisted eligibility timestamp.
    JobsWaitingRetry,
    /// Outstanding consumer messages.
    ConsumerPending,
    /// Outstanding consumer messages awaiting acknowledgement.
    ConsumerAckPending,
    /// Authoritatively missing declared consumers.
    ConsumerMissing,
    /// Maximum consumer no-progress age.
    ConsumerProgressAge,
    /// Unresolved dead-letter entries by state.
    DeadLetters,
    /// Projection broker backlog.
    ProjectionPending,
    /// Projection no-progress age.
    ProjectionProgressAge,
    /// Aggregate materialized resource bindings.
    ResourceBindings,
    /// Component ready state as 0/1.
    ComponentReady,
    /// Unix seconds of the last successful component observation.
    ComponentObservedTime,
    /// Unix seconds of the last successful telemetry snapshot.
    SnapshotObservedTime,
}

impl ObservableFamily {
    /// Catalog metric name.
    pub fn name(self) -> &'static str {
        match self {
            Self::ConnectionCount => "trellis.connection.count",
            Self::AuthCoverageCount => "trellis.auth.coverage.count",
            Self::AuthPostCommitPending => "trellis.auth.post_commit.pending",
            Self::AuthPostCommitOldestAge => "trellis.auth.post_commit.oldest.age",
            Self::JobsReady => "trellis.jobs.ready",
            Self::JobsOldestReadyAge => "trellis.jobs.oldest_ready.age",
            Self::JobsDead => "trellis.jobs.dead",
            Self::JobsWorkerRegistrations => "trellis.jobs.worker.registrations",
            Self::JobsWaitingRetry => "trellis.jobs.waiting_retry",
            Self::ConsumerPending => "trellis.consumer.pending",
            Self::ConsumerAckPending => "trellis.consumer.ack_pending",
            Self::ConsumerMissing => "trellis.consumer.missing",
            Self::ConsumerProgressAge => "trellis.consumer.progress.age",
            Self::DeadLetters => "trellis.dlq.entries",
            Self::ProjectionPending => "trellis.projection.pending",
            Self::ProjectionProgressAge => "trellis.projection.progress.age",
            Self::ResourceBindings => "trellis.resource.bindings",
            Self::ComponentReady => "trellis.runtime.component.ready",
            Self::ComponentObservedTime => "trellis.runtime.component.observed.time",
            Self::SnapshotObservedTime => "trellis.snapshot.observed.time",
        }
    }

    /// Catalog unit.
    fn unit(self) -> &'static str {
        match self {
            Self::AuthPostCommitOldestAge
            | Self::JobsOldestReadyAge
            | Self::ConsumerProgressAge
            | Self::ProjectionProgressAge
            | Self::ComponentObservedTime
            | Self::SnapshotObservedTime => "s",
            Self::ComponentReady => "1",
            Self::JobsReady | Self::JobsDead | Self::JobsWaitingRetry => "{job}",
            Self::JobsWorkerRegistrations => "{registration}",
            Self::ConsumerPending | Self::ConsumerAckPending | Self::ProjectionPending => {
                "{message}"
            }
            Self::ConsumerMissing => "{consumer}",
            Self::DeadLetters => "{entry}",
            Self::ResourceBindings => "{resource}",
            Self::ConnectionCount => "{connection}",
            Self::AuthCoverageCount => "{context}",
            Self::AuthPostCommitPending => "{action}",
        }
    }
}

/// One metric hand-off to an observable gauge callback.
///
/// A source returns the current values and attributes for its registration;
/// it must be an in-memory read only, never a database or broker operation.
pub type ObservationSource = Arc<dyn Fn() -> Vec<(f64, Vec<KeyValue>)> + Send + Sync + 'static>;

/// One live observable source registration.
///
/// The registry holds the source while the handle is alive; dropping the
/// handle removes precisely that source so a stopped sampler cannot keep
/// exporting stale values. The handle is not `Clone`: an owner that needs to
/// share it must share one instance, so dropping an arbitrary copy can never
/// unregister a still-live owner.
pub struct ObservationRegistration {
    #[allow(dead_code)]
    family: &'static str,
    id: u64,
}

impl ObservationRegistration {
    /// Removes this registration from its family's live sources.
    /// Removes this owner's source from its family.
    ///
    /// The family instrument and its callback stay registered: an empty source
    /// list and instrument existence are separate state, so a later
    /// registration is exported through the same single callback.
    pub fn unregister(&self) {
        if let Some(registry) = OBSERVABLE_SOURCES.get() {
            let mut sources = registry.lock().expect("observable registry");
            if let Some((_, family_sources)) = sources
                .iter_mut()
                .find(|(_, registered)| registered.sources.iter().any(|(id, _)| *id == self.id))
            {
                family_sources.sources.retain(|(id, _)| *id != self.id);
            }
        }
    }
}

impl Drop for ObservationRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// Live sources for one observable family plus the meter generation they bound.
struct ObservableFamilySources {
    generation: u64,
    sources: Vec<(u64, ObservationSource)>,
}

type ObservableSources = Mutex<Vec<(&'static str, ObservableFamilySources)>>;
static OBSERVABLE_SOURCES: OnceLock<ObservableSources> = OnceLock::new();
static NEXT_SOURCE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Collects one family's live sources under the lock, then invokes them
/// without holding the registry lock so collection never blocks registration.
fn live_sources(family: &'static str) -> Vec<ObservationSource> {
    let Some(registry) = OBSERVABLE_SOURCES.get() else {
        return Vec::new();
    };
    let sources = registry.lock().expect("observable registry");
    sources
        .iter()
        .find(|(name, _)| *name == family)
        .map(|(_, family_sources)| {
            family_sources
                .sources
                .iter()
                .map(|(_, source)| Arc::clone(source))
                .collect()
        })
        .unwrap_or_default()
}

type HandleRegistry<T> = Mutex<std::collections::BTreeMap<&'static str, T>>;

/// Generation of the process meter provider.
///
/// Instrument handles are created against the meter provider that was
/// installed when they were first used. If a process installs a different
/// provider (a host-owned provider in tests, or a re-initialization), cached
/// handles would keep exporting to the dead provider, so handles are keyed by
/// this generation and rebuilt when it advances.
static METER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Records that the process meter provider changed.
///
/// Provider owners call this immediately after installing or replacing the
/// global meter provider so subsequent instruments bind to the new meter.
pub fn note_meter_provider_changed() {
    METER_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // Rebind already-registered observable families to the newly installed
    // meter now, preserving their live sources and registration ids. This
    // finishes early-instrument initialization for a supported provider owner.
    let Some(registry) = OBSERVABLE_SOURCES.get() else {
        return;
    };
    let generation = meter_generation();
    let families: Vec<(&'static str, &'static str)> = {
        let Ok(sources) = registry.lock() else {
            return;
        };
        sources
            .iter()
            .filter(|(_, family_sources)| family_sources.generation != generation)
            .map(|(name, _)| (*name, family_name_unit(name)))
            .collect()
    };
    for (name, unit) in families {
        global::meter(INSTRUMENTATION_SCOPE)
            .f64_observable_gauge(name)
            .with_unit(unit)
            .with_callback(move |observer| {
                for source in live_sources(name) {
                    for (value, attributes) in source() {
                        observer.observe(value, &attributes);
                    }
                }
            })
            .build();
        if let Ok(mut sources) = registry.lock() {
            if let Some((_, family_sources)) = sources.iter_mut().find(|(n, _)| *n == name) {
                family_sources.generation = generation;
            }
        }
    }
}

/// Catalog unit for one observable family name.
fn family_name_unit(name: &'static str) -> &'static str {
    for family in [
        ObservableFamily::ConnectionCount,
        ObservableFamily::AuthCoverageCount,
        ObservableFamily::AuthPostCommitPending,
        ObservableFamily::AuthPostCommitOldestAge,
        ObservableFamily::JobsReady,
        ObservableFamily::JobsOldestReadyAge,
        ObservableFamily::JobsDead,
        ObservableFamily::JobsWorkerRegistrations,
        ObservableFamily::JobsWaitingRetry,
        ObservableFamily::ConsumerPending,
        ObservableFamily::ConsumerAckPending,
        ObservableFamily::ConsumerMissing,
        ObservableFamily::ConsumerProgressAge,
        ObservableFamily::DeadLetters,
        ObservableFamily::ProjectionPending,
        ObservableFamily::ProjectionProgressAge,
        ObservableFamily::ResourceBindings,
        ObservableFamily::ComponentReady,
        ObservableFamily::ComponentObservedTime,
        ObservableFamily::SnapshotObservedTime,
    ] {
        if family.name() == name {
            return family.unit();
        }
    }
    "{count}"
}

/// Current instrument generation for handle-cache invalidation.
fn meter_generation() -> u64 {
    METER_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Returns the shared histogram for one family in the current generation.
fn histogram_for(family: DurationFamily) -> &'static Histogram<f64> {
    static HANDLES: OnceLock<HandleRegistry<(u64, &'static Histogram<f64>)>> = OnceLock::new();
    let handles = HANDLES.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()));
    let generation = meter_generation();
    let mut registry = handles.lock().expect("histogram registry");
    if let Some((registered_generation, handle)) = registry.get(family.name()) {
        if *registered_generation == generation {
            return handle;
        }
    }
    let handle: &'static Histogram<f64> = Box::leak(Box::new(
        global::meter(INSTRUMENTATION_SCOPE)
            .f64_histogram(family.name())
            .with_unit("s")
            .with_boundaries(family.boundaries().to_vec())
            .build(),
    ));
    registry.insert(family.name(), (generation, handle));
    handle
}

/// Returns the shared counter for one family in the current generation.
fn counter_for(family: CounterFamily) -> &'static Counter<u64> {
    static HANDLES: OnceLock<HandleRegistry<(u64, &'static Counter<u64>)>> = OnceLock::new();
    let handles = HANDLES.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()));
    let generation = meter_generation();
    let mut registry = handles.lock().expect("counter registry");
    if let Some((registered_generation, handle)) = registry.get(family.name()) {
        if *registered_generation == generation {
            return handle;
        }
    }
    let handle: &'static Counter<u64> = Box::leak(Box::new(
        global::meter(INSTRUMENTATION_SCOPE)
            .u64_counter(family.name())
            .with_unit(family.unit())
            .build(),
    ));
    registry.insert(family.name(), (generation, handle));
    handle
}

/// Returns the shared up/down counter for one family in the current generation.
fn updown_for(family: UpDownFamily) -> &'static UpDownCounter<i64> {
    static HANDLES: OnceLock<HandleRegistry<(u64, &'static UpDownCounter<i64>)>> = OnceLock::new();
    let handles = HANDLES.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()));
    let generation = meter_generation();
    let mut registry = handles.lock().expect("updown registry");
    if let Some((registered_generation, handle)) = registry.get(family.name()) {
        if *registered_generation == generation {
            return handle;
        }
    }
    let handle: &'static UpDownCounter<i64> = Box::leak(Box::new(
        global::meter(INSTRUMENTATION_SCOPE)
            .i64_up_down_counter(family.name())
            .with_unit(family.unit())
            .build(),
    ));
    registry.insert(family.name(), (generation, handle));
    handle
}

/// Registers one observable gauge family whose callback copies local values.
///
/// The family instrument is created once; its callback always consults the
/// current live source registry, so later registrations are exported too.
pub fn register_observable(
    family: ObservableFamily,
    source: ObservationSource,
) -> ObservationRegistration {
    let registry = OBSERVABLE_SOURCES.get_or_init(|| Mutex::new(Vec::new()));
    let id = NEXT_SOURCE_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let generation = meter_generation();
    {
        let mut sources = registry.lock().expect("observable registry");
        // A provider change rebinds the family callback to the new meter; an
        // existing generation with an empty source list is reused unchanged, so
        // re-registration never creates a second callback for the same family.
        let current_generation = sources
            .iter()
            .find(|(name, _)| *name == family.name())
            .map(|(_, family_sources)| family_sources.generation);
        if current_generation != Some(generation) {
            // Rebind the callback to the newly installed meter while preserving
            // every live source entry and its registration id: an existing
            // owner must not disappear because another family re-registered.
            global::meter(INSTRUMENTATION_SCOPE)
                .f64_observable_gauge(family.name())
                .with_unit(family.unit())
                .with_callback(move |observer| {
                    for source in live_sources(family.name()) {
                        for (value, attributes) in source() {
                            observer.observe(value, &attributes);
                        }
                    }
                })
                .build();
            match sources.iter_mut().find(|(name, _)| *name == family.name()) {
                Some((_, family_sources)) => family_sources.generation = generation,
                None => sources.push((
                    family.name(),
                    ObservableFamilySources {
                        generation,
                        sources: Vec::new(),
                    },
                )),
            }
        }
        let (_, family_sources) = sources
            .iter_mut()
            .find(|(name, _)| *name == family.name())
            .expect("family was just registered");
        family_sources.sources.push((id, source));
    }
    ObservationRegistration {
        family: family.name(),
        id,
    }
}

/// Records one accepted Auth/contract duration sample.
pub fn record_duration(
    metric: AuthDurationMetric,
    elapsed: Duration,
    surface: &'static str,
    operation: &'static str,
    phase: &'static str,
    outcome: Outcome,
) {
    let family = match metric {
        AuthDurationMetric::AuthFlow => DurationFamily::AuthFlow,
        AuthDurationMetric::AuthCallout => DurationFamily::AuthCallout,
        AuthDurationMetric::ContractAnalysis => DurationFamily::ContractAnalysis,
    };
    histogram_for(family).record(
        elapsed.as_secs_f64(),
        &[
            KeyValue::new("trellis.surface", surface),
            KeyValue::new("trellis.operation", operation),
            KeyValue::new("trellis.phase", phase),
            KeyValue::new("trellis.outcome", outcome.as_str()),
        ],
    );
}

/// Records one duration sample for a family with explicit attributes.
pub fn record_family_duration(family: DurationFamily, elapsed: Duration, attributes: &[KeyValue]) {
    histogram_for(family).record(elapsed.as_secs_f64(), attributes);
}

/// Adds one counter sample for a family with explicit attributes.
pub fn add_counter(family: CounterFamily, value: u64, attributes: &[KeyValue]) {
    counter_for(family).add(value, attributes);
}

/// Adds one up/down counter delta for a family with explicit attributes.
pub fn add_updown(family: UpDownFamily, delta: i64, attributes: &[KeyValue]) {
    updown_for(family).add(delta, attributes);
}

/// Records exactly one cache request sample for a completed lookup decision.
pub fn record_cache_request(kind: CacheKind, result: CacheResult) {
    // Generation-aware so first use before provider initialization cannot
    // permanently bind this counter to a no-op meter.
    static CACHE_REQUESTS: OnceLock<Mutex<(u64, &'static Counter<u64>)>> = OnceLock::new();
    let cache = CACHE_REQUESTS.get_or_init(|| {
        Mutex::new((
            u64::MAX,
            Box::leak(Box::new(
                global::meter(INSTRUMENTATION_SCOPE)
                    .u64_counter("trellis.contract.cache.requests")
                    .with_unit("{request}")
                    .build(),
            )),
        ))
    });
    let generation = meter_generation();
    let counter = {
        let mut cache = cache.lock().expect("cache request counter");
        if cache.0 != generation {
            *cache = (
                generation,
                Box::leak(Box::new(
                    global::meter(INSTRUMENTATION_SCOPE)
                        .u64_counter("trellis.contract.cache.requests")
                        .with_unit("{request}")
                        .build(),
                )),
            );
        }
        cache.1
    };
    counter.add(
        1,
        &[
            KeyValue::new("trellis.cache.kind", kind.as_str()),
            KeyValue::new("trellis.cache.result", result.as_str()),
        ],
    );
}

/// Registration family for the bounded route catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteFamily {
    /// Descriptor-backed unary RPC routes.
    Rpc,
    /// Declared event identities.
    Event,
    /// Declared consumer identities.
    Consumer,
    /// Declared job names.
    Job,
    /// Declared operation names.
    Operation,
    /// Registered HTTP route templates.
    Http,
}

impl RouteFamily {
    /// Stable catalog value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Event => "event",
            Self::Consumer => "consumer",
            Self::Job => "job",
            Self::Operation => "operation",
            Self::Http => "http",
        }
    }
}

/// Interned bounded route tokens per family.
struct RouteCatalog {
    tokens: Mutex<std::collections::BTreeMap<&'static str, Vec<&'static str>>>,
}

impl RouteCatalog {
    /// Registers or resolves one route token.
    fn token(&self, family: RouteFamily, raw: &str) -> &'static str {
        let is_valid = !raw.is_empty() && raw.len() <= ROUTE_TOKEN_MAX_BYTES;
        let mut tokens = self.tokens.lock().expect("route catalog");
        let entries = tokens.entry(family.as_str()).or_default();
        if let Some(token) = entries.iter().find(|token| **token == raw) {
            return token;
        }
        if !is_valid || entries.len() >= ROUTE_LIMIT_PER_FAMILY {
            add_counter(
                CounterFamily::RouteOverflow,
                1,
                &[KeyValue::new("trellis.family", family.as_str())],
            );
            return other_token(family);
        }
        let token: &'static str = Box::leak(raw.to_owned().into_boxed_str());
        entries.push(token);
        entries.sort_unstable();
        token
    }
}

/// Returns the shared `_other` token for one family.
fn other_token(family: RouteFamily) -> &'static str {
    static OTHER: OnceLock<&'static str> = OnceLock::new();
    let _ = family;
    OTHER.get_or_init(|| "_other")
}

/// Returns the shared `_unknown` token.
pub fn unknown_route() -> &'static str {
    "_unknown"
}

/// Resolves one bounded registered route token for metric attributes.
///
/// Registration happens when a descriptor, job, consumer, or HTTP route is
/// mounted; a hostile request subject can never create a token. Over-limit or
/// overlong registrations map to `_other` and increment the overflow counter
/// once at registration.
pub fn route_token(family: RouteFamily, raw: &str) -> &'static str {
    static CATALOG: OnceLock<RouteCatalog> = OnceLock::new();
    CATALOG
        .get_or_init(|| RouteCatalog {
            tokens: Mutex::new(std::collections::BTreeMap::new()),
        })
        .token(family, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_tokens_are_bounded_and_stable() {
        let first = route_token(RouteFamily::Rpc, "trellis.auth@v1:Sessions.Me");
        let second = route_token(RouteFamily::Rpc, "trellis.auth@v1:Sessions.Me");
        assert_eq!(first, second);
        assert!(first.contains("Sessions.Me"));
        assert_eq!(unknown_route(), "_unknown");
    }

    #[test]
    fn overlong_route_tokens_map_to_other() {
        let long = "x".repeat(ROUTE_TOKEN_MAX_BYTES + 1);
        assert_eq!(route_token(RouteFamily::Rpc, &long), "_other");
    }

    #[test]
    fn duration_families_have_catalog_names_and_units() {
        assert_eq!(
            DurationFamily::RpcServer.name(),
            "trellis.rpc.server.duration"
        );
        assert_eq!(CounterFamily::RpcClientAttempts.unit(), "{attempt}");
        assert_eq!(ObservableFamily::JobsReady.name(), "trellis.jobs.ready");
    }
}

#[cfg(test)]
mod observable_registry_tests {
    use super::*;
    use crate::telemetry::capture::MetricCapture;

    fn gauge_points(capture: &MetricCapture, name: &str) -> Vec<(String, f64)> {
        capture
            .points_for(name)
            .into_iter()
            .map(|point| {
                let key = point
                    .attributes
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(",");
                (key, point.value)
            })
            .collect()
    }

    #[tokio::test]
    async fn later_sources_are_exported_and_removal_stops_only_that_source() {
        let _guard = crate::telemetry::capture::meter_test_lock().await;
        let capture = crate::telemetry::capture::process_capture();
        capture.flush();

        let first = register_observable(
            ObservableFamily::ConsumerPending,
            Arc::new(|| vec![(1.0, vec![KeyValue::new("source", "registry-first")])]),
        );
        let second = register_observable(
            ObservableFamily::ConsumerPending,
            Arc::new(|| vec![(2.0, vec![KeyValue::new("source", "registry-second")])]),
        );

        capture.flush();
        let mut both = gauge_points(&capture, ObservableFamily::ConsumerPending.name());
        both.retain(|(attributes, _)| attributes.contains("registry-"));
        both.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            both,
            vec![
                ("source=registry-first".to_string(), 1.0),
                ("source=registry-second".to_string(), 2.0)
            ],
            "both registered sources must be exported"
        );

        drop(second);
        capture.flush();
        let remaining = gauge_points(&capture, ObservableFamily::ConsumerPending.name());
        assert_eq!(
            remaining,
            vec![("source=registry-first".to_string(), 1.0)],
            "dropping one registration must remove only that source"
        );

        drop(first);
        capture.flush();
    }
}
