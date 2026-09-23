//! Consumer-side live subscription: prepared handle, activation, control pump,
//! bounded ingress, credit and close.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::{Stream, StreamExt};

use trellis_protocol::{
    LiveControl, LiveControlAck, LiveControlAckAction, LiveControlActivate, LiveControlClose,
    LiveControlCredit, LiveControlEndAck, LiveControlError, LiveControlPulse, LiveEndReason,
    LiveErrorCode, LiveFrame, LiveOffer, U64s, ACK_FRAME_THRESHOLD, ACK_MAX_DELAY_MS,
    CHALLENGE_RETRY_MS, CLOSE_EXCHANGE_MS, CLOSE_RETRY_MS, CONSUMER_STALL_MS,
    HEARTBEAT_INTERVAL_MS, PEER_INACTIVITY_MS, WINDOW_BYTES, WINDOW_FRAMES,
};

use super::authority::{LiveAuthorityGuard, LiveAuthorityLost};
use super::types::{
    end_reason_for_code, CloseCleanupState, CloseRemoteState, LiveCancellation, LiveCloseReceipt,
    LiveEnd, LiveStreamError,
};
use crate::client::TrellisClientError;

/// One verified and admitted application item.
pub(crate) struct AdmittedItem<T> {
    pub value: T,
    pub encoded_len: u64,
}

/// One ordered receive slot.
///
/// Received DATA occupies a slot in sequence order until it is handed to the
/// application or deliberately filtered. A filtered slot behind an unread
/// value cannot advance the consumed prefix past that value.
pub(crate) enum Slot<T> {
    Value(AdmittedItem<T>),
    Filtered,
}

/// Consumer-visible session phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsumerPhase {
    Prepared,
    Activating,
    Active,
    Draining,
    Closing,
    Closed,
}

/// The consumer session core shared by the handle and its pump.
pub(crate) struct ConsumerCore<T> {
    pub(crate) session_id: String,
    pub(crate) phase: Mutex<ConsumerPhase>,
    /// Bounded ordered slot queue; count and encoded bytes stay within window.
    pub(crate) queue: Mutex<VecDeque<Slot<T>>>,
    pub(crate) queued_bytes: AtomicU64,
    pub(crate) received_seq: AtomicU64,
    pub(crate) consumed_seq: AtomicU64,
    pub(crate) cancelled: AtomicBool,
    pub(crate) end: Mutex<Option<LiveEnd>>,
    pub(crate) end_notify: tokio::sync::Notify,
    /// One pending item already admitted to the application.
    pub(crate) pending: Mutex<Option<AdmittedItem<T>>>,
    /// Waker registered by the application's last pending poll.
    pub(crate) waker: Mutex<Option<std::task::Waker>>,
    /// Whether the committed failure was already yielded once.
    pub(crate) error_reported: AtomicBool,
    /// A verified remote normal end, pending until the queue drains.
    pub(crate) pending_end: Mutex<Option<LiveEnd>>,
    pub(crate) start: tokio::sync::Notify,
    /// Wakes the control pump when the application consumes an item.
    pub(crate) credit_notify: tokio::sync::Notify,
    /// One telemetry owner for this endpoint's whole local lifetime.
    pub(crate) telemetry: Mutex<super::telemetry::LiveTelemetryOwner>,
}

impl<T> ConsumerCore<T> {
    pub(crate) fn new(session_id: String, kind: trellis_protocol::LiveSessionKind) -> Self {
        Self {
            session_id,
            telemetry: Mutex::new(super::telemetry::LiveTelemetryOwner::new(
                kind,
                super::telemetry::LiveSide::Consumer,
            )),
            phase: Mutex::new(ConsumerPhase::Prepared),
            queue: Mutex::new(VecDeque::new()),
            queued_bytes: AtomicU64::new(0),
            received_seq: AtomicU64::new(0),
            consumed_seq: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
            end: Mutex::new(None),
            end_notify: tokio::sync::Notify::new(),
            pending: Mutex::new(None),
            waker: Mutex::new(None),
            error_reported: AtomicBool::new(false),
            pending_end: Mutex::new(None),
            start: tokio::sync::Notify::new(),
            credit_notify: tokio::sync::Notify::new(),
        }
    }

    /// Record one verified remote normal end for draining.
    pub(crate) fn set_pending_end(&self, end: LiveEnd) {
        if let Ok(mut slot) = self.pending_end.lock() {
            if slot.is_none() {
                *slot = Some(end);
            }
        }
        self.wake();
    }

