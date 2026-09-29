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

/// One authoritative, latched reason the logical connection can no longer serve
/// application work. A generation revocation is not one of these: it is
/// recoverable by adopting the newest authorization. Only the authorization
/// controller's terminal refresh result or an explicit owner close latches one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LogicalTerminalCause {
    /// The authorization controller authoritatively ended this connection; a
    /// refresh for the installed session was rejected with a terminal code.
    Authorization(String),
    /// The owner explicitly closed the logical connection.
    Closed,
}

impl LogicalTerminalCause {
    /// The client error a waiting surface reports when the logical connection is
    /// terminally finished.
    pub(crate) fn client_error(&self) -> TrellisClientError {
        match self {
            Self::Authorization(code) => TrellisClientError::AuthorizationUnavailable(format!(
                "authorization context is no longer valid: {code}"
            )),
            Self::Closed => TrellisClientError::TransportUnavailable(
                "logical transport connection is closed".into(),
            ),
        }
    }
}

/// Why one generation's framework intake is being retired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GenerationIntakeRetireReason {
    /// A newer generation superseded this one; accepted work continues.
    Superseded,
    /// Current authority no longer covers the admitted policy.
    AuthorityReduced,
    /// The physical attachment was lost.
    PhysicalFailure,
    /// The logical connection reached an authoritative terminal state.
    LogicalTerminal,
    /// The logical connection is shutting down.
    Shutdown,
}

impl GenerationIntakeRetireReason {
    /// Whether this retirement must terminate the generation's support without
    /// waiting for accepted work: a physical loss, a policy no longer covered by
    /// authority, an authoritative terminal, or a logical shutdown. Normal
    /// supersession is graceful and waits for accepted work to drain.
    fn is_forced(self) -> bool {
        matches!(
            self,
            Self::AuthorityReduced | Self::PhysicalFailure | Self::LogicalTerminal | Self::Shutdown
        )
    }
}

/// A framework owner's broker-ready generic intake on one generation.
pub(crate) trait GenerationIntakeHandle: Send + Sync {
    /// Stop accepting new intake. Idempotent and nonblocking: already-accepted
    /// work continues on its own until [`Self::intake_stopped`] resolves.
    fn retire(&self, reason: GenerationIntakeRetireReason);

    /// Resolve once every outstanding intake is accounted for: no queued broker
    /// delivery remains, and every accepted handler future has finished. A
    /// handle may conservatively wait its sequential handler loop rather than
    /// only unsubscribing.
    fn intake_stopped(&self) -> BoxFuture<'_, ()>;

