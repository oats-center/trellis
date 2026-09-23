//! Monotonic deadline owner shared by the live provider and consumer drivers.
//!
//! This is the single source of truth for live-session timing policy. It is a
//! pure reducer: every transition takes an explicit [`Instant`], so the same
//! production state machine runs under a real clock or a paused test clock.
//! Durations originate in the shared protocol constants; no second timing
//! policy or per-live timeout knob exists here.
//!
//! There is no total ACTIVE age limit. The reservation deadline runs only from
//! allocation to activation. An ACTIVE session uses independent peer,
//! consumption and (provider) challenge deadlines; a healthy idle session never
//! expires.

use std::time::Duration;

use tokio::time::Instant;

use trellis_protocol::{
    ACK_FRAME_THRESHOLD, ACK_MAX_DELAY_MS, CHALLENGE_RETRY_MS, CLEANUP_GRACE_MS, CLOSE_EXCHANGE_MS,
    CONSUMER_STALL_MS, HEARTBEAT_INTERVAL_MS, OPEN_RESERVATION_MS, PEER_INACTIVITY_MS,
    TOMBSTONE_MS,
};

/// Live-session phase tracked by the deadline owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveDeadlinePhase {
    /// Provider reservation created; no source and no activation yet.
    Offered,
    /// Consumer prepared handle; no source and no activation yet.
    Prepared,
    /// Activation round trip in progress.
    Activating,
    /// Activation committed; independent peer/consumption/challenge deadlines.
    Active,
    /// Normal remote end verified; bounded queue still draining locally.
    Draining,
    /// Explicit close in progress.
    Closing,
    /// Terminal; only the bounded receipt tombstone remains.
    Closed,
}

/// Typed action returned by [`LiveDeadlines::evaluate`].
///
/// The driver performs and fences the action; this owner never touches the
/// network, authority or application state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeadlineAction {
    /// The opening reservation elapsed before activation committed.
    ReservationExpired,
    /// One new delivery-path challenge is due.
    ChallengeDue,
    /// The outstanding challenge should be re-published unchanged.
    ChallengeRetry,
    /// No fresh round trip arrived within the peer-inactivity bound.
    PeerInactive,
    /// Application data stayed unconsumed past the consumption-stall bound.
    ConsumerStalled,
    /// Accumulated consumption credit should be sent.
    CreditDue,
    /// The bounded close-exchange budget elapsed.
    CloseExchangeElapsed,
    /// The owned-cleanup grace elapsed.
    CleanupGraceElapsed,
}

/// Production live-session deadline state.
#[derive(Debug)]
pub(crate) struct LiveDeadlines {
    phase: LiveDeadlinePhase,
    reservation_until: Option<Instant>,
    last_fresh_round_trip_at: Option<Instant>,
    next_challenge_at: Option<Instant>,
    challenge_retry_at: Option<Instant>,
    outstanding_challenge: Option<String>,
    unconsumed_since: Option<Instant>,
    last_consumption_at: Option<Instant>,
    next_credit_at: Option<Instant>,
    close_until: Option<Instant>,
    cleanup_until: Option<Instant>,
    tombstone_until: Option<Instant>,
    generation: u64,
}

impl LiveDeadlines {
    /// Create one offered provider reservation with an absolute deadline.
    #[must_use]
    pub(crate) fn reserved(now: Instant) -> Self {
        Self::with_phase(now, LiveDeadlinePhase::Offered)
    }

    /// Create one prepared consumer handle with an absolute deadline.
    #[must_use]
    pub(crate) fn prepared(now: Instant) -> Self {
        Self::with_phase(now, LiveDeadlinePhase::Prepared)
    }

    /// Create one prepared consumer handle from an already-started reservation.
    ///
    /// The consumer's opening budget begins before the opening exchange, so the
    /// pump installs the same absolute deadline rather than restarting it.
    #[must_use]
    pub(crate) fn prepared_until(deadline: Instant) -> Self {
        let mut deadlines = Self::with_phase(deadline, LiveDeadlinePhase::Prepared);
        deadlines.reservation_until = Some(deadline);
        deadlines
    }

