//! Errors surfaced by the test harness.

/// Failure while owning, configuring, or talking to a test runtime.
#[derive(Debug, thiserror::Error)]
pub enum TrellisTestError {
    /// Filesystem or process failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Bootstrap bundle generation failed.
    #[error("bootstrap error: {0}")]
    Bootstrap(String),
    /// The managed NATS server could not be started or stopped.
    #[error("nats error: {0}")]
    Nats(String),
    /// The control plane failed to start, exited early, or refused to stop.
    #[error("runtime error: {0}")]
    Runtime(String),
    /// A runtime configuration value was missing or invalid.
    #[error("config error: {0}")]
    Config(String),
    /// An HTTP request to the control plane failed.
    #[error("http error: {0}")]
    Http(String),
    /// The runtime did not reach the expected state before its timeout.
    #[error("timed out {0}")]
    TimedOut(String),
    /// An operation was requested in a state that does not allow it.
    #[error("invalid state: {0}")]
    InvalidState(String),
}
