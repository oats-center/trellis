//! Automatic transport generations for one logical Trellis connection.
//!
//! A logical connection owns one [`TransportGenerationManager`]. Each physical
//! NATS connection the manager opens is one immutable **generation**: it retains
//! the exact authorization context it connected with, the policy the broker
//! admitted for it, and its own lease accounting. Routine authorization renewal
//! never opens a generation; only an effective transport-policy change does.
//!
//! The manager is the single adoption worker for the logical connection. It
//! coalesces authorization promotion and call demand, opens the newest desired
//! policy make-before-break, discards a stale candidate, and closes generations
//! whose admitted policy current authority no longer covers. It never hides a
//! real denial: an ungranted requirement falls through to the current
//! attachment and the ordinary server decision.
//!
//! Physical loss is not repaired inside a generation. A generation that reports
//! a disconnect or a broker rejection is marked closed and never leased again;
//! it also refuses to reattach itself, so a reconnect can never reuse its frozen
//! credential. The worker then promotes a safe survivor or opens a new
//! generation from the latest authorization.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_nats::ConnectOptions;
use futures_util::future::BoxFuture;
use serde::Serialize;
use tokio::sync::watch;
use trellis_protocol::{TransportAuthorizationV1, TransportPolicyClass};

use crate::client::authorization::{
    policy_covers, read_own_admission, AuthorizationContextCache, OwnTransportSnapshot,
    TransportAuthorizationState,
};
use crate::client::{SessionAuth, TrellisClientError};
use crate::live::manager::LiveSessionManager;

/// Connection token format carried in the NATS CONNECT as the Auth Callout input.
const NATS_CONNECT_TOKEN_FORMAT: &str = "trellis.nats-connect-token.v2";

/// Bounded broker own-admission read used to correlate a generation's CONNECT.
const ADMISSION_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a waiting caller re-requests adoption while its deadline holds.
/// This keeps retries demand-driven: an idle logical connection never retries a
/// failed open on its own. A caller that re-requests adoption waits on a
/// separate condition channel, so its own request never resolves its own wait.
const ADOPTION_RETRY_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationState {
    Current = 0,
    Draining = 1,
    Closed = 2,
}

impl GenerationState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Current,
            1 => Self::Draining,
            _ => Self::Closed,
        }
    }
}

/// A framework owner's broker-ready generic intake on one generation.
pub(crate) trait GenerationIntakeHandle: Send + Sync {
    /// Stop accepting new intake; already-accepted work continues to completion.
    fn retire(&self);
}

/// One framework owner that must have broker-ready intake on a candidate
/// **before** the manager makes that candidate the default generation.
///
/// The manager adopts every registered owner on a candidate during the
/// preactivation barrier; if any adoption fails the candidate is discarded and
/// the current generation is left untouched.
pub(crate) trait GenerationIntake: Send + Sync {
    /// Install broker-ready intake on `generation` and return its handle. The
    /// manager retires the handle when the generation is superseded.
    fn adopt<'a>(
        &'a self,
        generation: Arc<TransportGeneration>,
    ) -> BoxFuture<'a, Result<Box<dyn GenerationIntakeHandle>, TrellisClientError>>;
}

/// A candidate being opened but not yet published.
struct Opening {
    id: u64,
    /// Set once the candidate's physical connection is built, so a loss between
    /// admission and publication is observed rather than lost.
    generation: Option<Arc<TransportGeneration>>,
    /// Set when a lifecycle event arrives before the connection exists.
    failed: Arc<AtomicBool>,
}

/// NATS CONNECT token presented to the Auth Callout.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NatsConnectToken {
    format: &'static str,
    context_digest: String,
    routing_jwt: String,
}

/// One immutable physical NATS attachment with an exact admitted policy.
pub(crate) struct TransportGeneration {
    id: u64,
    nats: async_nats::Client,
    context_digest: String,
    admitted_policy: TransportAuthorizationV1,
    /// Retained immutable generation record (invariant G2); observed through
    /// telemetry today and reserved for generation diagnostics.
    #[allow(dead_code)]
    admitted_policy_digest: String,
    /// Broker-authenticated physical attachment identity (invariant G2).
    #[allow(dead_code)]
    physical_connection_id: String,
    /// Terminal state is monotonic: once `Closed`, it never reactivates.
    state: Arc<AtomicU8>,
    leases: AtomicUsize,
    /// The manager's work channel; a lease release requests a reap.
    work: watch::Sender<u64>,
    /// Framework owners' broker-ready intake installed on this generation before
    /// it became the default. Retired when the generation is superseded and
    /// dropped with the generation.
    intake: Mutex<Vec<Box<dyn GenerationIntakeHandle>>>,
    /// Whether broker-ready intake is currently installed. A superseded
    /// generation's intake is retired; restoring it as current must re-adopt
    /// owners rather than resurrecting a current with dead intake.
    intake_active: AtomicBool,
}