    fn with_phase(now: Instant, phase: LiveDeadlinePhase) -> Self {
        Self {
            phase,
            reservation_until: Some(now + Duration::from_millis(OPEN_RESERVATION_MS)),
            last_fresh_round_trip_at: None,
            next_challenge_at: None,
            challenge_retry_at: None,
            outstanding_challenge: None,
            unconsumed_since: None,
            last_consumption_at: None,
            next_credit_at: None,
            close_until: None,
            cleanup_until: None,
            tombstone_until: None,
            generation: 0,
        }
    }

    /// Return the current phase.
    #[must_use]
    pub(crate) fn phase(&self) -> LiveDeadlinePhase {
        self.phase
    }

    /// Return the current fencing generation.
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Return the outstanding challenge nonce, when one awaits a response.
    #[must_use]
    pub(crate) fn outstanding_challenge(&self) -> Option<&str> {
        self.outstanding_challenge.as_deref()
    }

    /// Enter ACTIVATING and issue one challenge to retry until answered.
    ///
    /// The original reservation deadline is retained; activating does not
    /// extend it.
    pub(crate) fn begin_activating(&mut self, now: Instant, challenge_id: String) {
        self.phase = LiveDeadlinePhase::Activating;
        self.outstanding_challenge = Some(challenge_id);
        self.challenge_retry_at = Some(now + Duration::from_millis(CHALLENGE_RETRY_MS));
    }

    /// Commit ACTIVE on the first valid fresh round trip.
    ///
    /// Clears the reservation deadline permanently and starts the independent
    /// peer/consumption deadlines. The provider schedules its next challenge
    /// one heartbeat interval later.
    pub(crate) fn commit_active(&mut self, now: Instant, provider: bool) {
        self.phase = LiveDeadlinePhase::Active;
        self.reservation_until = None;
        self.last_fresh_round_trip_at = Some(now);
        self.outstanding_challenge = None;
        self.challenge_retry_at = None;
        self.next_challenge_at =
            provider.then(|| now + Duration::from_millis(HEARTBEAT_INTERVAL_MS));
    }

    /// Record one fresh challenge answer / matching acknowledgement.
    ///
    /// Moves the fresh-round-trip time once, clears the outstanding challenge
    /// and schedules the next provider challenge from this time. A duplicate
    /// or replayed answer must not call this again.
    pub(crate) fn fresh_round_trip(&mut self, now: Instant, provider: bool) {
        self.last_fresh_round_trip_at = Some(now);
        self.outstanding_challenge = None;
        self.challenge_retry_at = None;
        if provider {
            self.next_challenge_at = Some(now + Duration::from_millis(HEARTBEAT_INTERVAL_MS));
        }
    }

    /// Install one new challenge nonce and arm its retry.
    pub(crate) fn begin_challenge(&mut self, now: Instant, challenge_id: String) {
        self.outstanding_challenge = Some(challenge_id);
        self.challenge_retry_at = Some(now + Duration::from_millis(CHALLENGE_RETRY_MS));
        self.next_challenge_at = None;
    }

    /// Re-arm the retry for the same outstanding challenge.
    pub(crate) fn rearm_challenge_retry(&mut self, now: Instant) {
        if self.outstanding_challenge.is_some() {
            self.challenge_retry_at = Some(now + Duration::from_millis(CHALLENGE_RETRY_MS));
        }
    }

    /// Record that application data became outstanding after an empty period.
    pub(crate) fn data_admitted(&mut self, now: Instant) {
        if self.unconsumed_since.is_none() {
            self.unconsumed_since = Some(now);
        }
    }

    /// Record real consumption progress.
    pub(crate) fn note_consumption(&mut self, now: Instant, newly_consumed: u64) {
        self.last_consumption_at = Some(now);
        if newly_consumed >= ACK_FRAME_THRESHOLD {
            self.next_credit_at = Some(now);
        } else if self.next_credit_at.is_none() {
            self.next_credit_at = Some(now + Duration::from_millis(ACK_MAX_DELAY_MS));
        }
    }

    /// Reset the consumption-stall clock without scheduling consumer credit.
    ///
    /// Used by the provider, which applies credit but never sends consumption
    /// acknowledgements itself.
    pub(crate) fn note_stall_reset(&mut self, now: Instant) {
        self.last_consumption_at = Some(now);
    }

