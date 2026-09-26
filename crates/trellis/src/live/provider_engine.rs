//! Provider-side live session engine: opening admission, signed offer, owner
//! control handling, activation, serialized publication and owned cleanup.
//!
//! One engine instance is driven by the service router per accepted opening
//! request. It never starts a domain source before the delivery-path challenge
//! round trip completes.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::time::Instant;

use trellis_protocol::{
    LiveControl, LiveControlAck, LiveEndReason, LiveErrorCode, LiveSessionKind, LiveSessionState,
    WireTerminal,
};

use super::authority::{LiveAuthorityGuard, LiveAuthorityLost, PinnedPeerIdentity};
use super::deadlines::{DeadlineAction, LiveDeadlines};
use super::manager::{LiveSessionManager, ProviderPermit};
use super::provider::{
    bounded_message, challenge_frame, control_ack, control_error, data_frame, end_frame,
    provider_failure, receipt_for, terminal_from_end, ChallengeState, ProviderPhase,
    ProviderReservationIdentity, ProviderSession, ProviderSessionSubjects,
};
use super::types::{CloseCleanupState, LiveEnd, LiveStreamError};

/// One accepted opening request ready to reserve a session.
pub(crate) struct ProviderOpenRequest {
    pub kind: LiveSessionKind,
    pub base_subject: String,
    pub open_id: String,
    pub consumer: PinnedPeerIdentity,
    pub consumer_max_payload_bytes: u64,
    pub canonical_open_hash: String,
}

/// One domain source item ready for publication.
pub(crate) enum SourceItem {
    /// One application value encoded by the generated codec.
    Value(serde_json::Value),
    /// Normal end of the source.
    End,
}

/// One provider source bound at activation.
pub(crate) type ProviderSource = Pin<Box<dyn Stream<Item = Result<SourceItem, String>> + Send>>;

/// One owned cleanup callback run under the shared provider grace.
pub(crate) type ProviderCleanup =
    Box<dyn FnOnce() -> Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>;

/// One registered source factory invoked exactly once at activation.
pub(crate) type ProviderSourceFactory = Box<dyn FnOnce() -> ProviderSource + Send>;

/// One committed logical control and its cached semantic outcome.
#[derive(Clone)]
pub(crate) struct CachedControl {
    seq: u64,
    hash: String,
    state: LiveSessionState,
    terminal: Option<WireTerminal>,
    cleanup: Option<trellis_protocol::CleanupStatus>,
    challenge: Option<ChallengeState>,
    defer_cleanup: bool,
}

/// One deferred close acknowledgement awaiting owned cleanup settlement.
pub(crate) struct PendingCloseAck {
    pub reply: String,
    pub request_id: String,
    pub control: LiveControl,
}

/// One registered provider session with its source factory and cleanup hook.
pub(crate) struct ProviderSessionRecord {
    pub session: Arc<ProviderSession>,
    pub manager: Weak<LiveSessionManager>,
    pub permit: std::sync::Mutex<Option<ProviderPermit>>,
    pub own_guard: LiveAuthorityGuard,
    /// Shared with the manager's receipt window so a closed session's retry can
    /// still authenticate its caller.
    pub caller_guard: Arc<LiveAuthorityGuard>,
    pub source_factory: std::sync::Mutex<Option<ProviderSourceFactory>>,
    pub cleanup: std::sync::Mutex<Vec<ProviderCleanup>>,
    pub terminal: std::sync::Mutex<Option<LiveEnd>>,
    /// Cancellation for this session's source scope.
    pub cancellation: super::types::LiveCancellation,
    pub source_started: AtomicBool,
    pub max_data_body_bytes: u64,
    /// Single monotonic deadline owner for this session.
    pub deadlines: std::sync::Mutex<LiveDeadlines>,
    /// Wakes the session timer when a deadline changes.
    pub deadline_notify: tokio::sync::Notify,
    /// Guards one-time closure and receipt recording.
    pub finished: AtomicBool,
    /// Server-side live telemetry ownership, including the Live-only projection.
    pub telemetry: std::sync::Mutex<super::telemetry::LiveTelemetryOwner>,
    /// Serializes every signed handoff for this session: DATA, challenge, END,
    /// acknowledgements and error replies share one ordering boundary.
    pub output_lane: tokio::sync::Mutex<()>,
    /// Owned source driver task, joined on the one termination path.
    pub source_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// One close driver per session.
    pub cleanup_driver_started: AtomicBool,
    /// Observed cleanup outcome once the driver records it.
    pub cleanup_result: std::sync::Mutex<Option<CloseCleanupState>>,
    /// Last committed logical control and its cached semantic outcome.
    pub control_receipt: std::sync::Mutex<Option<CachedControl>>,
    /// Latest close acknowledgement, when a retry arrives while cleanup runs.
    pub pending_close_ack: std::sync::Mutex<Option<PendingCloseAck>>,
    /// Exactly one terminal frame is published per session.
    pub end_sent: AtomicBool,
}