impl TransportGeneration {
    fn state(&self) -> GenerationState {
        GenerationState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Transition the generation state, refusing to leave the terminal state.
    ///
    /// A concurrent [`close`](Self::set_state) wins: a closed generation is
    /// never reactivated as current or draining.
    fn set_state(&self, state: GenerationState) {
        let next = state as u8;
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if current == GenerationState::Closed as u8 {
                return;
            }
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    fn leases(&self) -> usize {
        self.leases.load(Ordering::Acquire)
    }

    /// Start one generation lease; release is automatic on drop.
    pub(crate) fn lease(self: &Arc<Self>) -> TransportLease {
        self.leases.fetch_add(1, Ordering::AcqRel);
        TransportLease {
            generation: self.clone(),
        }
    }

    fn request_work(&self) {
        let next = self.work.borrow().wrapping_add(1);
        self.work.send_replace(next);
    }

    /// Attach one framework owner's broker-ready intake handle to this
    /// generation.
    fn push_intake(&self, handle: Box<dyn GenerationIntakeHandle>) {
        if let Ok(mut intake) = self.intake.lock() {
            intake.push(handle);
        }
        self.intake_active.store(true, Ordering::Release);
    }

    /// Whether broker-ready intake is currently installed on this generation.
    fn intake_active(&self) -> bool {
        self.intake_active.load(Ordering::Acquire)
    }

    /// Stop accepting new generic intake on this generation; already-accepted
    /// work continues until its own lease releases. The installed handles are
    /// dropped: each retires its own intake and drains its accepted work.
    fn retire_intake(&self) {
        self.intake_active.store(false, Ordering::Release);
        if let Ok(mut intake) = self.intake.lock() {
            for handle in intake.iter() {
                handle.retire();
            }
            intake.clear();
        }
    }
}

/// One RAII lease that pins a generation for the duration of physical work.
///
/// The lease releases on drop. A superseded generation closes once its state is
/// draining and its lease count reaches zero.
pub(crate) struct TransportLease {
    generation: Arc<TransportGeneration>,
}

impl TransportLease {
    /// The pinned physical NATS connection.
    pub(crate) fn nats(&self) -> &async_nats::Client {
        &self.generation.nats
    }

    /// The immutable identity of the generation this lease pins.
    pub(crate) fn generation_id(&self) -> u64 {
        self.generation.id
    }
}

impl Clone for TransportLease {
    /// Retain one more independent owner of the same generation. Each owner
    /// releases its own count on drop, so one owner can never release another
    /// owner's only remaining lease.
    fn clone(&self) -> Self {
        self.generation.leases.fetch_add(1, Ordering::AcqRel);
        Self {
            generation: Arc::clone(&self.generation),
        }
    }
}

impl Drop for TransportLease {
    fn drop(&mut self) {
        let previous = self.generation.leases.fetch_sub(1, Ordering::AcqRel);
        if previous == 1 {
            // A draining generation may now be eligible to close; request a
            // reap from the worker.
            self.generation.request_work();
        }
    }
}

/// Manager state: the current generation, draining generations, and one
/// candidate being opened.
#[derive(Default)]
struct ManagerState {
    current: Option<Arc<TransportGeneration>>,
    draining: Vec<Arc<TransportGeneration>>,
    opening: Option<Opening>,
}

/// Shared, connection-owned collaborators for every generation.
struct ManagerInner {
    auth: Arc<SessionAuth>,
    contexts: Arc<AuthorizationContextCache>,
    timeout_ms: u64,
    live_slot: Arc<Mutex<Option<Weak<LiveSessionManager>>>>,
    closed: AtomicBool,
    state: Mutex<ManagerState>,
    /// Condition channel for callers waiting on adoption progress. It changes
    /// only when the ready set changes, never on a caller's own work request.
    state_version: watch::Sender<u64>,
    /// Work channel for the single adoption worker.
    work: watch::Sender<u64>,
    /// Framework owners whose broker-ready intake must exist on a candidate
    /// before it becomes the default generation (preactivation barrier).
    intake_owners: Mutex<Vec<Arc<dyn GenerationIntake>>>,
    reconcile: tokio::sync::Mutex<()>,
    next_id: AtomicU64,
    /// The initial framework-loop generation whose loss is the only generation
    /// loss allowed to suspend the shared live-observation manager.
    baseline_id: AtomicU64,
    /// Retained admitted-versus-renewed notice for surfaces not yet migrated to
    /// generation leases. It always reflects the current generation.
    notice: Option<TransportAuthorizationState>,
}

impl ManagerInner {
    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ManagerState>, TrellisClientError> {
        self.state
            .lock()
            .map_err(|_| TrellisClientError::Bootstrap("transport manager lock poisoned".into()))
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::AcqRel)
    }

    /// Signal that the ready set changed so condition waiters re-check.
    fn signal_state(&self) {
        let next = self.state_version.borrow().wrapping_add(1);
        self.state_version.send_replace(next);
    }

    /// Request reconciliation from the adoption worker.
    fn request_work(&self) {
        let next = self.work.borrow().wrapping_add(1);
        self.work.send_replace(next);
    }

    /// Adopt every registered framework owner on `generation` under **one**
    /// absolute connection-budget deadline shared across all owners. A failed,
    /// timed-out, or cancelled adoption retires any partial installation and
    /// returns the error, so a caller never publishes a generation with
    /// partially installed intake; dropping this future drops each in-flight
    /// owner adoption, whose locally-held subscriptions unsubscribe on drop.
    async fn adopt_owners(
        &self,
        generation: &Arc<TransportGeneration>,
    ) -> Result<Vec<Box<dyn GenerationIntakeHandle>>, TrellisClientError> {
        let owners = self
            .intake_owners
            .lock()
            .map(|owners| owners.clone())
            .unwrap_or_default();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(self.timeout_ms);
        let mut handles = Vec::with_capacity(owners.len());
        for owner in &owners {
            match tokio::time::timeout_at(deadline, owner.adopt(generation.clone())).await {
                Ok(Ok(handle)) => handles.push(handle),
                Ok(Err(error)) => {
                    drop(handles);
                    return Err(error);
                }
                Err(_) => {
                    drop(handles);
                    return Err(TrellisClientError::TransportUnavailable(
                        "framework intake adoption timed out".into(),
                    ));
                }
            }
        }
        Ok(handles)
    }

    fn clear_opening(&self, id: u64) {
        if let Ok(mut state) = self.lock_state() {
            if state
                .opening
                .as_ref()
                .is_some_and(|opening| opening.id == id)
            {
                state.opening = None;
            }
        }
    }

    fn is_current(&self, id: u64) -> bool {
        self.lock_state()
            .ok()
            .and_then(|state| state.current.as_ref().map(|generation| generation.id))
            == Some(id)
    }

    /// Handle one physical NATS event for a specific generation.
    fn on_generation_event(&self, id: u64, event: async_nats::Event) {
        match event {
            // A lost attachment is never leased again: mark it closed and let the
            // worker promote a safe survivor or open a recovery generation from
            // the newest authorization. Authorization is not suspended here — the
            // context remains valid and unrelated to this physical loss.
            async_nats::Event::Disconnected | async_nats::Event::Closed => {
                let baseline_lost = self.baseline_id.load(Ordering::Acquire) == id;
                self.mark_failed(id);
                if baseline_lost {
                    self.suspend_live();
                }
                tracing::info!(
                    event = "transport_generation.disconnected",
                    generation_id = id,
                    "physical attachment lost; scheduling generation recovery"
                );
            }
            async_nats::Event::Connected => {
                if self.baseline_id.load(Ordering::Acquire) == id {
                    if let Ok(slot) = self.live_slot.lock() {
                        if let Some(live) = slot.as_ref().and_then(Weak::upgrade) {
                            live.resume();
                        }
                    }
                }
            }
            async_nats::Event::ServerError(async_nats::ServerError::AuthorizationViolation)
                if self.is_current(id) =>
            {
                self.contexts.suspend();
                self.contexts.request_refresh();
                if let Some(notice) = &self.notice {
                    notice.mark_disconnected();
                }
                self.mark_failed(id);
                tracing::info!(
                    event = "transport_generation.rejected",
                    generation_id = id,
                    "broker rejected the attachment; scheduling recovery"
                );
            }
            _ => {}
        }
    }

    fn suspend_live(&self) {
        if let Ok(slot) = self.live_slot.lock() {
            if let Some(live) = slot.as_ref().and_then(Weak::upgrade) {
                live.suspend();
            }
        }
    }

    /// Mark a known or opening generation closed and close its physical
    /// connection now.
    ///
    /// Closing immediately (rather than waiting for the worker) prevents the
    /// lost attachment from continuing to reconnect with its frozen, possibly
    /// expired credential inside an immutable generation. A candidate that has
    /// not yet produced a connection records the loss so it is never published.
    fn mark_failed(&self, id: u64) {
        let mut built = None;
        if let Ok(state) = self.lock_state() {
            if state
                .current
                .as_ref()
                .is_some_and(|generation| generation.id == id)
            {
                built = state.current.clone();
            } else if let Some(generation) =
                state.draining.iter().find(|generation| generation.id == id)
            {
                built = Some(generation.clone());
            } else if let Some(opening) = state.opening.as_ref().filter(|opening| opening.id == id)
            {
                match &opening.generation {
                    Some(generation) => built = Some(generation.clone()),
                    None => opening.failed.store(true, Ordering::Release),
                }
            }
        }
        if let Some(generation) = built {
            close_generation(&generation);
        }
        self.request_work();
        self.signal_state();
    }

    fn record_notice(&self, generation: &TransportGeneration, desired: &TransportAuthorizationV1) {
        if let Some(notice) = &self.notice {
            let now = self.contexts.corrected_now_seconds().unwrap_or(0);
            let _ = notice.record_admission(
                generation.context_digest.clone(),
                generation.admitted_policy.clone(),
                Some(desired.clone()),
                now,
            );
        }
    }
}

/// One automatic transport-generation manager per logical connection.
#[derive(Clone)]
pub(crate) struct TransportGenerationManager {
    inner: Arc<ManagerInner>,
}

impl TransportGenerationManager {
    /// Open the initial generation, start the one adoption worker, and return the
    /// manager with a baseline lease that keeps the initial framework loop's
    /// attachment alive until provider/live intake migrates.
    pub(crate) async fn connect(
        auth: Arc<SessionAuth>,
        contexts: Arc<AuthorizationContextCache>,
        timeout_ms: u64,
        live_slot: Arc<Mutex<Option<Weak<LiveSessionManager>>>>,
        notice: Option<TransportAuthorizationState>,
    ) -> Result<(Self, TransportLease), TrellisClientError> {
        let (state_version, _) = watch::channel(0u64);
        let (work, work_rx) = watch::channel(0u64);
        let inner = Arc::new(ManagerInner {
            auth,
            contexts,
            timeout_ms,
            live_slot,
            closed: AtomicBool::new(false),
            state: Mutex::new(ManagerState::default()),
            state_version,
            work,
            intake_owners: Mutex::new(Vec::new()),
            reconcile: tokio::sync::Mutex::new(()),
            next_id: AtomicU64::new(1),
            baseline_id: AtomicU64::new(0),
            notice,
        });
        let snapshot = inner.contexts.own_transport_snapshot()?;
        let id = inner.alloc_id();
        inner.baseline_id.store(id, Ordering::Release);
        let generation = open_generation(&inner, id, &snapshot, "initial").await?;
        generation.set_state(GenerationState::Current);
        let lost = {
            let mut state = inner.lock_state()?;
            // Consume the opening slot atomically with the initial publication so
            // a loss recorded during admission is never published.
            let lost = state
                .opening
                .as_ref()
                .filter(|opening| opening.id == id)
                .map(|opening| opening.failed.load(Ordering::Acquire))
                .unwrap_or(true)
                || generation.state() == GenerationState::Closed;
            state.opening = None;
            if !lost {
                state.current = Some(generation.clone());
            }
            lost
        };
        if lost {
            let _ = generation.nats.drain().await;
            return Err(TrellisClientError::TransportUnavailable(
                "initial transport generation was lost during admission".into(),
            ));
        }
        inner.record_notice(&generation, &snapshot.policy);
        let baseline = generation.lease();
        // Subscribe before the worker is spawned so no work request is missed.
        let worker_inner = inner.clone();
        tokio::spawn(async move { run_worker(worker_inner, work_rx).await });
        Ok((Self { inner }, baseline))
    }

