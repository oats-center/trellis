//! Runtime-owned reuse of immutable installed evidence and its deterministic
//! compatibility results.
//!
//! Compiled package graphs and selected-surface compatibility reports are pure
//! functions of immutable, digest-identified installed evidence. This cache
//! reuses those results inside one opened authorization store so warm
//! connection paths never repeat the semantic work. It holds no authority,
//! grant, provider choice, readiness, or revocation state.
//!
//! The guarantee is one computation per concurrent miss and no repeat
//! computation while the result is resident. Least-recently-used eviction may
//! require a later recomputation; eviction is a memory decision, not a failure
//! or an authorization transition.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::FutureExt;
use tokio::sync::{watch, Semaphore};
use trellis_idl::{
    ActionSelection, ApiId, CapabilityId, CompatibilityReport, InteractionSelection, PackageGraph,
};

use super::AuthorizationStateError;
use crate::telemetry::{
    record_cache_request, record_duration, CacheKind, CacheResult, DurationMetric, Outcome,
};

/// Ready compiled evidence documents retained per store.
const READY_EVIDENCE_LIMIT: usize = 16;
/// Ready compatibility reports retained per store.
const READY_COMPATIBILITY_LIMIT: usize = 256;
/// Distinct in-flight semantic jobs, combined across both caches.
const IN_FLIGHT_JOB_LIMIT: usize = 8;
/// Simultaneous CPU jobs for compile/compare.
const CPU_JOB_LIMIT: usize = 2;

/// Immutable compiled identity for one exact installed evidence document.
#[derive(Clone)]
pub(crate) struct CompiledInstalledEvidence {
    /// Exact canonical evidence-document digest.
    pub evidence_digest: String,
    /// Independently verified root semantic package digest.
    pub package_digest: String,
    /// Immutable compiled closure shared across callers.
    pub graph: Arc<PackageGraph>,
}

/// Cloneable semantic-job failure shared with every waiter.
#[derive(Clone, Debug)]
pub(in crate::platform::auth) enum SemanticJobError {
    /// The exact installed evidence document is absent.
    MissingDocument,
    /// The stored document fails digest or semantic verification.
    InvalidEvidence(String),
    /// The authorization store failed.
    Storage(String),
    /// The bounded semantic worker failed.
    Worker(String),
}

impl SemanticJobError {
    /// Map the shared failure back to its existing authorization category.
    pub(in crate::platform::auth) fn into_state_error(self) -> AuthorizationStateError {
        match self {
            Self::MissingDocument => AuthorizationStateError::ParticipantMissing,
            Self::InvalidEvidence(message) => AuthorizationStateError::InvalidRecord(message),
            Self::Storage(message) | Self::Worker(message) => {
                AuthorizationStateError::Storage(message)
            }
        }
    }
}

type GraphJobResult = Result<Arc<CompiledInstalledEvidence>, SemanticJobError>;
type ComparisonJobResult = Result<Arc<CompatibilityReport>, SemanticJobError>;
type GraphJobReceiver = watch::Receiver<Option<GraphJobResult>>;
type ComparisonJobReceiver = watch::Receiver<Option<ComparisonJobResult>>;

/// Exact immutable identity of one selected-surface comparison.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CompatibilityKey {
    consumer_evidence_digest: String,
    provider_evidence_digest: String,
    api: ApiId,
    actions: BTreeSet<ActionSelection>,
    optional_capabilities: BTreeSet<CapabilityId>,
}

struct ReadyGraph {
    value: Arc<CompiledInstalledEvidence>,
    used: u64,
}

struct ReadyComparison {
    value: Arc<CompatibilityReport>,
    used: u64,
}

/// One short metadata lock protects both ready maps and both in-flight maps so
/// a lookup and its flight reservation share one atomic decision.
struct Metadata {
    tick: u64,
    graphs: BTreeMap<String, ReadyGraph>,
    comparisons: BTreeMap<CompatibilityKey, ReadyComparison>,
    in_flight_graphs: BTreeMap<String, GraphJobReceiver>,
    in_flight_comparisons: BTreeMap<CompatibilityKey, ComparisonJobReceiver>,
}

/// One request's initial lookup result.
enum Lookup<T> {
    Ready(Arc<T>),
    Wait(watch::Receiver<Option<Result<Arc<T>, SemanticJobError>>>),
    Miss,
}

