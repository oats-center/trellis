//! Provider-side live session: prepared source, serialized publication, credit,
//! challenge liveness and owned cleanup.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use trellis_protocol::{
    LiveControl, LiveControlAck, LiveControlAckAction, LiveControlError, LiveDataFrame,
    LiveEndFrame, LiveEndReason, LiveErrorCode, LiveOfferKind, LiveSessionKind, LiveSessionState,
    U64s, WireTerminal, CLEANUP_GRACE_MS, WINDOW_BYTES, WINDOW_FRAMES,
};

use super::authority::{LiveAuthorityLost, PinnedPeerIdentity};
use super::manager::ClosedReceipt;
use super::types::{
    CloseCleanupState, CloseRemoteState, LiveCloseReceipt, LiveEnd, LiveStreamError,
};

/// Provider session phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderPhase {
    Offered,
    Activating,
    Active,
    Closing,
    Closed,
}

/// One application frame staged for serialized publication.
pub(crate) struct StagedFrame {
    pub encoded: Bytes,
    pub len: u64,
}

/// One outstanding sent frame's credit cost.
pub(crate) struct OutstandingFrame {
    pub seq: u64,
    pub len: u64,
}

/// Provider-side session state, owned by one manager.
pub(crate) struct ProviderSession {
    pub session_id: String,
    pub open_id: String,
    pub kind: LiveSessionKind,
    pub base_subject: String,
    pub data_subject: String,
    pub control_subject: String,
    pub consumer: PinnedPeerIdentity,
    pub phase: Mutex<ProviderPhase>,
    /// The single outstanding challenge's nonce and exact `lastSentSeq`.
    ///
    /// Timing (retry/heartbeat/peer deadlines) lives in [`super::deadlines`];
    /// this holds only the values needed to re-publish the same challenge.
    pub challenge: Mutex<Option<ChallengeState>>,
    pub highest_sent: AtomicU64,
    pub highest_received: AtomicU64,
    pub highest_consumed: AtomicU64,
    pub outstanding: Mutex<VecDeque<OutstandingFrame>>,
    pub outstanding_bytes: AtomicU64,
    pub cleanup_complete: AtomicBool,
    /// One staged producer application frame; a second concurrent emit is rejected.
    pub staged: Mutex<Option<StagedFrame>>,
    pub credit: tokio::sync::Notify,
}

/// Identity fields of one offered provider reservation.
pub(crate) struct ProviderReservationIdentity {
    pub session_id: String,
    pub open_id: String,
    pub kind: LiveSessionKind,
}

/// Exact subjects derived for one provider session.
pub(crate) struct ProviderSessionSubjects {
    pub base_subject: String,
    pub data_subject: String,
    pub control_subject: String,
}

/// One outstanding delivery-path challenge.
///
/// Timing is owned by [`super::deadlines`]; only the identifying nonce and the
/// exact `lastSentSeq` captured when the challenge was generated live here, so a
/// retry re-publishes byte-identical liveness evidence.
#[derive(Clone, Debug)]
pub(crate) struct ChallengeState {
    pub challenge_id: String,
    pub last_sent_seq: u64,
}

