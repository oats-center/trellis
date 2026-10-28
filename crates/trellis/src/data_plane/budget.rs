//! Process-wide receive admission for Transfer windows.
//!
//! A receiver reserves the exact window it offers before frames can arrive and
//! releases it when the transfer ends, on every path. Admission never
//! overcommits: a window that does not fit is shrunk by the shared policy when
//! offered, or rejected when reserved.

use std::sync::atomic::{AtomicU64, Ordering};
use trellis_protocol::transfer::{transfer_receive_window, TRANSFER_PROCESS_RECEIVE_BYTES};

/// Receive capacity shared by every transfer of one process.
#[derive(Debug)]
pub(crate) struct ReceiveBudget {
    total: u64,
    reserved: AtomicU64,
}

static PROCESS: ReceiveBudget = ReceiveBudget::new(TRANSFER_PROCESS_RECEIVE_BYTES);

impl ReceiveBudget {
    pub(crate) const fn new(total: u64) -> Self {
        Self {
            total,
            reserved: AtomicU64::new(0),
        }
    }

    /// The process-wide budget.
    pub(crate) fn process() -> &'static Self {
        &PROCESS
    }

    /// Window (frames, bytes) to offer now, or `None` when exhausted.
    pub(crate) fn offer(&self, max_frame_bytes: u64) -> Option<(u64, u64)> {
        let available = self
            .total
            .saturating_sub(self.reserved.load(Ordering::Acquire));
        transfer_receive_window(available, max_frame_bytes)
    }

    /// Reserve exactly `bytes`, or `None` if they no longer fit.
    pub(crate) fn reserve(&'static self, bytes: u64) -> Option<ReceiveReservation> {
        let mut current = self.reserved.load(Ordering::Acquire);
        loop {
            let next = current
                .checked_add(bytes)
                .filter(|next| *next <= self.total)?;
            match self.reserved.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ReceiveReservation {
                        budget: self,
                        bytes,
                    })
                }
                Err(actual) => current = actual,
            }
        }
    }
}

/// Reserved receive capacity, released when dropped.
#[derive(Debug)]
pub(crate) struct ReceiveReservation {
    budget: &'static ReceiveBudget,
    bytes: u64,
}

impl Drop for ReceiveReservation {
    fn drop(&mut self) {
        self.budget.reserved.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trellis_protocol::transfer::{TRANSFER_WINDOW_BYTES, TRANSFER_WINDOW_FRAMES};

    #[test]
    fn admission_shrinks_offers_rejects_overcommit_and_releases_on_drop() {
        static BUDGET: ReceiveBudget = ReceiveBudget::new(TRANSFER_WINDOW_BYTES * 2 + 1024);
        let frame = 256 * 1024;
        assert_eq!(
            BUDGET.offer(frame),
            Some((TRANSFER_WINDOW_FRAMES, TRANSFER_WINDOW_BYTES))
        );
        let first = BUDGET.reserve(TRANSFER_WINDOW_BYTES).unwrap();
        let second = BUDGET.reserve(TRANSFER_WINDOW_BYTES).unwrap();
        // 1 KiB remains: smaller than one frame, so nothing is offered or reserved.
        assert_eq!(BUDGET.offer(frame), None);
        assert!(BUDGET.reserve(TRANSFER_WINDOW_BYTES).is_none());
        drop(first);
        let (frames, bytes) = BUDGET.offer(frame).unwrap();
        assert_eq!(bytes, TRANSFER_WINDOW_BYTES);
        assert!(frames >= 1);
        drop(second);
        assert_eq!(BUDGET.reserved.load(Ordering::Acquire), 0);
    }

    #[test]
    fn partial_budget_offers_a_proportionally_smaller_window() {
        static BUDGET: ReceiveBudget = ReceiveBudget::new(TRANSFER_WINDOW_BYTES / 2);
        let (frames, bytes) = BUDGET.offer(64 * 1024).unwrap();
        assert_eq!(bytes, TRANSFER_WINDOW_BYTES / 2);
        assert_eq!(frames, TRANSFER_WINDOW_FRAMES.div_ceil(2));
    }
}