    /// Release owned resources. Idempotent and invoked once, asynchronously,
    /// after [`Self::intake_stopped`] on the graceful path. It must not await
    /// accepted work: on a force termination it is invoked without an
    /// `intake_stopped` wait, so a never-ending accepted item can never block the
    /// release (abandoned work governs itself through its own lease/RAII).
    fn dispose(&self) -> BoxFuture<'_, ()>;
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

/// A preactivation adoption failure and the owners that had already installed
/// broker-ready intake before it. The caller owns draining the installed handles
/// so a later owner's failure never drops earlier accepted work.
struct AdoptFailure {
    error: TrellisClientError,
    installed: Vec<Box<dyn GenerationIntakeHandle>>,
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
    /// Authoritative lease admission and live-count predicate. `closed` is the
    /// irreversible admission fence: once set (a close or a reap decided to
    /// close), no new fresh lease can be admitted, so a close can never race a
    /// lease that starts after the close observed zero.
    lease_state: Arc<std::sync::Mutex<LeaseState>>,
    /// Wake-only epoch for lease waiters. The payload is never trusted as the
    /// predicate (the count is read from `lease_state`); a `watch` is used so
    /// every waiter and a late subscriber observe a version change.
    lease_wake: watch::Sender<u64>,
    /// Broadcast force latch: once set, every in-flight and later disposal sweep
    /// stops waiting for accepted work and disposes, so a force termination can
    /// override a graceful sweep (including retained baseline/Live leases).
    disposal_force: watch::Sender<bool>,
    /// The manager's work channel; a lease release requests a reap.
    work: watch::Sender<u64>,
    /// Framework owners' broker-ready intake installed on this generation before
    /// it became the default. A superseded generation's intake is retired, its
    /// accepted work drained, and its handles disposed before the physical
    /// attachment closes.
    intake: Mutex<Vec<Box<dyn GenerationIntakeHandle>>>,
    /// Whether broker-ready intake is currently installed. A superseded
    /// generation's intake is retired; restoring it as current must re-adopt
    /// owners rather than resurrecting a current with dead intake.
    intake_active: AtomicBool,
    /// Number of live installed handles across every install on this generation.
    intake_installed: AtomicUsize,
    /// Number of in-flight retire+dispose sweeps. Shared with each detached sweep
    /// so a completion is observed without blocking the retire.
    intake_disposing: Arc<AtomicUsize>,
}

impl TransportGeneration {
    fn state(&self) -> GenerationState {
        GenerationState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Transition the generation state, refusing to leave the terminal state.
    ///
    /// A concurrent [`close`](Self::set_state) wins: a closed generation is
    /// never reactivated as current or draining.
    /// Transition the generation state under the lease lock and broadcast the
    /// change, so a disposal waiter that must observe "Draining AND zero" reads a
    /// consistent state and is woken on every transition.
    fn set_state(&self, state: GenerationState) {
        let _lease = self
            .lease_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.set_state_locked(state);
    }

    /// [`Self::set_state`] with the lease lock already held by the caller.
    fn set_state_locked(&self, state: GenerationState) {
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
                Ok(_) => {
                    if current != next {
                        self.wake_lease_waiters();
                    }
                    return;
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// The authoritative live lease count, read under the lease lock.
    fn leases(&self) -> usize {
        self.lease_state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
    }

    /// Start one generation lease, or refuse when the generation is closed or
    /// its admission fence is set. Admission is checked under the lease lock, so
    /// a fresh lease can never start after a close or a disposal has begun.
    pub(crate) fn lease(self: &Arc<Self>) -> Option<TransportLease> {
        let mut state = self
            .lease_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.disposing
            || state.forced
            || state.closed
            || self.state() == GenerationState::Closed
        {
            return None;
        }
        state.live += 1;
        drop(state);
        Some(TransportLease {
            generation: self.clone(),
        })
    }

    /// Whether this generation is fenced for admission and restoration: disposal
    /// has begun, it was force-terminated, or it closed. A fenced generation can
    /// be neither leased nor restored as a survivor; a fresh generation is opened
    /// instead.
    fn is_fenced(&self) -> bool {
        let state = self
            .lease_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.disposing || state.forced || state.closed
    }

    /// Conditionally promote to Current only when the generation is not fenced,
    /// under the same lease lock as the disposal decision. Returns whether it
    /// became Current; a fenced generation is never published as a usable current.
    fn promote_to_current(&self) -> bool {
        let lease = self
            .lease_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if lease.disposing || lease.forced || lease.closed {
            return false;
        }
        self.set_state_locked(GenerationState::Current);
        drop(lease);
        self.state() == GenerationState::Current
    }

    /// Irreversibly fence lease admission. Set once a close commits, so the
    /// close can never race a fresh lease that starts after it observed zero.
    fn close_leases(&self) {
        let already = {
            let mut state = self
                .lease_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let already = state.closed;
            state.closed = true;
            already
        };
        if !already {
            self.wake_lease_waiters();
        }
    }

    /// Irreversibly latch force termination for this generation. A forced
    /// generation is fenced for admission and restoration, and every in-flight
    /// sweep stops its graceful wait. The single entrypoint for force.
    fn force_leases(&self) {
        let newly = {
            let mut state = self
                .lease_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let newly = !state.forced;
            state.forced = true;
            newly
        };
        if newly {
            // Wake lease waiters on the shared epoch, and wake the in-flight
            // `intake_stopped` select on the force watch.
            self.disposal_force.send_replace(true);
            self.wake_lease_waiters();
        }
    }

    /// Wake lease waiters without carrying the predicate: a waiter re-reads the
    /// authoritative count under the lease lock.
    fn wake_lease_waiters(&self) {
        let next = self.lease_wake.borrow().wrapping_add(1);
        self.lease_wake.send_replace(next);
    }

    fn request_work(&self) {
        let next = self.work.borrow().wrapping_add(1);
        self.work.send_replace(next);
    }

    /// Attach one framework owner's broker-ready intake handle to this
    /// generation, or force-own it immediately when the generation is already
    /// closed or its disposal has begun. The closed/admission check, the
    /// registration, and the active flag all happen under the intake lock,
    /// mutually exclusive with [`Self::take_intake`], so a handle can never be
    /// registered after the generation stopped owning intake or once disposal
    /// started.
    fn push_intake(&self, handle: Box<dyn GenerationIntakeHandle>) {
        let mut intake = self
            .intake
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fenced = {
            let lease = self
                .lease_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            lease.disposing || lease.forced || lease.closed
        };
        if fenced || self.state() == GenerationState::Closed {
            drop(intake);
            // The generation closed or began disposing under the install: it
            // still owns and force-disposes the late handle.
            handle.retire(GenerationIntakeRetireReason::PhysicalFailure);
            self.spawn_disposal(vec![handle], true);
            return;
        }
        intake.push(handle);
        self.intake_installed.fetch_add(1, Ordering::AcqRel);
        self.intake_active.store(true, Ordering::Release);
    }

    /// Whether broker-ready intake is currently installed on this generation.
    fn intake_active(&self) -> bool {
        self.intake_active.load(Ordering::Acquire)
    }

    /// Whether the generation owns no live intake and no in-flight disposal
    /// sweep. A draining generation closes only when this holds and its physical
    /// leases have released.
    fn intake_idle(&self) -> bool {
        self.intake_installed.load(Ordering::Acquire) == 0
            && self.intake_disposing.load(Ordering::Acquire) == 0
    }

    /// Atomically transition the generation state (when requested) and extract
    /// every installed handle under the intake lock.
    ///
    /// The transition and the extraction share the lock with
    /// [`Self::push_intake`], so no handle can be registered in the window
    /// between "stop owning intake" and "become Draining/Closed": a late install
    /// either lands before the transition and is extracted, or observes the
    /// terminal state and force-disposes itself.
    fn take_intake(&self, next: Option<GenerationState>) -> Vec<Box<dyn GenerationIntakeHandle>> {
        let mut intake = self
            .intake
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(state) = next {
            self.set_state(state);
        }
        self.intake_active.store(false, Ordering::Release);
        let handles = std::mem::take(&mut *intake);
        self.intake_installed
            .fetch_sub(handles.len(), Ordering::AcqRel);
        if next == Some(GenerationState::Closed) {
            // Fence lease admission while still holding the intake lock, so the
            // close transition and the fence are one step against `push_intake`
            // and `lease`.
            self.close_leases();
        }
        handles
    }

    /// Signal extracted handles and hand them to a detached disposal sweep.
    fn retire_handles(
        &self,
        handles: Vec<Box<dyn GenerationIntakeHandle>>,
        reason: GenerationIntakeRetireReason,
    ) {
        if handles.is_empty() {
            return;
        }
        for handle in &handles {
            handle.retire(reason);
        }
        self.spawn_disposal(handles, reason.is_forced());
    }

    /// Stop intake without changing the state: the generation stays available for
    /// a survivor re-adoption. Accepted work drains gracefully.
    fn retire_intake(&self, reason: GenerationIntakeRetireReason) {
        let handles = self.take_intake(None);
        self.retire_handles(handles, reason);
    }

    /// Stop intake and atomically mark the generation Draining.
    fn retire_intake_draining(&self, reason: GenerationIntakeRetireReason) {
        let handles = self.take_intake(Some(GenerationState::Draining));
        self.retire_handles(handles, reason);
    }

    /// Stop intake and atomically mark the generation Closed.
    fn retire_intake_closed(&self, reason: GenerationIntakeRetireReason) {
        let handles = self.take_intake(Some(GenerationState::Closed));
        self.retire_handles(handles, reason);
    }

    /// Drive one retire sweep to completion on a detached task.
    ///
    /// Graceful order: retire (already signalled) -> `intake_stopped` AND every
    /// accepted lease released -> `dispose`. A force termination sets the force
    /// latch, which overrides both an already-running graceful sweep and every
    /// later sweep so they dispose without waiting for accepted work. Never
    /// awaited inline, so a retire never blocks the adoption worker or a
    /// successor.
    fn spawn_disposal(&self, handles: Vec<Box<dyn GenerationIntakeHandle>>, forced: bool) {
        if handles.is_empty() {
            return;
        }
        if forced {
            self.force_leases();
        }
        self.intake_disposing.fetch_add(1, Ordering::AcqRel);
        let guard = DisposalGuard {
            disposing: Arc::clone(&self.intake_disposing),
            work: self.work.clone(),
            completed: false,
        };
        let lease_state = Arc::clone(&self.lease_state);
        let state = Arc::clone(&self.state);
        let lease_wake = self.lease_wake.clone();
        let mut force = self.disposal_force.subscribe();
        tokio::spawn(async move {
            let mut guard = guard;
            for handle in handles {
                if !*force.borrow() {
                    // Graceful: wait for intake to stop, then for accepted leases
                    // to release. Either wait is overridden by a force latch, a
                    // restore to Current, or an already-started disposal.
                    tokio::select! {
                        _ = handle.intake_stopped() => {
                            wait_lease_idle(&lease_state, &state, &lease_wake, &mut force).await;
                        }
                        _ = force.changed() => {}
                    }
                }
                handle.dispose().await;
            }
            guard.completed = true;
        });
    }
}

/// The authoritative lease admission, live-count, and disposal-start state.
///
/// `disposing` is the irreversible disposal-start fence: set atomically with
/// observing zero, after which no fresh lease may be admitted and no survivor may
/// be restored. It is distinct from `closed` (an explicit/terminal close).
#[derive(Default)]
struct LeaseState {
    live: usize,
    disposing: bool,
    forced: bool,
    closed: bool,
}

/// RAII completion for one disposal sweep: releases the in-flight counter and
/// signals a reap on drop, including on unwind, so a failed or cancelled
/// disposal can never permanently strand the generation's bookkeeping. An
/// interrupted (unwound/cancelled) sweep leaves an observable warning.
struct DisposalGuard {
    disposing: Arc<AtomicUsize>,
    work: watch::Sender<u64>,
    completed: bool,
}

impl Drop for DisposalGuard {
    fn drop(&mut self) {
        self.disposing.fetch_sub(1, Ordering::AcqRel);
        if !self.completed {
            tracing::warn!(
                event = "transport_generation.intake_disposal_interrupted",
                "framework intake disposal ended before completing; released bookkeeping"
            );
        }
        let next = self.work.borrow().wrapping_add(1);
        self.work.send_replace(next);
    }
}

/// Wait until the generation is Draining with zero live leases, or the scope is
/// terminated.
///
/// The count predicate and the state are read together under the lease lock (the
/// same lock `set_state` takes), so "disposal permitted" is observed atomically
/// with admission and the state transition. Returns when: force is latched or
/// disposal already began (the shared irreversible fence); or the generation is
/// Draining with zero live leases, at which point it sets the disposal-start
/// fence. A restored Current generation is **not** exempted: its retire-scope
/// cleanup stays pending until the generation is Draining with zero again, and
/// the epoch broadcast wakes this wait on every state transition so it never
/// spins while Current+zero. `force.changed()` overrides an in-flight wait.
async fn wait_lease_idle(
    lease_state: &std::sync::Mutex<LeaseState>,
    state: &AtomicU8,
    lease_wake: &watch::Sender<u64>,
    force: &mut watch::Receiver<bool>,
) {
    let mut receiver = lease_wake.subscribe();
    loop {
        {
            let mut lease = lease_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if lease.forced || lease.disposing {
                return;
            }
            if GenerationState::from_u8(state.load(Ordering::Acquire)) == GenerationState::Draining
                && lease.live == 0
            {
                // Irreversible disposal-start fence, set under the same lock as
                // the zero predicate, admission, and the state transition.
                lease.disposing = true;
                drop(lease);
                let next = lease_wake.borrow().wrapping_add(1);
                lease_wake.send_replace(next);
                return;
            }
        }
        if *force.borrow() {
            return;
        }
        tokio::select! {
            result = receiver.changed() => {
                if result.is_err() {
                    return;
                }
            }
            _ = force.changed() => {}
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

impl std::fmt::Debug for TransportLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransportLease")
            .field("generation_id", &self.generation.id)
            .finish()
    }
}

impl Clone for TransportLease {
    /// Retain one more independent owner of the same generation. Each owner
    /// releases its own count on drop, so one owner can never release another
    /// owner's only remaining lease. A clone extends already-admitted work, so it
    /// counts under the same lease lock but is not a fresh admission; the source
    /// lease keeps the count non-zero, so it can never race a zero observation.
    fn clone(&self) -> Self {
        let mut state = self
            .generation
            .lease_state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.live += 1;
        drop(state);
        Self {
            generation: Arc::clone(&self.generation),
        }
    }
}

impl Drop for TransportLease {
    fn drop(&mut self) {
        let zero = {
            let mut state = self
                .generation
                .lease_state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.live = state.live.saturating_sub(1);
            state.live == 0
        };
        if zero {
            // The last live lease released: wake disposal waiters and request a
            // reap. The wake carries no predicate; waiters re-read the count.
            self.generation.wake_lease_waiters();
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
    /// The one latched authoritative terminal cause for the logical connection.
    /// `None` while the connection may still recover; once set it is never
    /// overwritten. A generation revocation is not a terminal cause.
    terminal: watch::Sender<Option<LogicalTerminalCause>>,
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
    ) -> Result<Vec<Box<dyn GenerationIntakeHandle>>, AdoptFailure> {
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
                    return Err(AdoptFailure {
                        error,
                        installed: handles,
                    })
                }
                Err(_) => {
                    return Err(AdoptFailure {
                        error: TrellisClientError::TransportUnavailable(
                            "framework intake adoption timed out".into(),
                        ),
                        installed: handles,
                    })
                }
            }
        }
        Ok(handles)
    }

    /// Retire a generation's framework intake and keep it tracked as draining so
    /// its already-accepted work survives instead of being dropped. A generation
    /// already physically closed is only swept, never re-tracked.
    fn track_draining(
        &self,
        generation: &Arc<TransportGeneration>,
        reason: GenerationIntakeRetireReason,
    ) {
        generation.retire_intake_draining(reason);
        if generation.state() != GenerationState::Closed {
            if let Ok(mut state) = self.lock_state() {
                if !state
                    .draining
                    .iter()
                    .any(|existing| existing.id == generation.id)
                {
                    state.draining.push(generation.clone());
                }
            }
        }
        self.request_work();
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

    /// The error every waiting surface reports once the logical connection is
    /// finished for good: an authoritative terminal cause, or an explicit close.
    fn terminated_error(&self) -> Option<TrellisClientError> {
        match self.terminal.borrow().as_ref() {
            Some(cause) => Some(cause.client_error()),
            None if self.closed.load(Ordering::Acquire) => {
                Some(LogicalTerminalCause::Closed.client_error())
            }
            None => None,
        }
    }

    /// Whether the logical connection is terminated for good.
    fn is_terminated(&self) -> bool {
        self.terminated_error().is_some()
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
                let still_current = self.is_current(id);
                self.mark_failed(id);
                // Suspend shared live observation only when the *current*
                // generation is lost with no healthier successor. A superseded
                // generation's loss (for example the reclaimed baseline after a
                // real growth) must not suspend live observation that a newer
                // current generation is already serving.
                if baseline_lost && still_current {
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

    /// Re-point shared live observation at a newly published current generation.
    /// Live controls and availability then follow the current attachment instead
    /// of a superseded one; the logical registry and sessions are preserved.
    fn rebind_live(&self, generation: &Arc<TransportGeneration>) {
        if let Ok(slot) = self.live_slot.lock() {
            if let Some(live) = slot.as_ref().and_then(Weak::upgrade) {
                live.rebind(generation.nats.clone());
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
            close_generation(&generation, GenerationIntakeRetireReason::PhysicalFailure);
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
        let (terminal, _) = watch::channel(None);
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
            terminal,
        });
        let snapshot = inner.contexts.own_transport_snapshot()?;
        let id = inner.alloc_id();
        inner.baseline_id.store(id, Ordering::Release);
        let generation = open_generation(&inner, id, &snapshot, "initial").await?;
        let promoted = generation.promote_to_current();
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
                || !promoted
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
        let baseline = generation.lease().ok_or_else(|| {
            TrellisClientError::TransportUnavailable(
                "initial transport generation was closed before its baseline lease".into(),
            )
        })?;
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
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
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
        if self.inner.is_terminated()
            || generation.state() == GenerationState::Closed
            || !still_current
        {
            // The generation changed under the install: hand the handle to the
            // generation so its accepted work drains instead of being dropped.
            generation.push_intake(handle);
            generation.retire_intake(GenerationIntakeRetireReason::Superseded);
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

    /// The latched authoritative terminal cause, if the logical connection can
    /// no longer recover. A generation revocation is not a terminal cause.
    pub(crate) fn terminal(&self) -> Option<LogicalTerminalCause> {
        self.inner.terminal.borrow().clone()
    }

    /// Wait one bounded pacing step, returning early with the terminal cause if
    /// the logical connection latches one first. Used to pace transient recovery
    /// without an arbitrary per-loop terminal window.
    pub(crate) async fn pace_or_terminal(&self, pace: Duration) -> Option<LogicalTerminalCause> {
        if let Some(cause) = self.terminal() {
            return Some(cause);
        }
        let mut receiver = self.inner.terminal.subscribe();
        tokio::select! {
            _ = tokio::time::sleep(pace) => None,
            _ = receiver.changed() => self.terminal(),
        }
    }

    /// Latch an authoritative terminal cause. The first cause wins: a later
    /// stale result never overwrites it.
    pub(crate) fn publish_terminal(&self, cause: LogicalTerminalCause) {
        // First wins atomically: `send_if_modified` runs the predicate under the
        // watch's own lock, so concurrent writers (an explicit close and an
        // authoritative auth failure) can never overwrite each other's cause.
        let published = self.inner.terminal.send_if_modified(|value| {
            if value.is_some() {
                false
            } else {
                *value = Some(cause.clone());
                true
            }
        });
        if published {
            self.inner.request_work();
            self.inner.signal_state();
        }
    }

    /// Wait until the logical connection latches a terminal cause.
    pub(crate) async fn wait_terminal(&self) -> LogicalTerminalCause {
        let mut receiver = self.inner.terminal.subscribe();
        loop {
            if let Some(cause) = receiver.borrow_and_update().clone() {
                return cause;
            }
            if receiver.changed().await.is_err() {
                return LogicalTerminalCause::Closed;
            }
        }
    }

    /// Unregister a framework intake owner so no future candidate adopts it.
    ///
    /// The owner's currently installed intake is retired and drained by the
    /// owner itself; this only removes the registration so a stopped host can
    /// never be resurrected by a later generation's adoption barrier.
    pub(crate) fn detach_intake(&self, owner: &Arc<dyn GenerationIntake>) {
        if let Ok(mut owners) = self.inner.intake_owners.lock() {
            owners.retain(|existing| !Arc::ptr_eq(existing, owner));
        }
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
            if let Some(error) = self.inner.terminated_error() {
                return Err(error);
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
            if let Some(error) = self.inner.terminated_error() {
                return Err(error);
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
    ///
    /// Ownership: the only caller is [`TrellisClient`]'s `Drop`, so an explicit
    /// `Closed` cause is reachable only from dropping the client. `Drop` takes
    /// `&mut self`, and every public refresh (`refresh_authorization_context`) is
    /// `&self`, so Rust's borrow rules make a concurrent drop during a public
    /// promotion impossible; no extra synchronization is needed for that pairing.
    /// An authoritative auth terminal is published separately under the
    /// own-transition guard.
    pub(crate) fn close(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.publish_terminal(LogicalTerminalCause::Closed);
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
                if let Some(lease) = generation.lease() {
                    return Ok(Some(lease));
                }
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
                Ok(current.lease())
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
    if let Some(error) = inner.terminated_error() {
        return Err(error);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(TrellisClientError::Timeout);
    }
    request_adoption_step(&inner.work, rx, remaining).await;
    if let Some(error) = inner.terminated_error() {
        return Err(error);
    }
    Ok(())
}

/// One long-lived adoption worker for a logical connection.
async fn run_worker(inner: Arc<ManagerInner>, mut work_rx: watch::Receiver<u64>) {
    loop {
        if inner.is_terminated() {
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
        if inner.is_terminated() {
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
        if inner.is_terminated() {
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
            .filter(|generation| {
                generation.state() != GenerationState::Closed && !generation.is_fenced()
            })
            .find(|generation| generation_serves(generation, &desired.policy, now).unwrap_or(false))
            .cloned();
        let mut needs_adoption = serving.is_none();
        let mut chosen = serving.or_else(|| {
            keep.iter()
                .filter(|generation| {
                    generation.state() != GenerationState::Closed && !generation.is_fenced()
                })
                .max_by_key(|generation| generation.id)
                .cloned()
        });
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
                Err(failure) => {
                    // A partial re-adoption must not drop earlier owners' accepted
                    // work or a Live control: the survivor takes the installed
                    // handles and drains them.
                    for handle in failure.installed {
                        candidate.push_intake(handle);
                    }
                    candidate.retire_intake(GenerationIntakeRetireReason::Superseded);
                    tracing::warn!(
                        error = %failure.error,
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
                for handle in promoted_intake.drain(..) {
                    candidate.push_intake(handle);
                }
                candidate.retire_intake(GenerationIntakeRetireReason::Superseded);
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
                if let Some(chosen) = &chosen {
                    for handle in promoted_intake.drain(..) {
                        chosen.push_intake(handle);
                    }
                    chosen.retire_intake(GenerationIntakeRetireReason::Shutdown);
                }
                return Ok(());
            }
            let survivor_closed = chosen
                .as_ref()
                .is_some_and(|candidate| candidate.state() == GenerationState::Closed);
            // Conditional promote under the lease lock: a concurrent disposal fence
            // (or close) makes promotion fail, so the survivor is never published
            // as an unusable current. Its prepared intake is force-owned and
            // drained, and a fresh generation is opened instead.
            let promoted = chosen
                .as_ref()
                .is_some_and(|candidate| !survivor_closed && candidate.promote_to_current());
            if !promoted && chosen.is_some() {
                if let Some(candidate) = &chosen {
                    for handle in promoted_intake.drain(..) {
                        candidate.push_intake(handle);
                    }
                    candidate.retire_intake_draining(GenerationIntakeRetireReason::Superseded);
                }
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
            if let Some(chosen) = &chosen {
                inner.rebind_live(chosen);
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
                // Every demotion stops generic intake exactly once (idempotent)
                // while marking the generation Draining, so a demoted current can
                // never keep its retired-scope intake active. Accepted work is
                // preserved by the graceful retire. This is the same rule the
                // candidate publication applies to its superseded predecessor.
                generation.retire_intake_draining(GenerationIntakeRetireReason::Superseded);
            }
        }
        for generation in &close {
            close_generation(generation, GenerationIntakeRetireReason::AuthorityReduced);
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
        // state lock to preserve the contexts-before-state lock order. This branch
        // is **pre-barrier**: `adopt_owners` has not run, so the candidate owns no
        // generic intake and no accepted work, and closing it strands nothing. A
        // candidate that becomes stale *after* the barrier owns its intake and
        // follows the safe-drain rule below instead.
        let latest = match inner.contexts.own_transport_snapshot() {
            Ok(snapshot) => snapshot.policy.digest()?,
            Err(_) => {
                inner.clear_opening(id);
                close_generation(&candidate, GenerationIntakeRetireReason::Superseded);
                return Ok(());
            }
        };
        if latest != desired_policy_digest {
            inner.clear_opening(id);
            close_generation(&candidate, GenerationIntakeRetireReason::Superseded);
            continue;
        }

        // Preactivation readiness barrier: every framework owner installs
        // broker-ready generic intake on the candidate **before** the candidate
        // becomes the default generation, with each adoption bounded by the
        // connection budget. A failed or timed-out adoption rolls the candidate
        // back and leaves the current generation untouched.
        let intake = match inner.adopt_owners(&candidate).await {
            Ok(intake) => intake,
            Err(failure) => {
                inner.clear_opening(id);
                // Partial installation: earlier owners already hold accepted work.
                // Take their handles, retire them, and keep the candidate draining
                // so the work survives instead of being dropped.
                for handle in failure.installed {
                    candidate.push_intake(handle);
                }
                inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
                tracing::warn!(
                    event = "transport_generation.open_failed",
                    generation_id = id,
                    error = %failure.error,
                    "candidate intake barrier failed; draining"
                );
                return Ok(());
            }
        };
        // Ownership of the installed intake is taken before any further await, so
        // a candidate that turns out stale below drains its accepted work rather
        // than dropping it (and never duplicates a logical owner).
        for handle in intake {
            candidate.push_intake(handle);
        }
        // A candidate that became stale while its intake was prepared must not be
        // published; the next reconcile opens a fresh one from the newest policy.
        let latest_after_barrier = match inner.contexts.own_transport_snapshot() {
            Ok(snapshot) => snapshot.policy.digest()?,
            Err(_) => {
                inner.clear_opening(id);
                inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
                return Ok(());
            }
        };
        if latest_after_barrier != desired_policy_digest {
            inner.clear_opening(id);
            if candidate_is_safe(inner, &candidate) {
                inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
            } else {
                close_generation(&candidate, GenerationIntakeRetireReason::AuthorityReduced);
            }
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
                inner.track_draining(&candidate, GenerationIntakeRetireReason::PhysicalFailure);
                return Ok(());
            }
            let previous = state.current.take();
            if let Some(previous) = previous {
                if previous.id != candidate.id {
                    // Retire the superseded generation's generic intake
                    // atomically with marking it Draining, so a concurrent install
                    // either lands before and is drained, or observes Closed.
                    previous.retire_intake_draining(GenerationIntakeRetireReason::Superseded);
                    state.draining.push(previous);
                }
            }
            // Conditional promote under the lease lock: a concurrent disposal fence
            // or close makes promotion fail, so the candidate is never published as
            // an unusable current. The candidate is drained, the previous stays as a
            // draining survivor, and the next reconcile restores or opens fresh.
            if !candidate.promote_to_current() {
                drop(state);
                inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
                return Ok(());
            }
            state.current = Some(candidate.clone());
            inner.rebind_live(&candidate);
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
            // A draining generation closes only after its physical leases release
            // AND its retired intake has fully stopped and disposed, so a queued
            // delivery still being handled is never cut off by the close.
            if generation.state() == GenerationState::Draining
                && generation.leases() == 0
                && generation.intake_idle()
            {
                // Fence lease admission at the close decision, under the same
                // manager-state lock that `acquire_for` uses to select and lease,
                // so a fresh lease can never start after this zero observation.
                generation.close_leases();
                to_close.push(generation);
            } else {
                still.push(generation);
            }
        }
        state.draining = still;
    }
    for generation in &to_close {
        close_generation(generation, GenerationIntakeRetireReason::Superseded);
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
    // A logical shutdown retires every physical generation's framework intake
    // before closing the attachment, the same discipline as reduction/reap. An
    // authoritative terminal cause is distinguished from an explicit close.
    let reason = match inner.terminal.borrow().as_ref() {
        Some(LogicalTerminalCause::Authorization(_)) => {
            GenerationIntakeRetireReason::LogicalTerminal
        }
        _ => GenerationIntakeRetireReason::Shutdown,
    };
    for generation in known {
        generation.retire_intake_closed(reason);
        let _ = generation.nats.drain().await;
    }
}

/// Mark a generation closed and close its physical connection.
///
/// `drain` unsubscribes, flushes, and then makes the connection handler exit,
/// which closes the socket; it is the SDK-side cooperative close. The broker
/// remains authoritative for reductions and kicks any uncooperative socket.
fn close_generation(generation: &Arc<TransportGeneration>, reason: GenerationIntakeRetireReason) {
    // Atomically mark the generation Closed and extract its framework intake under
    // the intake lock, so a concurrent install cannot register a handle after the
    // close. `retire_intake_closed` signals retire and hands the handles to an
    // asynchronous disposal sweep without awaiting held handlers (the force path).
    generation.retire_intake_closed(reason);
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

/// Whether a candidate's admitted policy is still safe under the newest
/// authorization. An unreadable snapshot is treated as unsafe, so an
/// unclassifiable candidate is force-closed rather than kept.
fn candidate_is_safe(inner: &ManagerInner, candidate: &Arc<TransportGeneration>) -> bool {
    let Ok(snapshot) = inner.contexts.own_transport_snapshot() else {
        return false;
    };
    let now = inner.contexts.corrected_now_seconds().unwrap_or(0);
    generation_is_safe(candidate, &snapshot.policy, now).unwrap_or(false)
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
    let (lease_wake, _) = watch::channel(0u64);
    let (disposal_force, _) = watch::channel(false);
    let generation = Arc::new(TransportGeneration {
        id,
        nats,
        context_digest: snapshot.context_digest.clone(),
        admitted_policy: snapshot.policy.clone(),
        admitted_policy_digest,
        physical_connection_id: admission.authenticated_user,
        state,
        lease_state: Arc::new(std::sync::Mutex::new(LeaseState::default())),
        lease_wake,
        disposal_force,
        work: inner.work.clone(),
        intake: Mutex::new(Vec::new()),
        intake_active: AtomicBool::new(true),
        intake_installed: AtomicUsize::new(0),
        intake_disposing: Arc::new(AtomicUsize::new(0)),
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

    /// A lease wake carries no predicate: a stale or out-of-order notification
    /// must never let a waiter treat a non-zero count as idle, a force latch must
    /// override an unresolved wait, and observing zero must set the irreversible
    /// disposal-start fence atomically. This is the defect the previous
    /// `watch<usize>` payload could not express.
    #[tokio::test]
    async fn lease_idle_wait_uses_the_authoritative_predicate() {
        let draining = Arc::new(AtomicU8::new(GenerationState::Draining as u8));
        let (wake, _) = watch::channel(0u64);

        // A stale wake with a non-zero count must not resolve the wait.
        let (force_tx, _) = watch::channel(false);
        let state = Arc::new(std::sync::Mutex::new(LeaseState {
            live: 1,
            disposing: false,
            forced: false,
            closed: false,
        }));
        let waiting = {
            let state = Arc::clone(&state);
            let draining = Arc::clone(&draining);
            let wake = wake.clone();
            let mut force = force_tx.subscribe();
            tokio::spawn(async move { wait_lease_idle(&state, &draining, &wake, &mut force).await })
        };
        wake.send_replace(1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !waiting.is_finished(),
            "a wake must not treat a non-zero count as idle"
        );

        // A force latch overrides an unresolved wait on a non-zero count.
        force_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("a force latch must override an unresolved wait")
            .unwrap();

        // A fresh scope: dropping to zero under the lock and waking resolves it
        // and sets the irreversible disposal-start fence.
        let (force_tx, _) = watch::channel(false);
        let state = Arc::new(std::sync::Mutex::new(LeaseState {
            live: 1,
            disposing: false,
            forced: false,
            closed: false,
        }));
        let waiting = {
            let state = Arc::clone(&state);
            let draining = Arc::clone(&draining);
            let wake = wake.clone();
            let mut force = force_tx.subscribe();
            tokio::spawn(async move { wait_lease_idle(&state, &draining, &wake, &mut force).await })
        };
        state.lock().unwrap().live = 0;
        wake.send_replace(2);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("a waiter must resolve once the authoritative count is zero")
            .unwrap();
        assert!(
            state.lock().unwrap().disposing,
            "observing zero must set the disposal-start fence"
        );
    }

    /// Disposal-start requires a Draining generation with zero live leases: a
    /// shared fence ends a later sweep immediately (no stale-counter wait), and a
    /// Current+zero generation is **not** fenced (the retire-scope cleanup stays
    /// pending, without spinning) until it is Draining with zero again.
    #[tokio::test]
    async fn disposal_start_requires_draining_and_zero() {
        let (wake, _) = watch::channel(0u64);
        let (force_tx, _) = watch::channel(false);

        // A later sweep sharing an already-fenced generation returns immediately
        // even though the counter is non-zero (a stale counter must not hang it).
        let draining = Arc::new(AtomicU8::new(GenerationState::Draining as u8));
        let fenced = Arc::new(std::sync::Mutex::new(LeaseState {
            live: 5,
            disposing: true,
            forced: false,
            closed: false,
        }));
        let mut force = force_tx.subscribe();
        tokio::time::timeout(
            Duration::from_millis(200),
            wait_lease_idle(&fenced, &draining, &wake, &mut force),
        )
        .await
        .expect("a later sweep must not wait behind the shared fence");

        // A Current generation with zero leases must NOT fence and must not spin:
        // the retire scope stays pending.
        let state = Arc::new(AtomicU8::new(GenerationState::Current as u8));
        let restored = Arc::new(std::sync::Mutex::new(LeaseState {
            live: 0,
            disposing: false,
            forced: false,
            closed: false,
        }));
        let waiting = {
            let restored = Arc::clone(&restored);
            let state = Arc::clone(&state);
            let wake = wake.clone();
            let mut force = force_tx.subscribe();
            tokio::spawn(async move { wait_lease_idle(&restored, &state, &wake, &mut force).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "a Current generation must not be fenced"
        );
        assert!(
            !restored.lock().unwrap().disposing,
            "Current+zero must not set the disposal fence"
        );

        // A state transition to Draining (broadcast) lets the same wait fence.
        state.store(GenerationState::Draining as u8, Ordering::Release);
        wake.send_replace(1);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("Draining+zero must permit disposal")
            .unwrap();
        assert!(
            restored.lock().unwrap().disposing,
            "Draining+zero must set the disposal fence"
        );
    }
}
