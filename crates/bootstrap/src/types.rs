//! Shared bootstrap types.

/// NATS listener ports resolved from a generated managed bundle's `nats.conf`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NatsListeners {
    /// Native NATS client listener port.
    pub native: u16,
    /// HTTP monitoring listener port.
    pub monitor: u16,
    /// Browser websocket listener port.
    pub websocket: u16,
}
