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
    policy_covers, read_own_admission, AuthorizationContextCache, AuthorizationRuntimeBinding,
    OwnTransitionGuard, OwnTransportSnapshot, PreparedOwnCandidate,
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
    /// Monotonic publication fence: parked private stages are not app carriers.
    once_published: AtomicBool,
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
            || self.nats.connection_state() != async_nats::connection::State::Connected
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
    /// Wait for irreversible loss of this exact attachment, not logical recovery.
    pub(crate) async fn wait_lost(&self) {
        let mut changes = self.generation.lease_wake.subscribe();
        loop {
            if self.generation.state() == GenerationState::Closed {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }

    /// The pinned physical NATS connection.
    pub(crate) fn nats(&self) -> &async_nats::Client {
        &self.generation.nats
    }

    /// The immutable identity of the generation this lease pins.
    pub(crate) fn generation_id(&self) -> u64 {
        self.generation.id
    }

    /// The exact generation's lease source without retaining a lifetime pin.
    pub(crate) fn weak_generation(&self) -> Weak<TransportGeneration> {
        Arc::downgrade(&self.generation)
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

/// One prepared own-candidate coverage acquisition.
///
/// The lease pins a generation that can carry the candidate's initial registry
/// coverage. When no already-admitted safe carrier existed, `stage` is the scoped
/// private stage owning the provisional generation opened for it.
pub(crate) struct OwnCoveragePreparation {
    /// A lease on the generation providing the candidate's coverage.
    pub(crate) lease: TransportLease,
    /// The exact owned candidate digest this preparation covers.
    pub(crate) context_digest: String,
    /// Exact prepared instance identity the warm must validate at promotion, so a
    /// signed-identical replacement is never substituted.
    pub(crate) instance: Arc<()>,
    /// The candidate's runtime binding, read from the exact guarded snapshot this
    /// preparation validated, so a promotion applies that candidate's runtime
    /// rather than an unguarded candidate-first cache read.
    pub(crate) runtime: AuthorizationRuntimeBinding,
    /// Immutable server-clock offset from the exact prepared instance, threaded
    /// through the warm so signature/window verification uses the candidate's own
    /// corrected clock rather than the promoted predecessor's.
    pub(crate) clock_offset_ms: i64,
    /// The scoped private stage, present only when a fresh provisional generation
    /// was opened because no already-admitted safe carrier was available.
    pub(crate) stage: Option<OwnCoverageStage>,
}

/// A scoped private stage: one provisional generation opened to carry an exact
/// own candidate's initial coverage while the candidate is not yet publishable.
///
/// Opened only when no already-admitted safe carrier exists. The stage holds the
/// manager's reconcile ownership for its whole life, so the adoption worker
/// cannot classify, publish, or close the provisional generation; it is kept out
/// of the current and draining sets and out of every ordinary acquire path. The
/// caller finishes the stage once the candidate is ready for promotion, or drops
/// it to abandon only this stage's generation and opening.
pub(crate) struct OwnCoverageStage {
    inner: Arc<ManagerInner>,
    generation: Arc<TransportGeneration>,
    context_digest: String,
    instance: Arc<()>,
    /// Retained reconcile ownership: dropped only on finish or stage drop.
    reconcile: Option<tokio::sync::OwnedMutexGuard<()>>,
    finished: bool,
}

#[allow(dead_code)]
impl OwnCoverageStage {
    /// The provisional generation this stage opened.
    pub(crate) fn generation_id(&self) -> u64 {
        self.generation.id
    }

    /// Finish the stage under the caller's own-installation transition guard.
    ///
    /// `transition` is the caller's own-installation guard; taking it here again
    /// would deadlock on the non-reentrant transition lock, so it is a checked
    /// parameter rather than a documented precondition. The manager's state lock
    /// is taken here and held across `commit`, preserving the documented
    /// own-transition -> manager-state -> own/provider-state lock order. `commit`
    /// must be synchronous, must not await, and must not re-acquire the
    /// own-installation transition or re-enter the manager; it performs the
    /// own-context promotion and the pending-to-installed lease swap using the
    /// transition the caller already holds (guard-taking `*_locked` forms) under
    /// the same manager state.
    ///
    /// Before running `commit`, the exact prepared candidate instance, the fresh
    /// candidate window and hard authority deadline, the manager's nonterminal
    /// state, this stage's exact managed opening (matching id, no recorded
    /// failure, and pointer-identical generation), and the provisional
    /// generation's physical readiness are rechecked. Only a successful `commit`
    /// parks the stage as a tracked, intake-inactive draining survivor and clears
    /// its matching opening, without publishing or activating it. The reconcile
    /// ownership is then released so ordinary adoption (signalled by
    /// `authorization_promoted`) can install intake and promote it.
    ///
    /// Any failure or a dropped stage closes only this stage's generation, clears
    /// only its matching opening, and releases the reconcile ownership; a healthy
    /// prior current or draining generation and the logical connection are never
    /// touched.
    ///
    /// # Errors
    ///
    /// Returns the candidate, terminal, opening, readiness, or `commit` failure;
    /// the stage is abandoned and only its own generation is closed.
    pub(crate) fn finish(
        mut self,
        transition: &OwnTransitionGuard<'_>,
        commit: impl FnOnce() -> Result<(), TrellisClientError>,
    ) -> Result<(), TrellisClientError> {
        // Manager state is held inside for the whole commit, in the documented
        // own-transition -> manager-state -> own/provider-state order.
        let mut state = self.inner.lock_state()?;
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        // The caller holds the own-installation transition; revalidate the exact
        // prepared candidate instance and its fresh window under it.
        let candidate = self
            .inner
            .contexts
            .own_candidate_transport_snapshot_locked(
                transition,
                &self.context_digest,
                &self.instance,
            )?;
        let now = candidate.corrected_now_seconds;
        if now < candidate.not_before || now >= candidate.expires_at {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "authorization candidate is no longer current".into(),
            ));
        }
        // The opening must be this stage's exact managed generation: matching id,
        // no recorded failure (`mark_failed` can record a pre-connection loss
        // under the state lock and close outside it, so an id-only check would
        // miss it), and pointer-identical generation so a replacement opening is
        // never treated as this stage's.
        let opening_matches = state.opening.as_ref().is_some_and(|opening| {
            opening.id == self.generation.id
                && !opening.failed.load(Ordering::Acquire)
                && opening
                    .generation
                    .as_ref()
                    .is_some_and(|generation| Arc::ptr_eq(generation, &self.generation))
        });
        if !opening_matches {
            return Err(TrellisClientError::TransportUnavailable(
                "private stage is no longer the managed opening".into(),
            ));
        }
        if self.generation.state() == GenerationState::Closed || self.generation.is_fenced() {
            return Err(TrellisClientError::TransportUnavailable(
                "private stage generation is no longer physically ready".into(),
            ));
        }
        // Hard admitted-authority deadline against the fresh corrected clock.
        if !generation_is_safe(&self.generation, &candidate.policy, now)? {
            return Err(TrellisClientError::TransportUnavailable(
                "private stage generation is not safe under the fresh candidate".into(),
            ));
        }
        commit()?;
        // Park as a tracked, intake-inactive draining survivor without publishing
        // or activating it: the adoption worker installs intake and promotes it,
        // or closes it if authority no longer covers it.
        self.generation
            .retire_intake(GenerationIntakeRetireReason::Superseded);
        state.opening = None;
        if !state
            .draining
            .iter()
            .any(|generation| generation.id == self.generation.id)
        {
            state.draining.push(self.generation.clone());
        }
        drop(state);
        // Release reconcile ownership so ordinary adoption can take the stage.
        self.reconcile = None;
        self.finished = true;
        self.inner.request_work();
        Ok(())
    }
}

impl Drop for OwnCoverageStage {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // Close only this provisional generation: its opening is cleared so it is
        // never published, and its physical attachment is drained.
        close_generation(&self.generation, GenerationIntakeRetireReason::Superseded);
        self.inner.clear_opening(self.generation.id);
    }
}

