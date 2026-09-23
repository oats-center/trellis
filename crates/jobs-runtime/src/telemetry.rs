//! Read-only Jobs telemetry snapshot types.
//!
//! The snapshot deliberately separates the exact ready subset from retry
//! rows: `Retry` has no persisted next-eligible timestamp, so it is counted as
//! `waiting_retry` with no ready-age alert rather than assumed ready.

/// Read-only Jobs projection snapshot for observable gauges.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobsTelemetrySnapshot {
    /// Pending rows whose queue-age anchor is due and whose deadline is unexpired.
    pub ready: u64,
    /// Age in seconds of the oldest ready row; zero when none are ready.
    pub oldest_ready_age_seconds: f64,
    /// Retry-state rows without an asserted eligibility time.
    pub waiting_retry: u64,
    /// Undismissed dead jobs in the current projection.
    pub dead: u64,
    /// Fresh queue-worker registrations under the existing freshness window.
    pub worker_registrations: u64,
}
