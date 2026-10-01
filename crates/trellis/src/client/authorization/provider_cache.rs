use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use futures_util::StreamExt;
use trellis_protocol::{
    parse_authorization_context, verify_authorization_context, AuthorizationContextPurpose,
    AuthorizationIssuerKey, AuthorizationIssuerState, AuthorizationVerificationPolicy,
    SignedAuthorizationContext, VerifiedAuthorizationContext,
};

use super::super::TrellisClientError;
use super::bootstrap_http::BootstrapHttp;
use super::own_context::{system_now_millis, AuthorizationContextCache, OwnTransitionGuard};
use super::registry::{
    validate_digest_key, AuthorizationRegistryReader, PinnedOwnCandidateSource, RegistryAttachment,
    RegistryWatchEntry, RegistryWatchEvent, REVOCATION_PREFIX,
};
use super::types::AuthorizationRegistryBinding;

#[cfg(feature = "runtime-internals")]
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct RuntimeAuthorizationTrust {
    /// Configured origin for public issuer-key resolution.
    pub trellis_origin: String,
    /// Locally owned issuer, if this runtime issues contexts itself.
    pub issuer: Option<AuthorizationIssuerKey>,
    /// Bounds applied to every resolved context.
    pub policy: AuthorizationVerificationPolicy,
}

#[derive(Clone, Debug)]
pub(crate) struct AuthorizationProviderCacheHealth {
    pub(crate) healthy: bool,
}

#[cfg(feature = "runtime-internals")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeAuthorizationIoCounters {
    pub context_resolves: u64,
}

const MAX_CACHED_CONTEXTS: usize = 256;

#[derive(Default)]
struct CachedVerifications {
    live: Option<VerifiedAuthorizationContext>,
    historical: Option<VerifiedAuthorizationContext>,
}

pub(crate) struct CachedContext {
    signed: SignedAuthorizationContext,
    issuer: AuthorizationIssuerKey,
    verified: Mutex<CachedVerifications>,
    covered: Arc<AtomicBool>,
    watch: tokio::task::AbortHandle,
    leases: AtomicUsize,
    last_used: AtomicU64,
}

impl Drop for CachedContext {
    fn drop(&mut self) {
        self.covered.store(false, Ordering::Release);
        self.watch.abort();
    }
}

/// Lease keeping a cached authorization context and its revocation watch alive.
#[doc(hidden)]
pub struct AuthorizationContextLease {
    entry: Arc<CachedContext>,
    context: VerifiedAuthorizationContext,
}

impl Deref for AuthorizationContextLease {
    type Target = VerifiedAuthorizationContext;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl AuthorizationContextLease {
    pub(crate) fn entry(&self) -> &Arc<CachedContext> {
        &self.entry
    }

    pub(crate) fn context_digest(&self) -> &str {
        self.context.context_digest()
    }
}

impl Drop for AuthorizationContextLease {
    fn drop(&mut self) {
        self.entry.leases.fetch_sub(1, Ordering::Release);
    }
}

/// One pending own-candidate retention: the exact attempt token that owns it and
/// the coverage lease it pins.
///
/// The token distinguishes attempts that retain the same signed digest, so an
/// older attempt's cleanup can never discard a newer attempt's pin even when
/// both resolved to the very same cached entry.
struct PendingOwnLease {
    token: Arc<()>,
    lease: AuthorizationContextLease,
}

/// Scoped ownership of one own-candidate installation attempt's pending coverage
/// pin.
///
/// Created before the attempt's asynchronous retention so a pre-promotion
/// failure or a dropped future releases *only* this attempt's pending pin. Drop
/// compares the exact attempt token, so a newer candidate's pending slot —
/// including a signed-identical replacement — is never discarded, and the
/// healthy installed own lease is never released. A successful promotion
/// consumes the pending pin inside
/// [`AuthorizationProviderCache::finalize_own_installation`], after which the
/// caller [`disarm`](Self::disarm)s the guard.
pub(crate) struct OwnCandidateRetention {
    cache: AuthorizationProviderCache,
    digest: String,
    token: Arc<()>,
    entry: Option<Arc<CachedContext>>,
    armed: bool,
}

impl OwnCandidateRetention {
    /// Record the exact resolved entry owned by this attempt, even before pinning.
    fn track(&mut self, entry: Arc<CachedContext>) {
        self.entry = Some(entry);
    }

    /// Disarm cleanup once promotion has consumed the pending pin.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    /// The exact attempt token that owns this attempt's pending pin.
    ///
    /// The final promotion consumes the pending slot only when it is still owned
    /// by this token, so a newer same-digest attempt's pin is never consumed.
    pub(crate) fn token(&self) -> &Arc<()> {
        &self.token
    }
}

impl Drop for OwnCandidateRetention {
    fn drop(&mut self) {
        if self.armed {
            self.cache.release_own_candidate_retention(
                &self.digest,
                &self.token,
                self.entry.as_ref(),
            );
        }
    }
}

#[derive(Default)]
struct ProviderState {
    contexts: HashMap<String, Arc<CachedContext>>,
    issuers: HashMap<String, AuthorizationIssuerKey>,
    // Negative evidence is retained no longer than a possible live context lease.
    revocations: HashMap<String, (i64, i64)>,
}

impl ProviderState {
    fn revocation_time(&self, digest: &str) -> Result<Option<i64>, ()> {
        if let Some((revoked_at, _)) = self.revocations.get(digest) {
            return Ok(Some(*revoked_at));
        }
        if self
            .contexts
            .get(digest)
            .is_some_and(|entry| !entry.covered.load(Ordering::Acquire))
        {
            return Err(());
        }
        Ok(None)
    }

    fn insert_context(
        &mut self,
        digest: String,
        entry: Arc<CachedContext>,
    ) -> Result<(), Arc<CachedContext>> {
        if self.contexts.len() >= MAX_CACHED_CONTEXTS && !self.contexts.contains_key(&digest) {
            let Some(oldest) = self
                .contexts
                .iter()
                .filter(|(_, entry)| entry.leases.load(Ordering::Acquire) == 0)
                .min_by_key(|(_, entry)| entry.last_used.load(Ordering::Acquire))
                .map(|(digest, _)| digest.clone())
            else {
                return Err(entry);
            };
            self.contexts.remove(&oldest);
        }
        self.contexts.insert(digest, entry);
        Ok(())
    }
}

/// Connection-scoped caller-context verification using online issuer keys.
///
/// Cached digests retain separate live and historical verification results and
/// require their own exact revocation watch. Authority is keyed by context
/// digest, so a physical transport attachment never gates it.
#[derive(Clone)]
pub struct AuthorizationProviderCache {
    registry: AuthorizationRegistryReader,
    http: BootstrapHttp,
    own: Option<Arc<AuthorizationContextCache>>,
    own_lease: Arc<Mutex<Option<AuthorizationContextLease>>>,
    /// The one pending candidate-owned coverage lease, kept distinct from the
    /// installed `own_lease` until [`Self::finalize_own_installation`] promotes
    /// the exact candidate synchronously. A revoked, superseded, or failed
    /// candidate drops only this lease and never the healthy installed own
    /// coverage.
    own_candidate_lease: Arc<Mutex<Option<PendingOwnLease>>>,
    verification_policy: AuthorizationVerificationPolicy,
    state: Arc<RwLock<ProviderState>>,
    in_flight: Arc<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>>,
    issuer_resolution: Arc<tokio::sync::Mutex<()>>,
    closed: Arc<AtomicBool>,
    context_resolves: Arc<AtomicU64>,
    access_clock: Arc<AtomicU64>,
    coverage_probe: Arc<CoverageProbe>,
    /// Wakes retained live guards when coverage or revocation state moves.
    live_changes: Arc<tokio::sync::broadcast::Sender<()>>,
}

impl AuthorizationProviderCache {
    pub(crate) fn current_context_allows(
        &self,
        digest: &str,
        permission: &trellis_protocol::PermissionAtom,
    ) -> bool {
        self.health().is_ok()
            && self.revocation_time(digest).ok().flatten().is_none()
            && self
                .lease_cached_context(digest, false)
                .ok()
                .flatten()
                .is_some_and(|context| context.allows(permission))
            && self.revocation_time(digest).ok().flatten().is_none()
    }

