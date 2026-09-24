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
use super::own_context::{system_now_millis, AuthorizationContextCache};
use super::registry::{
    validate_digest_key, AuthorizationRegistryReader, RegistryWatchEvent, REVOCATION_PREFIX,
};
use super::types::AuthorizationRegistryBinding;

#[cfg(feature = "runtime-internals")]
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct RuntimeAuthorizationTrust {
    /// Configured origin for public issuer-key resolution.
    pub trellis_origin: String,
    /// Whether `trellis_origin` was explicitly allow-listed as an insecure origin.
    pub allow_insecure_origin: bool,
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
    epoch: u64,
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
    pub(crate) fn epoch(&self) -> u64 {
        self.entry.epoch
    }

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
/// require an exact revocation watch on the same NATS connection epoch.
#[derive(Clone)]
pub struct AuthorizationProviderCache {
    nats: async_nats::Client,
    registry: AuthorizationRegistryReader,
    http: BootstrapHttp,
    own: Option<Arc<AuthorizationContextCache>>,
    own_lease: Arc<Mutex<Option<AuthorizationContextLease>>>,
    verification_policy: AuthorizationVerificationPolicy,
    state: Arc<RwLock<ProviderState>>,
    in_flight: Arc<Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>>,
    issuer_resolution: Arc<tokio::sync::Mutex<()>>,
    closed: Arc<AtomicBool>,
    context_resolves: Arc<AtomicU64>,
    access_clock: Arc<AtomicU64>,
    coverage_probe: Arc<CoverageProbe>,
    /// Wakes retained live guards when coverage, revocation or epoch state moves.
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

    /// Retain one live-authority lease for an exact digest and epoch.
    ///
    /// Resolves the digest through the ordinary single-flight cache when it is
    /// not already retained, then returns a lease only when the exact entry is
    /// covered on the expected local transport epoch. The caller keeps the
    /// lease for the session lifetime, so the cache's ordinary cleanup retains
    /// the revocation watch and coverage evidence.
    pub(crate) async fn retain_live_guard_lease(
        &self,
        digest: &str,
        expected_epoch: u64,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        if self.epoch() != expected_epoch || !self.health()?.healthy {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "transport epoch changed before live retention".into(),
            ));
        }
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
        if lease.context_digest() != digest
            || lease.epoch() != expected_epoch
            || self.epoch() != expected_epoch
            || !self.health()?.healthy
            || !self.live_guard_entry_is_covered(&lease)
        {
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
                Arc::ptr_eq(entry, lease.entry())
                    && entry.covered.load(Ordering::Acquire)
                    && entry.epoch == lease.epoch()
            })
    }

    pub(crate) async fn attach(
        nats: async_nats::Client,
        binding: &AuthorizationRegistryBinding,
        own: Arc<AuthorizationContextCache>,
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
        )
        .await?;
        let digest = own.retained_context_digest()?;
        cache.retain_own_context(&digest, cache.epoch()).await?;
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
            BootstrapHttp::new(&trust.trellis_origin, trust.allow_insecure_origin)?,
            trust.issuer,
            trust.policy,
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
    ) -> Result<Self, TrellisClientError> {
        if let Some(issuer) = &issuer {
            issuer
                .verifying_key()
                .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        }
        let registry = AuthorizationRegistryReader::open(nats.clone(), binding).await?;
        let state = Arc::new(RwLock::new(ProviderState {
            issuers: issuer
                .into_iter()
                .map(|issuer| (issuer.key_id.clone(), issuer))
                .collect(),
            ..Default::default()
        }));
        let coverage_probe = Arc::new(CoverageProbe {
            closed: Arc::new(AtomicBool::new(false)),
            epoch: Arc::new(AtomicU64::new(
                nats.statistics().connects.load(Ordering::Acquire),
            )),
            connected: Arc::new(AtomicBool::new(
                nats.connection_state() == async_nats::connection::State::Connected,
            )),
        });
        let closed = Arc::new(AtomicBool::new(false));
        register_coverage_gauges(&state, &own, &coverage_probe);
        Ok(Self {
            nats,
            registry,
            http,
            own,
            own_lease: Arc::new(Mutex::new(None)),
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

    pub(crate) async fn retain_own_context(
        &self,
        digest: &str,
        expected_epoch: u64,
    ) -> Result<(), TrellisClientError> {
        let Some(own) = &self.own else {
            return Ok(());
        };
        let old = self
            .own_lease
            .lock()
            .map_err(|_| {
                TrellisClientError::AuthorizationUnavailable(
                    "own context lease lock poisoned".into(),
                )
            })?
            .take();
        drop(old);
        let lease = self
            .resolve_context(digest, own.corrected_now_seconds()?)
            .await?;
        if lease.context_digest() != digest
            || lease.epoch() != expected_epoch
            || self.epoch() != expected_epoch
            || !self.health()?.healthy
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "own context coverage changed before installation".into(),
            ));
        }
        *self.own_lease.lock().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("own context lease lock poisoned".into())
        })? = Some(lease);
        self.notify_live_changes();
        tracing::info!(
            context_digest = digest,
            transport_epoch = expected_epoch,
            "retained own authorization coverage"
        );
        Ok(())
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
                    let epoch = self.epoch();
                    let connected = self.health()?.healthy;
                    let mut state = self.write_state()?;
                    let before = state.contexts.len();
                    state.contexts.retain(|_, entry| {
                        entry.leases.load(Ordering::Acquire) > 0
                            || (connected && entry.epoch == epoch && entry.covered.load(Ordering::Acquire))
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
        self.coverage_probe.observe(true, self.epoch(), false);
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
                "provider is not connected".into(),
            ));
        }
        Ok(())
    }

    #[cfg(feature = "runtime-internals")]
    #[doc(hidden)]
    pub async fn wait_until_ready(&self) -> Result<(), TrellisClientError> {
        if !self.health()?.healthy {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider is not connected".into(),
            ));
        }
        Ok(())
    }

    /// Returns current provider health and mirrors the lifecycle for gauges.
    ///
    /// This is a read-only observation of the owner's existing state; it never
    /// issues network reads or changes authority.
    pub(crate) fn health(&self) -> Result<AuthorizationProviderCacheHealth, TrellisClientError> {
        let closed = self.closed.load(Ordering::Acquire);
        let healthy =
            !closed && self.nats.connection_state() == async_nats::connection::State::Connected;
        self.coverage_probe.observe(closed, self.epoch(), healthy);
        Ok(AuthorizationProviderCacheHealth { healthy })
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

    pub(crate) fn epoch(&self) -> u64 {
        self.nats.statistics().connects.load(Ordering::Acquire)
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
                // still-valid active predecessor stays untouched.
                own.mark_revoked(digest);
                own.invalidate_candidate_locked(transition, digest);
                own.request_refresh();
            }
        }
        Ok(())
    }

    /// Publish the guarded final own-installation transition.
    ///
    /// Runs on one short local synchronization boundary with no network or HTTP
    /// await: the expected digest must still own the retained lease on the
    /// current admitted epoch with initialized live coverage and no stored
    /// revocation. `promote` publishes a prepared candidate; otherwise the
    /// retained installation is resumed after coverage reinitialization.
    pub(crate) fn finalize_own_installation(
        &self,
        expected_digest: &str,
        promote: bool,
    ) -> Result<(), TrellisClientError> {
        let Some(own) = &self.own else {
            return Ok(());
        };
        let transition = own.lock_own_transition()?;
        let expected_epoch = self.epoch();
        if !self.health()?.healthy {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization provider is not connected".into(),
            ));
        }
        if own.revocation_marker().as_deref() == Some(expected_digest) {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization context is revoked".into(),
            ));
        }
        if promote && own.candidate_digest_locked(&transition)? != expected_digest {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate changed before publication".into(),
            ));
        }
        let lease = self.own_lease.lock().map_err(|_| {
            TrellisClientError::AuthorizationUnavailable("own context lease lock poisoned".into())
        })?;
        let Some(lease) = lease.as_ref() else {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization coverage lease is unavailable".into(),
            ));
        };
        if lease.context_digest() != expected_digest || lease.epoch() != expected_epoch {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization coverage changed before publication".into(),
            ));
        }
        {
            let state = self.read_state()?;
            if state.revocations.contains_key(expected_digest) {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization context is revoked".into(),
                ));
            }
            match state.contexts.get(expected_digest) {
                Some(entry)
                    if Arc::ptr_eq(entry, lease.entry())
                        && entry.covered.load(Ordering::Acquire)
                        && entry.epoch == expected_epoch => {}
                _ => {
                    return Err(TrellisClientError::AuthorizationUnavailable(
                        "authorization coverage entry changed before publication".into(),
                    ));
                }
            }
        }
        if promote {
            own.promote_locked(&transition, expected_digest)?;
        } else {
            own.resume_availability_locked(&transition, expected_digest)?;
        }
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
        self.resolve_context_for(digest, now, false).await
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
        self.resolve_context_for(digest, event_time, true).await
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
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        validate_digest_key(digest)?;
        if !self.health()?.healthy {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider is not connected".into(),
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
            self.resolve_context_once(digest, verification_time, historical),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
    }

    async fn resolve_context_once(
        &self,
        digest: &str,
        verification_time: i64,
        historical: bool,
    ) -> Result<AuthorizationContextLease, TrellisClientError> {
        let epoch = self.epoch();
        self.context_resolves.fetch_add(1, Ordering::Relaxed);
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
                if let Some(value) = self.registry.get_context(digest).await? {
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
        let now = self.now_seconds()?;
        let live = issuer.state == AuthorizationIssuerState::Active
            && signed.unsigned.not_before <= now
            && signed.unsigned.expires_at > now;
        let mut watch = self.registry.watch_revocation(digest).await?;
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
        if !self.health()?.healthy || self.epoch() != epoch {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "connection changed during context resolution".into(),
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
        let task = tokio::spawn(observe_context_revocation(
            watch,
            weak_state,
            own,
            watch_covered,
            watch_digest,
            revocation_deadline,
            self.live_changes.clone(),
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
            epoch,
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
        if state
            .contexts
            .get(digest)
            .is_some_and(|existing| existing.leases.load(Ordering::Acquire) > 0)
        {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "provider context is still leased".into(),
            ));
        }
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
        if !self.health()?.healthy {
            return Ok(None);
        }
        let now = self.now_seconds()?;
        let epoch = self.epoch();
        let state = self.write_state()?;
        let Some(entry) = state.contexts.get(digest) else {
            return Ok(None);
        };
        if entry.epoch != epoch || !entry.covered.load(Ordering::Acquire) {
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

async fn observe_context_revocation(
    mut watch: impl futures_util::Stream<Item = Result<RegistryWatchEvent, TrellisClientError>> + Unpin,
    weak_state: Weak<RwLock<ProviderState>>,
    own: Option<Weak<AuthorizationContextCache>>,
    watch_covered: Arc<AtomicBool>,
    watch_digest: String,
    revocation_deadline: i64,
    live_changes: Arc<tokio::sync::broadcast::Sender<()>>,
) {
    let entry = watch.next().await;
    let revoked_at = match entry {
        Some(Ok(RegistryWatchEvent::Entry(entry)))
            if !entry.removed
                && entry.revision > 0
                && entry.key == format!("{REVOCATION_PREFIX}{watch_digest}") =>
        {
            parse_revocation_record(&entry.value).ok()
        }
        _ => None,
    };
    let own = own.and_then(|own| own.upgrade());
    let transition = match own.as_ref() {
        Some(own) => match own.lock_own_transition() {
            Ok(transition) => Some(transition),
            Err(error) => {
                tracing::warn!(%error, "own transition lock is unavailable for invalidation");
                None
            }
        },
        None => None,
    };
    // Capture the indexed-entry identity before any removal, then apply coverage
    // and negative evidence inside the same transition boundary.
    let was_current_entry = if let Some(state) = weak_state.upgrade() {
        if let Ok(mut state) = state.write() {
            let current = state
                .contexts
                .get(&watch_digest)
                .is_some_and(|entry| Arc::ptr_eq(&entry.covered, &watch_covered));
            if let Some(at) = revoked_at {
                state
                    .revocations
                    .insert(watch_digest.clone(), (at, revocation_deadline));
                // Genuine revocation evidence applies to every indexed entry for
                // the digest, including a successor coverage that replaced the
                // callback's own retired watch entry.
                if let Some(indexed) = state.contexts.get(&watch_digest) {
                    indexed.covered.store(false, Ordering::Release);
                }
            }
            watch_covered.store(false, Ordering::Release);
            if state.contexts.get(&watch_digest).is_some_and(|entry| {
                Arc::ptr_eq(&entry.covered, &watch_covered)
                    && entry.leases.load(Ordering::Acquire) == 0
            }) {
                state.contexts.remove(&watch_digest);
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
    if let (Some(own), Some(transition)) = (&own, transition.as_ref()) {
        if own
            .stored_context_digest()
            .is_ok_and(|digest| digest == watch_digest)
        {
            if revoked_at.is_some() {
                own.mark_revoked(&watch_digest);
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
                own.mark_revoked(&watch_digest);
            }
            own.invalidate_candidate_locked(transition, &watch_digest);
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
            epoch: 1,
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
            epoch: Arc::new(AtomicU64::new(1)),
            connected: Arc::new(AtomicBool::new(true)),
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

        observe_context_revocation(
            futures_util::stream::iter([Ok(RegistryWatchEvent::Entry(
                super::super::registry::RegistryWatchEntry {
                    key: "revocation.digest".into(),
                    value: br#"{"revokedAt":1150}"#.to_vec(),
                    removed: false,
                    revision: 2,
                },
            ))]),
            Arc::downgrade(&state),
            None,
            covered.clone(),
            "digest".into(),
            2_000,
            Arc::new(tokio::sync::broadcast::channel(4).0),
        )
        .await;

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
    /// Connect epoch of the owner, mirrored from its existing counter.
    epoch: Arc<AtomicU64>,
    /// Whether the owner's transport is currently usable.
    connected: Arc<AtomicBool>,
}

impl CoverageProbe {
    /// Updates the mirrored lifecycle state from the live owner.
    pub(crate) fn observe(&self, closed: bool, epoch: u64, connected: bool) {
        self.closed.store(closed, Ordering::Release);
        self.epoch.store(epoch, Ordering::Release);
        self.connected.store(connected, Ordering::Release);
    }

    /// Whether one peer entry at the given epoch is live coverage.
    fn peer_is_live(&self, entry: &CachedContext, now: i64, revoked: bool) -> bool {
        !revoked
            && entry.covered.load(Ordering::Acquire)
            && entry.issuer.state == AuthorizationIssuerState::Active
            && entry.signed.unsigned.not_before <= now
            && entry.signed.unsigned.expires_at > now
            && !self.closed.load(Ordering::Acquire)
            && self.connected.load(Ordering::Acquire)
            && entry.epoch == self.epoch.load(Ordering::Acquire)
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
    use futures_util::stream;

    use super::*;
    use crate::client::authorization::own_context::tests::test_support::{
        installation, now_seconds, own_context_fixture, OwnContextFixture,
    };
    use crate::client::authorization::registry::RegistryWatchEntry;

    fn entry_for(
        fixture: &OwnContextFixture,
        covered: Arc<AtomicBool>,
        epoch: u64,
    ) -> Arc<CachedContext> {
        Arc::new(CachedContext {
            signed: fixture.signed.clone(),
            issuer: fixture.issuer_key.clone(),
            verified: Mutex::new(CachedVerifications::default()),
            epoch,
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

    async fn observe(
        events: Vec<Result<RegistryWatchEvent, TrellisClientError>>,
        state: &Arc<RwLock<ProviderState>>,
        own: &Arc<AuthorizationContextCache>,
        covered: Arc<AtomicBool>,
        digest: &str,
    ) {
        observe_context_revocation(
            stream::iter(events),
            Arc::downgrade(state),
            Some(Arc::downgrade(own)),
            covered,
            digest.to_owned(),
            now_seconds() + 3_600,
            Arc::new(tokio::sync::broadcast::channel(4).0),
        )
        .await;
    }

    #[tokio::test]
    async fn retired_watch_distinguishes_revocation_from_coverage_loss_retired_loss() {
        let fixture = own_context_fixture(1);
        let own = Arc::new(fixture.cache.clone());
        let successor_covered = Arc::new(AtomicBool::new(true));
        let state = provider_state(
            &fixture.digest,
            entry_for(&fixture, successor_covered.clone(), 1),
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
            entry_for(&fixture, successor_covered.clone(), 1),
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
            own.transport_credentials().is_err(),
            "the revoked digest is not presented to the transport"
        );
    }

    #[tokio::test]
    async fn retired_watch_distinguishes_revocation_from_coverage_loss_newer_active() {
        let older = own_context_fixture(1);
        let newer = own_context_fixture(2);
        let own = Arc::new(newer.cache.clone());
        let covered = Arc::new(AtomicBool::new(true));
        let state = provider_state(&older.digest, entry_for(&older, covered.clone(), 1));

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
            .unwrap();
        assert_ne!(candidate_digest, fixture.digest);
        let state = provider_state(
            &fixture.digest,
            entry_for(&fixture, Arc::new(AtomicBool::new(true)), 1),
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
}
