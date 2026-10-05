//! Coalesced credit scheduling; the endpoint owns its latest cumulative cursor.
use std::time::Duration;
use tokio::time::Instant;

/// Scheduling policy; `None` disables the byte threshold (Live).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CreditPolicy {
    pub frame_step: u64,
    pub byte_step: Option<u64>,
    pub max_delay: Duration,
}

/// One pending deadline, never a queue of acknowledgements.
#[derive(Debug)]
pub(crate) struct CreditScheduler {
    policy: CreditPolicy,
    deadline: Option<Instant>,
}

impl CreditScheduler {
    /// Create a scheduler using the owning protocol's unchanged policy.
    pub(crate) fn new(policy: CreditPolicy) -> Self {
        Self {
            policy,
            deadline: None,
        }
    }

    /// Schedule from cumulative *unreported* frame/byte counts, not per-call deltas.
    /// Repeated updates never postpone the first pending consumption deadline.
    pub(crate) fn note_pending(&mut self, now: Instant, frames: u64, bytes: u64) {
        if frames >= self.policy.frame_step
            || self.policy.byte_step.is_some_and(|step| bytes >= step)
        {
            self.force(now);
        } else if self.deadline.is_none() {
            self.deadline = Some(now + self.policy.max_delay);
        }
    }
    /// Force a final or context-renewal credit using the endpoint's latest cursor.
    pub(crate) fn force(&mut self, now: Instant) {
        self.deadline = Some(self.deadline.map_or(now, |old| old.min(now)));
    }
    /// Earliest credit deadline for the owning task's timer/select.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.deadline
    }
    /// Whether cumulative credit should be handed off now.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
    /// Clear after handoff or lifecycle teardown; in-flight coalescing stays endpoint-owned.
    pub(crate) fn clear(&mut self) {
        self.deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credit_coalesces_without_postponing_and_thresholds_force_delivery() {
        let now = Instant::now();
        let mut scheduler = CreditScheduler::new(CreditPolicy {
            frame_step: 8,
            byte_step: Some(100),
            max_delay: Duration::from_millis(25),
        });
        scheduler.note_pending(now, 1, 10);
        scheduler.note_pending(now + Duration::from_millis(20), 2, 20);
        assert!(!scheduler.is_due(now + Duration::from_millis(24)));
        assert!(scheduler.is_due(now + Duration::from_millis(25)));
        scheduler.clear();
        scheduler.note_pending(now, 8, 80);
        assert!(scheduler.is_due(now));
        scheduler.clear();
        scheduler.note_pending(now, 1, 100);
        assert!(scheduler.is_due(now));
        scheduler.clear();
        scheduler.note_pending(now, 1, 10);
        scheduler.force(now);
        assert!(scheduler.is_due(now));
    }
}
