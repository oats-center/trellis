//! Sender accounting. The owner serializes validate/handoff/commit on its output lane.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Negotiated frame size and outstanding capacity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FlowLimits {
    pub max_frame_bytes: u64,
    pub window_frames: u64,
    pub window_bytes: u64,
}

/// Accounting failures; each protocol maps these onto its own error surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FlowError {
    PayloadTooLarge,
    WindowFull,
    InvalidCursor,
    SequenceExhausted,
    Unavailable,
}

/// Retained prefix accounting plus the bounded outstanding suffix.
#[derive(Debug, Default)]
struct Ledger {
    frames: VecDeque<(u64, u64)>,
    // Live has no cumulative byte wire bound: u64 sequences of u64 lengths
    // fit in u128 without changing Live's sequence-only credit semantics.
    consumed_bytes: u128,
}

/// One bounded sender ledger. Only consumed credit releases capacity.
#[derive(Debug)]
pub(crate) struct SenderWindow {
    limits: FlowLimits,
    pub highest_sent: AtomicU64,
    pub highest_received: AtomicU64,
    pub highest_consumed: AtomicU64,
    outstanding: Mutex<Ledger>,
    outstanding_bytes: AtomicU64,
    /// Register/enable the notification before rechecking capacity, then select
    /// against session cancellation. `notify_waiters` does not retain a permit.
    pub credit: tokio::sync::Notify,
}

impl SenderWindow {
    /// Create an empty window with protocol-owned limits.
    pub(crate) fn new(limits: FlowLimits) -> Self {
        Self {
            limits,
            highest_sent: AtomicU64::new(0),
            highest_received: AtomicU64::new(0),
            highest_consumed: AtomicU64::new(0),
            outstanding: Mutex::new(Ledger::default()),
            outstanding_bytes: AtomicU64::new(0),
            credit: tokio::sync::Notify::new(),
        }
    }

    /// Next sequence without advancing the sent watermark. Hold the output lane.
    pub(crate) fn next_frame_seq(&self) -> Result<u64, FlowError> {
        self.highest_sent
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(FlowError::SequenceExhausted)
    }

    /// Validate capacity without consuming it. Hold the lane through handoff/commit.
    /// `max_frame_bytes` may further narrow the negotiated limit for this handoff.
    pub(crate) fn validate_frame_slot(
        &self,
        len: u64,
        max_frame_bytes: u64,
    ) -> Result<(), FlowError> {
        if len > self.limits.max_frame_bytes.min(max_frame_bytes) {
            return Err(FlowError::PayloadTooLarge);
        }
        let outstanding = self
            .outstanding
            .lock()
            .map_err(|_| FlowError::Unavailable)?;
        if outstanding.frames.len() as u64 >= self.limits.window_frames
            || len
                > self
                    .limits
                    .window_bytes
                    .saturating_sub(self.outstanding_bytes.load(Ordering::Acquire))
        {
            return Err(FlowError::WindowFull);
        }
        Ok(())
    }

    /// Record successful transport handoff; failed handoffs must not call this.
    pub(crate) fn commit_frame(&self, seq: u64, len: u64) -> Result<(), FlowError> {
        self.validate_frame_slot(len, self.limits.max_frame_bytes)?;
        let mut outstanding = self
            .outstanding
            .lock()
            .map_err(|_| FlowError::Unavailable)?;
        if seq != self.next_frame_seq()? {
            return Err(FlowError::InvalidCursor);
        }
        outstanding.frames.push_back((seq, len));
        self.outstanding_bytes.fetch_add(len, Ordering::AcqRel);
        self.highest_sent.store(seq, Ordering::Release);
        Ok(())
    }

    /// Reject impossible or regressing authenticated cumulative cursors.
    /// When provided, the byte total must equal the exact consumed prefix.
    /// Live omits it because its wire carries sequence cursors only.
    pub(crate) fn validate_credit(
        &self,
        received: u64,
        consumed: u64,
        consumed_bytes: Option<u64>,
    ) -> Result<(), FlowError> {
        let ledger = self
            .outstanding
            .lock()
            .map_err(|_| FlowError::Unavailable)?;
        self.validate_credit_locked(&ledger, received, consumed, consumed_bytes)
    }

    fn validate_credit_locked(
        &self,
        ledger: &Ledger,
        received: u64,
        consumed: u64,
        consumed_bytes: Option<u64>,
    ) -> Result<(), FlowError> {
        if consumed > received
            || received > self.highest_sent.load(Ordering::Acquire)
            || received < self.highest_received.load(Ordering::Acquire)
            || consumed < self.highest_consumed.load(Ordering::Acquire)
        {
            return Err(FlowError::InvalidCursor);
        }
        if let Some(reported) = consumed_bytes {
            let expected = ledger.consumed_bytes
                + ledger
                    .frames
                    .iter()
                    .take_while(|(seq, _)| *seq <= consumed)
                    .map(|(_, len)| u128::from(*len))
                    .sum::<u128>();
            if u128::from(reported) != expected {
                return Err(FlowError::InvalidCursor);
            }
        }
        Ok(())
    }