    /// Notify the adoption worker that current authorization changed.
    pub(crate) fn authorization_promoted(&self) {
        self.inner.request_work();
        self.inner.signal_state();
    }

    /// Register one framework owner and install its broker-ready intake on the
    /// current generation.
    ///
    /// Serialized with candidate activation, so a candidate cannot become the
    /// default without this owner's intake: either the preactivation barrier
    /// already adopted it, or this attach adopts the newly-current generation.
    pub(crate) async fn attach_intake(
        &self,
        owner: Arc<dyn GenerationIntake>,
    ) -> Result<(), TrellisClientError> {
        let _guard = self.inner.reconcile.lock().await;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(TrellisClientError::TransportUnavailable(
                "logical transport connection is closed".into(),
            ));
        }
        let generation = self.inner.lock_state()?.current.clone().ok_or_else(|| {
            TrellisClientError::TransportUnavailable(
                "no current transport generation is available".into(),
            )
        })?;
        // Install broker-ready intake **before** registering the owner, so a
        // failed or timed-out install never leaves a resurrectable registration
        // behind that a later candidate would blindly adopt.
        let budget = Duration::from_millis(self.inner.timeout_ms);
        let handle = match tokio::time::timeout(budget, owner.adopt(generation.clone())).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                return Err(TrellisClientError::TransportUnavailable(
                    "framework intake adoption timed out".into(),
                ))
            }
        };
        // Recheck after the await: a close or a supersession during adoption must
        // not leave the freshly installed intake registered on a stale generation.
        let still_current = self
            .inner
            .lock_state()
            .ok()
            .and_then(|state| state.current.as_ref().map(|current| current.id))
            == Some(generation.id);
        if self.inner.closed.load(Ordering::Acquire)
            || generation.state() == GenerationState::Closed
            || !still_current
        {
            drop(handle);
            return Err(TrellisClientError::TransportUnavailable(
                "the current transport generation changed during intake adoption".into(),
            ));
        }
        generation.push_intake(handle);
        if let Ok(mut owners) = self.inner.intake_owners.lock() {
            owners.push(owner);
        }
        Ok(())
    }

    /// Acquire a ready generation that can perform the exact requirement.
    ///
    /// A requirement covered by current application authority but not yet
    /// admitted waits for automatic adoption within `deadline`. A requirement
    /// current authority does not grant falls through to the current attachment
    /// so the ordinary broker decision applies. Suspended or invalid
    /// authorization is never bypassed by leasing an already-ready transport.
    pub(crate) async fn acquire_for(
        &self,
        publish: &[String],
        subscribe: &[String],
        deadline: Instant,
    ) -> Result<TransportLease, TrellisClientError> {
        let mut rx = self.inner.state_version.subscribe();
        loop {
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(TrellisClientError::TransportUnavailable(
                    "logical transport connection is closed".into(),
                ));
            }
            let desired = match self.inner.contexts.own_transport_snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => return Err(error),
            };
            let now = self.inner.contexts.corrected_now_seconds().unwrap_or(0);
            let policy = &desired.policy;
            if let Some(lease) = self.try_acquire_covering(publish, subscribe, now, policy)? {
                return Ok(lease);
            }
            if !policy_covers(policy, publish, subscribe, now)? {
                // Not granted: use a safe current attachment and let the broker
                // produce the ordinary denial.
                if let Some(lease) = self.try_acquire_current(now, policy)? {
                    return Ok(lease);
                }
                return Err(TrellisClientError::TransportUnavailable(
                    "no admitted transport is available".into(),
                ));
            }
            await_adoption(&self.inner, &mut rx, deadline).await?;
        }
    }

    /// Wait until the current generation serves the newest desired policy.
    ///
    /// A current attachment that already serves the newest policy returns
    /// immediately without any physical change.
    pub(crate) async fn ensure_adopted(&self) -> Result<(), TrellisClientError> {
        let deadline = Instant::now() + Duration::from_millis(self.inner.timeout_ms);
        let mut rx = self.inner.state_version.subscribe();
        loop {
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(TrellisClientError::TransportUnavailable(
                    "logical transport connection is closed".into(),
                ));
            }
            let desired = self.inner.contexts.own_transport_snapshot()?;
            if self.current_serves(&desired.policy)? {
                return Ok(());
            }
            await_adoption(&self.inner, &mut rx, deadline).await?;
        }
    }

    fn current_serves(
        &self,
        policy: &TransportAuthorizationV1,
    ) -> Result<bool, TrellisClientError> {
        let now = self.inner.contexts.corrected_now_seconds().unwrap_or(0);
        let state = self.inner.lock_state()?;
        Ok(state.current.as_ref().is_some_and(|generation| {
            generation.state() != GenerationState::Closed
                && generation_serves(generation, policy, now).unwrap_or(false)
        }))
    }

    /// Close every generation and stop automatic adoption.
    pub(crate) fn close(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.request_work();
        self.inner.signal_state();
    }

    fn try_acquire_covering(
        &self,
        publish: &[String],
        subscribe: &[String],
        now: i64,
        desired: &TransportAuthorizationV1,
    ) -> Result<Option<TransportLease>, TrellisClientError> {
        let state = self.inner.lock_state()?;
        let mut candidates: Vec<&Arc<TransportGeneration>> = Vec::new();
        if let Some(current) = &state.current {
            candidates.push(current);
        }
        candidates.extend(state.draining.iter());
        for generation in candidates {
            if generation.state() == GenerationState::Closed {
                continue;
            }
            // Never hand out a generation current authority no longer covers.
            if !generation_is_safe(generation, desired, now)? {
                continue;
            }
            if policy_covers(&generation.admitted_policy, publish, subscribe, now)? {
                return Ok(Some(generation.lease()));
            }
        }
        Ok(None)
    }

    fn try_acquire_current(
        &self,
        now: i64,
        desired: &TransportAuthorizationV1,
    ) -> Result<Option<TransportLease>, TrellisClientError> {
        let state = self.inner.lock_state()?;
        match &state.current {
            Some(current) if current.state() != GenerationState::Closed => {
                if !generation_is_safe(current, desired, now)? {
                    return Ok(None);
                }
                Ok(Some(current.lease()))
            }
            _ => Ok(None),
        }
    }
}