    /// Retain one live-authority lease for an exact digest.
    ///
    /// Resolves the digest through the ordinary single-flight cache when it is
    /// not already retained, then returns a lease only when the exact entry is
    /// covered (its own continuous revocation watch) and within its validity
    /// window. The caller keeps the lease for the session lifetime, so the
    /// cache's ordinary cleanup retains the revocation watch and coverage
    /// evidence.
    pub(crate) async fn retain_live_guard_lease(
        &self,
        digest: &str,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        let lease = self
            .resolve_context(digest, self.now_seconds()?)
            .await
            .or_else(|error| match error {
                // A retained covered entry is already sufficient; a resolution
                // failure for a second copy must not drop usable retention.
                TrellisClientError::AuthorizationUnavailable(_) => {
                    self.lease_cached_context(digest, false)?.ok_or(error)
                }
                other => Err(other),
            })?;
        // Retained authorization validity follows *coverage*, not the baseline
        // transport's connectedness: a covered entry stays authoritative even
        // while the attachment that admitted it is being replaced. The Live
        // socket's own lifetime is lease-owned separately.
        if lease.context_digest() != digest || !self.live_guard_entry_is_covered(&lease) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "live authority coverage changed before retention".into(),
            ));
        }
        Ok(lease)
    }

    /// Return the consumer's selected provider deployment for one API.
    pub(crate) fn provider_deployment_id(
        &self,
        api_id: &str,
    ) -> Result<String, TrellisClientError> {
        let own = self.own.as_ref().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "selected deployment requires an installed own context".into(),
            )
        })?;
        own.provider_deployment_id(api_id)
    }

    /// Return whether one lease still points at the current covered entry.
    pub(crate) fn live_guard_entry_is_covered(&self, lease: &AuthorizationContextLease) -> bool {
        let Ok(state) = self.read_state() else {
            return false;
        };
        state
            .contexts
            .get(lease.context_digest())
            .is_some_and(|entry| {
                Arc::ptr_eq(entry, lease.entry()) && entry.covered.load(Ordering::Acquire)
            })
    }

    pub(crate) async fn attach(
        nats: async_nats::Client,
        binding: &AuthorizationRegistryBinding,
        own: Arc<AuthorizationContextCache>,
        manager: Option<crate::client::TransportGenerationManager>,
    ) -> Result<Self, TrellisClientError> {
        let bundle = own.bundle()?;
        let policy = bundle
            .policy
            .verification_policy(own.corrected_now_seconds()?)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let cache = Self::open(
            nats,
            binding,
            own.http().clone(),
            Some(bundle.issuer),
            policy,
            Some(own.clone()),
            manager,
        )
        .await?;
        let digest = own.retained_context_digest()?;
        cache.retain_own_context(&digest).await?;
        Ok(cache)
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn attach_runtime(
        nats: async_nats::Client,
        binding: &AuthorizationRegistryBinding,
        trust: RuntimeAuthorizationTrust,
    ) -> Result<Self, TrellisClientError> {
        Self::open(
            nats,
            binding,
            BootstrapHttp::new(&trust.trellis_origin)?,
            trust.issuer,
            trust.policy,
            None,
            None,
        )
        .await
    }

    async fn open(
        nats: async_nats::Client,
        binding: &AuthorizationRegistryBinding,
        http: BootstrapHttp,
        issuer: Option<AuthorizationIssuerKey>,
        verification_policy: AuthorizationVerificationPolicy,
        own: Option<Arc<AuthorizationContextCache>>,
        manager: Option<crate::client::TransportGenerationManager>,
    ) -> Result<Self, TrellisClientError> {
        if let Some(issuer) = &issuer {
            issuer
                .verifying_key()
                .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        }
        let registry = AuthorizationRegistryReader::open(nats.clone(), binding, manager).await?;
        let state = Arc::new(RwLock::new(ProviderState {
            issuers: issuer
                .into_iter()
                .map(|issuer| (issuer.key_id.clone(), issuer))
                .collect(),
            ..Default::default()
        }));
        // The probe mirrors the cache's own lifecycle and coverage; it holds no
        // baseline transport and never reads a socket.
        let coverage_probe = Arc::new(CoverageProbe {
            closed: Arc::new(AtomicBool::new(false)),
        });
        let closed = Arc::new(AtomicBool::new(false));
        register_coverage_gauges(&state, &own, &coverage_probe);
        Ok(Self {
            registry,
            http,
            own,
            own_lease: Arc::new(Mutex::new(None)),
            own_candidate_lease: Arc::new(Mutex::new(None)),
            verification_policy,
            state,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            issuer_resolution: Arc::new(tokio::sync::Mutex::new(())),
            closed,
            context_resolves: Arc::new(AtomicU64::new(0)),
            access_clock: Arc::new(AtomicU64::new(0)),
            coverage_probe,
            live_changes: Arc::new(tokio::sync::broadcast::channel(64).0),
        })
    }

    /// Retain this connection's own coverage for the installed context using the
    /// ordinary exact published current attachment.
    ///
    /// An *unusable* (uncovered) installed lease for the exact digest is released
    /// first so the ordinary resolve path can discard and reload the exact
    /// evidence; a healthy covered same-digest lease stays pinned and is
    /// re-resolved in place. The installed lease is only replaced once the new
    /// coverage is fully ready, so a failed resolution never drops healthy own
    /// coverage.
    pub(crate) async fn retain_own_context(&self, digest: &str) -> Result<(), TrellisClientError> {
        let Some(own) = &self.own else {
            return Ok(());
        };
        {
            let mut held = self.own_lease.lock().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "own context lease lock poisoned".into(),
                )
            })?;
            let unusable_same_digest = held.as_ref().is_some_and(|existing| {
                existing.context_digest() == digest && !self.live_guard_entry_is_covered(existing)
            });
            if unusable_same_digest {
                held.take();
            }
        }
        let lease = self
            .resolve_context_with(
                digest,
                own.corrected_now_seconds()?,
                RegistryAttachment::Published,
            )
            .await?;
        // Cached authority is keyed by digest; the retained lease's own coverage
        // is authoritative, not a socket.
        if lease.context_digest() != digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own context coverage changed before installation".into(),
            ));
        }
        // Replace only once the new coverage is fully ready, so a failed
        // resolution never drops healthy old own coverage.
        let previous = self
            .own_lease
            .lock()
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "own context lease lock poisoned".into(),
                )
            })?
            .replace(lease);
        drop(previous);
        self.notify_live_changes();
        tracing::info!(
            context_digest = digest,
            "retained own authorization coverage"
        );
        Ok(())
    }

    /// Begin a scoped own-candidate installation attempt.
    ///
    /// The returned guard cleans up only this attempt's pending pin if it is
    /// dropped before promotion, so nothing has to be unwound by every early
    /// return on the install path.
    pub(crate) fn begin_own_candidate_retention(&self, digest: &str) -> OwnCandidateRetention {
        OwnCandidateRetention {
            cache: self.clone(),
            digest: digest.to_owned(),
            token: Arc::new(()),
            entry: None,
            armed: true,
        }
    }

    /// Establish the initial revocation coverage for a freshly prepared own
    /// candidate before it is promoted.
    ///
    /// The candidate is not yet the published installation, so its cold registry
    /// read and initial revocation watch may acquire any safe already-admitted
    /// generation of the same logical connection when the exact published current
    /// is physically dead or unsafe. The candidate's own signed policy and
    /// corrected clock decide that safety; the retained (possibly revoked)
    /// predecessor policy is never used as a stand-in.
    ///
    /// The resolved coverage is retained *pending*: only
    /// [`Self::finalize_own_installation`] makes it the installed own lease, so a
    /// candidate that is revoked, superseded, or fails before promotion never
    /// drops the healthy installed own coverage.
    ///
    /// `source` is the single pinned attachment chosen once by
    /// [`crate::client::TransportGenerationManager::prepare_own_coverage`]: both
    /// the cold registry read and the initial revocation watch run on exactly that
    /// attachment under the candidate's own corrected clock, so no survivor is
    /// re-selected mid-warm.
    pub(crate) async fn retain_own_candidate_context(
        &self,
        retention: &mut OwnCandidateRetention,
        source: &PinnedOwnCandidateSource,
    ) -> Result<(), TrellisClientError> {
        let digest = retention.digest.clone();
        let digest = digest.as_str();
        let Some(own) = &self.own else {
            return Ok(());
        };
        // A healthy installed lease already carries this candidate's exact digest
        // (a same-digest renewal); reuse it in place rather than churning the own
        // pin, and drop any stale pending copy. An *unusable* same-digest installed
        // lease is released so the ordinary resolve path can discard and reload the
        // exact evidence.
        let reuse_installed = {
            let mut installed = self.own_lease.lock().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "own context lease lock poisoned".into(),
                )
            })?;
            let same_digest = installed
                .as_ref()
                .is_some_and(|lease| lease.context_digest() == digest);
            if !same_digest {
                false
            } else if installed
                .as_ref()
                .is_some_and(|lease| self.live_guard_entry_is_covered(lease))
            {
                true
            } else {
                installed.take();
                false
            }
        };
        if reuse_installed {
            self.take_pending_own_lease();
            return Ok(());
        }
        // Release any pending candidate lease (superseded, or an unusable
        // same-digest copy) so the ordinary resolve path can discard and reload
        // the exact evidence. Only one candidate can be pending.
        self.take_pending_own_lease();
        let lease = self
            .resolve_context_with(
                digest,
                source.corrected_now_seconds()?,
                RegistryAttachment::PinnedOwnCandidate(source.clone()),
            )
            .await?;
        // Scope this attempt to the exact entry it just resolved *before* any
        // fallible digest/transition validation or pending-slot installation, so
        // a supersession that raced the resolution still lets this attempt's
        // guard reclaim the abandoned, unborrowed, non-installed exact entry even
        // though it never installed a pending pin. No await separates resolution
        // success from this tracking.
        retention.track(lease.entry().clone());
        // Cached authority is keyed by digest; the retained lease's own coverage
        // is authoritative, not a socket.
        if lease.context_digest() != digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own candidate coverage changed before retention".into(),
            ));
        }
        // Retain the pending lease only while this is still the prepared
        // candidate: a promotion or supersession that raced the resolution must
        // not resurrect stale candidate coverage.
        {
            let transition = own.lock_own_transition()?;
            if own.candidate_digest_locked(&transition).ok().as_deref() != Some(digest) {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate changed before coverage retention".into(),
                ));
            }
            self.set_pending_own_lease(retention.token.clone(), lease)?;
        }
        self.notify_live_changes();
        tracing::info!(
            context_digest = digest,
            "retained own candidate authorization coverage"
        );
        Ok(())
    }

    /// Drop the pending candidate-owned coverage lease, if any.
    fn take_pending_own_lease(&self) {
        if let Ok(mut pending) = self.own_candidate_lease.lock() {
            pending.take();
        }
    }

    /// Replace the pending candidate-owned coverage lease, dropping any previous
    /// one so an obsolete pending lease can never outlive a newer candidate.
    fn set_pending_own_lease(
        &self,
        token: Arc<()>,
        lease: AuthorizationContextLease,
    ) -> Result<(), TrellisClientError> {
        let previous = self
            .own_candidate_lease
            .lock()
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "own candidate lease lock poisoned".into(),
                )
            })?
            .replace(PendingOwnLease { token, lease });
        drop(previous);
        Ok(())
    }

    /// Release only this attempt's pending candidate pin and reclaim its
    /// abandoned, unborrowed, non-current cache entry, then wake observers.
    fn release_own_candidate_retention(
        &self,
        digest: &str,
        token: &Arc<()>,
        retained: Option<&Arc<CachedContext>>,
    ) {
        let Some(retained) = retained else {
            return;
        };
        let current = self
            .own
            .as_ref()
            .and_then(|own| own.stored_context_digest().ok());
        if release_candidate_pin(
            &Arc::downgrade(&self.state),
            &self.own_candidate_lease,
            token,
            current.as_deref(),
            digest,
            retained,
        ) {
            self.notify_live_changes();
        }
    }

    pub(crate) async fn run(
        &self,
        mut stop: tokio::sync::watch::Receiver<()>,
    ) -> Result<(), TrellisClientError> {
        let mut cleanup = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = cleanup.tick() => {
                    let now = self.now_seconds()?;
                    let mut state = self.write_state()?;
                    let before = state.contexts.len();
                    // Cached authority is keyed by digest, so cleanup keeps a
                    // covered entry regardless of which transport attachment
                    // carried its coverage; a live lease always pins it. Only a
                    // genuine coverage loss drops an unleased entry.
                    state.contexts.retain(|_, entry| {
                        entry.leases.load(Ordering::Acquire) > 0
                            || entry.covered.load(Ordering::Acquire)
                    });
                    state.revocations.retain(|_, (_, expires_at)| *expires_at > now);
                    let changed = state.contexts.len() != before;
                    drop(state);
                    if changed {
                        self.notify_live_changes();
                    }
                }
            }
        }
        self.closed.store(true, Ordering::Release);
        self.coverage_probe.observe(true);
        Ok(())
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn run_runtime(
        &self,
        stop: tokio::sync::watch::Receiver<()>,
    ) -> Result<(), TrellisClientError> {
        self.run(stop).await
    }

    pub(crate) async fn wait_ready(
        &self,
        stop: tokio::sync::watch::Receiver<()>,
    ) -> Result<(), TrellisClientError> {
        if stop.has_changed().is_err() || !self.health()?.healthy {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider is not running".into(),
            ));
        }
        Ok(())
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn wait_until_ready(&self) -> Result<(), TrellisClientError> {
        if !self.health()?.healthy {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider is not running".into(),
            ));
        }
        Ok(())
    }

    /// Returns the cache's lifecycle health and mirrors coverage for gauges.
    ///
    /// Health is the cache's **own lifecycle** (`!closed`). Cached authority is
    /// keyed by context digest and carried by each entry's continuous watch
    /// coverage, so a physical transport change never vetoes a covered entry.
    /// Entry coverage, revocation evidence and the validity window remain the
    /// authoritative checks; this is a read-only observation that never issues
    /// network reads or changes authority.
    pub(crate) fn health(&self) -> Result<AuthorizationProviderCacheHealth, TrellisClientError> {
        let closed = self.closed.load(Ordering::Acquire);
        self.coverage_probe.observe(closed);
        Ok(AuthorizationProviderCacheHealth { healthy: !closed })
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn runtime_healthy(&self) -> bool {
        self.health().is_ok_and(|health| health.healthy)
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    #[must_use]
    pub fn runtime_io_counters(&self) -> RuntimeAuthorizationIoCounters {
        RuntimeAuthorizationIoCounters {
            context_resolves: self.context_resolves.load(Ordering::Relaxed),
        }
    }

    /// Return the connection's installed own-context digest, if any.
    pub(crate) fn current_local_context_digest(&self) -> Option<String> {
        self.own
            .as_ref()
            .and_then(|own| own.stored_context_digest().ok())
    }

    /// Subscribe to local coverage/revocation changes that can invalidate a
    /// retained live guard. Lagged receivers observe a change and re-check.
    pub(crate) fn subscribe_live_changes(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.live_changes.subscribe()
    }

    fn notify_live_changes(&self) {
        let _ = self.live_changes.send(());
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn runtime_lease_cached_context(
        &self,
        digest: &str,
    ) -> Result<Option<AuthorizationContextLease>, TrellisClientError> {
        self.lease_cached_context(digest, false)
    }

    pub(crate) fn revocation_time(&self, digest: &str) -> Result<Option<i64>, TrellisClientError> {
        self.read_state()?.revocation_time(digest).map_err(|()| {
            TrellisClientError::AuthorizationUnavailable(
                "exact context revocation watch is unavailable".into(),
            )
        })
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn runtime_revocation_time(&self, digest: &str) -> Result<Option<i64>, TrellisClientError> {
        self.revocation_time(digest)
    }

    fn observe_revocation(&self, digest: &str, revoked_at: i64) -> Result<(), TrellisClientError> {
        let deadline = self
            .now_seconds()?
            .saturating_add(i64::from(
                self.verification_policy.maximum_context_lifetime_seconds,
            ))
            .saturating_add(i64::from(
                self.verification_policy.allowed_clock_skew_seconds,
            ));
        let transition = self
            .own
            .as_ref()
            .map(|own| own.lock_own_transition())
            .transpose()?;
        {
            let mut state = self.write_state()?;
            state
                .revocations
                .entry(digest.to_owned())
                .and_modify(|(at, until)| {
                    *at = (*at).max(revoked_at);
                    *until = (*until).max(deadline);
                })
                .or_insert((revoked_at, deadline));
            if let Some(entry) = state.contexts.get(digest) {
                entry.covered.store(false, Ordering::Release);
            }
        }
        if let (Some(own), Some(transition)) = (&self.own, transition.as_ref()) {
            if own
                .stored_context_digest()
                .is_ok_and(|current| current == digest)
            {
                own.mark_revoked(digest);
                // Retire the stale own lease so the suspended installation
                // cannot keep resources or revocation coverage alive.
                if let Ok(mut lease) = self.own_lease.lock() {
                    lease.take();
                }
                own.suspend_locked(transition);
                own.request_refresh();
            } else if own
                .candidate_digest_locked(transition)
                .is_ok_and(|candidate| candidate == digest)
            {
                // A revoked private candidate must never be published, and the
                // still-valid active predecessor stays untouched. Release the
                // abandoned candidate's pending coverage pin so an unleased
                // abandoned initial watch is reclaimed; an independently
                // borrowed entry is left to ordinary make-before-break migration.
                if let Ok(mut pending) = self.own_candidate_lease.lock() {
                    if pending
                        .as_ref()
                        .is_some_and(|pending| pending.lease.context_digest() == digest)
                    {
                        pending.take();
                    }
                }
                own.mark_revoked(digest);
                own.invalidate_candidate_locked(transition, digest);
                own.request_refresh();
            }
        }
        Ok(())
    }

    /// Publish the guarded final own-installation transition.
    ///
    /// Convenience entry that takes the own-installation transition for the
    /// installed-context resume path. Promotion of a private candidate must use
    /// [`Self::finalize_own_installation_locked`] with the exact prepared instance,
    /// the per-attempt token, and a transition the caller already holds, so the
    /// promotion can run inside the manager's state guard.
    pub(crate) fn finalize_own_installation(
        &self,
        expected_digest: &str,
        promote: bool,
    ) -> Result<(), TrellisClientError> {
        let Some(own) = &self.own else {
            return Ok(());
        };
        let transition = own.lock_own_transition()?;
        self.finalize_own_installation_locked(&transition, expected_digest, promote, None, None)
    }

    /// The guard-taking final own-installation transition body.
    ///
    /// Runs on one short local synchronization boundary with no network or HTTP
    /// await: the expected digest must own either the retained *pending* candidate
    /// coverage (a promotion) or the installed own lease (a normal resume) with
    /// initialized live coverage and no stored revocation. For a promotion the
    /// pending candidate lease becomes the installed own lease synchronously inside
    /// this transition and only then is the previous healthy installed lease
    /// released; a normal resume validates the installed retention and changes no
    /// lease.
    ///
    /// The caller already holds `transition`, so a promotion can run inside the
    /// manager's state guard (own-transition -> manager-state -> provider-state)
    /// without re-taking the non-reentrant transition lock. A promotion must supply
    /// the exact prepared `expected_instance` and the exact per-attempt
    /// `expected_token`: the pending slot is consumed only when it is still owned by
    /// that token, so a newer same-digest attempt's pin is never consumed, and the
    /// candidate is promoted only when its instance still matches.
    pub(crate) fn finalize_own_installation_locked(
        &self,
        transition: &OwnTransitionGuard<'_>,
        expected_digest: &str,
        promote: bool,
        expected_instance: Option<&Arc<()>>,
        expected_token: Option<&Arc<()>>,
    ) -> Result<(), TrellisClientError> {
        let Some(own) = &self.own else {
            return Ok(());
        };
        if self.closed.load(Ordering::Acquire) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization provider is stopped".into(),
            ));
        }
        if own.revocation_marker().as_deref() == Some(expected_digest) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context is revoked".into(),
            ));
        }
        if promote && own.candidate_digest_locked(transition)? != expected_digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate changed before publication".into(),
            ));
        }
        // The promotion source is the retained pending candidate coverage; a
        // normal resume validates the installed own lease. A same-digest healthy
        // installed lease is reused in place when no pending copy exists.
        let mut pending = self.own_candidate_lease.lock().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("own candidate lease lock poisoned".into())
        })?;
        let mut installed = self.own_lease.lock().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("own context lease lock poisoned".into())
        })?;
        // A pending pin for this digest must belong to this exact attempt: a newer
        // same-digest attempt's pin is never consumed by an older attempt's swap.
        let pending_same_digest = promote
            && pending
                .as_ref()
                .is_some_and(|pending| pending.lease.context_digest() == expected_digest);
        let token_matches = pending.as_ref().is_some_and(|pending| {
            expected_token.is_some_and(|token| Arc::ptr_eq(&pending.token, token))
        });
        if pending_same_digest && !token_matches {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate coverage lease is unavailable".into(),
            ));
        }
        let from_pending = pending_same_digest && token_matches;
        let source = if from_pending {
            pending.as_ref().map(|pending| &pending.lease)
        } else {
            installed.as_ref()
        };
        let Some(source) = source.filter(|lease| lease.context_digest() == expected_digest) else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization coverage lease is unavailable".into(),
            ));
        };
        // Cached authority is keyed by digest; a transport replacement never
        // changes publication validity. The exact entry identity and its live
        // coverage are rechecked under the same guard as the promotion.
        {
            let state = self.read_state()?;
            if state.revocations.contains_key(expected_digest) {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization context is revoked".into(),
                ));
            }
            match state.contexts.get(expected_digest) {
                Some(entry)
                    if Arc::ptr_eq(entry, source.entry())
                        && entry.covered.load(Ordering::Acquire) => {}
                _ => {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "authorization coverage entry changed before publication".into(),
                    ));
                }
            }
        }
        if promote {
            let instance = expected_instance.ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate instance is unavailable".into(),
                )
            })?;
            own.promote_locked(transition, expected_digest, instance)?;
        } else {
            own.resume_availability_locked(transition, expected_digest)?;
        }
        if from_pending {
            // The validated pending candidate lease becomes the installed own
            // lease; only now is the previous healthy installed lease released.
            let Some(candidate) = pending.take() else {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate coverage lease is unavailable".into(),
                ));
            };
            let previous = installed.replace(candidate.lease);
            drop(previous);
        }
        self.notify_live_changes();
        Ok(())
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn apply_runtime_revocation(
        &self,
        digest: &str,
        revoked_at: i64,
    ) -> Result<(), TrellisClientError> {
        validate_digest_key(digest)?;
        if revoked_at <= 0 {
            return Err(TrellisClientError::Bootstrap(
                "invalid revocation time".into(),
            ));
        }
        self.observe_revocation(digest, revoked_at)
    }

    fn now_seconds(&self) -> Result<i64, TrellisClientError> {
        self.own.as_ref().map_or_else(
            || system_now_millis().map(|now| now.div_euclid(1000)),
            |own| own.corrected_now_seconds(),
        )
    }

    pub(crate) fn policy(&self) -> Result<AuthorizationVerificationPolicy, TrellisClientError> {
        let mut policy = self.verification_policy.clone();
        policy.now_unix_seconds = self.now_seconds()?;
        Ok(policy)
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub fn runtime_policy(&self) -> Result<AuthorizationVerificationPolicy, TrellisClientError> {
        self.policy()
    }

    pub(crate) async fn resolve_context(
        &self,
        digest: &str,
        now: i64,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        self.resolve_context_with(digest, now, RegistryAttachment::Published)
            .await
    }

    /// Resolve one exact digest, acquiring any cold registry IO on the attachment
    /// selected by `attachment`.
    async fn resolve_context_with(
        &self,
        digest: &str,
        now: i64,
        attachment: RegistryAttachment,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        self.resolve_context_for(digest, now, false, attachment)
            .await
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn resolve_admission_context(
        &self,
        digest: &str,
        now: i64,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        self.resolve_context(digest, now).await
    }

    pub(crate) async fn resolve_event_context(
        &self,
        digest: &str,
        event_time: i64,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        self.resolve_context_for(digest, event_time, true, RegistryAttachment::Published)
            .await
    }

    pub(crate) async fn resolve_event_context_for_verification(
        &self,
        digest: &str,
        event_time: i64,
    ) -> Result<AuthorizationContextLease, crate::service::EventVerificationFailure> {
        self.resolve_event_context(digest, event_time)
            .await
            .map_err(classify_event_resolution_failure)
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn runtime_resolve_event_context(
        &self,
        digest: &str,
        event_time: i64,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        self.resolve_event_context(digest, event_time).await
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn runtime_resolve_event_context_for_verification(
        &self,
        digest: &str,
        event_time: i64,
    ) -> Result<AuthorizationContextLease, crate::service::EventVerificationFailure> {
        self.resolve_event_context_for_verification(digest, event_time)
            .await
    }

    async fn resolve_context_for(
        &self,
        digest: &str,
        verification_time: i64,
        historical: bool,
        attachment: RegistryAttachment,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        validate_digest_key(digest)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider is stopped".into(),
            ));
        }
        if self.read_state()?.revocations.contains_key(digest) {
            return Err(TrellisClientError::Bootstrap(
                "authorization context is revoked".into(),
            ));
        }
        if let Some(context) = self.lease_cached_context(digest, historical)? {
            return Ok(context);
        }
        let pending = {
            let mut pending = self.in_flight.lock().map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "provider resolution lock poisoned".into(),
                )
            })?;
            pending.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = pending.get(digest).and_then(Weak::upgrade) {
                lock
            } else {
                if pending.len() >= 32 {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "provider cold-resolution capacity reached".into(),
                    ));
                }
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                pending.insert(digest.to_owned(), Arc::downgrade(&lock));
                lock
            }
        };
        let _guard = pending.lock().await;
        if self.read_state()?.revocations.contains_key(digest) {
            return Err(TrellisClientError::Bootstrap(
                "authorization context is revoked".into(),
            ));
        }
        if let Some(context) = self.lease_cached_context(digest, historical)? {
            return Ok(context);
        }
        self.discard_unusable_context(digest)?;
        tokio::time::timeout(
            Duration::from_secs(30),
            self.resolve_context_once(digest, verification_time, historical, attachment),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
    }

    async fn resolve_context_once(
        &self,
        digest: &str,
        verification_time: i64,
        historical: bool,
        attachment: RegistryAttachment,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        self.context_resolves.fetch_add(1, Ordering::Relaxed);
        // A scoped own-candidate warm verifies under the candidate's own corrected
        // clock, never the promoted predecessor's: the immutable server-clock offset
        // is recomputed into a fresh "now" here rather than a timestamp captured
        // before the warm's awaits.
        let candidate_clock_offset = match &attachment {
            RegistryAttachment::PinnedOwnCandidate(source) => Some(source.clock_offset_ms()),
            RegistryAttachment::Published => None,
        };
        // The registry is eventually consistent, so an immutable context that
        // was just published may not be readable from this connection yet. Wait
        // a bounded window for it to become visible before reporting it missing,
        // so a freshly provisioned identity (for example a built-in live
        // provider during startup) is not denied while publication converges.
        // ponytail: fixed 5s convergence window; widen or make configurable if a
        // real propagation lag exceeds it.
        let value = {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(value) = self
                    .registry
                    .get_context(digest, attachment.clone())
                    .await?
                {
                    break value;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "context is missing from the registry".into(),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        let mut policy = self.policy()?;
        if value.len() > policy.maximum_context_bytes {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context exceeds size limit".into(),
            ));
        }
        let signed = parse_registry_context(&value)?;
        if signed.digest().map_err(|error| {
            TrellisClientError::AuthorizationUnavailable(format!(
                "authorization context registry entry is malformed: {error}"
            ))
        })? != digest
        {
            return Err(TrellisClientError::Bootstrap(
                "authorization context digest does not match its registry key".into(),
            ));
        }
        let key_id = &signed.unsigned.issuer_key_id;
        let known = self.read_state()?.issuers.get(key_id).cloned();
        let issuer = if let Some(issuer) = known {
            issuer
        } else {
            let _guard = self.issuer_resolution.lock().await;
            let known = self.read_state()?.issuers.get(key_id).cloned();
            if let Some(issuer) = known {
                issuer
            } else {
                let issuer = self.http.issuer_key(key_id).await?;
                self.write_state()?
                    .issuers
                    .insert(key_id.clone(), issuer.clone());
                issuer
            }
        };
        let now = match candidate_clock_offset {
            Some(offset) => system_now_millis()?
                .checked_add(offset)
                .ok_or_else(|| TrellisClientError::Bootstrap("context time overflow".into()))?
                .div_euclid(1000),
            None => self.now_seconds()?,
        };
        let live = issuer.state == AuthorizationIssuerState::Active
            && signed.unsigned.not_before <= now
            && signed.unsigned.expires_at > now;
        let candidate_owned = matches!(&attachment, RegistryAttachment::PinnedOwnCandidate(_));
        let mut watch = self
            .registry
            .watch_revocation(digest, None, attachment)
            .await?;
        loop {
            match watch.next().await {
                Some(Ok(RegistryWatchEvent::Initialized)) => break,
                Some(Ok(RegistryWatchEvent::Entry(entry)))
                    if !entry.removed
                        && entry.revision > 0
                        && entry.key == format!("{REVOCATION_PREFIX}{digest}") =>
                {
                    self.observe_revocation(digest, parse_revocation_record(&entry.value)?)?;
                }
                Some(Ok(RegistryWatchEvent::Entry(_))) => {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "authorization revocation evidence is unusable".into(),
                    ));
                }
                Some(Err(error)) => return Err(error),
                None => {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "authorization revocation watch ended during initialization".into(),
                    ));
                }
            }
        }
        if self.revocation_time(digest)?.is_some() {
            return Err(TrellisClientError::Bootstrap(
                "authorization context is revoked".into(),
            ));
        }
        // Cached authority is keyed by digest and carried by the entry's own
        // continuous coverage, so a transport replacement during resolution does
        // not invalidate the result. The finite registry IO already went through
        // the manager's attachment.
        if self.closed.load(Ordering::Acquire) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider stopped during context resolution".into(),
            ));
        }
        let purpose = if historical {
            AuthorizationContextPurpose::HistoricalEvent
        } else {
            AuthorizationContextPurpose::Live
        };
        policy.now_unix_seconds = if historical { verification_time } else { now };
        let verified = verify_authorization_context(&issuer, &signed, &policy, purpose)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        if !historical && !live {
            return Err(TrellisClientError::Bootstrap(
                "authorization context is not current".into(),
            ));
        }
        let covered = Arc::new(AtomicBool::new(true));
        let weak_state = Arc::downgrade(&self.state);
        let own = self.own.as_ref().map(Arc::downgrade);
        let watch_covered = covered.clone();
        let watch_digest = digest.to_owned();
        let revocation_deadline = now
            .saturating_add(i64::from(
                self.verification_policy.maximum_context_lifetime_seconds,
            ))
            .saturating_add(i64::from(
                self.verification_policy.allowed_clock_skew_seconds,
            ));
        // The task owns only weak cache references; dropping the entry aborts it.
        let observation = CoverageObservation {
            state: weak_state,
            own,
            covered: watch_covered,
            digest: watch_digest,
            live_changes: self.live_changes.clone(),
            closed: self.closed.clone(),
            pending_lease: self.own_candidate_lease.clone(),
            candidate_owned,
        };
        let task = tokio::spawn(observe_context_revocation(
            watch,
            observation,
            revocation_deadline,
        ));
        let mut verifications = CachedVerifications::default();
        if historical {
            verifications.historical = Some(verified.clone());
        } else {
            verifications.live = Some(verified.clone());
        }
        let entry = Arc::new(CachedContext {
            signed,
            issuer,
            verified: Mutex::new(verifications),
            covered,
            watch: task.abort_handle(),
            leases: AtomicUsize::new(1),
            last_used: AtomicU64::new(self.next_access()),
        });
        let mut state = self.write_state()?;
        if state.revocations.contains_key(digest) || !entry.covered.load(Ordering::Acquire) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "revocation coverage changed during resolution".into(),
            ));
        }
        // [`Self::discard_unusable_context`] already detached any predecessor, so
        // installing the resolved entry can no longer collide with a stale leased
        // entry for the same digest.
        state
            .insert_context(digest.to_owned(), entry.clone())
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "provider context cache capacity reached".into(),
                )
            })?;
        Ok(AuthorizationContextLease {
            entry,
            context: verified,
        })
    }

    fn lease_cached_context(
        &self,
        digest: &str,
        historical: bool,
    ) -> Result<Option<AuthorizationContextLease>, TrellisClientError> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(None);
        }
        let now = self.now_seconds()?;
        let state = self.write_state()?;
        let Some(entry) = state.contexts.get(digest) else {
            return Ok(None);
        };
        if !entry.covered.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut verifications = entry.verified.lock().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable(
                "provider verification cache lock poisoned".into(),
            )
        })?;
        let cached = if historical {
            &mut verifications.historical
        } else {
            &mut verifications.live
        };
        if cached.is_none() {
            let mut policy = self.policy()?;
            policy.now_unix_seconds = if historical {
                policy.now_unix_seconds
            } else {
                now
            };
            *cached = Some(
                verify_authorization_context(
                    &entry.issuer,
                    &entry.signed,
                    &policy,
                    if historical {
                        AuthorizationContextPurpose::HistoricalEvent
                    } else {
                        AuthorizationContextPurpose::Live
                    },
                )
                .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?,
            );
        }
        let context = cached.clone().ok_or_else(|| {
            TrellisClientError::AuthorizationUnavailable(
                "provider verification result is unavailable".into(),
            )
        })?;
        if !historical && (context.not_before() > now || context.expires_at() <= now) {
            return Ok(None);
        }
        entry.leases.fetch_add(1, Ordering::AcqRel);
        entry.last_used.store(self.next_access(), Ordering::Release);
        Ok(Some(AuthorizationContextLease {
            entry: entry.clone(),
            context,
        }))
    }

    fn next_access(&self) -> u64 {
        self.access_clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Detach an indexed entry that cannot serve new leases for the current
    /// transport.
    ///
    /// Reaching here means [`Self::lease_cached_context`] rejected the entry as
    /// uncovered or expired. Cached authority is keyed by context digest, not by
    /// physical transport generation, so a transport replacement never detaches a
    /// valid digest: an entry that is merely uncovered or expired keeps the
    /// fail-closed rule and a live lease pins it until its holder releases it, so
    /// unusable evidence is never silently replaced.
    fn discard_unusable_context(&self, digest: &str) -> Result<(), TrellisClientError> {
        let mut state = self.write_state()?;
        let Some(entry) = state.contexts.get(digest) else {
            return Ok(());
        };
        if entry.leases.load(Ordering::Acquire) > 0 {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider context is still leased".into(),
            ));
        }
        state.contexts.remove(digest);
        Ok(())
    }

    fn read_state(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, ProviderState>, TrellisClientError> {
        self.state.read().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("provider state lock poisoned".into())
        })
    }

    fn write_state(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, ProviderState>, TrellisClientError> {
        self.state.write().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("provider state lock poisoned".into())
        })
    }
}

