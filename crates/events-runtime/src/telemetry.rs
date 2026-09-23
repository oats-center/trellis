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

/// Whether one broker Consumer name belongs to the internal events projection.
///
/// The runtime telemetry sampler uses this to keep internal projector backlog
/// out of the application Consumer gauges while reporting it separately.
pub fn is_projection_consumer(name: &str) -> bool {
    crate::projector::is_events_projector_consumer(name)
}
