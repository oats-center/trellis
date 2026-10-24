//! Read-only Events telemetry snapshot types.
//!
//! Only unresolved dead-letter states are reported: terminal and dismissed
//! history is not outstanding backlog, and the projection checkpoint remains
//! the single source of truth for replay progress.

/// Unresolved dead-letter entries by mapped catalog state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DlqTelemetrySnapshot {
    /// Entries awaiting management.
    pub open: u64,
    /// Durable replay intent not yet dispatched.
    pub replay_pending: u64,
    /// One targeted replay generation dispatched.
    pub replaying: u64,
}