/// Wait one bounded adoption step: request work on the work channel, then wait
/// for a condition change or the retry interval, whichever comes first.
///
/// The caller's request never resolves its own wait because it is issued on a
/// different channel than the one it awaits. The state receiver must be
/// subscribed before the caller checks its condition so a change that lands
/// between the check and this await is never lost.
async fn request_adoption_step(
    work: &watch::Sender<u64>,
    state_rx: &mut watch::Receiver<u64>,
    remaining: Duration,
) {
    let next = work.borrow().wrapping_add(1);
    work.send_replace(next);
    let _ = tokio::time::timeout(remaining.min(ADOPTION_RETRY_INTERVAL), state_rx.changed()).await;
}

/// Wait one bounded step for adoption progress, re-requesting adoption while the
/// caller's deadline still holds.
async fn await_adoption(
    inner: &ManagerInner,
    rx: &mut watch::Receiver<u64>,
    deadline: Instant,
) -> Result<(), TrellisClientError> {
    if inner.closed.load(Ordering::Acquire) {
        return Err(TrellisClientError::TransportUnavailable(
            "logical transport connection is closed".into(),
        ));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(TrellisClientError::Timeout);
    }
    request_adoption_step(&inner.work, rx, remaining).await;
    if inner.closed.load(Ordering::Acquire) {
        return Err(TrellisClientError::TransportUnavailable(
            "logical transport connection is closed".into(),
        ));
    }
    Ok(())
}