/// Shared owner state one coverage observer needs to reach and commit to its
/// cached entry, grouped so the observer and its commit helper stay bounded.
struct CoverageObservation {
    state: Weak<RwLock<ProviderState>>,
    own: Option<Weak<AuthorizationContextCache>>,
    covered: Arc<AtomicBool>,
    digest: String,
    live_changes: Arc<tokio::sync::broadcast::Sender<()>>,
    closed: Arc<AtomicBool>,
    /// The provider's pending own-candidate retention slot, reachable from a
    /// terminal coverage observation so an abandoned candidate's pin is released.
    pending_lease: Arc<Mutex<Option<PendingOwnLease>>>,
    /// Whether this binding initialized coverage for a private own candidate.
    candidate_owned: bool,
}

/// How long a failed coverage-replacement preparation is paced before it is
/// retried against an unchanged published target.
const REPLACEMENT_RETRY: Duration = Duration::from_millis(100);

/// Observe one coverage binding to its terminal revocation/error/end.
///
/// The binding follows the connection's exact published current generation: an
/// already-published target that this binding is not leased on is reconciled
/// before any await, a same-id renewal is a no-op, and no published current is
/// not coverage loss. Throughout preparation and initialization the
/// authoritative old binding stays observed and leased; only a successor that
/// initializes and then commits under the manager's publication guard replaces
/// it, and a failed preparation leaves the healthy old binding in force and
/// retries the same target on a bounded pace while old-watch and publication
/// events are still selected.
async fn observe_context_revocation(
    mut watch: super::registry::RegistryWatch,
    observation: CoverageObservation,
    revocation_deadline: i64,
) {
    let own = observation.own.as_ref().and_then(|own| own.upgrade());
    let reader = watch.reader();
    let mut publication = watch.subscribe_publication();
    // Wakes this binding when retention, promotion, or coverage state moves, so
    // a deferred candidate handover is reevaluated without new traffic.
    let mut live_changes = observation.live_changes.subscribe();
    // The published target whose preparation most recently failed, with the
    // deadline for its bounded retry. A different target is attempted
    // immediately rather than waiting out this interval.
    let mut retry: Option<(u64, tokio::time::Instant)> = None;
    let entry = loop {
        if observation.closed.load(Ordering::Acquire) {
            // The provider stopped and its entries are being dropped: there is
            // no coverage left to hand over.
            return;
        }
        // Reconcile the retained published identity against the exact generation
        // this binding is leased on *before* awaiting anything. A terminal
        // logical connection cancels its retry but still observes the old watch
        // to its own loss.
        let target = if reader.transport_terminal() {
            None
        } else if observation.candidate_owned
            && own.as_ref().is_some_and(|own| {
                own.candidate_digest().ok().as_deref() == Some(observation.digest.as_str())
            })
        {
            // The binding carries the initial coverage of a still-private own
            // candidate. Following the published current now could migrate it
            // onto a socket classified under the revoked or wider predecessor
            // policy, so keep observing the candidate's own safe initialized
            // watch until it is promoted or abandoned.
            None
        } else {
            // A different published current is the target to follow; no receiver,
            // no published current, and a same-id renewal are all no-ops.
            publication
                .as_mut()
                .and_then(|receiver| *receiver.borrow_and_update())
                .filter(|target| Some(*target) != watch.generation_id())
        };
        if let Some(target) = target {
            let due = retry.is_none_or(|(failed, deadline)| {
                failed != target || tokio::time::Instant::now() >= deadline
            });
            if due {
                retry = None;
                let commit =
                    |binding: &mut super::registry::RegistryWatch,
                     successor: Box<super::registry::RegistryWatch>| {
                        commit_replacement(
                            &reader,
                            binding,
                            successor,
                            target,
                            &observation,
                            own.as_ref(),
                        )
                    };
                match watch.hand_over(target, commit).await {
                    super::registry::CoverageHandover::Observed(event) => break event,
                    super::registry::CoverageHandover::Retained
                    | super::registry::CoverageHandover::Superseded => continue,
                    super::registry::CoverageHandover::SetupFailed => {
                        retry = Some((target, tokio::time::Instant::now() + REPLACEMENT_RETRY));
                        continue;
                    }
                }
            }
        } else {
            retry = None;
        }
        // Keep observing the authoritative binding and the publication while a
        // retry is paced: an old-watch revocation/error/end remains immediate and
        // a new published target is picked up without waiting for the retry.
        tokio::select! {
            event = futures_util::StreamExt::next(&mut watch) => break event,
            _ = super::registry::publication_changed(&mut publication) => {}
            _ = retry_sleep(retry) => {}
            _ = live_changes.recv() => {}
        }
    };
    apply_terminal_observation(
        entry,
        &observation.state,
        own.as_ref(),
        &observation.covered,
        &observation.digest,
        revocation_deadline,
        &observation.live_changes,
        &observation.pending_lease,
    );
}