    /// Take the pending normal end once the queue is empty.
    ///
    /// Returns the committed end when draining completed, or `None` while
    /// items still wait to be handed to the application.
    pub(crate) fn drain_complete(&self) -> Option<LiveEnd> {
        let queue_empty = self.queue.lock().is_ok_and(|queue| queue.is_empty());
        if !queue_empty {
            return None;
        }
        let pending = self
            .pending_end
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())?;
        self.commit_end(pending.clone());
        Some(pending)
    }

    /// Return the consumed cursor.
    pub(crate) fn consumed_seq(&self) -> u64 {
        self.consumed_seq.load(Ordering::Acquire)
    }

    /// Return the received cursor.
    pub(crate) fn received_seq(&self) -> u64 {
        self.received_seq.load(Ordering::Acquire)
    }

    /// Record one verified received data frame.
    pub(crate) fn record_received(&self) {
        self.received_seq.fetch_add(1, Ordering::AcqRel);
    }

    /// Record credit for a verified frame that is deliberately filtered.
    ///
    /// The frame still occupies its ordered slot, so the consumed prefix cannot
    /// advance past an earlier unread value. Returns `false` when the bounded
    /// slot window cannot admit the marker.
    pub(crate) fn release_filtered(&self) -> bool {
        let Ok(mut queue) = self.queue.lock() else {
            return false;
        };
        if queue.len() as u64 + 1 > WINDOW_FRAMES {
            return false;
        }
        queue.push_back(Slot::Filtered);
        drop(queue);
        self.wake();
        true
    }

    /// Return whether a committed failure was already yielded.
    pub(crate) fn reported_error(&self) -> bool {
        self.error_reported.load(Ordering::Acquire)
    }

    /// Mark the committed failure as yielded.
    pub(crate) fn mark_error_reported(&self) {
        self.error_reported.store(true, Ordering::Release);
    }

    /// Wake the application's pending poll, if any.
    pub(crate) fn wake(&self) {
        let waker = self.waker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(crate) fn phase(&self) -> ConsumerPhase {
        self.phase
            .lock()
            .map_or(ConsumerPhase::Closed, |phase| *phase)
    }

    pub(crate) fn set_phase(&self, phase: ConsumerPhase) {
        if let Ok(mut current) = self.phase.lock() {
            if *current == phase {
                return;
            }
            *current = phase;
            drop(current);
            if let Ok(mut telemetry) = self.telemetry.lock() {
                match phase {
                    ConsumerPhase::Prepared => telemetry.prepared(),
                    ConsumerPhase::Activating => telemetry.activating(),
                    ConsumerPhase::Active => telemetry.active(),
                    ConsumerPhase::Draining => telemetry.draining(),
                    ConsumerPhase::Closing => telemetry.closing(),
                    ConsumerPhase::Closed => {}
                }
            }
        }
    }

    /// Commit one terminal outcome once; later calls are ignored.
    pub(crate) fn commit_end(&self, end: LiveEnd) {
        let Ok(mut slot) = self.end.lock() else {
            return;
        };
        if slot.is_none() {
            *slot = Some(end.clone());
            drop(slot);
            self.set_phase(ConsumerPhase::Closed);
            if let Ok(mut telemetry) = self.telemetry.lock() {
                telemetry.end(&end);
            }
            self.end_notify.notify_waiters();
            self.wake();
        }
    }

    /// Return the committed terminal outcome, if any.
    pub(crate) fn committed_end(&self) -> Option<LiveEnd> {
        self.end.lock().ok().and_then(|slot| slot.clone())
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.committed_end().is_some()
    }

    /// Admit one verified application item within the wire window.
    ///
    /// Returns `false` when the bounded queue would exceed the window; the
    /// caller closes the session with `consumer_slow`.
    pub(crate) fn admit(&self, item: AdmittedItem<T>) -> bool {
        let queued = self.queued_bytes.load(Ordering::Acquire);
        let (count, bytes) = {
            let Ok(queue) = self.queue.lock() else {
                return false;
            };
            (queue.len() as u64, queued)
        };
        if count + 1 > WINDOW_FRAMES || bytes + item.encoded_len > WINDOW_BYTES {
            return false;
        }
        let encoded_len = item.encoded_len;
        let Ok(mut queue) = self.queue.lock() else {
            return false;
        };
        queue.push_back(Slot::Value(item));
        drop(queue);
        self.queued_bytes.fetch_add(encoded_len, Ordering::AcqRel);
        if let Ok(telemetry) = self.telemetry.lock() {
            telemetry.buffered(encoded_len as i64);
        }
        self.wake();
        true
    }

    pub(crate) fn has_queued(&self) -> bool {
        self.queue.lock().is_ok_and(|queue| !queue.is_empty())
    }

    /// Take the next application item, advancing the contiguous consumed
    /// prefix across any filtered slots in front of it.
    pub(crate) fn consume(&self) -> Option<AdmittedItem<T>> {
        loop {
            let slot = {
                let Ok(mut queue) = self.queue.lock() else {
                    return None;
                };
                queue.pop_front()
            };
            match slot {
                Some(Slot::Filtered) => {
                    self.consumed_seq.fetch_add(1, Ordering::AcqRel);
                    self.credit_notify.notify_one();
                }
                Some(Slot::Value(item)) => {
                    self.queued_bytes
                        .fetch_sub(item.encoded_len, Ordering::AcqRel);
                    if let Ok(telemetry) = self.telemetry.lock() {
                        telemetry.buffered(-(item.encoded_len as i64));
                    }
                    self.consumed_seq.fetch_add(1, Ordering::AcqRel);
                    // Filtered markers behind this value need no application
                    // handoff, so the contiguous consumed prefix may advance
                    // across them immediately.
                    if let Ok(mut queue) = self.queue.lock() {
                        while matches!(queue.front(), Some(Slot::Filtered)) {
                            queue.pop_front();
                            self.consumed_seq.fetch_add(1, Ordering::AcqRel);
                        }
                    }
                    self.credit_notify.notify_one();
                    return Some(item);
                }
                None => return None,
            }
        }
    }

    /// Discard every queued item without advancing the consumed cursor.
    ///
    /// Abandoned items are not handoff to the application, so they neither
    /// grant credit nor count as consumed.
    pub(crate) fn discard_queue(&self) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        queue.clear();
        let released = self.queued_bytes.swap(0, Ordering::AcqRel);
        drop(queue);
        if released > 0 {
            if let Ok(telemetry) = self.telemetry.lock() {
                telemetry.buffered(-(released as i64));
            }
        }
    }

    /// Actual local cleanup completed; remove the session and release pending.
    pub(crate) fn cleanup_finished(&self) {
        if let Ok(mut telemetry) = self.telemetry.lock() {
            telemetry.cleanup_finished();
        }
    }

    /// Retained local cleanup exceeded the shared grace.
    pub(crate) fn cleanup_exceeded_grace(&self) {
        if let Ok(mut telemetry) = self.telemetry.lock() {
            telemetry.cleanup_exceeded_grace();
        }
    }
}

