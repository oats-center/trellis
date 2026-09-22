use jsonptr::PointerBuf;

/// Stable failure categories for signed authorization protocol values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationErrorCode {
    /// A value has the wrong protocol format or strict object shape.
    InvalidFormat,
    /// An integer cannot be represented exactly by interoperable JSON implementations.
    UnsafeJsonInteger,
    /// A binary value is not canonical unpadded base64url.
    InvalidEncoding,
    /// An Ed25519 public key is malformed.
    InvalidPublicKey,
    /// A declared key id does not match its public key.
    InvalidKeyId,
    /// A signature is malformed or cryptographically invalid.
    InvalidSignature,
    /// A critical extension is not understood.
    UnknownCriticalExtension,
    /// A set-like array is duplicated or out of canonical order.
    NonCanonicalSet,
    /// An authored validity interval is inconsistent.
    InvalidValidityWindow,
    /// An explicitly revoked issuer cannot verify live or retained authority.
    IssuerRevoked,
    /// A retired issuer is eligible only for historical event verification.
    IssuerRetired,
    /// A historical-event context handle cannot authorize live requests.
    HistoricalContext,
    /// The authorization context is not yet valid.
    ContextNotYetValid,
    /// The authorization context has expired.
    ContextExpired,
    /// The context lifetime exceeds explicit policy.
    ContextLifetimeExceeded,
    /// The session verification key is malformed.
    InvalidSessionKey,
    /// One or more exact permission atoms are absent.
    PermissionDenied,
    /// The canonical signed context exceeds explicit policy.
    ContextTooLarge,
    /// The request issue time is outside the accepted skew.
    ProofIatOutOfRange,
    /// The context-bound request signature is invalid.
    InvalidRequestProof,
    /// The request reply subject is outside the caller inbox prefix.
    ReplySubjectMismatch,
    /// The event time is not canonical RFC 3339 UTC.
    InvalidEventTime,
    /// The context-bound event signature is invalid.
    InvalidEventProof,
    /// The event's signing context was explicitly revoked.
    EventRevoked,
}

/// Stable failure categories for bootstrap and authorization-context refresh proofs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionProofErrorCode {
    /// A value has the wrong protocol format or strict object shape.
    InvalidFormat,
    /// An integer cannot be represented exactly by interoperable JSON implementations.
    UnsafeJsonInteger,
    /// A binary value is not canonical unpadded base64url.
    InvalidEncoding,
    /// An Ed25519 public key is malformed.
    InvalidPublicKey,
    /// A declared key id does not match its public key.
    InvalidKeyId,
    /// A NATS User NKey is malformed or does not encode the session public key.
    InvalidNatsKey,
    /// A signature is malformed or cryptographically invalid.
    InvalidSignature,
    /// The proof issue time is outside the accepted policy window.
    ProofIatOutOfRange,
}

/// Errors produced while validating or canonicalizing Trellis protocol values.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// JSON encoding or decoding failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A JSON number cannot be represented canonically.
    #[error("non-canonical JSON number: {0}")]
    NonCanonicalNumber(String),

    /// A protocol identifier is empty or contains forbidden characters.
    #[error("invalid {field}: {reason}")]
    InvalidIdentifier {
        /// The semantic field that failed validation.
        field: &'static str,
        /// The validation failure.
        reason: &'static str,
    },

    /// An action is not valid for its target kind.
    #[error("action '{action}' is not valid for {target}")]
    InvalidPermission {
        /// The invalid wire action.
        action: String,
        /// The target kind receiving the action.
        target: &'static str,
    },

    /// A grant set uses an unsupported wire format.
    #[error("unsupported grant-set format '{0}'")]
    InvalidGrantSetFormat(String),

    /// A signed authorization object or proof failed validation.
    #[error("authorization validation failed at '{path}' ({code:?}): {message}")]
    Authorization {
        /// Stable failure category.
        code: AuthorizationErrorCode,
        /// Exact authored RFC 6901 path.
        path: Box<PointerBuf>,
        /// Safe diagnostic that omits secrets and signed payloads.
        message: String,
    },

    /// A bootstrap or authorization-context refresh proof failed validation.
    #[error("session proof validation failed at '{path}' ({code:?}): {message}")]
    SessionProof {
        /// Stable failure category.
        code: SessionProofErrorCode,
        /// Exact authored RFC 6901 path.
        path: Box<PointerBuf>,
        /// Safe diagnostic that omits secrets and signed payloads.
        message: String,
    },
}