    /// Consume the cleanup-grace deadline once it fired.
    pub(crate) fn mark_cleanup_grace_elapsed(&mut self) {
        self.cleanup_until = None;
    }

    /// Disable the consumption deadline once nothing is outstanding.
    pub(crate) fn outstanding_cleared(&mut self) {
        self.unconsumed_since = None;
    }

    /// Record that accumulated credit was handed off.
    pub(crate) fn credit_sent(&mut self) {
        self.next_credit_at = None;
    }

    /// Enter DRAINING after a verified normal end with queued items.
    ///
    /// Reservation, peer and challenge deadlines are disabled; the local
    /// consumption deadline and own-authority checks remain.
    pub(crate) fn begin_draining(&mut self) {
        self.phase = LiveDeadlinePhase::Draining;
        self.reservation_until = None;
        self.next_challenge_at = None;
        self.challenge_retry_at = None;
        self.outstanding_challenge = None;
    }

    /// Enter CLOSING with one absolute close exchange and one cleanup grace.
    pub(crate) fn begin_closing(&mut self, now: Instant) {
        self.phase = LiveDeadlinePhase::Closing;
        self.next_credit_at = None;
        self.next_challenge_at = None;
        self.challenge_retry_at = None;
        self.close_until = Some(now + Duration::from_millis(CLOSE_EXCHANGE_MS));
        self.cleanup_until = Some(now + Duration::from_millis(CLEANUP_GRACE_MS));
    }

    /// Enter CLOSED, bump the generation and retain only the receipt window.
    ///
    /// The generation fence discards callbacks queued for an earlier life.
    pub(crate) fn closed(&mut self, now: Instant) {
        self.phase = LiveDeadlinePhase::Closed;
        self.reservation_until = None;
        self.last_fresh_round_trip_at = None;
        self.next_challenge_at = None;
        self.challenge_retry_at = None;
        self.outstanding_challenge = None;
        self.unconsumed_since = None;
        self.next_credit_at = None;
        self.close_until = None;
        self.cleanup_until = None;
        self.tombstone_until = Some(now + Duration::from_millis(TOMBSTONE_MS));
        self.generation = self.generation.wrapping_add(1);
    }

    /// Return whether the bounded receipt has expired.
    #[must_use]
    pub(crate) fn tombstone_expired(&self, now: Instant) -> bool {
        self.tombstone_until.is_some_and(|until| now >= until)
    }

    /// Return the earliest pending deadline, if any.
    #[must_use]
    pub(crate) fn next_due(&self) -> Option<Instant> {
        let mut due: Option<Instant> = None;
        let mut consider = |candidate: Option<Instant>| {
            if let Some(candidate) = candidate {
                due = Some(due.map_or(candidate, |current| current.min(candidate)));
            }
        };
        match self.phase {
            LiveDeadlinePhase::Offered
            | LiveDeadlinePhase::Prepared
            | LiveDeadlinePhase::Activating => {
                consider(self.reservation_until);
                consider(self.challenge_retry_at);
            }
            LiveDeadlinePhase::Active => {
                consider(self.challenge_retry_at);
                consider(self.next_challenge_at);
                consider(self.peer_deadline());
                consider(self.consumption_deadline());
                consider(self.next_credit_at);
            }
            LiveDeadlinePhase::Draining => consider(self.consumption_deadline()),
            LiveDeadlinePhase::Closing => {
                consider(self.close_until);
                consider(self.cleanup_until);
            }
            LiveDeadlinePhase::Closed => {}
        }
        due
    }