/// The public Rust owned live subscription handle.
pub struct LiveSubscription<T> {
    pub(crate) core: Arc<ConsumerCore<T>>,
    pub(crate) _drain: tokio::task::JoinHandle<()>,
    pub(crate) control: Arc<ConsumerControl>,
    pub(crate) cancellation: LiveCancellation,
    /// Set once the first poll installed the activation path.
    pub(crate) activated: bool,
    /// Retained provider authority; a loss fences queued yields.
    pub(crate) guard: Arc<super::authority::LiveAuthorityGuard>,
    /// One shared close result for repeated close calls.
    pub(crate) close_result: std::sync::Mutex<Option<LiveCloseReceipt>>,
}

impl<T> LiveSubscription<T> {
    /// Construct one owned handle from its prepared core and running pump.
    ///
    /// The core's bounded queue is the single ingress; the handle's receiver is
    /// fed by the core's consumption path, so `next()` both hands an item to the
    /// application and releases its credit.
    pub(crate) fn new(
        core: Arc<ConsumerCore<T>>,
        drain: tokio::task::JoinHandle<()>,
        control: Arc<ConsumerControl>,
        cancellation: LiveCancellation,
        guard: Arc<super::authority::LiveAuthorityGuard>,
    ) -> Self {
        Self {
            core,
            _drain: drain,
            control,
            cancellation,
            activated: false,
            guard,
            close_result: std::sync::Mutex::new(None),
        }
    }

    /// Return the committed terminal outcome, waiting for closure.
    pub async fn closed(&self) -> LiveEnd {
        loop {
            let notified = self.core.end_notify.notified();
            if let Some(end) = self.core.committed_end() {
                return end;
            }
            notified.await;
        }
    }

    /// Return whether the subscription has completed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.core.is_closed()
    }

    /// Return the session identifier (redacted from metrics by policy).
    #[must_use]
    pub(crate) fn session_id(&self) -> &str {
        &self.core.session_id
    }

    /// Explicitly close the observation and await the bounded close exchange.
    ///
    /// Repeated close calls share one exchange and receipt. The close keeps the
    /// session's actual cursors, not hard-coded zeros.
    pub async fn close(&mut self) -> Result<LiveCloseReceipt, TrellisClientError> {
        self.cancellation.cancel();
        self.core.discard_queue();
        self.core.wake();
        if let Some(receipt) = self.close_result.lock().ok().and_then(|slot| slot.clone()) {
            return Ok(receipt);
        }
        let receipt = self
            .control
            .begin_close(self.core.received_seq(), self.core.consumed_seq())
            .await;
        if let Ok(mut slot) = self.close_result.lock() {
            *slot = Some(receipt.clone());
        }
        Ok(receipt)
    }

    /// Map or filter application items while retaining the owning handle.
    pub(crate) fn map_items<U, F>(self, map: F) -> LiveMappedSubscription<T, U, F>
    where
        F: FnMut(T) -> LiveMapDecision<U>,
    {
        LiveMappedSubscription {
            inner: self,
            map,
            _item: std::marker::PhantomData,
        }
    }
}

impl<T> Drop for LiveSubscription<T> {
    fn drop(&mut self) {
        // Synchronous local fence: no new yields, no new scheduling, and every
        // blocked waiter wakes before any asynchronous work is enqueued.
        self.core.cancelled.store(true, Ordering::Release);
        self.cancellation.cancel();
        self.core.discard_queue();
        self.core.wake();
        // Remote cleanup is the ordinary signed bounded close exchange,
        // enqueued only when a runtime is already available. Dropping the pump
        // future immediately releases its admission and ingress.
        self.control
            .schedule_local_drop_cleanup(self.core.received_seq(), self.core.consumed_seq());
        self._drain.abort();
    }
}