/// One long-lived adoption worker for a logical connection.
async fn run_worker(inner: Arc<ManagerInner>, mut work_rx: watch::Receiver<u64>) {
    loop {
        if inner.closed.load(Ordering::Acquire) {
            shutdown(&inner).await;
            return;
        }
        {
            let _guard = inner.reconcile.lock().await;
            if let Err(error) = reconcile(&inner).await {
                tracing::warn!(%error, "transport generation reconciliation failed");
            }
            reap(&inner).await;
        }
        if inner.closed.load(Ordering::Acquire) {
            shutdown(&inner).await;
            return;
        }
        if work_rx.changed().await.is_err() {
            // The manager was dropped.
            return;
        }
    }
}

/// Reconcile ready generations against the newest desired policy.
///
/// Never mutates manager state until a decision is complete, so a classification
/// error leaves the ready set intact. Closes generations current authority no
/// longer covers, chooses a safe survivor as the default, and opens the newest
/// desired policy only when no safe generation fully serves it. A candidate is
/// published only while it is still live, its policy still matches the latest
/// desired policy, and the logical connection is still open.
async fn reconcile(inner: &Arc<ManagerInner>) -> Result<(), TrellisClientError> {
    loop {
        if inner.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let desired = match inner.contexts.own_transport_snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => return Ok(()),
        };
        let desired_policy_digest = desired.policy.digest()?;
        let now = inner.contexts.corrected_now_seconds().unwrap_or(0);

        // Snapshot without disturbing state, so an error below cannot abandon
        // ready generations.
        let (known, previous_current, previous_draining) = {
            let state = inner.lock_state()?;
            let known = state
                .current
                .iter()
                .chain(state.draining.iter())
                .cloned()
                .collect::<Vec<_>>();
            (
                known,
                state.current.as_ref().map(|generation| generation.id),
                state
                    .draining
                    .iter()
                    .map(|generation| generation.id)
                    .collect::<Vec<_>>(),
            )
        };

        let mut keep = Vec::new();
        let mut close = Vec::new();
        for generation in known {
            let safe = generation.state() != GenerationState::Closed
                && generation_is_safe(&generation, &desired.policy, now)?;
            if safe {
                keep.push(generation);
            } else {
                close.push(generation);
            }
        }

        let serving = keep
            .iter()
            .find(|generation| generation_serves(generation, &desired.policy, now).unwrap_or(false))
            .cloned();
        let mut needs_adoption = serving.is_none();
        let mut chosen =
            serving.or_else(|| keep.iter().max_by_key(|generation| generation.id).cloned());
        // A safe survivor restored as current may have had its generic intake
        // retired when it was superseded. Re-install broker-ready intake before
        // promoting it; on failure open a ready replacement rather than a current
        // with dead intake.
        let mut promoted_intake: Vec<Box<dyn GenerationIntakeHandle>> = Vec::new();
        if chosen
            .as_ref()
            .is_some_and(|candidate| !candidate.intake_active())
        {
            let candidate = chosen.clone().expect("restore candidate is chosen");
            match inner.adopt_owners(&candidate).await {
                Ok(handles) => promoted_intake = handles,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        generation_id = candidate.id,
                        "failed to restore survivor intake; opening a replacement"
                    );
                    chosen = None;
                    needs_adoption = true;
                }
            }
            // A newer desired policy after the await must restart reconciliation
            // against the new snapshot rather than publish a survivor (or open a
            // candidate) for the stale desired policy.
            let stale_after_adopt = match inner.contexts.own_transport_snapshot() {
                Ok(snapshot) => match snapshot.policy.digest() {
                    Ok(digest) => digest != desired_policy_digest,
                    Err(_) => true,
                },
                Err(_) => true,
            };
            if stale_after_adopt {
                drop(promoted_intake);
                continue;
            }
        }

        // Publication under one state lock, the same atomic discipline as the new
        // candidate: a survivor concurrently closed by `mark_failed` while its
        // intake was prepared is never republished as current, and no freshly
        // installed intake is attached to a lost generation.
        {
            let mut state = inner.lock_state()?;
            if inner.closed.load(Ordering::Acquire) {
                drop(state);
                drop(promoted_intake);
                return Ok(());
            }
            let survivor_closed = chosen
                .as_ref()
                .is_some_and(|candidate| candidate.state() == GenerationState::Closed);
            if survivor_closed {
                promoted_intake.clear();
                chosen = None;
                needs_adoption = true;
            }
            let chosen_id = chosen.as_ref().map(|generation| generation.id);
            state.current = chosen.clone();
            state.draining = keep
                .iter()
                .filter(|generation| Some(generation.id) != chosen_id)
                .cloned()
                .collect();
            // Terminal state is monotonic: a generation concurrently closed by
            // `mark_failed` is never reactivated as current or draining. The
            // state transition and the freshly prepared intake install share this
            // critical section with the current-pointer publication, so a
            // concurrent `mark_failed` cannot close the survivor after
            // publication but before its intake is attached.
            if let Some(chosen) = &chosen {
                chosen.set_state(GenerationState::Current);
                for handle in promoted_intake.drain(..) {
                    chosen.push_intake(handle);
                }
            }
        }
        let chosen_id = chosen.as_ref().map(|generation| generation.id);
        let next_draining: Vec<u64> = keep
            .iter()
            .filter(|generation| Some(generation.id) != chosen_id)
            .map(|generation| generation.id)
            .collect();
        for generation in &keep {
            if Some(generation.id) != chosen_id {
                generation.set_state(GenerationState::Draining);
            }
        }
        for generation in &close {
            close_generation(generation);
        }
        if let Some(chosen) = &chosen {
            inner.record_notice(chosen, &desired.policy);
        }
        // Only signal a real ready-set change so the worker does not busy-loop
        // on its own signal.
        if previous_current != chosen_id || previous_draining != next_draining || !close.is_empty()
        {
            inner.signal_state();
        }
        // Framework owners follow the current generation identity.

        if !needs_adoption {
            return Ok(());
        }

        // Open a candidate for the newest desired policy.
        let id = inner.alloc_id();
        let candidate = match open_generation(inner, id, &desired, "authorization_growth").await {
            Ok(candidate) => candidate,
            Err(error) => {
                inner.clear_opening(id);
                // Do not self-signal: retries stay demand-driven so an outage
                // does not spin the worker.
                tracing::warn!(
                    event = "transport_generation.open_failed",
                    generation_id = id,
                    %error,
                    "failed to open transport generation"
                );
                return Err(error);
            }
        };

        // A superseded policy must not be published. Read it before taking the
        // state lock to preserve the contexts-before-state lock order.
        let latest = match inner.contexts.own_transport_snapshot() {
            Ok(snapshot) => snapshot.policy.digest()?,
            Err(_) => {
                inner.clear_opening(id);
                close_generation(&candidate);
                return Ok(());
            }
        };
        if latest != desired_policy_digest {
            inner.clear_opening(id);
            close_generation(&candidate);
            continue;
        }

        // Preactivation readiness barrier: every framework owner installs
        // broker-ready generic intake on the candidate **before** the candidate
        // becomes the default generation, with each adoption bounded by the
        // connection budget. A failed or timed-out adoption rolls the candidate
        // back and leaves the current generation untouched.
        let intake = match inner.adopt_owners(&candidate).await {
            Ok(intake) => intake,
            Err(error) => {
                inner.clear_opening(id);
                close_generation(&candidate);
                tracing::warn!(
                    event = "transport_generation.open_failed",
                    generation_id = id,
                    %error,
                    "candidate intake barrier failed; rolling back"
                );
                return Ok(());
            }
        };
        // A candidate that became stale while its intake was prepared must not be
        // published; the next reconcile opens a fresh one from the newest policy.
        let latest_after_barrier = match inner.contexts.own_transport_snapshot() {
            Ok(snapshot) => snapshot.policy.digest()?,
            Err(_) => {
                drop(intake);
                inner.clear_opening(id);
                close_generation(&candidate);
                return Ok(());
            }
        };
        if latest_after_barrier != desired_policy_digest {
            drop(intake);
            inner.clear_opening(id);
            close_generation(&candidate);
            continue;
        }

        {
            let mut state = inner.lock_state()?;
            // Atomically consume the opening slot and re-read the recorded loss
            // so a disconnect in the window between open and publish is never
            // lost, and a closed candidate is never activated.
            let lost = state
                .opening
                .as_ref()
                .filter(|opening| opening.id == id)
                .map(|opening| opening.failed.load(Ordering::Acquire))
                .unwrap_or(true);
            state.opening = None;
            if lost
                || inner.closed.load(Ordering::Acquire)
                || candidate.state() == GenerationState::Closed
            {
                drop(state);
                close_generation(&candidate);
                return Ok(());
            }
            let previous = state.current.take();
            if let Some(previous) = previous {
                if previous.id != candidate.id {
                    // Retire the superseded generation's generic intake promptly;
                    // its accepted work keeps running until its leases release.
                    previous.retire_intake();
                    previous.set_state(GenerationState::Draining);
                    state.draining.push(previous);
                }
            }
            candidate.set_state(GenerationState::Current);
            if candidate.state() == GenerationState::Closed {
                // Lost between the check above and this activation.
                state.current = None;
                drop(state);
                close_generation(&candidate);
                return Ok(());
            }
            state.current = Some(candidate.clone());
            // Install the broker-ready intake in the same critical section as the
            // current-pointer publication, so a concurrent `mark_failed` can
            // never close the candidate after publication but before its intake
            // is attached.
            for handle in intake {
                candidate.push_intake(handle);
            }
        }
        inner.record_notice(&candidate, &desired.policy);
        tracing::info!(
            event = "transport_generation.activated",
            generation_id = candidate.id,
            "activated transport generation"
        );
        inner.signal_state();
        return Ok(());
    }
}