impl ProviderSessionRecord {
    /// Reserve one offered session and install its owner-control subscription.
    ///
    /// # Errors
    ///
    /// Returns a wire error when the owner-control subscription cannot be
    /// installed or the provider admission bound is reached.
    pub(crate) async fn reserve(
        manager: &Arc<LiveSessionManager>,
        request: &ProviderOpenRequest,
        own_deployment_id: &str,
        max_data_body_bytes: u64,
        now_ms: u64,
    ) -> Result<(Arc<ProviderSession>, ProviderPermit), LiveErrorCode> {
        let permit = manager.clone().admit_provider(
            &request.consumer.connection_id,
            &request.consumer.session_key,
        )?;
        manager
            .register_owner_control(&request.base_subject)
            .await
            .map_err(|_| LiveErrorCode::ResourceExhausted)?;
        let _ = own_deployment_id;
        let _ = max_data_body_bytes;
        let session_id =
            trellis_protocol::generate_nonce().map_err(|_| LiveErrorCode::ProtocolError)?;
        let subjects = ProviderSessionSubjects {
            base_subject: request.base_subject.clone(),
            data_subject: trellis_protocol::derive_live_data_subject(
                manager.provider_connection_id(),
                &request.consumer.connection_id,
                &session_id,
            )
            .map_err(|_| LiveErrorCode::InvalidSubject)?,
            control_subject: trellis_protocol::derive_live_observe_subject(
                &request.base_subject,
                manager.provider_connection_id(),
                &session_id,
            )
            .map_err(|_| LiveErrorCode::InvalidSubject)?,
        };
        let session = Arc::new(ProviderSession::new(
            ProviderReservationIdentity {
                session_id,
                open_id: request.open_id.clone(),
                kind: request.kind,
            },
            subjects,
            request.consumer.clone(),
            now_ms,
        ));
        Ok((session, permit))
    }

    pub(crate) fn manager(&self) -> Result<Arc<LiveSessionManager>, LiveErrorCode> {
        self.manager.upgrade().ok_or(LiveErrorCode::Disconnected)
    }

    pub(crate) async fn signed_headers(
        &self,
        subject: &str,
        body: &[u8],
    ) -> Result<async_nats::HeaderMap, LiveErrorCode> {
        // Pause signed handoffs until any planned credential rotation has
        // re-established exact coverage for both retained guards.
        self.own_guard
            .reconcile()
            .await
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        self.caller_guard
            .reconcile()
            .await
            .map_err(|_| LiveErrorCode::PermissionDenied)?;
        let manager = self.manager()?;
        let digest = manager
            .contexts_handle()
            .context_digest()
            .map_err(|_| LiveErrorCode::AuthorizationUnavailable)?;
        if digest.is_empty() {
            return Err(LiveErrorCode::AuthorizationUnavailable);
        }
        let proof = trellis_protocol::sign_live_server_proof(
            &digest,
            subject,
            body,
            manager.auth_handle().live_signing_key(),
        )
        .map_err(|_| LiveErrorCode::AuthorizationUnavailable)?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("authorization-context", digest.as_str());
        headers.insert("session-key", manager.auth_handle().session_key.as_str());
        headers.insert("trellis-live-proof", proof.as_str());
        Ok(headers)
    }

    /// Build one signed offer body for this reservation.
    ///
    /// # Errors
    ///
    /// Returns a wire error when the offer cannot be serialized or the session
    /// has already left its offered state.
    pub(crate) fn offer_body(
        &self,
        request_id: &str,
        provider: &PinnedPeerIdentity,
        consumer: &PinnedPeerIdentity,
        limits: trellis_protocol::LiveOfferLimits,
        max_data_body_bytes: u64,
    ) -> Result<Bytes, LiveErrorCode> {
        let _ = max_data_body_bytes;
        let offer = trellis_protocol::LiveOffer {
            format: trellis_protocol::LIVE_VERSION.to_owned(),
            kind: trellis_protocol::LiveOfferKind::Offer,
            session_kind: self.session.kind,
            open_id: self.session.open_id.clone(),
            request_id: request_id.to_owned(),
            session_id: self.session.session_id.clone(),
            base_subject: self.session.base_subject.clone(),
            data_subject: self.session.data_subject.clone(),
            control_subject: self.session.control_subject.clone(),
            provider: trellis_protocol::LiveOfferProvider {
                connection_id: provider.connection_id.clone(),
                session_key: trellis_protocol::encode_subject_token(&provider.session_key),
                principal_id: provider.principal_id.clone(),
                participant_id: provider.participant_id.clone(),
                deployment_id: provider.deployment_id.clone().unwrap_or_default(),
                instance_id: provider.instance_id.clone().unwrap_or_default(),
            },
            consumer: trellis_protocol::LiveOfferConsumer {
                connection_id: consumer.connection_id.clone(),
                session_key: trellis_protocol::encode_subject_token(&consumer.session_key),
                principal_id: consumer.principal_id.clone(),
                participant_id: consumer.participant_id.clone(),
            },
            limits,
        };
        serde_json::to_vec(&offer)
            .map(Bytes::from)
            .map_err(|_| LiveErrorCode::ProtocolError)
    }