/// Outcome of one mapped live item.
pub(crate) enum LiveMapDecision<U> {
    /// Expose this item to the application.
    Emit(U),
    /// Drop the item after its credit was released.
    Skip,
    /// Stop iteration and cancel the underlying observation.
    Complete,
}

/// Internal mapping adapter that forwards close, Drop, and cancellation.
pub(crate) struct LiveMappedSubscription<T, U, F> {
    inner: LiveSubscription<T>,
    map: F,
    _item: std::marker::PhantomData<fn(T) -> U>,
}

impl<T, U, F> LiveMappedSubscription<T, U, F> {
    /// Explicitly close the underlying observation.
    pub async fn close(&mut self) -> Result<LiveCloseReceipt, TrellisClientError> {
        self.inner.close().await
    }

    /// Return the committed terminal outcome of the underlying observation.
    pub async fn closed(&self) -> LiveEnd {
        self.inner.closed().await
    }
}

impl<T, U, F> Stream for LiveMappedSubscription<T, U, F>
where
    F: FnMut(T) -> LiveMapDecision<U> + Unpin,
{
    type Item = Result<U, TrellisClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(item))) => match (this.map)(item) {
                    LiveMapDecision::Emit(item) => return Poll::Ready(Some(Ok(item))),
                    LiveMapDecision::Skip => continue,
                    LiveMapDecision::Complete => {
                        this.inner.cancellation.cancel();
                        return Poll::Ready(None);
                    }
                },
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<T> Stream for LiveSubscription<T> {
    type Item = Result<T, TrellisClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if !this.activated {
            this.activated = true;
            this.core.start.notify_one();
        }
        if this.core.cancelled.load(Ordering::Acquire) {
            this.core.discard_queue();
            return Poll::Ready(None);
        }
        if this.core.committed_end().is_none() && matches!(this.core.phase(), ConsumerPhase::Active)
        {
            if let Err(lost) = this.guard.check_now() {
                this.core.discard_queue();
                this.core.commit_end(authority_failure(&lost));
            }
        }
        if let Some(end) = this.core.committed_end() {
            if !end.is_complete() {
                this.core.discard_queue();
                if let Some(error) = end.error() {
                    if !this.core.reported_error() {
                        this.core.mark_error_reported();
                        return Poll::Ready(Some(Err(TrellisClientError::Live(error.clone()))));
                    }
                }
                return Poll::Ready(None);
            }
            if this.core.drain_complete().is_some() || !this.core.has_queued() {
                return Poll::Ready(None);
            }
        }
        if matches!(
            this.core.phase(),
            ConsumerPhase::Active | ConsumerPhase::Draining
        ) {
            if let Some(item) = this.core.consume() {
                // Handing out the last queued value commits a pending normal end
                // immediately; the application need not poll once more.
                let _ = this.core.drain_complete();
                return Poll::Ready(Some(Ok(item.value)));
            }
        }
        if this.core.drain_complete().is_some() {
            return Poll::Ready(None);
        }
        if let Ok(mut slot) = this.core.waker.lock() {
            *slot = Some(cx.waker().clone());
        }
        if matches!(
            this.core.phase(),
            ConsumerPhase::Active | ConsumerPhase::Draining
        ) && (this.core.has_queued() || this.core.committed_end().is_some())
        {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
}

/// Control owner for one consumer session.
///
/// Owns the NATS control path: fresh request proofs per attempt, logical
/// idempotence, close exchange and local shutdown notifications. It holds the
/// cloneable transport and signing material directly, so it never keeps the
/// whole connection client alive through a reference cycle.
pub(crate) struct ConsumerControl {
    pub(crate) nats: async_nats::Client,
    pub(crate) auth: Arc<crate::client::SessionAuth>,
    pub(crate) contexts: Arc<crate::client::AuthorizationContextCache>,
    pub(crate) inbox_prefix: String,
    pub(crate) session_id: String,
    pub(crate) control_subject: String,
    /// Provider runtime key pinned at offer acceptance.
    pub(crate) pinned_session_key: String,
    /// Provider identity tuple pinned at offer acceptance.
    pub(crate) pinned_identity: super::authority::PinnedPeerIdentity,
    /// Retained provider authority for responses and identity-preserving
    /// refresh replacement.
    pub(crate) provider_guard: Arc<super::authority::LiveAuthorityGuard>,
    pub(crate) close_started: AtomicBool,
    pub(crate) last_control_seq: AtomicU64,
}

impl ConsumerControl {
    /// Reserve the next logical control sequence for this session.
    pub(crate) fn next_control_seq(&self) -> u64 {
        self.last_control_seq.fetch_add(1, Ordering::AcqRel) + 1
    }
}

impl ConsumerControl {
    fn signed_headers(
        &self,
        reply: &str,
        body: &[u8],
    ) -> Result<(async_nats::HeaderMap, String), TrellisClientError> {
        let context_digest = self
            .contexts
            .context_digest()
            .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
        let iat = crate::client::now_iat_seconds() as i64;
        let request_id = crate::client::new_request_id();
        let proof = self.auth.create_request_proof(
            &context_digest,
            &self.control_subject,
            reply,
            body,
            iat,
            &request_id,
        )?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("authorization-context", context_digest.as_str());
        headers.insert("session-key", self.auth.session_key.as_str());
        headers.insert("proof", proof.as_str());
        headers.insert("iat", iat.to_string().as_str());
        headers.insert("request-id", request_id.as_str());
        Ok((headers, request_id))
    }
}

impl ConsumerControl {
    /// Enqueue the ordinary signed bounded close exchange after a Drop fence.
    ///
    /// Drop never creates a runtime and never blocks. When a runtime is already
    /// available the exact same signed close path used by `close()` runs on it.
    pub(crate) fn schedule_local_drop_cleanup(&self, received: u64, consumed: u64) {
        if self.close_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let control = ConsumerControl {
            nats: self.nats.clone(),
            auth: self.auth.clone(),
            contexts: self.contexts.clone(),
            inbox_prefix: self.inbox_prefix.clone(),
            session_id: self.session_id.clone(),
            control_subject: self.control_subject.clone(),
            pinned_session_key: self.pinned_session_key.clone(),
            pinned_identity: self.pinned_identity.clone(),
            provider_guard: self.provider_guard.clone(),
            close_started: AtomicBool::new(true),
            last_control_seq: AtomicU64::new(self.last_control_seq.load(Ordering::Acquire)),
        };
        handle.spawn(async move {
            let _ = control.begin_close(received, consumed).await;
        });
    }

    /// Send one logical control and await its signed acknowledgement.
    ///
    /// Each attempt uses a fresh request proof and reply inbox; the logical
    /// body and sequence stay identical across retries. One attempt has the
    /// protocol control-timeout budget and cannot exceed the caller's deadline.
    pub(crate) async fn send_control(
        &self,
        control: &LiveControl,
        control_seq: u64,
    ) -> Result<LiveControlAck, TrellisClientError> {
        self.last_control_seq.store(control_seq, Ordering::Release);
        let body = control_body_bytes(control)?;
        let reply = format!(
            "{}.{}",
            self.inbox_prefix,
            CONTROL_INBOX_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let (headers, request_id) = self.signed_headers(&reply, &body)?;
        let mut subscriber = self
            .nats
            .subscribe(reply.clone())
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
        self.nats
            .publish_with_reply_and_headers(
                self.control_subject.clone(),
                reply.clone(),
                headers,
                body,
            )
            .await
            .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
        let response = tokio::time::timeout(
            std::time::Duration::from_millis(trellis_protocol::CONTROL_TIMEOUT_MS),
            subscriber.next(),
        )
        .await
        .map_err(|_| TrellisClientError::Timeout)?
        .ok_or(TrellisClientError::Timeout)?;
        if response.subject.as_str() != reply {
            return Err(TrellisClientError::LiveProtocol(
                "control reply arrived on a foreign subject".into(),
            ));
        }
        self.reconcile_provider_context(&response).await?;
        parse_control_response(self, &response, &request_id, control_seq, control.action())
    }

    /// Accept an identity-preserving provider context from one signed reply.
    ///
    /// The response proof is already bound to this digest and the pinned key;
    /// this step retains exact coverage before the response is trusted.
    async fn reconcile_provider_context(
        &self,
        response: &async_nats::Message,
    ) -> Result<(), TrellisClientError> {
        let digest = response
            .headers
            .as_ref()
            .and_then(|headers| headers.get("authorization-context"))
            .map(ToString::to_string)
            .ok_or_else(|| {
                TrellisClientError::LiveProtocol("control reply omitted context".into())
            })?;
        if digest == self.provider_guard.context_digest() {
            return Ok(());
        }
        let candidate = self
            .provider_guard
            .prepare_replacement(&digest)
            .await
            .map_err(|lost| {
                TrellisClientError::AuthorizationUnavailable(format!(
                    "control reply context: {lost:?}"
                ))
            })?;
        self.provider_guard
            .commit_replacement(candidate)
            .map_err(|lost| {
                TrellisClientError::AuthorizationUnavailable(format!(
                    "control reply context: {lost:?}"
                ))
            })?;
        Ok(())
    }

    /// Send a bounded close exchange and report remote confirmation.
    ///
    /// The exchange keeps the session's actual cursors and shares one absolute
    /// five-second budget across every attempt and retry.
    pub(crate) async fn begin_close(&self, received: u64, consumed: u64) -> LiveCloseReceipt {
        let end = LiveEnd::new(LiveEndReason::Cancelled, None);
        let _ = self.close_started.swap(true, Ordering::AcqRel);
        let cleanup = self
            .retry_until_confirmed(received, consumed)
            .await
            .unwrap_or(CloseCleanupState::Unknown);
        let remote = if cleanup == CloseCleanupState::Unknown {
            CloseRemoteState::Unconfirmed
        } else {
            CloseRemoteState::Confirmed
        };
        LiveCloseReceipt::new(end, remote, cleanup)
    }

    /// Retry the same logical close within one total exchange budget.
    async fn retry_until_confirmed(
        &self,
        received: u64,
        consumed: u64,
    ) -> Option<CloseCleanupState> {
        let deadline = tokio::time::Instant::now() + close_exchange();
        let control_seq = self
            .last_control_seq
            .load(Ordering::Acquire)
            .saturating_add(1);
        let control = close_control(
            &self.session_id,
            control_seq,
            trellis_protocol::ConsumerCloseReason::Cancelled,
            received,
            consumed,
        );
        loop {
            let attempt_deadline =
                deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(1_000));
            let attempt = tokio::time::timeout_at(
                attempt_deadline,
                self.send_control(&control, control.control_seq().get()),
            )
            .await;
            match attempt {
                Ok(Ok(ack)) => {
                    return Some(match ack.cleanup {
                        Some(trellis_protocol::CleanupStatus::Complete) => {
                            CloseCleanupState::Complete
                        }
                        Some(trellis_protocol::CleanupStatus::Incomplete) => {
                            CloseCleanupState::Incomplete
                        }
                        None => CloseCleanupState::Unknown,
                    });
                }
                Ok(Err(_)) | Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
                }
                _ => return None,
            }
        }
    }
}