/// One request's role after acquiring a bounded job slot.
enum Claimed<T> {
    Ready(Arc<T>),
    Wait(watch::Receiver<Option<Result<Arc<T>, SemanticJobError>>>),
    Owner(
        watch::Sender<Option<Result<Arc<T>, SemanticJobError>>>,
        watch::Receiver<Option<Result<Arc<T>, SemanticJobError>>>,
    ),
}

struct CacheInner {
    metadata: Mutex<Metadata>,
    jobs: Arc<Semaphore>,
    cpu: Arc<Semaphore>,
    compile_attempts: AtomicU64,
    compile_successes: AtomicU64,
    comparison_attempts: AtomicU64,
    comparison_successes: AtomicU64,
    semantic_jobs: AtomicUsize,
    peak_semantic_jobs: AtomicUsize,
    peak_ready_graphs: AtomicUsize,
    peak_ready_comparisons: AtomicUsize,
}

/// Process-local, bounded reuse of immutable compiled evidence.
///
/// One cache belongs to one opened authorization store; clones of that store
/// share it, and separate stores never share cached documents.
pub(in crate::platform::auth) struct CompiledEvidenceCache {
    inner: Arc<CacheInner>,
}

impl std::fmt::Debug for CompiledEvidenceCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let metadata = self
            .inner
            .metadata
            .lock()
            .expect("compiled evidence cache lock poisoned");
        formatter
            .debug_struct("CompiledEvidenceCache")
            .field("ready_graphs", &metadata.graphs.len())
            .field("ready_comparisons", &metadata.comparisons.len())
            .field("in_flight_graphs", &metadata.in_flight_graphs.len())
            .field(
                "in_flight_comparisons",
                &metadata.in_flight_comparisons.len(),
            )
            .field(
                "semantic_jobs",
                &self.inner.semantic_jobs.load(Ordering::Relaxed),
            )
            .field(
                "available_job_permits",
                &self.inner.jobs.available_permits(),
            )
            .field(
                "compile_attempts",
                &self.inner.compile_attempts.load(Ordering::Relaxed),
            )
            .field(
                "compile_successes",
                &self.inner.compile_successes.load(Ordering::Relaxed),
            )
            .field(
                "comparison_attempts",
                &self.inner.comparison_attempts.load(Ordering::Relaxed),
            )
            .field(
                "comparison_successes",
                &self.inner.comparison_successes.load(Ordering::Relaxed),
            )
            .field(
                "peak_semantic_jobs",
                &self.inner.peak_semantic_jobs.load(Ordering::Relaxed),
            )
            .field(
                "peak_ready_graphs",
                &self.inner.peak_ready_graphs.load(Ordering::Relaxed),
            )
            .field(
                "peak_ready_comparisons",
                &self.inner.peak_ready_comparisons.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl CompiledEvidenceCache {
    /// Create one empty bounded semantic cache.
    pub(in crate::platform::auth) fn new() -> Self {
        Self {
            inner: Arc::new(CacheInner {
                metadata: Mutex::new(Metadata {
                    tick: 0,
                    graphs: BTreeMap::new(),
                    comparisons: BTreeMap::new(),
                    in_flight_graphs: BTreeMap::new(),
                    in_flight_comparisons: BTreeMap::new(),
                }),
                jobs: Arc::new(Semaphore::new(IN_FLIGHT_JOB_LIMIT)),
                cpu: Arc::new(Semaphore::new(CPU_JOB_LIMIT)),
                compile_attempts: AtomicU64::new(0),
                compile_successes: AtomicU64::new(0),
                comparison_attempts: AtomicU64::new(0),
                comparison_successes: AtomicU64::new(0),
                semantic_jobs: AtomicUsize::new(0),
                peak_semantic_jobs: AtomicUsize::new(0),
                peak_ready_graphs: AtomicUsize::new(0),
                peak_ready_comparisons: AtomicUsize::new(0),
            }),
        }
    }

    /// Return the bounded CPU permit source used by semantic jobs.
    pub(in crate::platform::auth) fn cpu_semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.inner.cpu)
    }

    /// Return successful compile and comparison counts for focused checks.
    #[cfg(test)]
    pub(in crate::platform::auth) fn counters(&self) -> (u64, u64) {
        (
            self.inner.compile_successes.load(Ordering::Relaxed),
            self.inner.comparison_successes.load(Ordering::Relaxed),
        )
    }

    /// Return attempted compile and comparison counts for focused checks.
    #[cfg(test)]
    pub(in crate::platform::auth) fn attempts(&self) -> (u64, u64) {
        (
            self.inner.compile_attempts.load(Ordering::Relaxed),
            self.inner.comparison_attempts.load(Ordering::Relaxed),
        )
    }

    /// Return resident ready-entry counts for focused checks.
    #[cfg(test)]
    pub(in crate::platform::auth) fn ready_counts(&self) -> (usize, usize) {
        let metadata = self
            .inner
            .metadata
            .lock()
            .expect("compiled evidence cache lock poisoned");
        (metadata.graphs.len(), metadata.comparisons.len())
    }

    /// Return observed peak resident and in-flight counts for focused checks.
    #[cfg(test)]
    pub(in crate::platform::auth) fn peaks(&self) -> (usize, usize, usize) {
        (
            self.inner.peak_semantic_jobs.load(Ordering::Relaxed),
            self.inner.peak_ready_graphs.load(Ordering::Relaxed),
            self.inner.peak_ready_comparisons.load(Ordering::Relaxed),
        )
    }

    /// Return the compiled graph for an exact installed evidence document.
    ///
    /// # Errors
    ///
    /// Returns the shared [`SemanticJobError`] produced by `load` when the
    /// document is missing or fails verification.
    pub(in crate::platform::auth) async fn compiled_installed_evidence<F, Fut>(
        &self,
        evidence_digest: &str,
        load: F,
    ) -> Result<Arc<CompiledInstalledEvidence>, SemanticJobError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<CompiledInstalledEvidence, SemanticJobError>> + Send + 'static,
    {
        let initial = {
            let mut metadata = lock_metadata(&self.inner);
            match touch_ready_graph(&mut metadata, evidence_digest) {
                Some(value) => Lookup::Ready(value),
                None => match metadata.in_flight_graphs.get(evidence_digest).cloned() {
                    Some(receiver) => Lookup::Wait(receiver),
                    None => Lookup::Miss,
                },
            }
        };
        match initial {
            Lookup::Ready(value) => {
                record_cache_request(CacheKind::Evidence, CacheResult::Hit);
                return Ok(value);
            }
            Lookup::Wait(mut receiver) => {
                record_cache_request(CacheKind::Evidence, CacheResult::Wait);
                return wait_for_job(&mut receiver).await.ok_or_else(closed_job)?;
            }
            Lookup::Miss => {}
        }
        let queue_started = Instant::now();
        let permit = match Arc::clone(&self.inner.jobs).acquire_owned().await {
            Ok(permit) => {
                record_duration(
                    DurationMetric::ContractAnalysis,
                    queue_started.elapsed(),
                    "contract",
                    "compile_evidence",
                    "job_queue",
                    Outcome::Ok,
                );
                permit
            }
            Err(_) => {
                record_duration(
                    DurationMetric::ContractAnalysis,
                    queue_started.elapsed(),
                    "contract",
                    "compile_evidence",
                    "job_queue",
                    Outcome::Error,
                );
                return Err(SemanticJobError::Storage(
                    "semantic job pool closed".to_owned(),
                ));
            }
        };
        let key = evidence_digest.to_owned();
        let claim = {
            let mut metadata = lock_metadata(&self.inner);
            match touch_ready_graph(&mut metadata, evidence_digest) {
                Some(value) => Claimed::Ready(value),
                None => match metadata.in_flight_graphs.get(evidence_digest).cloned() {
                    Some(receiver) => Claimed::Wait(receiver),
                    None => {
                        let (sender, receiver) = watch::channel(None);
                        metadata
                            .in_flight_graphs
                            .insert(key.clone(), receiver.clone());
                        Claimed::Owner(sender, receiver)
                    }
                },
            }
        };
        let (sender, mut receiver) = match claim {
            Claimed::Ready(value) => {
                drop(permit);
                record_cache_request(CacheKind::Evidence, CacheResult::Hit);
                return Ok(value);
            }
            Claimed::Wait(mut receiver) => {
                drop(permit);
                record_cache_request(CacheKind::Evidence, CacheResult::Wait);
                return wait_for_job(&mut receiver).await.ok_or_else(closed_job)?;
            }
            Claimed::Owner(sender, receiver) => (sender, receiver),
        };
        record_cache_request(CacheKind::Evidence, CacheResult::Miss);
        self.inner.compile_attempts.fetch_add(1, Ordering::Relaxed);
        self.inner.peak_semantic_jobs.fetch_max(
            self.inner.semantic_jobs.fetch_add(1, Ordering::Relaxed) + 1,
            Ordering::Relaxed,
        );
        let inner = Arc::clone(&self.inner);
        let job_key = key;
        tokio::spawn(async move {
            let result = AssertUnwindSafe(load())
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(SemanticJobError::Worker(
                        "semantic document job panicked".to_owned(),
                    ))
                })
                .map(Arc::new);
            {
                let mut metadata = lock_metadata(&inner);
                if let Ok(value) = &result {
                    install_ready_graph(&mut metadata, &job_key, Arc::clone(value));
                    inner
                        .peak_ready_graphs
                        .fetch_max(metadata.graphs.len(), Ordering::Relaxed);
                }
                metadata.in_flight_graphs.remove(&job_key);
            }
            if result.is_ok() {
                inner.compile_successes.fetch_add(1, Ordering::Relaxed);
            }
            sender.send_replace(Some(result));
            inner.semantic_jobs.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
        });
        wait_for_job(&mut receiver).await.ok_or_else(closed_job)?
    }

    /// Return the immutable compatibility report for an exact selection.
    ///
    /// # Errors
    ///
    /// Returns the shared [`SemanticJobError`] produced by `compare` when the
    /// semantic comparison cannot run.
    pub(in crate::platform::auth) async fn compare_selection<F, Fut>(
        &self,
        consumer: &CompiledInstalledEvidence,
        selection: &InteractionSelection,
        provider: &CompiledInstalledEvidence,
        compare: F,
    ) -> Result<Arc<CompatibilityReport>, SemanticJobError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<CompatibilityReport, SemanticJobError>> + Send + 'static,
    {
        let key = CompatibilityKey {
            consumer_evidence_digest: consumer.evidence_digest.clone(),
            provider_evidence_digest: provider.evidence_digest.clone(),
            api: selection.api.clone(),
            actions: selection.actions.clone(),
            optional_capabilities: selection.optional_capabilities.clone(),
        };
        let initial = {
            let mut metadata = lock_metadata(&self.inner);
            match touch_ready_comparison(&mut metadata, &key) {
                Some(value) => Lookup::Ready(value),
                None => match metadata.in_flight_comparisons.get(&key).cloned() {
                    Some(receiver) => Lookup::Wait(receiver),
                    None => Lookup::Miss,
                },
            }
        };
        match initial {
            Lookup::Ready(value) => {
                record_cache_request(CacheKind::Compatibility, CacheResult::Hit);
                return Ok(value);
            }
            Lookup::Wait(mut receiver) => {
                record_cache_request(CacheKind::Compatibility, CacheResult::Wait);
                return wait_for_job(&mut receiver).await.ok_or_else(closed_job)?;
            }
            Lookup::Miss => {}
        }
        let queue_started = Instant::now();
        let permit = match Arc::clone(&self.inner.jobs).acquire_owned().await {
            Ok(permit) => {
                record_duration(
                    DurationMetric::ContractAnalysis,
                    queue_started.elapsed(),
                    "contract",
                    "compare_selected",
                    "job_queue",
                    Outcome::Ok,
                );
                permit
            }
            Err(_) => {
                record_duration(
                    DurationMetric::ContractAnalysis,
                    queue_started.elapsed(),
                    "contract",
                    "compare_selected",
                    "job_queue",
                    Outcome::Error,
                );
                return Err(SemanticJobError::Storage(
                    "semantic job pool closed".to_owned(),
                ));
            }
        };
        let claim = {
            let mut metadata = lock_metadata(&self.inner);
            match touch_ready_comparison(&mut metadata, &key) {
                Some(value) => Claimed::Ready(value),
                None => match metadata.in_flight_comparisons.get(&key).cloned() {
                    Some(receiver) => Claimed::Wait(receiver),
                    None => {
                        let (sender, receiver) = watch::channel(None);
                        metadata
                            .in_flight_comparisons
                            .insert(key.clone(), receiver.clone());
                        Claimed::Owner(sender, receiver)
                    }
                },
            }
        };
        let (sender, mut receiver) = match claim {
            Claimed::Ready(value) => {
                drop(permit);
                record_cache_request(CacheKind::Compatibility, CacheResult::Hit);
                return Ok(value);
            }
            Claimed::Wait(mut receiver) => {
                drop(permit);
                record_cache_request(CacheKind::Compatibility, CacheResult::Wait);
                return wait_for_job(&mut receiver).await.ok_or_else(closed_job)?;
            }
            Claimed::Owner(sender, receiver) => (sender, receiver),
        };
        record_cache_request(CacheKind::Compatibility, CacheResult::Miss);
        self.inner
            .comparison_attempts
            .fetch_add(1, Ordering::Relaxed);
        self.inner.peak_semantic_jobs.fetch_max(
            self.inner.semantic_jobs.fetch_add(1, Ordering::Relaxed) + 1,
            Ordering::Relaxed,
        );
        let inner = Arc::clone(&self.inner);
        let job_key = key;
        tokio::spawn(async move {
            let result = AssertUnwindSafe(compare())
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err(SemanticJobError::Worker(
                        "semantic comparison job panicked".to_owned(),
                    ))
                })
                .map(Arc::new);
            {
                let mut metadata = lock_metadata(&inner);
                if let Ok(value) = &result {
                    install_ready_comparison(&mut metadata, &job_key, Arc::clone(value));
                    inner
                        .peak_ready_comparisons
                        .fetch_max(metadata.comparisons.len(), Ordering::Relaxed);
                }
                metadata.in_flight_comparisons.remove(&job_key);
            }
            if result.is_ok() {
                inner.comparison_successes.fetch_add(1, Ordering::Relaxed);
            }
            sender.send_replace(Some(result));
            inner.semantic_jobs.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
        });
        wait_for_job(&mut receiver).await.ok_or_else(closed_job)?
    }
}