    /// Handle one owner control, mutating state once and caching the outcome.
    ///
    /// # Errors
    ///
    /// Returns a typed control response error for an authenticated owner
    /// violation.
    pub(crate) async fn handle_control(
        &self,
        control: &LiveControl,
        now: Instant,
    ) -> Result<ControlOutcome, LiveErrorCode> {
        let seq = control.control_seq().get();
        let hash = trellis_protocol::logical_control_hash(control)
            .map_err(|_| LiveErrorCode::InvalidRequest)?;
        if let Some(cached) = self.cached_control(seq, &hash)? {
            // A replayed close reports whatever owned cleanup has settled by
            // now; while cleanup is still running the acknowledgement stays
            // deferred to the one close driver.
            let cleanup = if cached.defer_cleanup {
                cached.cleanup.or_else(|| {
                    self.cleanup_result
                        .lock()
                        .ok()
                        .and_then(|result| *result)
                        .map(|state| match state {
                            CloseCleanupState::Complete => {
                                trellis_protocol::CleanupStatus::Complete
                            }
                            _ => trellis_protocol::CleanupStatus::Incomplete,
                        })
                })
            } else {
                cached.cleanup
            };
            return Ok(ControlOutcome {
                state: cached.state,
                terminal: cached.terminal,
                cleanup,
                challenge: cached.challenge,
                start_source: false,
                defer_cleanup: cached.defer_cleanup && cleanup.is_none(),
            });
        }
        let outcome = match control {
            LiveControl::Activate(_) => {
                // A new logical activation is only legal from OFFERED; a replay
                // of the same sequence returned the cached outcome above.
                if self.session.phase() != ProviderPhase::Offered {
                    return Err(LiveErrorCode::StaleControl);
                }
                let challenge_id =
                    trellis_protocol::generate_nonce().map_err(|_| LiveErrorCode::ProtocolError)?;
                let last_sent_seq = self.session.highest_sent.load(Ordering::Acquire);
                self.session.set_phase(ProviderPhase::Activating);
                if let Ok(mut telemetry) = self.telemetry.lock() {
                    telemetry.activating();
                }
                let challenge = ChallengeState {
                    challenge_id,
                    last_sent_seq,
                };
                if let Ok(mut slot) = self.session.challenge.lock() {
                    *slot = Some(challenge.clone());
                }
                self.with_deadlines(|deadlines| {
                    deadlines.begin_activating(now, challenge.challenge_id.clone());
                });
                self.deadline_changed();
                ControlOutcome {
                    state: LiveSessionState::Activating,
                    terminal: None,
                    cleanup: None,
                    challenge: Some(challenge),
                    start_source: false,
                    defer_cleanup: false,
                }
            }
            LiveControl::Pulse(pulse) => {
                let activating = matches!(self.session.phase(), ProviderPhase::Activating);
                // Validate every cursor before any state mutation.
                self.session
                    .validate_credit(pulse.received_seq.get(), pulse.consumed_seq.get())?;
                let accepted = self
                    .with_deadlines(|deadlines| {
                        if deadlines.outstanding_challenge() != Some(pulse.challenge_id.as_str()) {
                            return false;
                        }
                        if activating {
                            deadlines.commit_active(now, true);
                        } else {
                            deadlines.fresh_round_trip(now, true);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !accepted {
                    return Err(LiveErrorCode::InvalidChallenge);
                }
                if let Ok(mut challenge) = self.session.challenge.lock() {
                    *challenge = None;
                }
                if activating {
                    self.session.set_phase(ProviderPhase::Active);
                    if let Ok(mut telemetry) = self.telemetry.lock() {
                        telemetry.active();
                    }
                }
                self.apply_credit_and_note(
                    pulse.received_seq.get(),
                    pulse.consumed_seq.get(),
                    now,
                )?;
                self.deadline_changed();
                ControlOutcome {
                    state: LiveSessionState::Active,
                    terminal: None,
                    cleanup: None,
                    challenge: None,
                    start_source: activating && !self.source_started.swap(true, Ordering::AcqRel),
                    defer_cleanup: false,
                }
            }
            LiveControl::Ack(ack) => {
                self.apply_credit_and_note(ack.received_seq.get(), ack.consumed_seq.get(), now)?;
                ControlOutcome {
                    state: current_state(self.session.phase()),
                    terminal: None,
                    cleanup: None,
                    challenge: None,
                    start_source: false,
                    defer_cleanup: false,
                }
            }
            LiveControl::Close(close) => {
                self.session
                    .apply_credit(close.received_seq.get(), close.consumed_seq.get())?;
                self.fence_and_begin_close(now);
                ControlOutcome {
                    state: LiveSessionState::Closed,
                    terminal: Some(terminal_from_end(&self.committed_end())),
                    cleanup: None,
                    challenge: None,
                    start_source: false,
                    defer_cleanup: true,
                }
            }
            LiveControl::EndAck(ack) => {
                self.session
                    .apply_credit(ack.received_seq.get(), ack.consumed_seq.get())?;
                self.fence_and_begin_close(now);
                ControlOutcome {
                    state: LiveSessionState::Closed,
                    terminal: Some(terminal_from_end(&self.committed_end())),
                    cleanup: None,
                    challenge: None,
                    start_source: false,
                    defer_cleanup: true,
                }
            }
        };
        self.store_control_receipt(seq, hash, &outcome);
        Ok(outcome)
    }

    /// Classify one incoming logical control against the committed receipt.
    ///
    /// A same-sequence replay with an identical canonical hash returns the
    /// cached semantic outcome and never mutates source, credit or liveness.
    fn cached_control(&self, seq: u64, hash: &str) -> Result<Option<CachedControl>, LiveErrorCode> {
        let receipt = self
            .control_receipt
            .lock()
            .map_err(|_| LiveErrorCode::ProtocolError)?;
        classify_control(receipt.as_ref(), seq, hash)
    }

    /// Store the semantic outcome of one committed logical control.
    fn store_control_receipt(&self, seq: u64, hash: String, outcome: &ControlOutcome) {
        if let Ok(mut receipt) = self.control_receipt.lock() {
            *receipt = Some(CachedControl {
                seq,
                hash,
                state: outcome.state,
                terminal: outcome.terminal.clone(),
                cleanup: outcome.cleanup,
                challenge: outcome.challenge.clone(),
                defer_cleanup: outcome.defer_cleanup,
            });
        }
    }

    /// Fence once and enter the bounded close exchange.
    fn fence_and_begin_close(&self, now: Instant) {
        self.commit_end(LiveEnd::new(LiveEndReason::Cancelled, None));
        self.begin_close(now);
    }

    /// Run every registered owned cleanup callback and join the owned source
    /// task under one shared grace.
    ///
    /// Cancelling the source token must make a source blocked in `next()`
    /// settle; the record owns the task handle, not merely the token, so this
    /// awaits actual settlement. A task that cannot settle is aborted and the
    /// result is reported incomplete rather than claiming detached work ended.
    pub(crate) async fn run_owned_cleanup(&self) -> CloseCleanupState {
        let callbacks = {
            let Ok(mut cleanup) = self.cleanup.lock() else {
                return CloseCleanupState::Incomplete;
            };
            std::mem::take(&mut *cleanup)
        };
        let mut futures = Vec::new();
        for callback in callbacks {
            futures.push(callback());
        }
        let deadline = tokio::time::Instant::now() + super::provider::cleanup_grace();
        let callbacks_settled =
            tokio::time::timeout_at(deadline, futures_util::future::join_all(futures))
                .await
                .is_ok();
        let source_task = self
            .source_task
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        let source_settled = match source_task {
            Some(mut handle) => {
                if tokio::time::timeout_at(deadline, &mut handle).await.is_ok() {
                    true
                } else {
                    handle.abort();
                    let _ = handle.await;
                    false
                }
            }
            None => true,
        };
        if callbacks_settled && source_settled {
            CloseCleanupState::Complete
        } else {
            CloseCleanupState::Incomplete
        }
    }

    /// Return the cancellation token for this session's source scope.
    #[must_use]
    pub(crate) fn cancellation_handle(&self) -> super::types::LiveCancellation {
        self.cancellation.clone()
    }

    /// Return the first committed terminal outcome, or a protocol error.
    pub(crate) fn committed_end(&self) -> LiveEnd {
        self.terminal
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_else(|| provider_failure(LiveErrorCode::ProtocolError, "session closed"))
    }

    /// Commit one terminal outcome once.
    pub(crate) fn commit_end(&self, end: LiveEnd) {
        if let Ok(mut slot) = self.terminal.lock() {
            if slot.is_none() {
                *slot = Some(end);
            }
        }
    }

    /// Build the receipt tombstone for this session after closure.
    pub(crate) fn tombstone(
        &self,
        cleanup: CloseCleanupState,
        now_ms: u64,
    ) -> super::manager::ClosedReceipt {
        let _permit = self.permit.lock().ok().and_then(|mut slot| slot.take());
        let end = self.committed_end();
        receipt_for(&self.session, &end, cleanup, now_ms)
    }

    /// Authenticate and apply one owner-control message, then reply if safe.
    pub(crate) async fn dispatch_control(
        self: &Arc<Self>,
        nats: &async_nats::Client,
        message: async_nats::Message,
    ) {
        let Some(reply) = message.reply.clone() else {
            return;
        };
        let Some(headers) = message.headers.as_ref() else {
            return;
        };
        if self
            .caller_guard
            .verify_control_request(
                message.subject.as_str(),
                reply.as_str(),
                &message.payload,
                headers,
            )
            .await
            .is_err()
        {
            return;
        }
        let Ok(control) = trellis_protocol::parse_live_control(&message.payload) else {
            return;
        };
        if control.session_id() != self.session.session_id {
            return;
        }
        if message.subject.as_str() != self.session.control_subject {
            return;
        }
        let now = Instant::now();
        let request_id = headers
            .get("request-id")
            .map_or_else(String::new, ToString::to_string);
        match self.handle_control(&control, now).await {
            // A close acknowledgement waits for owned cleanup to settle and is
            // published by the close driver, never on the dispatch path.
            Ok(outcome) if outcome.defer_cleanup => {
                if let Ok(mut pending) = self.pending_close_ack.lock() {
                    *pending = Some(PendingCloseAck {
                        reply: reply.to_string(),
                        request_id,
                        control,
                    });
                }
                self.spawn_close_driver(nats.clone());
            }
            Ok(outcome) => {
                let ack = ack_for(&self.session, &control, &request_id, &outcome);
                let Ok(body) = serde_json::to_vec(&ack) else {
                    return;
                };
                let published = {
                    let _lane = self.output_lane.lock().await;
                    let signed = self.signed_headers(reply.as_str(), &body).await;
                    let Ok(signed) = signed else {
                        return;
                    };
                    nats.publish_with_headers(reply, signed, Bytes::from(body))
                        .await
                        .is_ok()
                };
                // Only a successfully handed-off signed acknowledgement permits
                // the source to start. A failed handoff fences the reservation
                // instead of starting a source on a later retry.
                if !published {
                    self.commit_end(provider_failure(
                        LiveErrorCode::PeerLost,
                        "activation acknowledgement could not be handed off",
                    ));
                    self.begin_close(now);
                    self.spawn_close_driver(nats.clone());
                    return;
                }
                if let Some(challenge) = outcome.challenge.clone() {
                    let _ = publish_challenge(self, nats, &challenge).await;
                }
                if outcome.start_source {
                    self.start_source_once(nats.clone()).await;
                }
            }
            Err(code) => {
                let error = error_for(&self.session, &control, &request_id, code);
                let Ok(body) = serde_json::to_vec(&error) else {
                    return;
                };
                let _lane = self.output_lane.lock().await;
                let signed = self.signed_headers(reply.as_str(), &body).await;
                let Ok(signed) = signed else {
                    return;
                };
                let _ = nats
                    .publish_with_headers(reply, signed, Bytes::from(body))
                    .await;
            }
        }
    }

    /// Start the owned source exactly once after a fresh fence and guard check.
    async fn start_source_once(self: &Arc<Self>, nats: async_nats::Client) {
        if self.finished.load(Ordering::Acquire) || self.session.phase() != ProviderPhase::Active {
            return;
        }
        if let Err(lost) = self.own_guard.reconcile().await {
            self.commit_end(super::manager::authority_end(&lost));
            self.begin_close(Instant::now());
            self.spawn_close_driver(nats);
            return;
        }
        if let Err(lost) = self.caller_guard.reconcile().await {
            self.commit_end(super::manager::authority_end(&lost));
            self.begin_close(Instant::now());
            self.spawn_close_driver(nats);
            return;
        }
        let Ok(source) = take_source(&self.source_factory) else {
            self.commit_end(provider_failure(
                LiveErrorCode::ProtocolError,
                "live source factory is unavailable",
            ));
            self.begin_close(Instant::now());
            self.spawn_close_driver(nats);
            return;
        };
        let driver_record = Arc::clone(self);
        let driver_cancellation = self.cancellation_handle();
        let max_data_body_bytes = self.max_data_body_bytes;
        let handle = tokio::spawn(async move {
            drive_source(
                Arc::clone(&driver_record.session),
                driver_record,
                nats,
                max_data_body_bytes,
                driver_cancellation,
                source,
            )
            .await;
        });
        if let Ok(mut slot) = self.source_task.lock() {
            *slot = Some(handle);
        }
    }

    /// Return the earlier of the session deadline and the guards' expiry.
    #[must_use]
    pub(crate) fn guard_deadline(&self) -> Option<Instant> {
        let now = crate::client::now_iat_seconds() as i64;
        let mut earliest: Option<Instant> = None;
        for guard in [&self.own_guard, self.caller_guard.as_ref()] {
            let expires_at = guard.expires_at_seconds().saturating_sub(now).max(0) as u64;
            let deadline = Instant::now() + std::time::Duration::from_secs(expires_at);
            earliest = Some(earliest.map_or(deadline, |current: Instant| current.min(deadline)));
        }
        earliest
    }

    /// Reconcile both retained guards across a planned rotation, returning the
    /// first terminal authority loss, if any.
    pub(crate) async fn reconcile_authority(&self) -> Option<LiveAuthorityLost> {
        if let Err(lost) = self.own_guard.reconcile().await {
            return Some(lost);
        }
        self.caller_guard.reconcile().await.err()
    }

    /// Spawn the one owned cleanup-then-ack driver for this session.
    pub(crate) fn spawn_close_driver(self: &Arc<Self>, nats: async_nats::Client) {
        if self.cleanup_driver_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let record = Arc::clone(self);
        tokio::spawn(async move {
            let cleanup = record.run_owned_cleanup().await;
            let end = record.finish_closed(Instant::now(), cleanup).await;
            record.publish_pending_close_ack(&nats, cleanup, &end).await;
        });
    }

    /// Publish the latest deferred close acknowledgement, if one is pending.
    async fn publish_pending_close_ack(
        &self,
        nats: &async_nats::Client,
        cleanup: CloseCleanupState,
        end: &LiveEnd,
    ) {
        let Some(pending) = self
            .pending_close_ack
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        else {
            return;
        };
        let status = Some(match cleanup {
            CloseCleanupState::Complete => trellis_protocol::CleanupStatus::Complete,
            _ => trellis_protocol::CleanupStatus::Incomplete,
        });
        let ack = control_ack(
            &self.session,
            &pending.control,
            &pending.request_id,
            LiveSessionState::Closed,
            Some(terminal_from_end(end)),
            status,
        );
        let Ok(body) = serde_json::to_vec(&ack) else {
            return;
        };
        let _lane = self.output_lane.lock().await;
        let signed = self.signed_headers(&pending.reply, &body).await;
        let Ok(signed) = signed else {
            return;
        };
        let _ = nats
            .publish_with_headers(pending.reply, signed, Bytes::from(body))
            .await;
    }

    pub(crate) async fn finish_closed(&self, now: Instant, cleanup: CloseCleanupState) -> LiveEnd {
        if self.finished.swap(true, Ordering::AcqRel) {
            return self.committed_end();
        }
        let now_ms = crate::client::now_iat_seconds() * 1_000;
        if let Ok(mut result) = self.cleanup_result.lock() {
            *result = Some(cleanup);
        }
        // The cached close outcome now reports the settled cleanup state for
        // any same-sequence replay that still reaches this record.
        if let Ok(mut receipt) = self.control_receipt.lock() {
            if let Some(previous) = receipt.as_mut() {
                previous.cleanup = Some(match cleanup {
                    CloseCleanupState::Complete => trellis_protocol::CleanupStatus::Complete,
                    _ => trellis_protocol::CleanupStatus::Incomplete,
                });
            }
        }
        let receipt = self.tombstone(cleanup, now_ms);
        self.with_deadlines(|deadlines| deadlines.closed(now));
        self.deadline_changed();
        if let Ok(manager) = self.manager() {
            manager.insert_receipt(receipt, self.caller_guard.clone());
            manager.remove_provider_session(&self.session.session_id);
        }
        if let Ok(mut telemetry) = self.telemetry.lock() {
            if matches!(cleanup, CloseCleanupState::Incomplete) {
                telemetry.cleanup_exceeded_grace();
            }
            telemetry.end(&self.committed_end());
            telemetry.cleanup_finished();
        }
        self.committed_end()
    }

    /// Lock the deadline owner and apply one transition.
    fn with_deadlines<R>(&self, f: impl FnOnce(&mut LiveDeadlines) -> R) -> Option<R> {
        self.deadlines
            .lock()
            .ok()
            .map(|mut deadlines| f(&mut deadlines))
    }

    /// Wake the session timer after a deadline changed.
    fn deadline_changed(&self) {
        self.deadline_notify.notify_one();
    }

    /// Return the next deadline for the session timer, if any.
    #[must_use]
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.deadlines
            .lock()
            .ok()
            .and_then(|deadlines| deadlines.next_due())
    }

    /// Apply one accepted credit cursor and refresh the stall/liveness clocks.
    fn apply_credit_and_note(
        &self,
        received: u64,
        consumed: u64,
        now: Instant,
    ) -> Result<(), LiveErrorCode> {
        let before = self.session.highest_consumed.load(Ordering::Acquire);
        self.session.apply_credit(received, consumed)?;
        let after = self.session.highest_consumed.load(Ordering::Acquire);
        let advanced = after > before;
        self.with_deadlines(|deadlines| {
            if advanced {
                deadlines.note_stall_reset(now);
            }
            if self.session.outstanding_empty() {
                deadlines.outstanding_cleared();
            }
        });
        self.deadline_changed();
        Ok(())
    }

    /// Enter the bounded close exchange and cancel the owned source scope.
    pub(crate) fn begin_close(&self, now: Instant) {
        self.session.set_phase(ProviderPhase::Closing);
        if let Ok(mut telemetry) = self.telemetry.lock() {
            telemetry.closing();
        }
        self.cancellation.cancel();
        self.with_deadlines(|deadlines| deadlines.begin_closing(now));
        self.deadline_changed();
    }

    /// Record that one application frame became outstanding.
    pub(crate) fn note_data_admitted(&self, now: Instant) {
        self.with_deadlines(|deadlines| deadlines.data_admitted(now));
        self.deadline_changed();
    }

    /// Evaluate and perform one due deadline action.
    ///
    /// Returns `true` when the session finished and its timer should stop. The
    /// driver performs the action and then recomputes the next due event.
    pub(crate) async fn evaluate_deadlines(self: &Arc<Self>, nats: &async_nats::Client) -> bool {
        let now = Instant::now();
        let action = self
            .deadlines
            .lock()
            .ok()
            .and_then(|deadlines| deadlines.evaluate(now));
        match action {
            None => false,
            Some(DeadlineAction::ReservationExpired) => {
                self.commit_end(LiveEnd::new(LiveEndReason::SetupTimeout, None));
                self.begin_close(now);
                self.spawn_close_driver(nats.clone());
                true
            }
            Some(DeadlineAction::ChallengeRetry) => {
                let challenge = self
                    .session
                    .challenge
                    .lock()
                    .ok()
                    .and_then(|challenge| challenge.clone());
                if let Some(challenge) = challenge {
                    let _ = publish_challenge(self, nats, &challenge).await;
                }
                self.with_deadlines(|deadlines| deadlines.rearm_challenge_retry(now));
                self.deadline_changed();
                false
            }
            Some(DeadlineAction::ChallengeDue) => {
                let Ok(challenge_id) = trellis_protocol::generate_nonce() else {
                    self.commit_end(provider_failure(
                        LiveErrorCode::ProtocolError,
                        "challenge nonce unavailable",
                    ));
                    self.begin_close(now);
                    self.spawn_close_driver(nats.clone());
                    return true;
                };
                let last_sent_seq = self.session.highest_sent.load(Ordering::Acquire);
                let challenge = super::provider::ChallengeState {
                    challenge_id: challenge_id.clone(),
                    last_sent_seq,
                };
                if let Ok(mut slot) = self.session.challenge.lock() {
                    *slot = Some(challenge.clone());
                }
                self.with_deadlines(|deadlines| {
                    deadlines.begin_challenge(now, challenge_id.clone())
                });
                self.deadline_changed();
                let _ = publish_challenge(self, nats, &challenge).await;
                false
            }
            Some(DeadlineAction::PeerInactive) => {
                self.commit_end(provider_failure(
                    LiveErrorCode::PeerLost,
                    "live consumer silent past the inactivity bound",
                ));
                self.begin_close(now);
                self.spawn_close_driver(nats.clone());
                true
            }
            Some(DeadlineAction::ConsumerStalled) => {
                self.commit_end(provider_failure(
                    LiveErrorCode::ConsumerSlow,
                    "live consumer did not consume outstanding data",
                ));
                self.begin_close(now);
                self.spawn_close_driver(nats.clone());
                true
            }
            Some(DeadlineAction::CloseExchangeElapsed) => {
                self.session.set_phase(ProviderPhase::Closing);
                self.spawn_close_driver(nats.clone());
                true
            }
            Some(DeadlineAction::CreditDue) => {
                // The provider never schedules consumer credit; clear a stale
                // deadline so the timer cannot spin on a past instant.
                self.with_deadlines(|deadlines| deadlines.credit_sent());
                self.deadline_changed();
                false
            }
            Some(DeadlineAction::CleanupGraceElapsed) => {
                self.with_deadlines(|deadlines| deadlines.mark_cleanup_grace_elapsed());
                self.deadline_changed();
                false
            }
        }
    }
}

/// Outcome of one handled control.
pub(crate) struct ControlOutcome {
    pub state: LiveSessionState,
    pub terminal: Option<WireTerminal>,
    pub cleanup: Option<trellis_protocol::CleanupStatus>,
    /// One challenge to publish on the data subject after the acknowledgement.
    pub challenge: Option<ChallengeState>,
    /// Start the source after a verified first Pulse acknowledgement is sent.
    pub start_source: bool,
    /// The close acknowledgement waits for owned cleanup to settle.
    pub defer_cleanup: bool,
}

fn current_state(phase: ProviderPhase) -> LiveSessionState {
    match phase {
        ProviderPhase::Offered | ProviderPhase::Activating => LiveSessionState::Activating,
        ProviderPhase::Active => LiveSessionState::Active,
        ProviderPhase::Closing | ProviderPhase::Closed => LiveSessionState::Closed,
    }
}

/// Publish one provider data frame and account for its credit cost.
///
/// # Errors
///
/// Returns a wire error when the frame exceeds the window or the negotiated
/// body limit, or when the publication handoff fails.
pub(crate) async fn publish_data_frame(
    record: &ProviderSessionRecord,
    nats: &async_nats::Client,
    value: serde_json::Value,
    max_data_body_bytes: u64,
) -> Result<u64, LiveErrorCode> {
    let session = &record.session;
    // Every signed handoff for this session shares one ordering lane. The
    // watermark advances only after a successful transport handoff.
    let _lane = record.output_lane.lock().await;
    if matches!(
        session.phase(),
        ProviderPhase::Closing | ProviderPhase::Closed
    ) {
        return Err(LiveErrorCode::Closed);
    }
    let seq = session.next_frame_seq();
    let body = serde_json::to_vec(&data_frame(&session.session_id, seq, value))
        .map_err(|_| LiveErrorCode::ProtocolError)?;
    let body_len = body.len() as u64;
    // Admission is checked before anything is signed or published; a full
    // window returns ResourceExhausted without consuming the sequence.
    session.validate_frame_slot(body_len, max_data_body_bytes)?;
    let headers = record.signed_headers(&session.data_subject, &body).await?;
    // The serialized frame is retained payload until the wire handoff.
    if let Ok(telemetry) = record.telemetry.lock() {
        telemetry.buffered(body_len as i64);
    }
    let published = nats
        .publish_with_headers(session.data_subject.clone(), headers, Bytes::from(body))
        .await;
    if let Ok(telemetry) = record.telemetry.lock() {
        telemetry.buffered(-(body_len as i64));
    }
    published.map_err(|_| LiveErrorCode::PeerLost)?;
    session.commit_frame(seq, body_len)?;
    if let Ok(telemetry) = record.telemetry.lock() {
        telemetry.frame(
            super::telemetry::LiveFrameClass::Data,
            super::telemetry::LiveDirection::Send,
        );
    }
    Ok(seq)
}

/// Publish one provider challenge frame.
///
/// # Errors
///
/// Returns a wire error when the publication handoff fails.
pub(crate) async fn publish_challenge(
    record: &ProviderSessionRecord,
    nats: &async_nats::Client,
    challenge: &ChallengeState,
) -> Result<(), LiveErrorCode> {
    let session = &record.session;
    let _lane = record.output_lane.lock().await;
    if session.phase() == ProviderPhase::Closed {
        return Err(LiveErrorCode::Closed);
    }
    // The stored nonce and watermark are immutable across retries, so a retry
    // republishes byte-identical liveness evidence.
    let body = serde_json::to_vec(&challenge_frame(
        &session.session_id,
        &challenge.challenge_id,
        challenge.last_sent_seq,
    ))
    .map_err(|_| LiveErrorCode::ProtocolError)?;
    let headers = record.signed_headers(&session.data_subject, &body).await?;
    nats.publish_with_headers(session.data_subject.clone(), headers, Bytes::from(body))
        .await
        .map_err(|_| LiveErrorCode::PeerLost)?;
    if let Ok(telemetry) = record.telemetry.lock() {
        telemetry.frame(
            super::telemetry::LiveFrameClass::Control,
            super::telemetry::LiveDirection::Send,
        );
    }
    Ok(())
}

/// Publish one provider terminal frame.
///
/// # Errors
///
/// Returns a wire error when the publication handoff fails.
/// Bounded attempts for the one logical END handoff per session.
const END_PUBLISH_ATTEMPTS: usize = 3;

pub(crate) async fn publish_end(
    record: &ProviderSessionRecord,
    nats: &async_nats::Client,
    terminal: WireTerminal,
) -> Result<(), LiveErrorCode> {
    let session = &record.session;
    let _lane = record.output_lane.lock().await;
    // Exactly one terminal frame per session: a later source/close path cannot
    // publish a second END with a different cause. The claim is one-shot, but
    // the single handoff is retried within a bounded budget so a transient
    // publication failure does not lose the terminal frame.
    if record.end_sent.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    if session.phase() == ProviderPhase::Closed {
        return Err(LiveErrorCode::Closed);
    }
    let final_seq = session.highest_sent.load(Ordering::Acquire);
    let body = serde_json::to_vec(&end_frame(&session.session_id, final_seq, terminal))
        .map_err(|_| LiveErrorCode::ProtocolError)?;
    let headers = record.signed_headers(&session.data_subject, &body).await?;
    let mut last_error = None;
    for attempt in 0..END_PUBLISH_ATTEMPTS {
        match nats
            .publish_with_headers(
                session.data_subject.clone(),
                headers.clone(),
                Bytes::from(body.clone()),
            )
            .await
        {
            Ok(()) => {
                if let Ok(telemetry) = record.telemetry.lock() {
                    telemetry.frame(
                        super::telemetry::LiveFrameClass::Control,
                        super::telemetry::LiveDirection::Send,
                    );
                }
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < END_PUBLISH_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }
    let _ = last_error;
    Err(LiveErrorCode::PeerLost)
}

/// Build one acknowledgement for a handled control.
#[must_use]
pub(crate) fn ack_for(
    session: &ProviderSession,
    control: &LiveControl,
    request_id: &str,
    outcome: &ControlOutcome,
) -> LiveControlAck {
    control_ack(
        session,
        control,
        request_id,
        outcome.state,
        outcome.terminal.clone(),
        outcome.cleanup,
    )
}

/// Build one typed control error body.
#[must_use]
pub(crate) fn error_for(
    session: &ProviderSession,
    control: &LiveControl,
    request_id: &str,
    code: LiveErrorCode,
) -> trellis_protocol::LiveControlError {
    control_error(session, control, request_id, code)
}

/// Commit one source failure as this session's terminal outcome.
pub(crate) fn source_failure_end(message: &str) -> LiveEnd {
    LiveEnd::new(
        LiveEndReason::SourceError,
        Some(Arc::new(LiveStreamError::new(
            LiveErrorCode::SourceFailed,
            bounded_message(message),
        ))),
    )
}

/// Drain a provider source until it ends or the session closes.
///
/// The driver polls the source serially: it never starts a second poll while a
/// publication is in flight, so one staged frame per session bounds memory.
pub(crate) async fn drive_source<S>(
    session: Arc<ProviderSession>,
    record: Arc<ProviderSessionRecord>,
    nats: async_nats::Client,
    max_data_body_bytes: u64,
    cancellation: super::types::LiveCancellation,
    mut source: S,
) where
    S: Stream<Item = Result<SourceItem, String>> + Unpin,
{
    loop {
        if !matches!(session.phase(), ProviderPhase::Active) {
            // The one termination owner (close, deadline or authority fence)
            // owns the terminal/cleanup path.
            return;
        }
        let item = tokio::select! {
            // Cancellation stops reading the source immediately; the stream is
            // dropped with this driver, releasing any RAII-owned upstream work.
            _ = cancellation.cancelled() => {
                record.begin_close(Instant::now());
                return;
            }
            item = source.next() => item,
        };
        match item {
            None | Some(Ok(SourceItem::End)) => {
                let terminal = terminal_from_end(&LiveEnd::complete());
                let _ = publish_end(&record, &nats, terminal).await;
                record.commit_end(LiveEnd::complete());
                record.begin_close(Instant::now());
                record.spawn_close_driver(nats.clone());
                return;
            }
            Some(Err(message)) => {
                let end = source_failure_end(&message);
                let terminal = terminal_from_end(&end);
                let _ = publish_end(&record, &nats, terminal).await;
                record.commit_end(end);
                record.begin_close(Instant::now());
                record.spawn_close_driver(nats.clone());
                return;
            }
            Some(Ok(SourceItem::Value(value))) => {
                // Wait for credit without blocking control handling: the owner
                // control path runs on its own task and the output lane is
                // released between attempts.
                loop {
                    if !matches!(session.phase(), ProviderPhase::Active) {
                        return;
                    }
                    match publish_data_frame(&record, &nats, value.clone(), max_data_body_bytes)
                        .await
                    {
                        Ok(_) => {
                            record.note_data_admitted(Instant::now());
                            break;
                        }
                        Err(LiveErrorCode::ResourceExhausted) => {
                            tokio::select! {
                                _ = cancellation.cancelled() => {
                                    record.begin_close(Instant::now());
                                    return;
                                }
                                _ = session.credit.notified() => {}
                            }
                        }
                        Err(code) => {
                            record.commit_end(provider_failure(code, "live publication failed"));
                            record.begin_close(Instant::now());
                            record.spawn_close_driver(nats.clone());
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Invoke the source factory exactly once for an activated session.
///
/// # Errors
///
/// Returns a wire error when no factory was registered.
pub(crate) fn take_source(
    factory: &std::sync::Mutex<Option<ProviderSourceFactory>>,
) -> Result<ProviderSource, LiveErrorCode> {
    factory
        .lock()
        .map_err(|_| LiveErrorCode::ProtocolError)?
        .take()
        .map(|factory| factory())
        .ok_or(LiveErrorCode::ProtocolError)
}

/// Commit one authority loss as a provider terminal outcome.
pub(crate) fn authority_terminal_end(lost: &LiveAuthorityLost) -> LiveEnd {
    super::manager::authority_end(lost)
}

/// Classify one incoming logical control against the committed receipt.
///
/// `Ok(None)` means the control is the legitimate next logical control and
/// must be validated and applied; `Ok(Some(_))` is an identical replay whose
/// cached semantic outcome is returned without mutating source, credit or
/// liveness.
pub(crate) fn classify_control(
    receipt: Option<&CachedControl>,
    seq: u64,
    hash: &str,
) -> Result<Option<CachedControl>, LiveErrorCode> {
    match receipt {
        None => {
            if seq == 1 {
                Ok(None)
            } else {
                Err(LiveErrorCode::ControlGap)
            }
        }
        Some(previous) if seq == previous.seq => {
            if previous.hash == hash {
                Ok(Some(previous.clone()))
            } else {
                Err(LiveErrorCode::ControlConflict)
            }
        }
        Some(previous) if seq < previous.seq => Err(LiveErrorCode::StaleControl),
        Some(previous) if seq == previous.seq + 1 => Ok(None),
        Some(_) => Err(LiveErrorCode::ControlGap),
    }
}

#[cfg(test)]
mod control_receipt_tests {
    use super::*;

    fn cached(seq: u64, hash: &str) -> CachedControl {
        CachedControl {
            seq,
            hash: hash.to_owned(),
            state: trellis_protocol::LiveSessionState::Active,
            terminal: None,
            cleanup: None,
            challenge: None,
            defer_cleanup: false,
        }
    }

    #[test]
    fn the_first_and_next_logical_controls_are_admitted() {
        assert!(matches!(classify_control(None, 1, "h1"), Ok(None)));
        let receipt = cached(1, "h1");
        assert!(matches!(
            classify_control(Some(&receipt), 2, "h2"),
            Ok(None)
        ));
    }

    #[test]
    fn replayed_stale_gap_and_conflicting_controls_are_classified() {
        let receipt = cached(2, "h2");
        assert!(matches!(
            classify_control(Some(&receipt), 2, "h2"),
            Ok(Some(_))
        ));
        assert!(matches!(
            classify_control(Some(&receipt), 1, "h1"),
            Err(LiveErrorCode::StaleControl)
        ));
        assert!(matches!(
            classify_control(Some(&receipt), 4, "h4"),
            Err(LiveErrorCode::ControlGap)
        ));
        assert!(matches!(
            classify_control(Some(&receipt), 2, "different"),
            Err(LiveErrorCode::ControlConflict)
        ));
        assert!(matches!(
            classify_control(None, 2, "h2"),
            Err(LiveErrorCode::ControlGap)
        ));
    }
}