/// Close draining generations whose leases have all been released.
async fn reap(inner: &Arc<ManagerInner>) {
    let mut to_close = Vec::new();
    if let Ok(mut state) = inner.lock_state() {
        let mut still = Vec::new();
        for generation in state.draining.drain(..) {
            if generation.state() == GenerationState::Draining && generation.leases() == 0 {
                to_close.push(generation);
            } else {
                still.push(generation);
            }
        }
        state.draining = still;
    }
    for generation in &to_close {
        close_generation(generation);
    }
}

/// Drain every generation and clear manager state after logical close.
async fn shutdown(inner: &Arc<ManagerInner>) {
    let known = {
        let Ok(mut state) = inner.lock_state() else {
            return;
        };
        let mut all = Vec::new();
        if let Some(current) = state.current.take() {
            all.push(current);
        }
        all.append(&mut state.draining);
        if let Some(opening) = state.opening.take().and_then(|opening| opening.generation) {
            all.push(opening);
        }
        all
    };
    for generation in known {
        generation.set_state(GenerationState::Closed);
        let _ = generation.nats.drain().await;
    }
}

/// Mark a generation closed and close its physical connection.
///
/// `drain` unsubscribes, flushes, and then makes the connection handler exit,
/// which closes the socket; it is the SDK-side cooperative close. The broker
/// remains authoritative for reductions and kicks any uncooperative socket.
fn close_generation(generation: &Arc<TransportGeneration>) {
    generation.set_state(GenerationState::Closed);
    let nats = generation.nats.clone();
    let id = generation.id;
    tokio::spawn(async move {
        let _ = nats.drain().await;
        tracing::info!(
            event = "transport_generation.closed",
            generation_id = id,
            "closed transport generation"
        );
    });
}