/// Serialize one control body for the wire.
pub(crate) fn control_body_bytes(
    control: &LiveControl,
) -> Result<bytes::Bytes, TrellisClientError> {
    let value = match control {
        LiveControl::Activate(body) => serde_json::to_value(body)?,
        LiveControl::Pulse(body) => serde_json::to_value(body)?,
        LiveControl::Ack(body) => serde_json::to_value(body)?,
        LiveControl::Close(body) => serde_json::to_value(body)?,
        LiveControl::EndAck(body) => serde_json::to_value(body)?,
    };
    Ok(bytes::Bytes::from(serde_json::to_vec(&value)?))
}

/// Verified offer data retained by a prepared consumer handle.
pub(crate) struct PreparedOffer {
    pub offer: LiveOffer,
    pub authority: LiveAuthorityGuard,
}

/// Build one sanitized terminal outcome for a consumer-side failure.
#[must_use]
pub(crate) fn consumer_failure(code: LiveErrorCode, message: impl Into<String>) -> LiveEnd {
    LiveEnd::new(
        end_reason_for_code(code),
        Some(Arc::new(LiveStreamError::new(code, message))),
    )
}

/// Build one authority-loss terminal outcome.
#[must_use]
pub(crate) fn authority_failure(lost: &LiveAuthorityLost) -> LiveEnd {
    super::manager::authority_end(lost)
}

