use std::io;

/// Errors returned by Trellis auth and admin-session helpers.
#[derive(Debug, thiserror::Error)]
#[doc = concat!("Public Trellis value set `", stringify!(TrellisAuthError), "`.")]
pub enum TrellisAuthError {
    /// A JSON request or response could not be encoded or decoded.
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),

    /// A protocol-owned participant or proof value was invalid.
    #[error("auth protocol error: {0}")]
    Protocol(Box<trellis_protocol::ProtocolError>),

    /// A configured auth or portal URL was invalid.
    #[error("invalid url: {0}")]
    Url(#[from] url::ParseError),

    /// The HTTP transport used for agent login or bind requests failed.
    #[error("http client error: {0}")]
    Http(#[from] reqwest::Error),

    /// Local filesystem persistence or socket I/O failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The underlying Trellis RPC client returned an error.
    #[error("{0}")]
    TrellisClient(#[from] crate::client::TrellisClientError),

    /// Agent login never completed before the timeout.
    #[error("timed out waiting for agent login")]
    LoginTimedOut,

    /// The detached login flow shut down before completion.
    #[error("agent login was interrupted")]
    LoginInterrupted,

    /// The auth service returned a terminal flow error.
    #[error("auth flow failed: {0}")]
    AuthFlowFailed(String),

    /// The low-level bind endpoint returned a non-success HTTP response.
    #[error("bind failed: {0} {1}")]
    BindHttpFailure(u16, String),

    /// The auth-start endpoint returned a non-success HTTP response.
    #[error("auth request failed: {0} {1}")]
    AuthRequestHttpFailure(u16, String),

    /// The bind endpoint returned a response shape this crate does not understand.
    #[error("unexpected bind status: {0}")]
    UnexpectedBindStatus(String),

    /// The auth-start endpoint returned a response shape this crate does not understand.
    #[error("unexpected auth request status: {0}")]
    UnexpectedAuthRequestStatus(String),

    /// The caller supplied arguments that violate the Trellis auth invariants.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// A helper wrapped operation finished without a usable successful result.
    #[error("operation failed: {0}")]
    OperationFailed(String),

    /// The authenticated user completed login successfully but lacks admin capability.
    #[error("authenticated user lacks trellis.auth::admin capability")]
    NotAdmin,

    /// The current session belongs to a non-user participant.
    #[error("current session is not a user session: participantKind={0}")]
    NotUserSession(String),
}

impl From<trellis_protocol::ProtocolError> for TrellisAuthError {
    fn from(error: trellis_protocol::ProtocolError) -> Self {
        Self::Protocol(Box::new(error))
    }
}