    /// Evaluate the next due action at `now`.
    ///
    /// Simultaneous peer-inactivity and consumption-stall expiry resolve as
    /// peer expiry first, then consumer stall; exactly one cause is returned.
    #[must_use]
    pub(crate) fn evaluate(&self, now: Instant) -> Option<DeadlineAction> {
        match self.phase {
            LiveDeadlinePhase::Offered
            | LiveDeadlinePhase::Prepared
            | LiveDeadlinePhase::Activating => {
                if self.reservation_until.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::ReservationExpired);
                }
                if self.challenge_retry_at.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::ChallengeRetry);
                }
            }
            LiveDeadlinePhase::Active => {
                if self.peer_deadline().is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::PeerInactive);
                }
                if self
                    .consumption_deadline()
                    .is_some_and(|until| now >= until)
                {
                    return Some(DeadlineAction::ConsumerStalled);
                }
                if self.challenge_retry_at.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::ChallengeRetry);
                }
                if self.next_challenge_at.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::ChallengeDue);
                }
                if self.next_credit_at.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::CreditDue);
                }
            }
            LiveDeadlinePhase::Draining => {
                if self
                    .consumption_deadline()
                    .is_some_and(|until| now >= until)
                {
                    return Some(DeadlineAction::ConsumerStalled);
                }
            }
            LiveDeadlinePhase::Closing => {
                if self.close_until.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::CloseExchangeElapsed);
                }
                if self.cleanup_until.is_some_and(|until| now >= until) {
                    return Some(DeadlineAction::CleanupGraceElapsed);
                }
            }
            LiveDeadlinePhase::Closed => {}
        }
        None
    }

    fn peer_deadline(&self) -> Option<Instant> {
        self.last_fresh_round_trip_at
            .map(|at| at + Duration::from_millis(PEER_INACTIVITY_MS))
    }

    fn consumption_deadline(&self) -> Option<Instant> {
        let first_outstanding = self.unconsumed_since?;
        let last_progress = self
            .last_consumption_at
            .map_or(first_outstanding, |last| first_outstanding.max(last));
        Some(last_progress + Duration::from_millis(CONSUMER_STALL_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn reservation_boundary_is_exclusive_at_the_deadline() {
        let base = Instant::now();
        let deadlines = LiveDeadlines::reserved(base);
        assert_eq!(deadlines.evaluate(at(base, 14_999)), None);
        assert_eq!(
            deadlines.evaluate(at(base, 15_000)),
            Some(DeadlineAction::ReservationExpired)
        );
    }

    #[test]
    fn activation_before_the_deadline_survives_long_past_it() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.begin_activating(at(base, 1_000), "challenge".into());
        deadlines.fresh_round_trip(at(base, 1_000), false);
        deadlines.commit_active(at(base, 1_000), false);
        assert_eq!(deadlines.evaluate(at(base, 20_000)), None);
        assert_eq!(deadlines.phase(), LiveDeadlinePhase::Active);
    }

    #[test]
    fn healthy_round_trips_keep_a_session_active_without_an_age_cap() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.commit_active(base, true);
        for step in 1..=10_000u64 {
            let now = at(base, step * HEARTBEAT_INTERVAL_MS);
            assert_eq!(deadlines.evaluate(now), Some(DeadlineAction::ChallengeDue));
            deadlines.begin_challenge(now, format!("n{step}"));
            assert_eq!(
                deadlines.evaluate(now + Duration::from_millis(CHALLENGE_RETRY_MS)),
                Some(DeadlineAction::ChallengeRetry)
            );
            deadlines.fresh_round_trip(now, true);
        }
        assert_eq!(deadlines.phase(), LiveDeadlinePhase::Active);
    }

    #[test]
    fn peer_inactivity_is_exact_and_not_postponed_by_credit_or_data() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.commit_active(at(base, 100), false);
        deadlines.data_admitted(at(base, 20_000));
        assert_eq!(deadlines.evaluate(at(base, 35_099)), None);
        assert_eq!(
            deadlines.evaluate(at(base, 35_100)),
            Some(DeadlineAction::PeerInactive)
        );
    }

    #[test]
    fn challenge_retry_keeps_the_same_nonce_and_renews_once() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.begin_activating(base, "fixed".into());
        assert_eq!(
            deadlines.evaluate(at(base, 2_000)),
            Some(DeadlineAction::ChallengeRetry)
        );
        assert_eq!(deadlines.outstanding_challenge(), Some("fixed"));
        deadlines.fresh_round_trip(at(base, 2_500), true);
        assert_eq!(deadlines.outstanding_challenge(), None);
        assert_eq!(deadlines.evaluate(at(base, 3_000)), None);
    }

    #[test]
    fn consumer_stall_starts_at_first_unconsumed_data_and_ignores_pulses() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.commit_active(base, false);
        deadlines.data_admitted(at(base, 5_000));
        // A healthy pulse at t=30,000 keeps the peer alive but must not reset
        // the consumption clock that started at t=5,000.
        deadlines.fresh_round_trip(at(base, 30_000), false);
        assert_eq!(deadlines.evaluate(at(base, 39_999)), None);
        assert_eq!(
            deadlines.evaluate(at(base, 40_000)),
            Some(DeadlineAction::ConsumerStalled)
        );
    }

    #[test]
    fn consumption_progress_resets_and_emptying_disables_the_stall_clock() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.commit_active(base, false);
        deadlines.data_admitted(at(base, 1_000));
        deadlines.note_consumption(at(base, 30_000), 1);
        deadlines.credit_sent();
        deadlines.fresh_round_trip(at(base, 40_000), false);
        assert_eq!(deadlines.evaluate(at(base, 64_999)), None);
        assert_eq!(
            deadlines.evaluate(at(base, 65_000)),
            Some(DeadlineAction::ConsumerStalled)
        );
        deadlines.outstanding_cleared();
        deadlines.fresh_round_trip(at(base, 590_000), false);
        assert_eq!(deadlines.evaluate(at(base, 600_000)), None);
        deadlines.data_admitted(at(base, 600_000));
        deadlines.fresh_round_trip(at(base, 610_000), false);
        assert_eq!(
            deadlines.evaluate(at(base, 600_000 + CONSUMER_STALL_MS - 1)),
            None
        );
        assert_eq!(
            deadlines.evaluate(at(base, 600_000 + CONSUMER_STALL_MS)),
            Some(DeadlineAction::ConsumerStalled)
        );
    }

    #[test]
    fn credit_is_due_at_the_frame_threshold_or_the_delay() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.commit_active(base, true);
        deadlines.note_consumption(base, ACK_FRAME_THRESHOLD);
        assert_eq!(deadlines.evaluate(base), Some(DeadlineAction::CreditDue));
        deadlines.credit_sent();
        assert_eq!(deadlines.evaluate(base), None);
        deadlines.note_consumption(base, 1);
        assert_eq!(deadlines.evaluate(base), None);
        assert_eq!(
            deadlines.evaluate(base + Duration::from_millis(ACK_MAX_DELAY_MS)),
            Some(DeadlineAction::CreditDue)
        );
    }

    #[test]
    fn closing_has_one_absolute_exchange_and_one_cleanup_grace() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        deadlines.commit_active(base, true);
        deadlines.begin_closing(at(base, 1_000));
        assert_eq!(deadlines.evaluate(at(base, 2_999)), None);
        assert_eq!(
            deadlines.evaluate(at(base, 3_000)),
            Some(DeadlineAction::CleanupGraceElapsed)
        );
        assert_eq!(
            deadlines.evaluate(at(base, 6_000)),
            Some(DeadlineAction::CloseExchangeElapsed)
        );
    }

    #[test]
    fn closing_bumps_generation_and_receipt_expires() {
        let base = Instant::now();
        let mut deadlines = LiveDeadlines::reserved(base);
        let before = deadlines.generation();
        deadlines.closed(base);
        assert_eq!(deadlines.phase(), LiveDeadlinePhase::Closed);
        assert_eq!(deadlines.generation(), before + 1);
        assert!(!deadlines.tombstone_expired(at(base, 59_999)));
        assert!(deadlines.tombstone_expired(at(base, 60_000)));
    }

    #[tokio::test(start_paused = true)]
    async fn timer_adapter_fires_the_production_transition() {
        let start = Instant::now();
        let deadlines = LiveDeadlines::reserved(start);
        let due = deadlines.next_due().expect("reservation deadline");
        tokio::time::advance(std::time::Duration::from_millis(OPEN_RESERVATION_MS - 1)).await;
        assert!(Instant::now() < due);
        tokio::time::sleep_until(due).await;
        assert_eq!(
            deadlines.evaluate(Instant::now()),
            Some(DeadlineAction::ReservationExpired)
        );
    }
}