/// Return the peer-inactivity deadline as a duration.
#[must_use]
pub(crate) fn peer_inactivity() -> std::time::Duration {
    std::time::Duration::from_millis(PEER_INACTIVITY_MS)
}

/// Return the consumer-stall deadline as a duration.
#[must_use]
pub(crate) fn consumer_stall() -> std::time::Duration {
    std::time::Duration::from_millis(CONSUMER_STALL_MS)
}

/// Return the heartbeat interval as a duration.
#[must_use]
pub(crate) fn heartbeat_interval() -> std::time::Duration {
    std::time::Duration::from_millis(HEARTBEAT_INTERVAL_MS)
}

/// Return the challenge retry interval as a duration.
#[must_use]
pub(crate) fn challenge_retry() -> std::time::Duration {
    std::time::Duration::from_millis(CHALLENGE_RETRY_MS)
}

/// Return the total close exchange budget as a duration.
#[must_use]
pub(crate) fn close_exchange() -> std::time::Duration {
    std::time::Duration::from_millis(CLOSE_EXCHANGE_MS)
}

/// Return the close retry interval as a duration.
#[must_use]
pub(crate) fn close_retry() -> std::time::Duration {
    std::time::Duration::from_millis(CLOSE_RETRY_MS)
}

/// Return the credit ack delay as a duration.
#[must_use]
pub(crate) fn ack_max_delay() -> std::time::Duration {
    std::time::Duration::from_millis(ACK_MAX_DELAY_MS)
}

/// Return the credit frame threshold.
#[must_use]
pub(crate) const fn ack_frame_threshold() -> u64 {
    ACK_FRAME_THRESHOLD
}

/// Build the activate control body for one session.
#[must_use]
pub(crate) fn activate_control(session_id: &str) -> LiveControl {
    LiveControl::Activate(LiveControlActivate {
        action: trellis_protocol::ActivateAction::Activate,
        received_seq: U64s::new(0),
        consumed_seq: U64s::new(0),
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: trellis_protocol::LiveControlKind::Control,
        session_id: session_id.to_owned(),
        control_seq: U64s::new(1),
    })
}