    /// Apply cumulative credit atomically with ledger release and wake the sender.
    pub(crate) fn apply_credit(
        &self,
        received: u64,
        consumed: u64,
        consumed_bytes: Option<u64>,
    ) -> Result<(), FlowError> {
        let mut outstanding = self
            .outstanding
            .lock()
            .map_err(|_| FlowError::Unavailable)?;
        self.validate_credit_locked(&outstanding, received, consumed, consumed_bytes)?;
        while outstanding
            .frames
            .front()
            .is_some_and(|(seq, _)| *seq <= consumed)
        {
            if let Some((_, len)) = outstanding.frames.pop_front() {
                outstanding.consumed_bytes += u128::from(len);
                self.outstanding_bytes.fetch_sub(len, Ordering::AcqRel);
            }
        }
        self.highest_received.store(received, Ordering::Release);
        self.highest_consumed.store(consumed, Ordering::Release);
        drop(outstanding);
        self.wake_credit();
        Ok(())
    }

    /// Whether all handed-off frames have been consumed.
    pub(crate) fn outstanding_empty(&self) -> bool {
        self.outstanding
            .lock()
            .is_ok_and(|ledger| ledger.frames.is_empty())
    }

    /// Wake blocked capacity waits, including on lifecycle cancellation.
    pub(crate) fn wake_credit(&self) {
        self.credit.notify_waiters();
    }

    /// Require complete consumed delivery of the declared final sequence.
    pub(crate) fn validate_complete(&self, final_seq: u64) -> Result<(), FlowError> {
        if final_seq != self.highest_sent.load(Ordering::Acquire)
            || final_seq != self.highest_consumed.load(Ordering::Acquire)
            || !self.outstanding_empty()
        {
            return Err(FlowError::InvalidCursor);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn consumed_credit_releases_capacity_and_wakes_sender() {
        let window = SenderWindow::new(FlowLimits {
            max_frame_bytes: 6,
            window_frames: 2,
            window_bytes: 8,
        });
        window.commit_frame(1, 3).unwrap();
        window.commit_frame(2, 5).unwrap();
        assert_eq!(window.validate_frame_slot(1, 6), Err(FlowError::WindowFull));
        window.apply_credit(2, 0, None).unwrap();
        assert_eq!(window.validate_frame_slot(1, 6), Err(FlowError::WindowFull));
        let notified = window.credit.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        // Capacity remains full after registration. Credit arrives in the
        // precise recheck-to-await gap, before the waiter is first polled.
        assert_eq!(window.validate_frame_slot(1, 6), Err(FlowError::WindowFull));
        window.apply_credit(2, 1, None).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), notified)
            .await
            .unwrap();
        window.commit_frame(3, 3).unwrap();
        assert_eq!(window.validate_frame_slot(1, 6), Err(FlowError::WindowFull));
        assert_eq!(
            window.apply_credit(1, 1, None),
            Err(FlowError::InvalidCursor)
        );
        assert_eq!(
            window.apply_credit(3, 0, None),
            Err(FlowError::InvalidCursor)
        );
        assert_eq!(
            window.apply_credit(4, 3, None),
            Err(FlowError::InvalidCursor)
        );
        assert_eq!(
            window.apply_credit(2, 3, None),
            Err(FlowError::InvalidCursor)
        );
        assert_eq!(window.validate_complete(3), Err(FlowError::InvalidCursor));
        window.apply_credit(3, 3, None).unwrap();
        window.apply_credit(3, 3, None).unwrap();
        window.validate_complete(3).unwrap();
        assert_eq!(window.validate_complete(4), Err(FlowError::InvalidCursor));
        assert_eq!(window.commit_frame(5, 1), Err(FlowError::InvalidCursor));
        assert_eq!(
            window.validate_frame_slot(7, 6),
            Err(FlowError::PayloadTooLarge)
        );
        window.commit_frame(4, 6).unwrap();
        assert_eq!(window.validate_frame_slot(3, 6), Err(FlowError::WindowFull));
    }

    #[test]
    fn byte_credit_must_match_consumed_prefix_before_capacity_release() {
        let window = SenderWindow::new(FlowLimits {
            max_frame_bytes: 5,
            window_frames: 2,
            window_bytes: 8,
        });
        window.commit_frame(1, 3).unwrap();
        window.commit_frame(2, 5).unwrap();
        for bytes in [0, 2, 4, 8] {
            assert_eq!(
                window.validate_credit(2, 1, Some(bytes)),
                Err(FlowError::InvalidCursor)
            );
            assert_eq!(
                window.apply_credit(2, 1, Some(bytes)),
                Err(FlowError::InvalidCursor)
            );
            assert_eq!(window.validate_frame_slot(1, 5), Err(FlowError::WindowFull));
        }
        window.apply_credit(2, 1, Some(3)).unwrap();
        window.commit_frame(3, 3).unwrap();
        assert_eq!(
            window.apply_credit(3, 2, Some(5)),
            Err(FlowError::InvalidCursor)
        );
        assert_eq!(window.validate_frame_slot(1, 5), Err(FlowError::WindowFull));
        window.apply_credit(3, 2, Some(8)).unwrap();
        window.apply_credit(3, 2, Some(8)).unwrap();
        assert_eq!(
            window.apply_credit(3, 2, Some(9)),
            Err(FlowError::InvalidCursor)
        );
        window.apply_credit(3, 3, Some(11)).unwrap();
        window.validate_complete(3).unwrap();
    }
}
