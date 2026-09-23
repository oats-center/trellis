//! Public live-observation result types and cancellation primitives.

use std::sync::Arc;

use tokio::sync::watch;

pub use trellis_protocol::{LiveEndReason, LiveErrorCode};

/// Bounded `trellis.reason` value for one committed Live terminal.
pub(crate) fn live_end_reason(reason: LiveEndReason) -> &'static str {
    use LiveEndReason::*;
    match reason {
        Complete => "complete",
        Cancelled | LocalShutdown => "cancelled",
        Disconnected | PeerLost => "unavailable",
        AuthorizationLost | BindingChanged => "revoked",
        SetupTimeout | ConsumerSlow | DeliveryGap | SourceError | ProtocolError
        | ResourceExhausted => "error",
    }
}

/// One bounded live failure envelope surfaced to application code.
#[derive(Clone, Debug)]
pub struct LiveStreamError {
    code: LiveErrorCode,
    message: String,
    trace_id: Option<String>,
}

impl LiveStreamError {
    /// Construct one bounded live stream error.
    #[must_use]
    pub fn new(code: LiveErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            trace_id: None,
        }
    }

    /// Construct one live stream error that preserves a trace id.
    #[must_use]
    pub fn with_trace_id(
        code: LiveErrorCode,
        message: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            trace_id: Some(trace_id.into()),
        }
    }

    /// Return the stable wire error code.
    #[must_use]
    pub const fn code(&self) -> LiveErrorCode {
        self.code
    }

    /// Return the sanitized message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Return the trace id when one is available.
    #[must_use]
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    /// Return the public TypeScript-compatible code string.
    #[must_use]
    pub fn code_string(&self) -> String {
        format!("trellis.live.{}", self.code.as_str())
    }
}

impl std::fmt::Display for LiveStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for LiveStreamError {}

/// Committed local terminal outcome for one live observation.
#[derive(Clone, Debug)]
pub struct LiveEnd {
    reason: LiveEndReason,
    error: Option<Arc<LiveStreamError>>,
}

impl LiveEnd {
    /// Construct one terminal outcome.
    #[must_use]
    pub const fn new(reason: LiveEndReason, error: Option<Arc<LiveStreamError>>) -> Self {
        Self { reason, error }
    }

    /// Construct one normal completion.
    #[must_use]
    pub const fn complete() -> Self {
        Self {
            reason: LiveEndReason::Complete,
            error: None,
        }
    }

    /// Return the committed terminal reason.
    #[must_use]
    pub const fn reason(&self) -> LiveEndReason {
        self.reason
    }

    /// Return the bounded error envelope, absent for normal ends.
    #[must_use]
    pub fn error(&self) -> Option<&LiveStreamError> {
        self.error.as_deref()
    }

    /// Return whether this is a normal completion.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.reason, LiveEndReason::Complete)
    }
}

/// Remote cleanup confirmation state in a close receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseRemoteState {
    /// The provider confirmed the close and its owned cleanup result.
    Confirmed,
    /// No reply or `session_not_found` left remote cleanup unconfirmed.
    Unconfirmed,
    /// No verified offer existed, so there was nothing to close remotely.
    NotRequired,
}

/// Owned producer cleanup result in a close receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseCleanupState {
    /// Every owned resource settled.
    Complete,
    /// The cleanup grace elapsed with owned work unsettled.
    Incomplete,
    /// No provider cleanup result was confirmed.
    Unknown,
}

/// Result of an explicit close attempt.
#[derive(Clone, Debug)]
pub struct LiveCloseReceipt {
    end: LiveEnd,
    remote: CloseRemoteState,
    cleanup: CloseCleanupState,
}

impl LiveCloseReceipt {
    /// Construct one close receipt.
    #[must_use]
    pub const fn new(end: LiveEnd, remote: CloseRemoteState, cleanup: CloseCleanupState) -> Self {
        Self {
            end,
            remote,
            cleanup,
        }
    }

    /// Return the committed terminal outcome.
    #[must_use]
    pub const fn end(&self) -> &LiveEnd {
        &self.end
    }

    /// Return the remote confirmation state.
    #[must_use]
    pub const fn remote(&self) -> CloseRemoteState {
        self.remote
    }

    /// Return the owned cleanup state.
    #[must_use]
    pub const fn cleanup(&self) -> CloseCleanupState {
        self.cleanup
    }
}

/// Cloneable cancellation primitive for Rust optional cancellation inputs.
///
/// It has no authority constructor exposed to applications: it can only signal
/// cancellation, never grant or mutate authority.
#[derive(Clone, Debug)]
pub struct LiveCancellation {
    sender: Arc<watch::Sender<bool>>,
    receiver: watch::Receiver<bool>,
}

impl Default for LiveCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveCancellation {
    /// Create one new, uncancelled token.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
            receiver,
        }
    }

    /// Signal cancellation to every clone.
    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    /// Return whether cancellation was signalled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Await cancellation without missing a wakeup.
    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        if *receiver.borrow() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }

    /// Borrow this token for a session without transferring ownership.
    #[must_use]
    pub(crate) fn borrowed(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            receiver: self.receiver.clone(),
        }
    }

    /// Return a future resolving when cancellation is signalled.
    pub(crate) fn watcher(&self) -> watch::Receiver<bool> {
        self.receiver.clone()
    }
}

/// Map one wire error code to the public terminal reason it produces.
#[must_use]
pub(crate) fn end_reason_for_code(code: LiveErrorCode) -> LiveEndReason {
    match code {
        LiveErrorCode::UnsupportedProtocol
        | LiveErrorCode::InvalidRequest
        | LiveErrorCode::InvalidSubject => LiveEndReason::ProtocolError,
        LiveErrorCode::PermissionDenied
        | LiveErrorCode::AuthorizationExpired
        | LiveErrorCode::AuthorizationRevoked
        | LiveErrorCode::AuthorizationUnavailable => LiveEndReason::AuthorizationLost,
        LiveErrorCode::BindingChanged => LiveEndReason::BindingChanged,
        LiveErrorCode::ResourceExhausted => LiveEndReason::ResourceExhausted,
        LiveErrorCode::OpenConflict
        | LiveErrorCode::ControlConflict
        | LiveErrorCode::ControlGap
        | LiveErrorCode::InvalidCursor
        | LiveErrorCode::InvalidChallenge
        | LiveErrorCode::StaleControl
        | LiveErrorCode::ProtocolError => LiveEndReason::ProtocolError,
        LiveErrorCode::SessionNotFound => LiveEndReason::ProtocolError,
        LiveErrorCode::SetupTimeout => LiveEndReason::SetupTimeout,
        LiveErrorCode::PayloadTooLarge | LiveErrorCode::ConcurrentEmit => {
            LiveEndReason::SourceError
        }
        LiveErrorCode::Closed => LiveEndReason::Cancelled,
        LiveErrorCode::PeerLost => LiveEndReason::PeerLost,
        LiveErrorCode::Disconnected => LiveEndReason::Disconnected,
        LiveErrorCode::ConsumerSlow => LiveEndReason::ConsumerSlow,
        LiveErrorCode::DeliveryGap => LiveEndReason::DeliveryGap,
        LiveErrorCode::CallbackFailed | LiveErrorCode::SourceFailed => LiveEndReason::SourceError,
        LiveErrorCode::Cancelled => LiveEndReason::Cancelled,
        LiveErrorCode::LocalShutdown => LiveEndReason::LocalShutdown,
        LiveErrorCode::CleanupIncomplete => LiveEndReason::ProtocolError,
    }
}