/// Build one pulse control body.
#[must_use]
pub(crate) fn pulse_control(
    session_id: &str,
    control_seq: u64,
    challenge_id: &str,
    received: u64,
    consumed: u64,
) -> LiveControl {
    LiveControl::Pulse(LiveControlPulse {
        action: trellis_protocol::PulseAction::Pulse,
        challenge_id: challenge_id.to_owned(),
        received_seq: U64s::new(received),
        consumed_seq: U64s::new(consumed),
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: trellis_protocol::LiveControlKind::Control,
        session_id: session_id.to_owned(),
        control_seq: U64s::new(control_seq),
    })
}

/// Build one credit control body.
#[must_use]
pub(crate) fn credit_control(
    session_id: &str,
    control_seq: u64,
    received: u64,
    consumed: u64,
) -> LiveControl {
    LiveControl::Ack(LiveControlCredit {
        action: trellis_protocol::AckAction::Ack,
        received_seq: U64s::new(received),
        consumed_seq: U64s::new(consumed),
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: trellis_protocol::LiveControlKind::Control,
        session_id: session_id.to_owned(),
        control_seq: U64s::new(control_seq),
    })
}

/// Build one close control body.
#[must_use]
pub(crate) fn close_control(
    session_id: &str,
    control_seq: u64,
    reason: trellis_protocol::ConsumerCloseReason,
    received: u64,
    consumed: u64,
) -> LiveControl {
    LiveControl::Close(LiveControlClose {
        action: trellis_protocol::CloseAction::Close,
        reason,
        received_seq: U64s::new(received),
        consumed_seq: U64s::new(consumed),
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: trellis_protocol::LiveControlKind::Control,
        session_id: session_id.to_owned(),
        control_seq: U64s::new(control_seq),
    })
}

/// Build one end-ack control body.
#[must_use]
pub(crate) fn end_ack_control(
    session_id: &str,
    control_seq: u64,
    final_seq: u64,
    received: u64,
    consumed: u64,
) -> LiveControl {
    LiveControl::EndAck(LiveControlEndAck {
        action: trellis_protocol::EndAckAction::EndAck,
        final_seq: U64s::new(final_seq),
        received_seq: U64s::new(received),
        consumed_seq: U64s::new(consumed),
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: trellis_protocol::LiveControlKind::Control,
        session_id: session_id.to_owned(),
        control_seq: U64s::new(control_seq),
    })
}

/// Validate one provider frame against session sequence state.
///
/// # Errors
///
/// Returns the matching wire code for a gap, replay, or invalid terminal.
pub(crate) fn validate_frame_sequence(
    frame: &LiveFrame,
    expected_next: u64,
    received: u64,
) -> Result<Option<u64>, LiveErrorCode> {
    match frame {
        LiveFrame::Data(data) => {
            let seq = data.seq.get();
            if seq > expected_next {
                return Err(LiveErrorCode::DeliveryGap);
            }
            if seq < expected_next {
                // Duplicate already-received sequence: discard without credit.
                return Ok(None);
            }
            Ok(Some(seq))
        }
        LiveFrame::Challenge(challenge) => {
            if challenge.last_sent_seq.get() > received {
                return Err(LiveErrorCode::DeliveryGap);
            }
            Ok(None)
        }
        LiveFrame::End(end) => {
            if end.final_seq.get() != received {
                return Err(LiveErrorCode::DeliveryGap);
            }
            Ok(None)
        }
    }
}

/// Parse one signed provider control response.
///
/// # Errors
///
/// Returns a protocol error for an unknown response shape; a `control-error`
/// surfaces its wire code.
pub(crate) fn parse_control_response(
    control: &ConsumerControl,
    message: &async_nats::Message,
    expected_request_id: &str,
    expected_control_seq: u64,
    expected_action: &str,
) -> Result<LiveControlAck, TrellisClientError> {
    let headers = message
        .headers
        .as_ref()
        .ok_or_else(|| TrellisClientError::LiveProtocol("control reply omitted headers".into()))?;
    let context_digest = headers
        .get("authorization-context")
        .ok_or_else(|| TrellisClientError::LiveProtocol("control reply omitted context".into()))?
        .to_string();
    let session_key = headers
        .get("session-key")
        .ok_or_else(|| TrellisClientError::LiveProtocol("control reply omitted signer".into()))?
        .to_string();
    let proof = headers
        .get("trellis-live-proof")
        .ok_or_else(|| TrellisClientError::LiveProtocol("control reply omitted proof".into()))?
        .to_string();
    if session_key != control.pinned_session_key {
        return Err(TrellisClientError::LiveProtocol(
            "control reply signer does not match the pinned provider".into(),
        ));
    }
    trellis_protocol::verify_live_server_proof_encoded(
        &trellis_protocol::LiveServerProof::parse(proof)
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?,
        &context_digest,
        message.subject.as_str(),
        &message.payload,
        &control.pinned_session_key,
    )
    .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&message.payload)
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
    match value.get("type").and_then(|kind| kind.as_str()) {
        Some("control-ack") => {
            let ack: LiveControlAck = serde_json::from_value(value)
                .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
            if ack.session_id != control.session_id
                || ack.control_seq.get() != expected_control_seq
                || ack.request_id != expected_request_id
                || !ack_action_matches(ack.action, expected_action)
            {
                return Err(TrellisClientError::LiveProtocol(
                    "control reply does not answer this attempt".into(),
                ));
            }
            Ok(ack)
        }
        Some("control-error") => {
            let error: LiveControlError = serde_json::from_value(value)
                .map_err(|parse| TrellisClientError::LiveProtocol(parse.to_string()))?;
            if error.session_id != control.session_id
                || error.control_seq.get() != expected_control_seq
                || error.request_id != expected_request_id
            {
                return Err(TrellisClientError::LiveProtocol(
                    "control error does not answer this attempt".into(),
                ));
            }
            Err(TrellisClientError::LiveProtocol(format!(
                "live control rejected with '{:?}'",
                error.code
            )))
        }
        _ => Err(TrellisClientError::LiveProtocol(
            "unexpected live control response".into(),
        )),
    }
}