impl std::fmt::Debug for OwnCoverageStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnCoverageStage")
            .field("generation_id", &self.generation.id)
            .field("context_digest", &self.context_digest)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for OwnCoveragePreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnCoveragePreparation")
            .field("generation_id", &self.lease.generation_id())
            .field("context_digest", &self.context_digest)
            .field("staged", &self.stage.is_some())
            .finish_non_exhaustive()
    }
}

/// Owns the cleanup of a private stage's connect-and-open scope until the stage
/// takes ownership of the built generation.
///
/// If the connect fails or is cancelled, or the generation is built but a later
/// step before stage ownership fails, this closes exactly the generation and
/// opening it created and clears only a matching opening; a newer opening is
/// never touched.
#[allow(dead_code)]
struct StageConnectGuard {
    inner: Arc<ManagerInner>,
    id: u64,
    generation: Option<Arc<TransportGeneration>>,
    armed: bool,
}

#[allow(dead_code)]
impl StageConnectGuard {
    fn new(inner: Arc<ManagerInner>, id: u64) -> Self {
        Self {
            inner,
            id,
            generation: None,
            armed: true,
        }
    }

    /// Record the exact generation this opening built so a pre-ownership failure
    /// closes it instead of stranding its socket.
    fn own_generation(&mut self, generation: Arc<TransportGeneration>) {
        self.generation = Some(generation);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StageConnectGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(generation) = self.generation.take() {
            close_generation(&generation, GenerationIntakeRetireReason::PhysicalFailure);
        }
        self.inner.clear_opening(self.id);
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
    /// Retained *publication identity*: the id of the published current
    /// generation, `None` when there is no published current. It changes only
    /// when `state.current`'s identity actually changes — never for a same-current
    /// renewal, an unrelated draining generation's failure, or lease churn — so
    /// coverage/registry maintenance can follow the exact current attachment
    /// without waking on generic manager state.
    published: watch::Sender<Option<u64>>,
    /// Work channel for the single adoption worker.
    work: watch::Sender<u64>,
    /// Framework owners whose broker-ready intake must exist on a candidate
    /// before it becomes the default generation (preactivation barrier).
    intake_owners: Mutex<Vec<Arc<dyn GenerationIntake>>>,
    /// Serializes every open/classify/publish decision for this logical
    /// connection. A private own-candidate stage retains an owned guard here for
    /// its whole life, so its provisional generation stays out of the current
    /// and draining sets and out of every ordinary acquire path while unpromoted.
    reconcile: Arc<tokio::sync::Mutex<()>>,
    next_id: AtomicU64,
    /// The initial framework-loop generation whose loss is the only generation
    /// loss allowed to suspend the shared live-observation manager.
    baseline_id: AtomicU64,
    /// The one latched authoritative terminal cause for the logical connection.
    /// `None` while the connection may still recover; once set it is never
    /// overwritten. A generation revocation is not a terminal cause.
    terminal: watch::Sender<Option<LogicalTerminalCause>>,
    stopped: watch::Sender<bool>,
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

    /// Record the published current generation identity.
    ///
    /// Must be called with the state guard held at every site that mutates
    /// `state.current`, so the published identity always matches the state. A
    /// publication is emitted only when the identity actually changes; a
    /// same-current renewal or an unrelated draining generation's change never
    /// emits one.
    fn set_current(&self, state: &mut ManagerState, next: Option<&Arc<TransportGeneration>>) {
        if let Some(generation) = next {
            generation.once_published.store(true, Ordering::Release);
        }
        let next_id = next.map(|generation| generation.id);
        state.current = next.cloned();
        let previous = *self.published.borrow();
        if previous != next_id {
            self.published.send_replace(next_id);
        }
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
                    });
                }
                Err(_) => {
                    return Err(AdoptFailure {
                        error: TrellisClientError::TransportUnavailable(
                            "framework intake adoption timed out".into(),
                        ),
                        installed: handles,
                    });
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
    /// of a superseded one; the logical registry and sessions are preserved. The
    /// generation's immutable identity travels with its socket so owner controls
    /// are deduplicated per receiving generation, never per connect counter.
    fn rebind_live(&self, generation: &Arc<TransportGeneration>) {
        if let Ok(slot) = self.live_slot.lock() {
            if let Some(live) = slot.as_ref().and_then(Weak::upgrade) {
                live.rebind(
                    generation.nats.clone(),
                    generation.id,
                    Arc::downgrade(generation),
                );
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
        let mut lost_current = false;
        if let Ok(state) = self.lock_state() {
            if state
                .current
                .as_ref()
                .is_some_and(|generation| generation.id == id)
            {
                built = state.current.clone();
                lost_current = true;
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
        if lost_current && !self.is_terminated() {
            // Losing the published socket can also suspend its own revocation
            // coverage. Coverage-only retries cannot reopen that closed carrier;
            // the existing refresh transaction can warm a private replacement
            // before restoring application authority and publishing intake.
            self.contexts.request_refresh();
        }
        self.request_work();
        self.signal_state();
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
    ) -> Result<(Self, TransportLease), TrellisClientError> {
        let (state_version, _) = watch::channel(0u64);
        let (published, _) = watch::channel(None);
        let (work, work_rx) = watch::channel(0u64);
        let (terminal, _) = watch::channel(None);
        let (stopped, _) = watch::channel(false);
        let inner = Arc::new(ManagerInner {
            auth,
            contexts,
            timeout_ms,
            live_slot,
            closed: AtomicBool::new(false),
            state: Mutex::new(ManagerState::default()),
            state_version,
            published,
            work,
            intake_owners: Mutex::new(Vec::new()),
            reconcile: Arc::new(tokio::sync::Mutex::new(())),
            next_id: AtomicU64::new(1),
            baseline_id: AtomicU64::new(0),
            terminal,
            stopped,
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
                inner.set_current(&mut state, Some(&generation));
            }
            lost
        };
        if lost {
            let _ = generation.nats.drain().await;
            return Err(TrellisClientError::TransportUnavailable(
                "initial transport generation was lost during admission".into(),
            ));
        }
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
                ));
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

    /// Subscribe to the retained publication identity.
    ///
    /// The receiver carries the id of the published current generation (`None`
    /// when there is none) and changes only when that identity actually changes —
    /// never for a same-current renewal, an unrelated draining generation's
    /// failure, or internal lease churn. It is lease-free and never wakes on a
    /// caller's own work request.
    pub(crate) fn subscribe_publication(&self) -> tokio::sync::watch::Receiver<Option<u64>> {
        self.inner.published.subscribe()
    }

    /// Observe own coverage so fresh reservation waiters resume without a socket change.
    pub(crate) fn watch_own_availability(
        &self,
    ) -> watch::Receiver<crate::generated::AvailabilitySnapshot> {
        self.inner.contexts.watch_availability()
    }

    /// Acquire a finite internal lease on the **exact published** current attachment.
    ///
    /// This is coverage/registry maintenance on an already-admitted socket: it
    /// validates the manager lifecycle (terminal/closed fail closed), that the
    /// published identity still matches the state's current generation, that the
    /// generation is physically Current (never a draining or closed substitute)
    /// and that it is safe under the latest verified promoted application policy
    /// and hard authority deadline. It deliberately does **not** validate a
    /// CONNECT routing credential or require a fresh bootstrap snapshot, and it
    /// never waits for adoption. Fresh application work instead uses
    /// [`Self::acquire_for`] or [`Self::acquire_application_published`].
    ///
    /// The own-installation transition is acquired first and held across the whole
    /// call, so a concurrent authorization promotion (which takes the same guard)
    /// cannot replace the promoted policy or the corrected clock between the
    /// safety comparison and the lease. Under it the promoted policy and corrected
    /// clock are read under the manager's state guard, so the comparison and the
    /// lease use one coherent snapshot of the latest authority and the exact
    /// current generation rather than a policy or clock that predates a
    /// concurrent promotion.
    pub(crate) fn acquire_published(&self) -> Result<TransportLease, TrellisClientError> {
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        let transition = self.inner.contexts.lock_own_transition()?;
        let state = self.inner.lock_state()?;
        // The terminal/closed latch can move between the entry check and this
        // guard; re-read it so a lease is never granted on a finished connection.
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        // Read the promoted policy and corrected clock only under the state guard,
        // so the safety comparison and the lease use one coherent snapshot rather
        // than one that predates a concurrent promotion. A clock-source failure is
        // propagated: defaulting to the Unix epoch would let a generation past its
        // hard authority deadline pass as safe.
        let (policy, now) = self
            .inner
            .contexts
            .maintenance_transport_locked(&transition)?;
        self.lease_published_locked(&state, &policy, now)
    }

    /// Acquire fresh application work on the exact published attachment only
    /// while the installed signed authority and own coverage are usable.
    pub(crate) fn acquire_application_published(
        &self,
    ) -> Result<TransportLease, TrellisClientError> {
        let transition = self.inner.contexts.lock_own_transition()?;
        let state = self.inner.lock_state()?;
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        let (policy, now) = self
            .inner
            .contexts
            .application_transport_locked(&transition)?
            .ok_or_else(|| {
                TrellisClientError::AuthorizationUnavailable(
                    "authorization installation is suspended".into(),
                )
            })?;
        self.lease_published_locked(&state, &policy, now)
    }

    fn lease_published_locked(
        &self,
        state: &ManagerState,
        policy: &TransportAuthorizationV1,
        now: i64,
    ) -> Result<TransportLease, TrellisClientError> {
        let published = *self.inner.published.borrow();
        let Some(current) = state.current.as_ref() else {
            return Err(TrellisClientError::TransportUnavailable(
                "no published current transport generation".into(),
            ));
        };
        if Some(current.id) != published {
            return Err(TrellisClientError::TransportUnavailable(
                "published transport generation identity is inconsistent".into(),
            ));
        }
        if current.state() != GenerationState::Current {
            return Err(TrellisClientError::TransportUnavailable(
                "published transport generation is not the exact current attachment".into(),
            ));
        }
        if !generation_is_safe(current, policy, now)? {
            return Err(TrellisClientError::TransportUnavailable(
                "published transport generation is not safe under current authority".into(),
            ));
        }
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        current.lease().ok_or_else(|| {
            TrellisClientError::TransportUnavailable(
                "published transport generation refused a lease".into(),
            )
        })
    }

    /// Prepare coverage for one freshly prepared own authorization candidate.
    ///
    /// Every snapshot and retry is validated against `candidate`'s exact prepared
    /// instance, not the signed digest alone, so a concurrently prepared
    /// signed-identical replacement with fresh route/clock companion material is
    /// never adopted. Prefers an already-admitted safe carrier of the same logical
    /// connection and opens no socket when one exists. When none exists it opens
    /// one scoped private stage under the manager's reconcile ownership and returns
    /// a lease on that provisional generation plus the stage. The stage retains the
    /// reconcile lock until [`OwnCoverageStage::finish`] or drop, so the provisional
    /// generation stays out of the current and draining sets and out of every
    /// ordinary acquire path while it is unpromoted.
    ///
    /// # Errors
    ///
    /// Fails closed for a missing, changed, revoked, or expired candidate; a
    /// terminal or closed logical connection; or a private stage that cannot be
    /// admitted. The caller's bounded refresh loop retries.
    pub(crate) async fn prepare_own_coverage(
        &self,
        candidate: &PreparedOwnCandidate,
    ) -> Result<OwnCoveragePreparation, TrellisClientError> {
        // Prefer an already-admitted carrier: no socket churn.
        if let Some(preparation) = self.try_prepare_admitted_carrier(candidate)? {
            return Ok(preparation);
        }
        // Serialize private staging with the manager's own reconciliation so no
        // candidate is opened, classified, or published concurrently with this
        // stage. No std lock is held across the wait.
        let reconcile = self.inner.reconcile.clone().lock_owned().await;
        if let Some(error) = self.inner.terminated_error() {
            drop(reconcile);
            return Err(error);
        }
        // Recheck after waiting: a safe carrier may have appeared or been repaired
        // while the reconcile lock was contended; the originating candidate may
        // also have been replaced, which the exact-instance check rejects.
        if let Some(preparation) = self.try_prepare_admitted_carrier(candidate)? {
            return Ok(preparation);
        }
        // Capture the immutable CONNECT material from the private verified
        // candidate, revalidated against the exact instance this operation
        // prepared. The own-transition lock is released before the connect await.
        let (candidate_snapshot, instance, clock_offset_ms, runtime) = {
            let transition = self.inner.contexts.lock_own_transition()?;
            let snapshot = self
                .inner
                .contexts
                .own_candidate_transport_snapshot_locked(
                    &transition,
                    &candidate.context_digest,
                    &candidate.instance,
                )?;
            // A captured CONNECT route credential that is already expired can never
            // authenticate a *new* socket, so fail closed before opening one. This
            // gates only the new-socket path: reuse of an already-admitted carrier
            // above never revalidates the CONNECT credential.
            if snapshot.routing_jwt_expires_at <= snapshot.corrected_now_seconds {
                return Err(TrellisClientError::AuthorizationUnavailable(
                    "authorization candidate route credential is no longer current".into(),
                ));
            }
            let runtime = snapshot.runtime.clone();
            (
                OwnTransportSnapshot {
                    context_digest: snapshot.context_digest.clone(),
                    policy: snapshot.policy.clone(),
                    runtime: runtime.clone(),
                    routing_jwt: snapshot.routing_jwt.clone(),
                },
                snapshot.instance.clone(),
                snapshot.server_clock_offset_ms,
                runtime,
            )
        };
        let id = self.inner.alloc_id();
        let mut connect_guard = StageConnectGuard::new(Arc::clone(&self.inner), id);
        let generation = match open_generation(
            &self.inner,
            id,
            &candidate_snapshot,
            "own_candidate_stage",
        )
        .await
        {
            Ok(generation) => generation,
            Err(error) => {
                // The connect guard clears the matching opening on drop.
                return Err(error);
            }
        };
        // The generation now exists in the managed opening slot: if any step before
        // the stage takes ownership fails, the guard closes that exact generation.
        connect_guard.own_generation(Arc::clone(&generation));
        let lease = generation.lease().ok_or_else(|| {
            TrellisClientError::TransportUnavailable(
                "private stage generation refused a lease".into(),
            )
        })?;
        // The provisional generation is now owned by the stage; the connect guard
        // must not close it or clear its opening.
        connect_guard.disarm();
        Ok(OwnCoveragePreparation {
            lease,
            context_digest: candidate.context_digest.clone(),
            clock_offset_ms,
            instance: instance.clone(),
            runtime,
            stage: Some(OwnCoverageStage {
                inner: Arc::clone(&self.inner),
                generation,
                context_digest: candidate.context_digest.clone(),
                instance,
                reconcile: Some(reconcile),
                finished: false,
            }),
        })
    }

    /// Try to lease an already-admitted safe carrier for one candidate policy.
    ///
    /// The exact published current is preferred when it is physically `Current`
    /// and safe; otherwise a safe current/draining survivor of the same logical
    /// connection is used. Safety is decided against `allowed` (the candidate's own
    /// signed policy) at `now` (its corrected clock), never the retained promoted
    /// policy. No socket is opened and no CONNECT routing credential is validated.
    fn try_prepare_admitted_carrier(
        &self,
        candidate: &PreparedOwnCandidate,
    ) -> Result<Option<OwnCoveragePreparation>, TrellisClientError> {
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        let transition = self.inner.contexts.lock_own_transition()?;
        let state = self.inner.lock_state()?;
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        let snapshot = self
            .inner
            .contexts
            .own_candidate_transport_snapshot_locked(
                &transition,
                &candidate.context_digest,
                &candidate.instance,
            )?;
        let lease = self.try_lease_candidate_carrier(
            &state,
            &snapshot.context_digest,
            &snapshot.policy,
            snapshot.corrected_now_seconds,
        )?;
        Ok(lease.map(|lease| OwnCoveragePreparation {
            lease,
            context_digest: snapshot.context_digest.clone(),
            instance: snapshot.instance.clone(),
            clock_offset_ms: snapshot.server_clock_offset_ms,
            runtime: snapshot.runtime.clone(),
            stage: None,
        }))
    }

    /// Lease the safest already-admitted carrier for `allowed` at `now`.
    ///
    /// `generation_is_safe` enforces each generation's hard admitted-authority
    /// deadline against the candidate's corrected clock.
    fn try_lease_candidate_carrier(
        &self,
        state: &ManagerState,
        context_digest: &str,
        allowed: &TransportAuthorizationV1,
        now: i64,
    ) -> Result<Option<TransportLease>, TrellisClientError> {
        let mut ordered: Vec<&Arc<TransportGeneration>> = Vec::new();
        if let Some(current) = state.current.as_ref() {
            ordered.push(current);
        }
        ordered.extend(state.draining.iter());
        for generation in ordered {
            if generation.state() == GenerationState::Closed || generation.is_fenced() {
                continue;
            }
            if !generation_is_safe(generation, allowed, now)? {
                continue;
            }
            if let Some(lease) = generation.lease() {
                tracing::info!(
                    context_digest,
                    generation_id = generation.id,
                    corrected_now = now,
                    "acquired own-candidate coverage on an admitted safe generation"
                );
                return Ok(Some(lease));
            }
        }
        Ok(None)
    }

    /// Run `commit` with the exact published current generation pinned.
    ///
    /// This is the guarded final commit for a make-before-break coverage
    /// replacement: `commit` runs only while `expected` is still the published
    /// current generation, physically `Current`, not terminated or closed,
    /// identity-consistent with the retained publication, and safe under the
    /// latest valid signed application authority and hard authority deadline,
    /// even while coverage is suspended.
    /// The manager's state guard is held for the whole call, so the current
    /// generation and its publication identity cannot change between the final
    /// comparison and the caller's synchronous swap. The promoted policy and
    /// corrected clock are validated under that same state guard and the caller's
    /// own-installation transition, so they cannot be replaced between the
    /// safety comparison and the swap. `commit` must not await.
    pub(crate) fn commit_published_if_current(
        &self,
        transition: &OwnTransitionGuard<'_>,
        expected: u64,
        commit: impl FnOnce() -> bool,
    ) -> Result<bool, TrellisClientError> {
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        let state = self.inner.lock_state()?;
        // Read the promoted policy and corrected clock only once the manager's
        // state guard is held, so the safety comparison and the caller's swap use
        // one coherent snapshot rather than one that predates a concurrent
        // promotion.
        let (policy, now) = self
            .inner
            .contexts
            .maintenance_transport_locked(transition)?;
        // The terminal/closed latch can move between the entry check and this
        // guard; re-read it so a commit never runs on a finished connection.
        if let Some(error) = self.inner.terminated_error() {
            return Err(error);
        }
        let published = *self.inner.published.borrow();
        let Some(current) = state.current.as_ref() else {
            return Err(TrellisClientError::TransportUnavailable(
                "no published current transport generation".into(),
            ));
        };
        if current.id != expected || published != Some(expected) {
            return Err(TrellisClientError::TransportUnavailable(
                "expected transport generation is no longer the published current".into(),
            ));
        }
        if current.state() != GenerationState::Current {
            return Err(TrellisClientError::TransportUnavailable(
                "expected transport generation is not the exact current attachment".into(),
            ));
        }
        if !generation_is_safe(current, &policy, now)? {
            return Err(TrellisClientError::TransportUnavailable(
                "expected transport generation is not safe under current authority".into(),
            ));
        }
        Ok(commit())
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
        let mut own_rx = self.inner.contexts.watch_availability();
        loop {
            if let Some(error) = self.inner.terminated_error() {
                return Err(error);
            }
            {
                let transition = self.inner.contexts.lock_own_transition()?;
                let state = self.inner.lock_state()?;
                if let Some(error) = self.inner.terminated_error() {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(TrellisClientError::Timeout);
                }
                if let Some((policy, now)) = self
                    .inner
                    .contexts
                    .application_transport_locked(&transition)?
                {
                    if let Some(lease) = self
                        .try_acquire_covering(&state, publish, subscribe, now, &policy, deadline)?
                    {
                        return Ok(lease);
                    }
                    if !policy_covers(&policy, publish, subscribe, now)? {
                        if let Some(lease) =
                            self.try_acquire_current(&state, now, &policy, deadline)?
                        {
                            return Ok(lease);
                        }
                        return Err(TrellisClientError::TransportUnavailable(
                            "no admitted transport is available".into(),
                        ));
                    }
                }
            }
            tokio::select! {
                result = await_adoption(&self.inner, &mut rx, deadline) => result?,
                result = own_rx.changed() => {
                    result.map_err(|_| TrellisClientError::AuthorizationUnavailable(
                        "authorization installation watch closed".into(),
                    ))?;
                }
            }
        }
    }

    /// Close every generation and stop automatic adoption.
    ///
    /// The final client's `Drop` cannot race a borrowed refresh. Runtime-owned
    /// clients may still have child-task references at scope end; that close
    /// instead holds the own-transition guard through terminal publication.
    pub(crate) fn close(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.publish_terminal(LogicalTerminalCause::Closed);
        self.inner.request_work();
        self.inner.signal_state();
    }

    #[cfg(feature = "runtime-internals")]
    pub(crate) fn close_runtime(&self, _transition: &OwnTransitionGuard<'_>) {
        self.close();
    }

    #[cfg(feature = "runtime-internals")]
    pub(crate) async fn wait_stopped(&self) {
        let mut stopped = self.inner.stopped.subscribe();
        while !*stopped.borrow_and_update() {
            if stopped.changed().await.is_err() {
                break;
            }
        }
    }

    fn try_acquire_covering(
        &self,
        state: &ManagerState,
        publish: &[String],
        subscribe: &[String],
        now: i64,
        desired: &TransportAuthorizationV1,
        deadline: Instant,
    ) -> Result<Option<TransportLease>, TrellisClientError> {
        let mut candidates: Vec<&Arc<TransportGeneration>> = Vec::new();
        if let Some(current) = &state.current {
            candidates.push(current);
        }
        candidates.extend(state.draining.iter());
        for generation in candidates {
            if generation.state() == GenerationState::Closed
                || !generation.once_published.load(Ordering::Acquire)
            {
                continue;
            }
            // Never hand out a generation current authority no longer covers.
            if !generation_is_safe(generation, desired, now)? {
                continue;
            }
            if policy_covers(&generation.admitted_policy, publish, subscribe, now)? {
                if let Some(error) = self.inner.terminated_error() {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(TrellisClientError::Timeout);
                }
                if let Some(lease) = generation.lease() {
                    return Ok(Some(lease));
                }
            }
        }
        Ok(None)
    }

    fn try_acquire_current(
        &self,
        state: &ManagerState,
        now: i64,
        desired: &TransportAuthorizationV1,
        deadline: Instant,
    ) -> Result<Option<TransportLease>, TrellisClientError> {
        match &state.current {
            Some(current) if current.state() != GenerationState::Closed => {
                if !generation_is_safe(current, desired, now)? {
                    return Ok(None);
                }
                if let Some(error) = self.inner.terminated_error() {
                    return Err(error);
                }
                if Instant::now() >= deadline {
                    return Err(TrellisClientError::Timeout);
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
    let mut availability = inner.contexts.watch_availability();
    let mut terminal = inner.terminal.subscribe();
    loop {
        if inner.is_terminated() {
            shutdown(&inner).await;
            inner.stopped.send_replace(true);
            return;
        }
        {
            let _guard = inner.reconcile.lock().await;
            // A logical stop must preempt network admission, including a
            // candidate that retained an intake-owner snapshot before teardown.
            tokio::select! {
                biased;
                _ = terminal.wait_for(|cause| cause.is_some()) => {}
                result = reconcile(&inner) => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "transport generation reconciliation failed");
                    }
                }
            }
            reap(&inner).await;
        }
        if inner.is_terminated() {
            shutdown(&inner).await;
            inner.stopped.send_replace(true);
            return;
        }
        tokio::select! {
            result = work_rx.changed() => {
                if result.is_err() {
                    return;
                }
            }
            result = availability.changed() => {
                if result.is_err() {
                    return;
                }
            }
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
        let now = inner.contexts.corrected_now_seconds()?;

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
            // Coverage restoration wakes this worker independently of a context
            // promotion. Do not repeatedly prepare and retire intake while that
            // restoration is still pending; the guarded publication check below
            // remains the final authority fence after preparation.
            {
                let transition = inner.contexts.lock_own_transition()?;
                if !matches!(
                    inner.contexts.application_transport_locked(&transition),
                    Ok(Some(_))
                ) {
                    return Ok(());
                }
            }
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

        // Hold the own transition before manager state through classification and
        // publication. A promotion during preparation must not make an old
        // classification close a now-safe carrier or publish an unsafe survivor.
        {
            let transition = inner.contexts.lock_own_transition()?;
            let mut state = inner.lock_state()?;
            let Ok(Some((policy, now))) = inner.contexts.application_transport_locked(&transition)
            else {
                drop(state);
                if let Some(candidate) = &chosen {
                    for handle in promoted_intake.drain(..) {
                        candidate.push_intake(handle);
                    }
                    candidate.retire_intake(GenerationIntakeRetireReason::Superseded);
                }
                return Ok(());
            };
            let classifications_current = policy
                .digest()
                .is_ok_and(|digest| digest == desired_policy_digest)
                && keep.iter().all(|generation| {
                    generation.state() != GenerationState::Closed
                        && generation_is_safe(generation, &policy, now).is_ok_and(|safe| safe)
                })
                && close.iter().all(|generation| {
                    generation.state() == GenerationState::Closed
                        || generation_is_safe(generation, &policy, now).is_ok_and(|safe| !safe)
                });
            if !classifications_current {
                drop(state);
                if let Some(candidate) = &chosen {
                    for handle in promoted_intake.drain(..) {
                        candidate.push_intake(handle);
                    }
                    candidate.retire_intake(GenerationIntakeRetireReason::Superseded);
                }
                continue;
            }
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
            inner.set_current(&mut state, chosen.as_ref());
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
            drop(state);
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
            // Only signal a real ready-set change so the worker does not busy-loop
            // on its own signal.
            if previous_current != chosen_id
                || previous_draining != next_draining
                || !close.is_empty()
            {
                inner.signal_state();
            }
            // Framework owners follow the current generation identity.
        }

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
                    context_digest = %desired.context_digest,
                    logical_terminal = ?inner.terminated_error(),
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
        {
            // The final policy/window/clock read and publication are one guarded
            // step. Do not validate a CONNECT credential for an admitted socket.
            let transition = inner.contexts.lock_own_transition()?;
            let mut state = inner.lock_state()?;
            let Ok(Some((policy, now))) = inner.contexts.application_transport_locked(&transition)
            else {
                drop(state);
                inner.clear_opening(id);
                inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
                return Ok(());
            };
            let safe = match generation_is_safe(&candidate, &policy, now) {
                Ok(safe) => safe,
                Err(_) => {
                    drop(state);
                    inner.clear_opening(id);
                    inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
                    return Ok(());
                }
            };
            if !safe
                || !policy
                    .digest()
                    .is_ok_and(|digest| digest == desired_policy_digest)
            {
                drop(state);
                inner.clear_opening(id);
                if safe {
                    inner.track_draining(&candidate, GenerationIntakeRetireReason::Superseded);
                } else {
                    close_generation(&candidate, GenerationIntakeRetireReason::AuthorityReduced);
                }
                continue;
            }
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
            inner.set_current(&mut state, Some(&candidate));
            inner.rebind_live(&candidate);
        }
        tracing::info!(
            event = "transport_generation.activated",
            generation_id = candidate.id,
            context_digest = %candidate.context_digest,
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
        // The logical connection is closing: publish the absence of a current.
        inner.set_current(&mut state, None);
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

/// Cooperatively close one raw NATS client that never reached managed ownership.
///
/// [`open_generation`]'s future can be cancelled after `options.connect` returns
/// but before (or during) the own-admission read, when no [`TransportGeneration`]
/// exists to own the socket. Dropping an `async_nats::Client` handle is not a
/// close, so this guard drains the exact client on any unsuccessful or cancelled
/// exit and disarms only once a managed generation takes ownership. Cancellation
/// of `options.connect` itself is left to cancelling that future; no connect is
/// spawned.
struct RawConnectionGuard {
    nats: Option<async_nats::Client>,
}

impl RawConnectionGuard {
    fn new(nats: async_nats::Client) -> Self {
        Self { nats: Some(nats) }
    }

    fn disarm(&mut self) {
        self.nats = None;
    }
}

impl Drop for RawConnectionGuard {
    fn drop(&mut self) {
        if let Some(nats) = self.nats.take() {
            tokio::spawn(async move {
                let _ = nats.drain().await;
            });
        }
    }
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
    // The raw socket exists but is not yet owned by a managed generation: this
    // scoped guard drains it on any failure or cancellation before ownership is
    // taken, and disarms only once a `TransportGeneration` owns it.
    let mut raw = RawConnectionGuard::new(nats.clone());

    // G4 exact admission correlation: the broker marker must report exactly the
    // context this CONNECT presented. Missing, malformed, or differing evidence
    // closes the candidate and never publishes it.
    let admission = match read_own_admission(&nats, ADMISSION_READ_TIMEOUT).await {
        Some(admission) => admission,
        None => {
            inner.clear_opening(id);
            return Err(TrellisClientError::TransportUnavailable(
                "broker own-admission evidence is unavailable".into(),
            ));
        }
    };
    let Some(marker) = admission.context_digest else {
        inner.clear_opening(id);
        return Err(TrellisClientError::TransportUnavailable(
            "broker own-admission marker is malformed".into(),
        ));
    };
    if marker != snapshot.context_digest {
        inner.clear_opening(id);
        return Err(TrellisClientError::TransportUnavailable(
            "broker admitted a different authorization context".into(),
        ));
    }
    // A loss observed before the connection was built, or a logical close during
    // admission, must not publish the candidate.
    if failed.load(Ordering::Acquire) || inner.closed.load(Ordering::Acquire) {
        inner.clear_opening(id);
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
        once_published: AtomicBool::new(false),
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
        return Err(TrellisClientError::TransportUnavailable(
            "candidate was lost during admission".into(),
        ));
    }
    // The managed generation owns the socket now; the raw guard must not drain it.
    raw.disarm();
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