/// Whether policy `admitted` is still covered by current authority `allowed`.
fn generation_is_safe(
    generation: &TransportGeneration,
    allowed: &TransportAuthorizationV1,
    now: i64,
) -> Result<bool, TrellisClientError> {
    Ok(generation.admitted_policy.classify(allowed, now)?
        != TransportPolicyClass::ReductionRequired)
}

/// Whether policy `admitted` fully serves the desired policy `allowed`.
fn generation_serves(
    generation: &TransportGeneration,
    allowed: &TransportAuthorizationV1,
    now: i64,
) -> Result<bool, TrellisClientError> {
    Ok(allowed.classify(&generation.admitted_policy, now)?
        != TransportPolicyClass::ReductionRequired)
}

/// Open and admit one immutable generation with an exact captured context.
async fn open_generation(
    inner: &Arc<ManagerInner>,
    id: u64,
    snapshot: &OwnTransportSnapshot,
    reason: &'static str,
) -> Result<Arc<TransportGeneration>, TrellisClientError> {
    tracing::info!(
        event = "transport_generation.opening",
        generation_id = id,
        reason,
        context_digest = %snapshot.context_digest,
        "opening transport generation"
    );
    let native = snapshot.runtime.transports.native.as_ref().ok_or_else(|| {
        TrellisClientError::TransportUnavailable(
            "bootstrap did not offer a native NATS transport".into(),
        )
    })?;
    let key_pair = Arc::new(inner.auth.nkey_pair()?);
    let session_nkey = key_pair.public_key();
    let context_digest = snapshot.context_digest.clone();
    let routing_jwt = snapshot.routing_jwt.clone();
    let state = Arc::new(AtomicU8::new(GenerationState::Draining as u8));
    let failed = Arc::new(AtomicBool::new(false));
    // Register the candidate before connecting so a loss during connect is
    // recorded and the candidate is never published dead.
    {
        let mut guard = inner.lock_state()?;
        guard.opening = Some(Opening {
            id,
            generation: None,
            failed: failed.clone(),
        });
    }

    let event_inner = inner.clone();
    let auth_state = state.clone();
    let authenticated = Arc::new(AtomicBool::new(false));
    let options = ConnectOptions::with_auth_callback(move |nonce| {
        let key_pair = key_pair.clone();
        let session_nkey = session_nkey.clone();
        let routing_jwt = routing_jwt.clone();
        let context_digest = context_digest.clone();
        let auth_state = auth_state.clone();
        let authenticated = authenticated.clone();
        async move {
            // A generation the manager has already discarded must not reattach
            // itself: refusing here makes a reconnect with the frozen credential
            // fail closed instead of silently replacing the physical
            // attachment inside an immutable generation.
            if auth_state.load(Ordering::Acquire) == GenerationState::Closed as u8 {
                return Err(async_nats::AuthError::new(
                    "transport generation was closed before connect",
                ));
            }
            // A generation authenticates exactly one physical CONNECT. Any
            // later attempt is a reconnect and is refused directly, independent
            // of when the asynchronous lifecycle handler marks the generation
            // closed. async-nats treats an auth-callback error as fatal, so no
            // replacement physical connection can be established. Failover is
            // the manager's job: it opens a fresh generation.
            if authenticated.swap(true, Ordering::AcqRel) {
                return Err(async_nats::AuthError::new(
                    "transport generation already authenticated; refusing reconnect",
                ));
            }
            let mut credentials = async_nats::Auth::new();
            credentials.nkey = Some(session_nkey);
            credentials.jwt = None;
            credentials.signature =
                Some(key_pair.sign(&nonce).map_err(async_nats::AuthError::new)?);
            credentials.token = Some(
                serde_json::to_string(&NatsConnectToken {
                    format: NATS_CONNECT_TOKEN_FORMAT,
                    context_digest,
                    routing_jwt,
                })
                .map_err(async_nats::AuthError::new)?,
            );
            Ok(credentials)
        }
    })
    .connection_timeout(Duration::from_millis(inner.timeout_ms))
    // Bound in-place reconnects: a lost generation is replaced by the manager,
    // not repaired inside itself.
    .max_reconnects(1)
    .event_callback(move |event| {
        let inner = event_inner.clone();
        async move {
            inner.on_generation_event(id, event);
        }
    })
    .custom_inbox_prefix(snapshot.runtime.inbox_prefix.clone());
    let nats = match options.connect(native.nats_servers.clone()).await {
        Ok(nats) => nats,
        Err(error) => {
            inner.clear_opening(id);
            return Err(TrellisClientError::NatsConnect(error.to_string()));
        }
    };

    // G4 exact admission correlation: the broker marker must report exactly the
    // context this CONNECT presented. Missing, malformed, or differing evidence
    // closes the candidate and never publishes it.
    let admission = match read_own_admission(&nats, ADMISSION_READ_TIMEOUT).await {
        Some(admission) => admission,
        None => {
            inner.clear_opening(id);
            let _ = nats.drain().await;
            return Err(TrellisClientError::TransportUnavailable(
                "broker own-admission evidence is unavailable".into(),
            ));
        }
    };
    let Some(marker) = admission.context_digest else {
        inner.clear_opening(id);
        let _ = nats.drain().await;
        return Err(TrellisClientError::TransportUnavailable(
            "broker own-admission marker is malformed".into(),
        ));
    };
    if marker != snapshot.context_digest {
        inner.clear_opening(id);
        let _ = nats.drain().await;
        return Err(TrellisClientError::TransportUnavailable(
            "broker admitted a different authorization context".into(),
        ));
    }
    // A loss observed before the connection was built, or a logical close during
    // admission, must not publish the candidate.
    if failed.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
        inner.clear_opening(id);
        let _ = nats.drain().await;
        return Err(TrellisClientError::TransportUnavailable(
            "candidate was lost during admission".into(),
        ));
    }
    let admitted_policy_digest = snapshot.policy.digest()?;
    tracing::info!(
        event = "transport_generation.admitted",
        generation_id = id,
        context_digest = %marker,
        policy_digest = %admitted_policy_digest,
        "admitted transport generation"
    );
    let generation = Arc::new(TransportGeneration {
        id,
        nats,
        context_digest: snapshot.context_digest.clone(),
        admitted_policy: snapshot.policy.clone(),
        admitted_policy_digest,
        physical_connection_id: admission.authenticated_user,
        state,
        leases: AtomicUsize::new(0),
        work: inner.work.clone(),
        intake: Mutex::new(Vec::new()),
        intake_active: AtomicBool::new(true),
    });
    // Install the built generation under the state lock and re-read the recorded
    // loss atomically. A disconnect that lands after the earlier check but
    // before this install either set `failed` while the slot held no generation
    // (observed here) or observes the installed generation and closes it; there
    // is no window in which a lost candidate is published.
    let lost = {
        let mut guard = inner.lock_state()?;
        let lost = guard
            .opening
            .as_ref()
            .filter(|opening| opening.id == id)
            .map(|opening| opening.failed.load(Ordering::Acquire))
            .unwrap_or(true)
            || inner.closed.load(Ordering::Acquire);
        if lost {
            guard.opening = None;
        } else if let Some(opening) = guard.opening.as_mut().filter(|opening| opening.id == id) {
            opening.generation = Some(generation.clone());
        }
        lost
    };
    if lost {
        let _ = generation.nats.drain().await;
        return Err(TrellisClientError::TransportUnavailable(
            "candidate was lost during admission".into(),
        ));
    }
    Ok(generation)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller that requests work must not observe its own request as a
    /// condition change; otherwise the wait degrades into a busy spin. The step
    /// is paced by the retry interval until a real ready-set change arrives.
    #[tokio::test]
    async fn adoption_wait_is_paced_not_self_woken() {
        let (state, mut state_rx) = watch::channel(0u64);
        let (work, _work_rx) = watch::channel(0u64);

        let started = Instant::now();
        let mut iterations = 0u32;
        while started.elapsed() < ADOPTION_RETRY_INTERVAL / 4 {
            request_adoption_step(&work, &mut state_rx, Duration::from_secs(30)).await;
            iterations += 1;
        }
        assert!(
            iterations <= 1,
            "adoption wait must be paced by the retry interval, not self-woken; ran {iterations} steps"
        );

        // A real condition change resolves the wait immediately.
        state.send_replace(1);
        let changed = tokio::time::timeout(Duration::from_millis(100), state_rx.changed()).await;
        assert!(changed.is_ok(), "a ready-set change must wake the waiter");
    }
}