/// Return whether one acknowledgement action answers the sent logical action.
#[must_use]
fn ack_action_matches(action: LiveControlAckAction, expected: &str) -> bool {
    matches!(
        (action, expected),
        (LiveControlAckAction::Activate, "activate")
            | (LiveControlAckAction::Pulse, "pulse")
            | (LiveControlAckAction::Ack, "ack")
            | (LiveControlAckAction::Close, "close")
            | (LiveControlAckAction::EndAck, "end-ack")
    )
}

static CONTROL_INBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Return the serialized byte length of one encoded application value.
#[must_use]
pub(crate) fn encoded_len(value: &serde_json::Value) -> u64 {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waker_recheck_does_not_consume_credit() {
        let core = ConsumerCore::new(
            "session".into(),
            trellis_protocol::LiveSessionKind::Standalone,
        );
        assert!(!core.has_queued());
        assert_eq!(core.consumed_seq(), 0);
        assert!(core.admit(AdmittedItem {
            value: 7_u8,
            encoded_len: 1,
        }));
        assert!(core.has_queued());
        assert_eq!(core.consumed_seq(), 0);
        let item = core.consume().expect("queued item");
        assert_eq!(item.value, 7);
        assert_eq!(core.consumed_seq(), 1);
        assert!(!core.has_queued());
    }

    #[test]
    fn filtered_slot_holds_the_consumed_prefix() {
        // G09: a filtered frame behind an unread value cannot advance the
        // consumed prefix past that value.
        let core = ConsumerCore::new(
            "session".into(),
            trellis_protocol::LiveSessionKind::Standalone,
        );
        assert!(core.admit(AdmittedItem {
            value: 1_u8,
            encoded_len: 10,
        }));
        assert!(core.release_filtered());
        assert_eq!(core.consumed_seq(), 0);
        let item = core.consume().expect("first value");
        assert_eq!(item.value, 1);
        assert_eq!(core.consumed_seq(), 2);
        assert!(core.consume().is_none());
    }

    #[test]
    fn last_handoff_commits_a_pending_normal_end() {
        // G10: handing out the final queued value commits `complete` without
        // another `next()` call.
        let core = ConsumerCore::new(
            "session".into(),
            trellis_protocol::LiveSessionKind::Standalone,
        );
        assert!(core.admit(AdmittedItem {
            value: 9_u8,
            encoded_len: 4,
        }));
        core.set_pending_end(LiveEnd::complete());
        assert!(core.committed_end().is_none());
        let item = core.consume().expect("final value");
        assert_eq!(item.value, 9);
        let committed = core.drain_complete().expect("drain commits the end");
        assert!(committed.is_complete());
        assert!(core.committed_end().is_some_and(|end| end.is_complete()));
    }

    #[test]
    fn seventy_thousand_transitions_cross_the_old_count_boundary() {
        // T01: actual production sequence/credit transitions cross 65,535
        // without wrap or reuse. No broker/crypto/serialization overhead.
        let core = ConsumerCore::new(
            "session".into(),
            trellis_protocol::LiveSessionKind::Standalone,
        );
        for index in 1..=70_001_u64 {
            assert!(core.admit(AdmittedItem {
                value: index,
                encoded_len: 8,
            }));
            if index % 4 == 0 {
                assert!(core.release_filtered());
            }
            let item = core.consume().expect("queued item");
            assert_eq!(item.value, index);
        }
        let extra = core.consume();
        assert!(extra.is_none());
        // Every value plus every filtered marker is accounted exactly once.
        assert_eq!(core.consumed_seq(), 70_001 + 17_500);
    }

    #[test]
    fn abnormal_end_discards_queued_items() {
        let core = ConsumerCore::new(
            "session".into(),
            trellis_protocol::LiveSessionKind::Standalone,
        );
        assert!(core.admit(AdmittedItem {
            value: 1_u8,
            encoded_len: 1,
        }));
        core.commit_end(consumer_failure(
            LiveErrorCode::AuthorizationRevoked,
            "revoked",
        ));
        core.discard_queue();
        assert!(!core.has_queued());
        assert!(core.committed_end().is_some());
        assert_eq!(core.consumed_seq(), 0);
    }
}