fn lock_metadata(inner: &Arc<CacheInner>) -> std::sync::MutexGuard<'_, Metadata> {
    inner
        .metadata
        .lock()
        .expect("compiled evidence cache lock poisoned")
}

fn closed_job() -> SemanticJobError {
    SemanticJobError::Worker("semantic job channel closed".to_owned())
}

async fn wait_for_job<T: Clone>(receiver: &mut watch::Receiver<Option<T>>) -> Option<T> {
    loop {
        let current = receiver.borrow().clone();
        if let Some(result) = current {
            return Some(result);
        }
        if receiver.changed().await.is_err() {
            return None;
        }
    }
}

fn touch_ready_graph(metadata: &mut Metadata, key: &str) -> Option<Arc<CompiledInstalledEvidence>> {
    metadata.tick = metadata.tick.wrapping_add(1);
    let tick = metadata.tick;
    let entry = metadata.graphs.get_mut(key)?;
    entry.used = tick;
    Some(Arc::clone(&entry.value))
}

fn install_ready_graph(metadata: &mut Metadata, key: &str, value: Arc<CompiledInstalledEvidence>) {
    metadata.tick = metadata.tick.wrapping_add(1);
    let tick = metadata.tick;
    metadata
        .graphs
        .insert(key.to_owned(), ReadyGraph { value, used: tick });
    while metadata.graphs.len() > READY_EVIDENCE_LIMIT {
        let Some(oldest) = metadata
            .graphs
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        metadata.graphs.remove(&oldest);
    }
}

fn touch_ready_comparison(
    metadata: &mut Metadata,
    key: &CompatibilityKey,
) -> Option<Arc<CompatibilityReport>> {
    metadata.tick = metadata.tick.wrapping_add(1);
    let tick = metadata.tick;
    let entry = metadata.comparisons.get_mut(key)?;
    entry.used = tick;
    Some(Arc::clone(&entry.value))
}

fn install_ready_comparison(
    metadata: &mut Metadata,
    key: &CompatibilityKey,
    value: Arc<CompatibilityReport>,
) {
    metadata.tick = metadata.tick.wrapping_add(1);
    let tick = metadata.tick;
    metadata
        .comparisons
        .insert(key.clone(), ReadyComparison { value, used: tick });
    while metadata.comparisons.len() > READY_COMPATIBILITY_LIMIT {
        let Some(oldest) = metadata
            .comparisons
            .iter()
            .min_by_key(|(_, entry)| entry.used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        metadata.comparisons.remove(&oldest);
    }
}