/// Sleep until a paced replacement retry is due, or forever when none is set.
async fn retry_sleep(retry: Option<(u64, tokio::time::Instant)>) {
    match retry {
        Some((_, deadline)) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

/// Commit one initialized replacement under the manager's publication guard.
///
/// The own-installation transition is acquired first and held across the whole
/// commit, so a concurrent authorization promotion (which takes the same guard)
/// cannot replace the policy or clock the manager reads under its publication
/// guard. That guard then runs the callback with the published current pinned, so
/// a cutover cannot interleave between its final comparison and the swap. Under
/// it the existing own-transition -> manager-state -> provider-state order
/// revalidates that the original cache entry is still the authoritative covered
/// binding for `observation.digest` and that the cache is still running, then
/// swaps the binding. A refusal discards the provisional watcher and its exact
/// lease and preserves the healthy original. No cache entry, coverage flag, live
/// lease, verifier handle or generation is replaced.
fn commit_replacement(
    reader: &AuthorizationRegistryReader,
    binding: &mut super::registry::RegistryWatch,
    successor: Box<super::registry::RegistryWatch>,
    expected: u64,
    observation: &CoverageObservation,
    own: Option<&Arc<AuthorizationContextCache>>,
) -> bool {
    let Some(own) = own else {
        return false;
    };
    let transition = match own.lock_own_transition() {
        Ok(transition) => transition,
        Err(_) => return false,
    };
    reader
        .commit_published_if_current(&transition, expected, || {
            if observation.closed.load(Ordering::Acquire) {
                return false;
            }
            let Some(provider) = observation.state.upgrade() else {
                return false;
            };
            let Ok(state) = provider.write() else {
                return false;
            };
            let authoritative = state
                .contexts
                .get(&observation.digest)
                .is_some_and(|entry| Arc::ptr_eq(&entry.covered, &observation.covered))
                && observation.covered.load(Ordering::Acquire);
            if !authoritative {
                return false;
            }
            let old = std::mem::replace(binding, *successor);
            drop(old);
            true
        })
        .unwrap_or(false)
}

/// Release exactly one attempt's pending candidate pin and reclaim its
/// abandoned coverage entry.
///
/// `token` identifies the attempt doing the cleanup and `retained` is the exact
/// entry it pinned. The pending slot is only emptied when it is still owned by
/// that exact attempt token, so a newer candidate's pending slot — including a
/// signed-identical replacement that reused the very same cache entry — is never
/// discarded. The entry is removed from the index only when it is still the
/// exact indexed entry, is unborrowed, and `current_digest` does not name it, so
/// a borrowed or installed healthy context is preserved. Returns whether
/// anything changed so the caller can wake observers.
fn release_candidate_pin(
    weak_state: &Weak<RwLock<ProviderState>>,
    pending_lease: &Arc<Mutex<Option<PendingOwnLease>>>,
    token: &Arc<()>,
    current_digest: Option<&str>,
    digest: &str,
    retained: &Arc<CachedContext>,
) -> bool {
    let mut changed = false;
    if let Ok(mut pending) = pending_lease.lock() {
        if pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(&pending.token, token))
        {
            pending.take();
            changed = true;
        }
    }
    if current_digest != Some(digest) {
        if let Some(state) = weak_state.upgrade() {
            if let Ok(mut state) = state.write() {
                let reclaim = state.contexts.get(digest).is_some_and(|indexed| {
                    Arc::ptr_eq(indexed, retained) && indexed.leases.load(Ordering::Acquire) == 0
                });
                if reclaim {
                    state.contexts.remove(digest);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Apply one terminal coverage observation for one watch binding.
///
/// `entry` is the single decoded observation the real observer saw: either a
/// genuine revocation record for `watch_digest`, or any other (or absent) event
/// meaning a binding-local coverage loss. Genuine revocation evidence is
/// digest-global and marks every indexed entry for the digest unusable, so it
/// reaches a successor coverage that replaced a retired binding. A binding-local
/// loss only affects the authoritative binding that actually lost coverage, so a
/// retired binding's loss cannot suspend a successor coverage or a newer own
/// installation. This is the production bookkeeping the observer invokes after it
/// observes the event; it is not a runtime constructor.
#[allow(clippy::too_many_arguments)]
fn apply_terminal_observation(
    entry: Option<Result<RegistryWatchEvent, TrellisClientError>>,
    weak_state: &Weak<RwLock<ProviderState>>,
    own: Option<&Arc<AuthorizationContextCache>>,
    watch_covered: &Arc<AtomicBool>,
    watch_digest: &str,
    revocation_deadline: i64,
    live_changes: &tokio::sync::broadcast::Sender<()>,
    pending_lease: &Arc<Mutex<Option<PendingOwnLease>>>,
) {
    let revoked_at = match entry {
        Some(Ok(RegistryWatchEvent::Entry(entry))) => genuine_revocation_at(&entry, watch_digest),
        _ => None,
    };
    let transition = match own {
        Some(own) => match own.lock_own_transition() {
            Ok(transition) => Some(transition),
            Err(error) => {
                tracing::warn!(%error, "own transition lock is unavailable for invalidation");
                None
            }
        },
        None => None,
    };
    // A terminal coverage observation for the exact entry a pending own-candidate
    // lease pins releases that pin before the entry is considered for removal, so
    // an abandoned candidate's unleased watch is reclaimed. A retired binding's
    // loss (different coverage identity) or an independently borrowed entry is
    // left to ordinary make-before-break migration.
    if let Ok(mut pending) = pending_lease.lock() {
        if pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(&pending.lease.entry().covered, watch_covered))
        {
            pending.take();
        }
    }
    // Capture the indexed-entry identity before any removal, then apply coverage
    // and negative evidence inside the same transition boundary.
    let was_current_entry = if let Some(state) = weak_state.upgrade() {
        if let Ok(mut state) = state.write() {
            let current = state
                .contexts
                .get(watch_digest)
                .is_some_and(|entry| Arc::ptr_eq(&entry.covered, watch_covered));
            if let Some(at) = revoked_at {
                state
                    .revocations
                    .insert(watch_digest.to_owned(), (at, revocation_deadline));
                // Genuine revocation evidence applies to every indexed entry for
                // the digest, including a successor coverage that replaced the
                // callback's own retired watch entry.
                if let Some(indexed) = state.contexts.get(watch_digest) {
                    indexed.covered.store(false, Ordering::Release);
                }
            }
            watch_covered.store(false, Ordering::Release);
            if state.contexts.get(watch_digest).is_some_and(|entry| {
                Arc::ptr_eq(&entry.covered, watch_covered)
                    && entry.leases.load(Ordering::Acquire) == 0
            }) {
                state.contexts.remove(watch_digest);
            }
            current
        } else {
            false
        }
    } else {
        false
    };
    // Retired coverage loss is not evidence about the digest: only a genuine
    // revocation may invalidate an own installation from a retired watch.
    if revoked_at.is_none() && !was_current_entry {
        return;
    }
    // Wake retained live guards so a quiet session fences without waiting for
    // the next frame or a timer tick.
    let _ = live_changes.send(());
    if let (Some(own), Some(transition)) = (own, transition.as_ref()) {
        if own
            .stored_context_digest()
            .is_ok_and(|digest| digest == watch_digest)
        {
            if revoked_at.is_some() {
                own.mark_revoked(watch_digest);
            }
            own.suspend_locked(transition);
            if revoked_at.is_some() {
                own.request_refresh();
            } else {
                own.request_coverage_reconciliation();
            }
        } else if own
            .candidate_digest_locked(transition)
            .is_ok_and(|digest| digest == watch_digest)
        {
            if revoked_at.is_some() {
                own.mark_revoked(watch_digest);
            }
            own.invalidate_candidate_locked(transition, watch_digest);
            own.request_refresh();
        }
    }
}

fn classify_event_resolution_failure(
    error: TrellisClientError,
) -> crate::service::EventVerificationFailure {
    if matches!(
        error,
        TrellisClientError::AuthorizationUnavailable(_)
            | TrellisClientError::BootstrapHttp { .. }
            | TrellisClientError::Io(_)
            | TrellisClientError::Nats(_)
            | TrellisClientError::NatsConnect(_)
            | TrellisClientError::NatsRequest(_)
            | TrellisClientError::ServiceUnavailable(_)
            | TrellisClientError::Timeout
    ) {
        crate::service::EventVerificationFailure::retryable(error.to_string())
    } else {
        crate::service::EventVerificationFailure::rejected(error.to_string())
    }
}

fn parse_registry_context(value: &[u8]) -> Result<SignedAuthorizationContext, TrellisClientError> {
    let json = serde_json::from_slice(value).map_err(|error| {
        TrellisClientError::AuthorizationUnavailable(format!(
            "authorization context registry entry is malformed: {error}"
        ))
    })?;
    parse_authorization_context(&json).map_err(|error| {
        TrellisClientError::AuthorizationUnavailable(format!(
            "authorization context registry entry is malformed: {error}"
        ))
    })
}

/// Classify one watch entry as genuine revocation evidence for `digest`.
///
/// Returns `Some(revoked_at)` only for a valid, present revocation record on the
/// exact key with a positive revision. A malformed or removed entry yields
/// `None`, so a provisional source's unusable evidence is never treated as
/// digest-global revocation.
pub(crate) fn genuine_revocation_at(entry: &RegistryWatchEntry, digest: &str) -> Option<i64> {
    if entry.removed || entry.revision == 0 || entry.key != format!("{REVOCATION_PREFIX}{digest}") {
        return None;
    }
    parse_revocation_record(&entry.value).ok()
}

fn parse_revocation_record(value: &[u8]) -> Result<i64, TrellisClientError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Record {
        revoked_at: i64,
    }
    let record: Record = serde_json::from_slice(value)
        .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
    if record.revoked_at <= 0 {
        return Err(TrellisClientError::AuthorizationUnavailable(
            "invalid context revocation record".into(),
        ));
    }
    Ok(record.revoked_at)
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    fn test_context() -> (
        SignedAuthorizationContext,
        AuthorizationIssuerKey,
        VerifiedAuthorizationContext,
    ) {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../integration/fixtures/protocol/authorization-context/vectors.json"
        ))
        .unwrap();
        let complete = &vectors["completeChain"];
        let value: serde_json::Value =
            serde_json::from_str(complete["contextCanonicalJson"].as_str().unwrap()).unwrap();
        let signed = parse_authorization_context(&value).unwrap();
        let issuer = AuthorizationIssuerKey {
            key_id: complete["issuerKeyId"].as_str().unwrap().to_owned(),
            public_key: complete["issuerPublicKey"].as_str().unwrap().to_owned(),
            state: AuthorizationIssuerState::Active,
        };
        let policy = AuthorizationVerificationPolicy::new(1_200, 30, 1_000, 100_000, 100).unwrap();
        let verified = verify_authorization_context(
            &issuer,
            &signed,
            &policy,
            AuthorizationContextPurpose::Live,
        )
        .unwrap();
        (signed, issuer, verified)
    }

    fn test_entry(
        signed: &SignedAuthorizationContext,
        issuer: &AuthorizationIssuerKey,
        verified: &VerifiedAuthorizationContext,
        last_used: u64,
        cancelled: Arc<AtomicBool>,
    ) -> Arc<CachedContext> {
        struct Cancellation(Arc<AtomicBool>);

        impl Drop for Cancellation {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let cancellation = Cancellation(cancelled);
        let task = tokio::spawn(async move {
            let _cancellation = cancellation;
            std::future::pending::<()>().await;
        });
        Arc::new(CachedContext {
            signed: signed.clone(),
            issuer: issuer.clone(),
            verified: Mutex::new(CachedVerifications {
                live: Some(verified.clone()),
                historical: None,
            }),
            covered: Arc::new(AtomicBool::new(true)),
            watch: task.abort_handle(),
            leases: AtomicUsize::new(0),
            last_used: AtomicU64::new(last_used),
        })
    }

    #[tokio::test]
    async fn peer_coverage_requires_live_signed_interval_active_issuer_and_no_revocation() {
        let (signed, issuer, verified) = test_context();
        let entry = test_entry(&signed, &issuer, &verified, 0, Arc::default());
        let probe = CoverageProbe {
            closed: Arc::new(AtomicBool::new(false)),
        };
        let now = signed.unsigned.not_before;
        assert!(probe.peer_is_live(&entry, now, false));
        assert!(!probe.peer_is_live(&entry, now - 1, false));
        assert!(!probe.peer_is_live(&entry, signed.unsigned.expires_at, false));
        assert!(!probe.peer_is_live(&entry, now, true));
        let mut inactive = issuer.clone();
        inactive.state = AuthorizationIssuerState::Retired;
        let inactive_entry = test_entry(&signed, &inactive, &verified, 0, Arc::default());
        assert!(!probe.peer_is_live(&inactive_entry, now, false));
        assert_eq!(entry.leases.load(Ordering::Acquire), 0);
        assert_eq!(entry.last_used.load(Ordering::Acquire), 0);
    }

    #[test]
    fn revocation_is_additively_tolerant() {
        assert_eq!(
            parse_revocation_record(br#"{"revokedAt":123}"#).unwrap(),
            123
        );
        assert_eq!(
            parse_revocation_record(br#"{"revokedAt":123,"future":true}"#).unwrap(),
            123
        );
        assert!(parse_revocation_record(br#"{"revokedAt":0}"#).is_err());
    }

    #[test]
    fn unavailable_evidence_requires_redelivery_but_invalid_digest_does_not() {
        for error in [
            TrellisClientError::AuthorizationUnavailable("watch disconnected".into()),
            TrellisClientError::BootstrapHttp {
                status: 503,
                code: "unavailable".into(),
            },
            TrellisClientError::BootstrapHttp {
                status: 404,
                code: "key_not_found".into(),
            },
        ] {
            assert!(matches!(
                classify_event_resolution_failure(error),
                crate::service::EventVerificationFailure::Retryable(_)
            ));
        }
        assert!(matches!(
            classify_event_resolution_failure(validate_digest_key("invalid").unwrap_err()),
            crate::service::EventVerificationFailure::Rejected(_)
        ));
    }

    #[test]
    fn malformed_registry_context_is_unavailable() {
        for value in [br#"{"#.as_slice(), br#"{}"#.as_slice()] {
            assert!(matches!(
                parse_registry_context(value),
                Err(TrellisClientError::AuthorizationUnavailable(_))
            ));
        }
    }

    #[tokio::test]
    async fn initialized_watch_put_revokes_cached_context() {
        let (signed, issuer, verified) = test_context();
        let entry = test_entry(&signed, &issuer, &verified, 0, Arc::default());
        let covered = entry.covered.clone();
        let state = Arc::new(RwLock::new(ProviderState::default()));
        state
            .write()
            .unwrap()
            .contexts
            .insert("digest".into(), entry);

        // The production terminal bookkeeping applied to the same single decoded
        // revocation observation the real observer would see.
        apply_terminal_observation(
            Some(Ok(RegistryWatchEvent::Entry(
                super::super::registry::RegistryWatchEntry {
                    key: "revocation.digest".into(),
                    value: br#"{"revokedAt":1150}"#.to_vec(),
                    removed: false,
                    revision: 2,
                },
            ))),
            &Arc::downgrade(&state),
            None,
            &covered,
            "digest",
            2_000,
            &Arc::new(tokio::sync::broadcast::channel(4).0),
            &Arc::new(Mutex::new(None)),
        );

        assert!(!covered.load(Ordering::Acquire));
        let state = state.read().unwrap();
        assert_eq!(state.revocation_time("digest"), Ok(Some(1_150)));
        assert!(!state.contexts.contains_key("digest"));
    }

    #[tokio::test]
    async fn bounded_lru_retains_leased_revoked_context_until_release() {
        let (signed, issuer, verified) = test_context();
        let cancellations = (0..=MAX_CACHED_CONTEXTS)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let mut state = ProviderState::default();
        let first = test_entry(&signed, &issuer, &verified, 0, cancellations[0].clone());
        first.leases.store(1, Ordering::Release);
        assert!(state.insert_context("0".into(), first.clone()).is_ok());
        let lease = AuthorizationContextLease {
            entry: first.clone(),
            context: verified.clone(),
        };
        for (index, cancellation) in cancellations
            .iter()
            .enumerate()
            .take(MAX_CACHED_CONTEXTS)
            .skip(1)
        {
            assert!(state
                .insert_context(
                    index.to_string(),
                    test_entry(
                        &signed,
                        &issuer,
                        &verified,
                        index as u64,
                        cancellation.clone(),
                    ),
                )
                .is_ok());
        }

        assert!(state
            .insert_context(
                MAX_CACHED_CONTEXTS.to_string(),
                test_entry(
                    &signed,
                    &issuer,
                    &verified,
                    MAX_CACHED_CONTEXTS as u64,
                    cancellations[MAX_CACHED_CONTEXTS].clone(),
                ),
            )
            .is_ok());
        assert_eq!(state.contexts.len(), MAX_CACHED_CONTEXTS);
        assert!(state.contexts.contains_key("0"));
        assert!(!state.contexts.contains_key("1"));
        assert!(!cancellations[0].load(Ordering::Acquire));

        state.revocations.insert("0".into(), (1_201, 2_000));
        first.covered.store(false, Ordering::Release);
        assert_eq!(state.revocation_time("0"), Ok(Some(1_201)));
        assert!(state.contexts.contains_key("0"));

        drop(lease);
        drop(first);
        assert!(state
            .insert_context(
                "next".into(),
                test_entry(&signed, &issuer, &verified, 257, Arc::default()),
            )
            .is_ok());
        assert!(!state.contexts.contains_key("0"));
        tokio::task::yield_now().await;
        assert!(cancellations[0].load(Ordering::Acquire));
        assert!(cancellations[1].load(Ordering::Acquire));
    }
}

/// One live provider-cache registration for the aggregate coverage gauge.
type CoverageRegistry = std::sync::Mutex<Vec<WeakCoverageEntry>>;

/// Weak references to one live provider cache.
struct WeakCoverageEntry {
    state: std::sync::Weak<RwLock<ProviderState>>,
    own: std::sync::Weak<AuthorizationContextCache>,
    probe: std::sync::Weak<CoverageProbe>,
}

/// Transport-free lifecycle mirror for the aggregate coverage gauge.
///
/// The provider owner updates these atomics from its existing state; the gauge
/// reads them without holding the transport, issuing network reads, or keeping
/// a credential alive after the owner is gone.
pub(crate) struct CoverageProbe {
    /// Whether the provider owner has been closed.
    closed: Arc<AtomicBool>,
}

impl CoverageProbe {
    /// Updates the mirrored lifecycle state from the live owner.
    pub(crate) fn observe(&self, closed: bool) {
        self.closed.store(closed, Ordering::Release);
    }

    /// Whether one peer entry is live coverage.
    ///
    /// Coverage is the entry's own continuous watch evidence plus its validity
    /// window and revocation state; it never depends on a physical transport.
    fn peer_is_live(&self, entry: &CachedContext, now: i64, revoked: bool) -> bool {
        !revoked
            && entry.covered.load(Ordering::Acquire)
            && entry.issuer.state == AuthorizationIssuerState::Active
            && entry.signed.unsigned.not_before <= now
            && entry.signed.unsigned.expires_at > now
            && !self.closed.load(Ordering::Acquire)
    }
}

static COVERAGE_REGISTRY: std::sync::OnceLock<CoverageRegistry> = std::sync::OnceLock::new();

/// Process-lifetime coverage gauge registration.
///
/// Retained for the process lifetime: dropping it would remove the family
/// callback while the registry keeps updating.
static COVERAGE_GAUGE: std::sync::OnceLock<crate::telemetry::instruments::ObservationRegistration> =
    std::sync::OnceLock::new();

/// Registers one provider-cache pair for the aggregate coverage gauge.
///
/// The registry holds weak references only, so a dropped client disappears
/// from the aggregate without any deregistration step and no connection or
/// credential is retained strongly by telemetry.
fn register_coverage_gauges(
    state: &std::sync::Arc<RwLock<ProviderState>>,
    own: &Option<std::sync::Arc<AuthorizationContextCache>>,
    probe: &std::sync::Arc<CoverageProbe>,
) {
    let registry = COVERAGE_REGISTRY.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut entries) = registry.lock() {
        entries.push(WeakCoverageEntry {
            state: std::sync::Arc::downgrade(state),
            own: own
                .as_ref()
                .map_or_else(std::sync::Weak::new, std::sync::Arc::downgrade),
            probe: std::sync::Arc::downgrade(probe),
        });
    }
    COVERAGE_GAUGE.get_or_init(|| {
        crate::telemetry::instruments::register_observable(
            crate::telemetry::instruments::ObservableFamily::AuthCoverageCount,
            std::sync::Arc::new(|| {
                let mut own_covered = 0u64;
                let mut own_unavailable = 0u64;
                let mut peer_covered = 0u64;
                let mut peer_unavailable = 0u64;
                if let Some(registry) = COVERAGE_REGISTRY.get() {
                    if let Ok(mut entries) = registry.lock() {
                        // Prune dead weak entries so a closed client stops
                        // contributing without a deregistration step.
                        entries.retain(|entry| {
                            entry.state.strong_count() > 0 || entry.own.strong_count() > 0
                        });
                        for entry in entries.iter() {
                            let own = entry.own.upgrade();
                            // Read the corrected clock before acquiring provider state.
                            let now = own.as_ref().map_or_else(
                                || system_now_millis().map(|ms| ms.div_euclid(1000)),
                                |own| own.corrected_now_seconds(),
                            );
                            if let Some(own) = own.as_ref() {
                                // The authoritative own-installation predicate,
                                // not the mere retention of the cache object.
                                if own.availability().is_usable() {
                                    own_covered += 1;
                                } else {
                                    own_unavailable += 1;
                                }
                            }
                            if let Some(state) = entry.state.upgrade() {
                                if let Ok(state) = state.read() {
                                    let own_digest = own
                                        .as_ref()
                                        .and_then(|own| own.stored_context_digest().ok());
                                    let probe = entry.probe.upgrade();
                                    for (digest, cached) in state.contexts.iter() {
                                        // The retained own lease is not a peer.
                                        if own_digest.as_deref() == Some(digest.as_str()) {
                                            continue;
                                        }
                                        let live = probe.as_ref().is_some_and(|probe| {
                                            now.as_ref().is_ok_and(|now| {
                                                probe.peer_is_live(
                                                    cached,
                                                    *now,
                                                    state.revocations.contains_key(digest),
                                                )
                                            })
                                        });
                                        if live {
                                            peer_covered += 1;
                                        } else {
                                            peer_unavailable += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                vec![
                    (
                        own_covered as f64,
                        vec![
                            crate::telemetry::KeyValue::new("trellis.kind", "own"),
                            crate::telemetry::KeyValue::new("trellis.state", "covered"),
                        ],
                    ),
                    (
                        own_unavailable as f64,
                        vec![
                            crate::telemetry::KeyValue::new("trellis.kind", "own"),
                            crate::telemetry::KeyValue::new("trellis.state", "unavailable"),
                        ],
                    ),
                    (
                        peer_covered as f64,
                        vec![
                            crate::telemetry::KeyValue::new("trellis.kind", "peer"),
                            crate::telemetry::KeyValue::new("trellis.state", "covered"),
                        ],
                    ),
                    (
                        peer_unavailable as f64,
                        vec![
                            crate::telemetry::KeyValue::new("trellis.kind", "peer"),
                            crate::telemetry::KeyValue::new("trellis.state", "unavailable"),
                        ],
                    ),
                ]
            }),
        )
    });
}

#[cfg(test)]
mod retired_watch_tests {
    use super::*;
    use crate::client::authorization::own_context::tests::test_support::{
        installation, now_seconds, own_context_fixture, signed_context, OwnContextFixture,
    };
    use crate::client::authorization::registry::RegistryWatchEntry;

    fn entry_for(fixture: &OwnContextFixture, covered: Arc<AtomicBool>) -> Arc<CachedContext> {
        Arc::new(CachedContext {
            signed: fixture.signed.clone(),
            issuer: fixture.issuer_key.clone(),
            verified: Mutex::new(CachedVerifications::default()),
            covered,
            watch: tokio::spawn(async {}).abort_handle(),
            leases: AtomicUsize::new(0),
            last_used: AtomicU64::new(0),
        })
    }

    fn provider_state(digest: &str, entry: Arc<CachedContext>) -> Arc<RwLock<ProviderState>> {
        Arc::new(RwLock::new(ProviderState {
            contexts: HashMap::from([(digest.to_owned(), entry)]),
            issuers: HashMap::new(),
            revocations: HashMap::new(),
        }))
    }

    fn revocation_event(
        digest: &str,
        revision: u64,
    ) -> Result<RegistryWatchEvent, TrellisClientError> {
        Ok(RegistryWatchEvent::Entry(RegistryWatchEntry {
            key: format!("{REVOCATION_PREFIX}{digest}"),
            value: serde_json::to_vec(&serde_json::json!({ "revokedAt": 111 })).unwrap(),
            removed: false,
            revision,
        }))
    }

    /// Apply the production terminal bookkeeping to the first of `events`.
    ///
    /// The real observer consumes an initialized `RegistryWatch`; this exercises
    /// the same production bookkeeping with the single decoded observation it
    /// would apply, rather than manufacturing an initialized runtime stream.
    async fn observe(
        events: Vec<Result<RegistryWatchEvent, TrellisClientError>>,
        state: &Arc<RwLock<ProviderState>>,
        own: &Arc<AuthorizationContextCache>,
        covered: Arc<AtomicBool>,
        digest: &str,
    ) {
        apply_terminal_observation(
            events.into_iter().next(),
            &Arc::downgrade(state),
            Some(own),
            &covered,
            digest,
            now_seconds() + 3_600,
            &Arc::new(tokio::sync::broadcast::channel(4).0),
            &Arc::new(Mutex::new(None)),
        );
    }

    #[tokio::test]
    async fn retired_watch_distinguishes_revocation_from_coverage_loss_retired_loss() {
        let fixture = own_context_fixture(1);
        let own = Arc::new(fixture.cache.clone());
        let successor_covered = Arc::new(AtomicBool::new(true));
        let state = provider_state(
            &fixture.digest,
            entry_for(&fixture, successor_covered.clone()),
        );
        let retired_covered = Arc::new(AtomicBool::new(true));

        observe(
            Vec::new(),
            &state,
            &own,
            retired_covered.clone(),
            &fixture.digest,
        )
        .await;

        assert!(
            successor_covered.load(Ordering::Acquire),
            "a retired watch ending must not retire the successor coverage"
        );
        assert!(
            own.context_digest().is_ok(),
            "retired coverage loss must not suspend own usability"
        );
        assert!(!retired_covered.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn retired_watch_distinguishes_revocation_from_coverage_loss_retired_revocation() {
        let fixture = own_context_fixture(1);
        let own = Arc::new(fixture.cache.clone());
        let successor_covered = Arc::new(AtomicBool::new(true));
        let state = provider_state(
            &fixture.digest,
            entry_for(&fixture, successor_covered.clone()),
        );

        observe(
            vec![revocation_event(&fixture.digest, 7)],
            &state,
            &own,
            Arc::new(AtomicBool::new(true)),
            &fixture.digest,
        )
        .await;

        assert!(
            state
                .read()
                .unwrap()
                .revocations
                .contains_key(&fixture.digest),
            "genuine revocation evidence is retained for the digest"
        );
        assert!(
            !successor_covered.load(Ordering::Acquire),
            "an indexed successor entry for the revoked digest is marked unusable"
        );
        assert!(
            own.context_digest().is_err(),
            "genuine revocation suspends the active own installation"
        );
        assert!(
            own.own_transport_snapshot().is_err(),
            "the revoked digest is not presented to the transport"
        );
    }

    #[tokio::test]
    async fn retired_watch_distinguishes_revocation_from_coverage_loss_newer_active() {
        let older = own_context_fixture(1);
        let newer = own_context_fixture(2);
        let own = Arc::new(newer.cache.clone());
        let covered = Arc::new(AtomicBool::new(true));
        let state = provider_state(&older.digest, entry_for(&older, covered.clone()));

        observe(
            vec![revocation_event(&older.digest, 7)],
            &state,
            &own,
            Arc::new(AtomicBool::new(true)),
            &older.digest,
        )
        .await;

        assert!(
            state
                .read()
                .unwrap()
                .revocations
                .contains_key(&older.digest),
            "negative evidence for the older digest is retained"
        );
        assert!(
            own.context_digest().is_ok(),
            "revoking an older digest must not suspend a newer active installation"
        );
        assert_eq!(own.context_digest().unwrap(), newer.digest);
    }

    #[tokio::test]
    async fn retired_watch_distinguishes_revocation_from_coverage_loss_private_candidate() {
        let fixture = own_context_fixture(1);
        let own = Arc::new(fixture.cache.clone());
        let candidate_digest = own
            .prepare(installation(
                &fixture.issuer,
                &fixture.session,
                &fixture.connection_id,
                2,
                now_seconds(),
            ))
            .unwrap()
            .context_digest;
        assert_ne!(candidate_digest, fixture.digest);
        let state = provider_state(
            &fixture.digest,
            entry_for(&fixture, Arc::new(AtomicBool::new(true))),
        );

        observe(
            vec![revocation_event(&candidate_digest, 9)],
            &state,
            &own,
            Arc::new(AtomicBool::new(true)),
            &candidate_digest,
        )
        .await;

        assert!(
            own.candidate_digest().is_err(),
            "a revoked private candidate is discarded"
        );
        assert_eq!(
            own.context_digest().unwrap(),
            fixture.digest,
            "the still-valid active predecessor stays usable"
        );
    }

    /// Verify the fixture's installed signed context for a directly built lease.
    fn verified_for(fixture: &OwnContextFixture) -> VerifiedAuthorizationContext {
        let bundle = fixture.cache.bundle().unwrap();
        let policy = bundle.policy.verification_policy(now_seconds()).unwrap();
        verify_authorization_context(
            &fixture.issuer_key,
            &fixture.signed,
            &policy,
            AuthorizationContextPurpose::Live,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn terminal_candidate_observation_releases_only_the_pending_pin() {
        let fixture = own_context_fixture(1);
        let own = Arc::new(fixture.cache.clone());
        let candidate_digest = own
            .prepare(installation(
                &fixture.issuer,
                &fixture.session,
                &fixture.connection_id,
                2,
                now_seconds(),
            ))
            .unwrap()
            .context_digest;
        assert_ne!(candidate_digest, fixture.digest);

        // A pending own-candidate coverage lease pinning the candidate entry.
        let candidate_covered = Arc::new(AtomicBool::new(true));
        let candidate = entry_for(&fixture, candidate_covered.clone());
        candidate.leases.store(1, Ordering::Release);
        let pending = Arc::new(Mutex::new(Some(PendingOwnLease {
            token: Arc::new(()),
            lease: AuthorizationContextLease {
                entry: candidate.clone(),
                context: verified_for(&fixture),
            },
        })));
        let state = provider_state(&candidate_digest, candidate);

        // A *retired* binding's terminal loss (a different coverage identity)
        // must not release the pending pin or dismantle the indexed entry.
        apply_terminal_observation(
            None,
            &Arc::downgrade(&state),
            Some(&own),
            &Arc::new(AtomicBool::new(true)),
            &candidate_digest,
            now_seconds() + 3_600,
            &Arc::new(tokio::sync::broadcast::channel(4).0),
            &pending,
        );
        assert!(
            pending.lock().unwrap().is_some(),
            "a retired binding's loss leaves the pending candidate pin in force"
        );
        assert!(state
            .read()
            .unwrap()
            .contexts
            .contains_key(&candidate_digest));

        // The candidate's own terminal coverage loss releases the pin, so the
        // abandoned entry is reclaimed, and the installed own context stays
        // usable.
        apply_terminal_observation(
            None,
            &Arc::downgrade(&state),
            Some(&own),
            &candidate_covered,
            &candidate_digest,
            now_seconds() + 3_600,
            &Arc::new(tokio::sync::broadcast::channel(4).0),
            &pending,
        );
        assert!(pending.lock().unwrap().is_none());
        assert!(!state
            .read()
            .unwrap()
            .contexts
            .contains_key(&candidate_digest));
        assert!(own.candidate_digest().is_err());
        assert_eq!(own.context_digest().unwrap(), fixture.digest);
    }

    fn pending_lease(
        fixture: &OwnContextFixture,
        token: Arc<()>,
        entry: Arc<CachedContext>,
    ) -> PendingOwnLease {
        PendingOwnLease {
            token,
            lease: AuthorizationContextLease {
                entry,
                context: verified_for(fixture),
            },
        }
    }

    #[tokio::test]
    async fn scoped_candidate_release_reclaims_only_its_own_unborrowed_entry() {
        let fixture = own_context_fixture(1);
        let candidate_digest = "candidate-digest";
        let token = Arc::new(());
        let entry = entry_for(&fixture, Arc::new(AtomicBool::new(true)));
        entry.leases.store(1, Ordering::Release);
        let state = provider_state(candidate_digest, entry.clone());
        let pending = Arc::new(Mutex::new(Some(pending_lease(
            &fixture,
            token.clone(),
            entry.clone(),
        ))));

        assert!(release_candidate_pin(
            &Arc::downgrade(&state),
            &pending,
            &token,
            Some(fixture.digest.as_str()),
            candidate_digest,
            &entry,
        ));
        assert!(pending.lock().unwrap().is_none());
        assert!(
            !state
                .read()
                .unwrap()
                .contexts
                .contains_key(candidate_digest),
            "an abandoned unborrowed candidate entry is reclaimed"
        );
        assert_eq!(
            fixture.cache.context_digest().unwrap(),
            fixture.digest,
            "the healthy installed own context stays usable"
        );
    }

    #[tokio::test]
    async fn scoped_candidate_release_never_discards_a_newer_same_entry_pending_pin() {
        let fixture = own_context_fixture(1);
        let candidate_digest = "candidate-digest";
        // The very same cached entry retained by two attempts: only the token
        // distinguishes them, so the superseded attempt's cleanup must leave the
        // newer pending pin untouched even though the entry identity matches.
        let ours = Arc::new(());
        let newer = Arc::new(());
        let entry = entry_for(&fixture, Arc::new(AtomicBool::new(true)));
        entry.leases.store(1, Ordering::Release);
        let state = provider_state(candidate_digest, entry.clone());
        let pending = Arc::new(Mutex::new(Some(pending_lease(
            &fixture,
            newer.clone(),
            entry.clone(),
        ))));

        assert!(
            !release_candidate_pin(
                &Arc::downgrade(&state),
                &pending,
                &ours,
                Some(fixture.digest.as_str()),
                candidate_digest,
                &entry,
            ),
            "an older attempt's cleanup changes nothing"
        );
        assert!(
            pending
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|pending| Arc::ptr_eq(&pending.token, &newer)),
            "a newer pending pin survives the older attempt's cleanup"
        );
        assert!(state
            .read()
            .unwrap()
            .contexts
            .contains_key(candidate_digest));
    }

    #[tokio::test]
    async fn scoped_candidate_release_preserves_borrowed_and_current_entries() {
        let fixture = own_context_fixture(1);
        let candidate_digest = "candidate-digest";
        // The pending pin plus one independent borrower.
        let token = Arc::new(());
        let borrowed = entry_for(&fixture, Arc::new(AtomicBool::new(true)));
        borrowed.leases.store(2, Ordering::Release);
        let state = provider_state(candidate_digest, borrowed.clone());
        let pending = Arc::new(Mutex::new(Some(pending_lease(
            &fixture,
            token.clone(),
            borrowed.clone(),
        ))));
        assert!(release_candidate_pin(
            &Arc::downgrade(&state),
            &pending,
            &token,
            Some(fixture.digest.as_str()),
            candidate_digest,
            &borrowed,
        ));
        assert!(
            state
                .read()
                .unwrap()
                .contexts
                .contains_key(candidate_digest),
            "an independently borrowed candidate entry is preserved"
        );

        // The installed digest itself is never reclaimed even when unborrowed.
        let digest = fixture.digest.as_str();
        let token = Arc::new(());
        let current = entry_for(&fixture, Arc::new(AtomicBool::new(true)));
        current.leases.store(1, Ordering::Release);
        let state = provider_state(digest, current.clone());
        let pending = Arc::new(Mutex::new(Some(pending_lease(
            &fixture,
            token.clone(),
            current.clone(),
        ))));
        assert!(release_candidate_pin(
            &Arc::downgrade(&state),
            &pending,
            &token,
            Some(digest),
            digest,
            &current,
        ));
        assert!(
            state.read().unwrap().contexts.contains_key(digest),
            "the current installed digest is never reclaimed"
        );
    }

    /// A supersession that races `retain_own_candidate_context`'s resolution must
    /// let the attempt's own guard reclaim the abandoned, unborrowed,
    /// non-installed exact cached entry even though the attempt never installed a
    /// pending pin; the healthy installed own coverage stays in force.
    #[tokio::test]
    async fn superseded_candidate_attempt_reclaims_its_resolved_entry_before_pin() {
        let fixture = own_context_fixture(1);
        let now = now_seconds();

        // A distinct, genuinely signed abandoned candidate keeps the resolved
        // entry's own signed digest equal to its index key, so reclamation is
        // proven against the exact entry identity.
        let abandoned = installation(
            &fixture.issuer,
            &fixture.session,
            &fixture.connection_id,
            2,
            now,
        );
        let abandoned_signed = signed_context(&abandoned);
        let abandoned_digest = abandoned_signed.digest().unwrap();
        assert_ne!(abandoned_digest, fixture.digest);

        let bundle = fixture.cache.bundle().unwrap();
        let policy = bundle.policy.verification_policy(now).unwrap();
        // A real client handle; no registry IO is reached because both digests
        // resolve from the component cache below. This is bookkeeping, not a live
        // broker cancellation.
        let nats = async_nats::ConnectOptions::new()
            .retry_on_initial_connect()
            .connect("nats://127.0.0.1:1")
            .await
            .unwrap();
        let provider = AuthorizationProviderCache::open(
            nats,
            &bundle.authorization_registry,
            BootstrapHttp::new("http://127.0.0.1:1/").unwrap(),
            Some(bundle.issuer),
            policy,
            Some(Arc::new(fixture.cache.clone())),
            None,
        )
        .await
        .unwrap();

        // Usable covered entries for the installed healthy digest and the
        // abandoned candidate digest.
        let installed_entry = entry_for(&fixture, Arc::new(AtomicBool::new(true)));
        let abandoned_entry = Arc::new(CachedContext {
            signed: abandoned_signed,
            issuer: fixture.issuer_key.clone(),
            verified: Mutex::new(CachedVerifications::default()),
            covered: Arc::new(AtomicBool::new(true)),
            watch: tokio::spawn(async {}).abort_handle(),
            leases: AtomicUsize::new(0),
            last_used: AtomicU64::new(0),
        });
        {
            let mut state = provider.state.write().unwrap();
            state
                .contexts
                .insert(fixture.digest.clone(), installed_entry);
            state
                .contexts
                .insert(abandoned_digest.clone(), abandoned_entry);
        }

        // Install the healthy own coverage through the production path.
        provider.retain_own_context(&fixture.digest).await.unwrap();

        // A different candidate is prepared, so the abandoned digest is no longer
        // the prepared candidate when its own retention runs.
        let prepared = fixture
            .cache
            .prepare(installation(
                &fixture.issuer,
                &fixture.session,
                &fixture.connection_id,
                3,
                now,
            ))
            .unwrap();
        assert_ne!(prepared.context_digest, abandoned_digest);

        let mut retention = provider.begin_own_candidate_retention(&abandoned_digest);
        let error = provider
            .retain_own_candidate_context(&mut retention, &PinnedOwnCandidateSource::fixed(0))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                TrellisClientError::AuthorizationUnavailable(message)
                    if message.contains("candidate changed before coverage retention")
            ),
            "the abandoned attempt fails on the real candidate-changed check, got {error:?}"
        );
        assert!(
            provider.own_candidate_lease.lock().unwrap().is_none(),
            "no pending pin was installed before the failure"
        );

        // Dropping the attempt's guard reclaims exactly the abandoned entry it
        // resolved; the healthy installed coverage and the installed own lease
        // survive untouched.
        drop(retention);
        {
            let state = provider.state.read().unwrap();
            assert!(
                !state.contexts.contains_key(&abandoned_digest),
                "the abandoned unborrowed candidate entry is reclaimed"
            );
            assert!(
                state.contexts.contains_key(&fixture.digest),
                "the installed healthy entry is preserved"
            );
        }
        {
            let installed = provider.own_lease.lock().unwrap();
            assert!(
                installed
                    .as_ref()
                    .is_some_and(|lease| lease.context_digest() == fixture.digest),
                "the healthy installed own lease is never touched by the failed attempt"
            );
        }
        assert_eq!(fixture.cache.context_digest().unwrap(), fixture.digest);
    }

    /// The guarded promotion consumes only the exact attempt's pending pin: a
    /// different attempt token is refused at the commit boundary and the candidate
    /// stays private, while the owning token promotes it and installs its coverage.
    #[tokio::test]
    async fn finalize_promotes_only_the_exact_attempt_pending_token() {
        let fixture = own_context_fixture(1);
        let now = now_seconds();
        // A distinct signed private candidate, prepared but not yet current.
        let candidate_install = installation(
            &fixture.issuer,
            &fixture.session,
            &fixture.connection_id,
            2,
            now,
        );
        let candidate_signed = signed_context(&candidate_install);
        let prepared = fixture.cache.prepare(candidate_install).unwrap();
        let candidate_digest = prepared.context_digest.clone();
        assert_ne!(candidate_digest, fixture.digest);
        // The exact identity the preparation returned fences the promotion.
        let instance = prepared.instance.clone();

        let bundle = fixture.cache.bundle().unwrap();
        let verify_policy = bundle.policy.verification_policy(now).unwrap();
        let candidate_context = verify_authorization_context(
            &fixture.issuer_key,
            &candidate_signed,
            &verify_policy,
            AuthorizationContextPurpose::Live,
        )
        .unwrap();
        // Real provider over the same own cache; no registry IO is reached because
        // the candidate's covered entry is already resolved in component state.
        let nats = async_nats::ConnectOptions::new()
            .retry_on_initial_connect()
            .connect("nats://127.0.0.1:1")
            .await
            .unwrap();
        let provider = AuthorizationProviderCache::open(
            nats,
            &bundle.authorization_registry,
            BootstrapHttp::new("http://127.0.0.1:1/").unwrap(),
            Some(bundle.issuer),
            bundle.policy.verification_policy(now).unwrap(),
            Some(Arc::new(fixture.cache.clone())),
            None,
        )
        .await
        .unwrap();
        let candidate_entry = Arc::new(CachedContext {
            signed: candidate_signed,
            issuer: fixture.issuer_key.clone(),
            verified: Mutex::new(CachedVerifications::default()),
            covered: Arc::new(AtomicBool::new(true)),
            watch: tokio::spawn(async {}).abort_handle(),
            leases: AtomicUsize::new(1),
            last_used: AtomicU64::new(0),
        });
        provider
            .state
            .write()
            .unwrap()
            .contexts
            .insert(candidate_digest.clone(), candidate_entry.clone());
        // The attempt's pending pin, owned by a distinct token.
        let owner_token = Arc::new(());
        provider
            .set_pending_own_lease(
                owner_token.clone(),
                AuthorizationContextLease {
                    entry: candidate_entry.clone(),
                    context: candidate_context,
                },
            )
            .unwrap();

        // A foreign attempt token must not consume this pending pin.
        {
            let transition = fixture.cache.lock_own_transition().unwrap();
            let foreign = Arc::new(());
            let error = provider
                .finalize_own_installation_locked(
                    &transition,
                    &candidate_digest,
                    true,
                    Some(&instance),
                    Some(&foreign),
                )
                .unwrap_err();
            assert!(matches!(
                error,
                TrellisClientError::AuthorizationUnavailable(_)
            ));
        }
        assert!(
            provider.own_candidate_lease.lock().unwrap().is_some(),
            "a foreign token leaves the pending pin in force"
        );
        assert_eq!(
            fixture.cache.context_digest().unwrap(),
            fixture.digest,
            "the refused attempt leaves the installed context untouched"
        );

        // The owning token promotes the candidate and installs its coverage.
        {
            let transition = fixture.cache.lock_own_transition().unwrap();
            provider
                .finalize_own_installation_locked(
                    &transition,
                    &candidate_digest,
                    true,
                    Some(&instance),
                    Some(&owner_token),
                )
                .unwrap();
        }
        assert!(provider.own_candidate_lease.lock().unwrap().is_none());
        assert!(
            provider
                .own_lease
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|lease| lease.context_digest() == candidate_digest),
            "the promoted pending coverage becomes the installed own lease"
        );
        assert_eq!(fixture.cache.context_digest().unwrap(), candidate_digest);
        assert!(fixture.cache.candidate_digest().is_err());
    }
}
