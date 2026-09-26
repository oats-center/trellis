//! Connection-owned classification of a planned authorization-credential
//! transport rotation.
//!
//! An authorization refresh may need the physical NATS attachment to rotate so
//! refreshed routing credentials are admitted. That physical transition is
//! maintenance of the existing logical Trellis connection, not a logical
//! disconnect and reconnect. This coordinator lets the connection event
//! callback distinguish the expected transition from genuine transport loss:
//! suppression applies only while a rotation is active and has not yet
//! observed its reconnect, so a later unrelated loss stays fail-closed.

use std::sync::Mutex;
use std::time::Duration;

/// Whether a raw transport event belongs to the active planned rotation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportRotationDisposition {
    /// The event is the expected mechanical transition of a planned rotation.
    Planned,
    /// The event is genuine transport loss or an unrelated transition.
    Unexpected,
}

#[derive(Default)]
struct RotationState {
    active: bool,
    reconnected: bool,
    loss_pending: bool,
}

/// Connection-owned, single-active classification of a planned rotation.
pub(crate) struct AuthorizationTransportRotation {
    state: Mutex<RotationState>,
    reconnected: tokio::sync::Notify,
}

impl AuthorizationTransportRotation {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RotationState::default()),
            reconnected: tokio::sync::Notify::new(),
        }
    }

    /// Begin one planned rotation.
    ///
    /// Returns `false` when a rotation is already active; the existing refresh
    /// serialization makes that a programming error rather than a normal race.
    pub(crate) fn begin(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.active {
            return false;
        }
        state.active = true;
        state.reconnected = false;
        state.loss_pending = false;
        true
    }

    /// Classify one physical disconnect event.
    pub(crate) fn observe_disconnect(&self) -> TransportRotationDisposition {
        let Ok(mut state) = self.state.lock() else {
            return TransportRotationDisposition::Unexpected;
        };
        if state.active && !state.reconnected {
            state.loss_pending = true;
            TransportRotationDisposition::Planned
        } else {
            TransportRotationDisposition::Unexpected
        }
    }

    /// Classify one physical (re)connect event.
    pub(crate) fn observe_connected(&self) -> TransportRotationDisposition {
        let Ok(mut state) = self.state.lock() else {
            return TransportRotationDisposition::Unexpected;
        };
        if state.active && !state.reconnected {
            state.reconnected = true;
            state.loss_pending = false;
            drop(state);
            self.reconnected.notify_waiters();
            TransportRotationDisposition::Planned
        } else {
            TransportRotationDisposition::Unexpected
        }
    }

    /// Return whether a planned rotation is currently active.
    pub(crate) fn is_active(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.active)
    }

    /// Wait for the expected reconnect, bounded by the connection timeout.
    pub(crate) async fn wait_reconnected(&self, timeout: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.reconnected.notified();
                if self.state.lock().is_ok_and(|state| state.reconnected) {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(timeout, wait).await.is_ok()
    }

    /// Finish a rotation after the candidate has been promoted.
    pub(crate) fn complete(&self) {
        self.clear();
    }

    /// Abandon a rotation, for example after an authorization rejection or a
    /// failed candidate installation.
    pub(crate) fn cancel(&self) {
        self.clear();
    }

    /// Abandon a timed-out rotation, reporting whether a physical loss was
    /// suppressed and must now be published as a real logical disconnect.
    pub(crate) fn escalate(&self) -> bool {
        let loss_pending = self.state.lock().map_or(false, |state| {
            state.active && !state.reconnected && state.loss_pending
        });
        self.clear();
        loss_pending
    }

    fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = RotationState::default();
        }
    }
}