impl ProviderSession {
    /// Create one offered reservation with an absolute monotonic deadline.
    #[must_use]
    pub(crate) fn new(
        reservation: ProviderReservationIdentity,
        subjects: ProviderSessionSubjects,
        consumer: PinnedPeerIdentity,
        _now_ms: u64,
    ) -> Self {
        let ProviderReservationIdentity {
            session_id,
            open_id,
            kind,
        } = reservation;
        let ProviderSessionSubjects {
            base_subject,
            data_subject,
            control_subject,
        } = subjects;
        Self {
            session_id,
            open_id,
            kind,
            base_subject,
            data_subject,
            control_subject,
            consumer,
            phase: Mutex::new(ProviderPhase::Offered),
            challenge: Mutex::new(None),
            highest_sent: AtomicU64::new(0),
            highest_received: AtomicU64::new(0),
            highest_consumed: AtomicU64::new(0),
            outstanding: Mutex::new(VecDeque::new()),
            outstanding_bytes: AtomicU64::new(0),
            cleanup_complete: AtomicBool::new(true),
            staged: Mutex::new(None),
            credit: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn phase(&self) -> ProviderPhase {
        self.phase
            .lock()
            .map_or(ProviderPhase::Closed, |phase| *phase)
    }

    pub(crate) fn set_phase(&self, phase: ProviderPhase) {
        if let Ok(mut current) = self.phase.lock() {
            *current = phase;
        }
    }

    /// Return whether the credit window has no outstanding frames.
    #[must_use]
    pub(crate) fn outstanding_empty(&self) -> bool {
        self.outstanding
            .lock()
            .is_ok_and(|outstanding| outstanding.is_empty())
    }

    pub(crate) fn wake_credit(&self) {
        self.credit.notify_waiters();
    }

    /// Return the next uncommitted DATA sequence.
    ///
    /// The caller holds the session output lane across frame construction and
    /// handoff, so the returned sequence cannot race another publication. The
    /// watermark only advances in [`Self::commit_frame`] after a successful
    /// handoff, so a challenge cannot advertise an unsent frame.
    #[must_use]
    pub(crate) fn next_frame_seq(&self) -> u64 {
        self.highest_sent.load(Ordering::Acquire) + 1
    }

    /// Validate one application frame against the exact credit window without
    /// consuming a sequence or publishing anything.
    ///
    /// # Errors
    ///
    /// Returns [`LiveErrorCode::PayloadTooLarge`] for an oversize frame and
    /// [`LiveErrorCode::ResourceExhausted`] when the window is full.
    pub(crate) fn validate_frame_slot(
        &self,
        len: u64,
        max_data_body_bytes: u64,
    ) -> Result<(), LiveErrorCode> {
        if len > max_data_body_bytes {
            return Err(LiveErrorCode::PayloadTooLarge);
        }
        let outstanding_frames = self
            .outstanding
            .lock()
            .map_err(|_| LiveErrorCode::ResourceExhausted)?
            .len() as u64;
        if outstanding_frames + 1 > WINDOW_FRAMES {
            return Err(LiveErrorCode::ResourceExhausted);
        }
        if self.outstanding_bytes.load(Ordering::Acquire) + len > WINDOW_BYTES {
            return Err(LiveErrorCode::ResourceExhausted);
        }
        Ok(())
    }

    /// Commit one successfully handed-off application frame.
    ///
    /// The watermark advances only here, after the handoff succeeded; a
    /// challenge or END can therefore never advertise an unsent frame.
    ///
    /// # Errors
    ///
    /// Returns [`LiveErrorCode::ResourceExhausted`] when the ledger is
    /// unavailable. Bounds were validated by [`Self::validate_frame_slot`].
    pub(crate) fn commit_frame(&self, seq: u64, len: u64) -> Result<(), LiveErrorCode> {
        let mut outstanding = self
            .outstanding
            .lock()
            .map_err(|_| LiveErrorCode::ResourceExhausted)?;
        outstanding.push_back(OutstandingFrame { seq, len });
        drop(outstanding);
        self.outstanding_bytes.fetch_add(len, Ordering::AcqRel);
        self.highest_sent.store(seq, Ordering::Release);
        Ok(())
    }

    /// Validate one reported cursor pair before any state mutation.
    ///
    /// # Errors
    ///
    /// Returns [`LiveErrorCode::InvalidCursor`] for impossible or regressing
    /// cursors reported by the authenticated owner.
    pub(crate) fn validate_credit(
        &self,
        received: u64,
        consumed: u64,
    ) -> Result<(), LiveErrorCode> {
        let highest_sent = self.highest_sent.load(Ordering::Acquire);
        let current_received = self.highest_received.load(Ordering::Acquire);
        let current_consumed = self.highest_consumed.load(Ordering::Acquire);
        if consumed > received || received > highest_sent {
            return Err(LiveErrorCode::InvalidCursor);
        }
        if received < current_received || consumed < current_consumed {
            return Err(LiveErrorCode::InvalidCursor);
        }
        Ok(())
    }

    /// Apply one accepted credit cursor from the authenticated owner.
    ///
    /// # Errors
    ///
    /// Returns [`LiveErrorCode::InvalidCursor`] for impossible or regressing
    /// cursors reported by the authenticated owner.
    pub(crate) fn apply_credit(&self, received: u64, consumed: u64) -> Result<(), LiveErrorCode> {
        self.validate_credit(received, consumed)?;
        self.highest_received.store(received, Ordering::Release);
        self.highest_consumed.store(consumed, Ordering::Release);
        let mut outstanding = self
            .outstanding
            .lock()
            .map_err(|_| LiveErrorCode::InvalidCursor)?;
        while let Some(front) = outstanding.front() {
            if front.seq <= consumed {
                let len = outstanding.pop_front().map(|frame| frame.len).unwrap_or(0);
                self.outstanding_bytes.fetch_sub(len, Ordering::AcqRel);
            } else {
                break;
            }
        }
        drop(outstanding);
        self.wake_credit();
        Ok(())
    }
}

/// Map one provider session into a closed receipt.
#[must_use]
pub(crate) fn receipt_for(
    session: &ProviderSession,
    end: &LiveEnd,
    cleanup: CloseCleanupState,
    now_ms: u64,
) -> ClosedReceipt {
    ClosedReceipt {
        session_id: session.session_id.clone(),
        owner_token: session.consumer.connection_id.clone(),
        base_subject: session.base_subject.clone(),
        reason: end.reason(),
        error_code: end.error().map(|error| error.code()),
        cleanup,
        final_seq: session.highest_sent.load(Ordering::Acquire),
        expires_at_ms: super::manager::receipt_expiry(now_ms),
    }
}

/// Build one signed-offer body for an accepted reservation.
#[must_use]
pub(crate) fn offer_ack_kind() -> LiveOfferKind {
    LiveOfferKind::Offer
}

/// Build a control acknowledgement for one processed control.
#[must_use]
pub(crate) fn control_ack(
    session: &ProviderSession,
    control: &LiveControl,
    request_id: &str,
    state: LiveSessionState,
    terminal: Option<WireTerminal>,
    cleanup: Option<trellis_protocol::CleanupStatus>,
) -> LiveControlAck {
    LiveControlAck {
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: LiveOfferKind::ControlAck,
        session_id: session.session_id.clone(),
        control_seq: control.control_seq(),
        request_id: request_id.to_owned(),
        action: match control {
            LiveControl::Activate(_) => LiveControlAckAction::Activate,
            LiveControl::Pulse(_) => LiveControlAckAction::Pulse,
            LiveControl::Ack(_) => LiveControlAckAction::Ack,
            LiveControl::Close(_) => LiveControlAckAction::Close,
            LiveControl::EndAck(_) => LiveControlAckAction::EndAck,
        },
        state,
        accepted_received_seq: U64s::new(session.highest_received.load(Ordering::Acquire)),
        accepted_consumed_seq: U64s::new(session.highest_consumed.load(Ordering::Acquire)),
        terminal,
        cleanup,
    }
}

/// Build one bounded control error for an authenticated owner violation.
#[must_use]
pub(crate) fn control_error(
    session: &ProviderSession,
    control: &LiveControl,
    request_id: &str,
    code: LiveErrorCode,
) -> LiveControlError {
    LiveControlError {
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: LiveOfferKind::ControlError,
        session_id: session.session_id.clone(),
        control_seq: control.control_seq(),
        request_id: request_id.to_owned(),
        code,
    }
}

/// Build one data frame body for a staged application value.
#[must_use]
pub(crate) fn data_frame(session_id: &str, seq: u64, value: serde_json::Value) -> LiveDataFrame {
    LiveDataFrame {
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: LiveOfferKind::Data,
        session_id: session_id.to_owned(),
        seq: U64s::new(seq),
        value,
    }
}

/// Build one challenge frame body.
#[must_use]
pub(crate) fn challenge_frame(
    session_id: &str,
    challenge_id: &str,
    last_sent_seq: u64,
) -> trellis_protocol::LiveChallengeFrame {
    trellis_protocol::LiveChallengeFrame {
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: LiveOfferKind::Challenge,
        session_id: session_id.to_owned(),
        challenge_id: challenge_id.to_owned(),
        last_sent_seq: U64s::new(last_sent_seq),
    }
}

/// Build one terminal frame body.
#[must_use]
pub(crate) fn end_frame(session_id: &str, final_seq: u64, terminal: WireTerminal) -> LiveEndFrame {
    LiveEndFrame {
        format: trellis_protocol::LIVE_VERSION.to_owned(),
        kind: LiveOfferKind::End,
        session_id: session_id.to_owned(),
        final_seq: U64s::new(final_seq),
        terminal,
    }
}

/// Build one source-failure terminal record.
#[must_use]
pub(crate) fn source_terminal(message: impl Into<String>) -> WireTerminal {
    WireTerminal {
        reason: LiveEndReason::SourceError,
        error: Some(trellis_protocol::WireTerminalError {
            code: LiveErrorCode::SourceFailed,
            message: bounded_message(message),
            trace_id: None,
        }),
    }
}

/// Build one authority-loss terminal record.
#[must_use]
pub(crate) fn authority_terminal(lost: &LiveAuthorityLost) -> WireTerminal {
    let end = super::manager::authority_end(lost);
    terminal_from_end(&end)
}

/// Convert one committed local outcome into a bounded wire terminal.
#[must_use]
pub(crate) fn terminal_from_end(end: &LiveEnd) -> WireTerminal {
    match end.error() {
        None => WireTerminal {
            reason: end.reason(),
            error: None,
        },
        Some(error) => WireTerminal {
            reason: end.reason(),
            error: Some(trellis_protocol::WireTerminalError {
                code: error.code(),
                message: bounded_message(error.message()),
                trace_id: error.trace_id().map(str::to_owned),
            }),
        },
    }
}

/// Build one close receipt from a provider session after owned cleanup.
#[must_use]
pub(crate) fn close_receipt_for(end: &LiveEnd, cleanup: CloseCleanupState) -> LiveCloseReceipt {
    let remote = if cleanup == CloseCleanupState::Unknown {
        CloseRemoteState::Unconfirmed
    } else {
        CloseRemoteState::Confirmed
    };
    LiveCloseReceipt::new(end.clone(), remote, cleanup)
}

/// Return the shared cleanup grace as a duration.
#[must_use]
pub(crate) fn cleanup_grace() -> std::time::Duration {
    std::time::Duration::from_millis(CLEANUP_GRACE_MS)
}

/// Truncate one diagnostic to the wire message bounds.
#[must_use]
pub(crate) fn bounded_message(message: impl Into<String>) -> String {
    let message = message.into();
    let scalars = message.chars().count();
    if scalars <= 512 && message.len() <= 2_048 {
        return message;
    }
    message.chars().take(512).collect()
}

/// Build one sanitized provider stream error.
#[must_use]
pub(crate) fn provider_failure(code: LiveErrorCode, message: impl Into<String>) -> LiveEnd {
    LiveEnd::new(
        super::types::end_reason_for_code(code),
        Some(Arc::new(LiveStreamError::new(code, message))),
    )
}
