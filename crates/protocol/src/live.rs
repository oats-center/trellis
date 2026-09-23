//! Pure Live observation protocol: constants, wire schemas, subject derivation,
//! canonical hashing, and provider proof construction/verification.
//!
//! Live sessions are authorized ephemeral views. Their wire protocol is
//! connection-scoped Core NATS traffic authenticated by the provider's runtime
//! signing key, negotiated over one finite request/reply opening exchange.
//! This module owns the semantic rules that must agree between the Rust and
//! TypeScript runtimes; it performs no transport, storage, or async work.
//!
//! # Wire grammar
//!
//! ```text
//! Live base:      live.v1.route.<b64(apiId)>.<b64(providerDeploymentId)>.<action>
//! Operation base: operation.v1.<b64(apiId)>.<b64(providerDeploymentId)>.<action>
//! Owner control:  <base>.observe.<b64(P)>.<sessionId>
//! Live delivery:  live.v1.data.<b64(P)>.<b64(C)>.<sessionId>
//! ```

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use jsonptr::PointerBuf;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{identifiers::validate_logical_name, subjects, ProtocolError};

/// Exact protocol discriminator for every live-session body.
pub const LIVE_VERSION: &str = "trellis.live.v1";
/// Provider server-message proof signature domain.
pub const LIVE_SERVER_PROOF_DOMAIN_V1: &str = "trellis.live-server-proof.v1";

/// Time from reservation to successful delivery-path activation.
pub const OPEN_RESERVATION_MS: u64 = 15_000;
/// Entire one-control attempt budget, including authority resolution and reply validation.
pub const CONTROL_TIMEOUT_MS: u64 = 5_000;
/// Delay from an answered challenge to the next challenge.
pub const HEARTBEAT_INTERVAL_MS: u64 = 10_000;
/// Re-send interval for the same outstanding challenge.
pub const CHALLENGE_RETRY_MS: u64 = 2_000;
/// Deadline since the last fresh challenge response.
pub const PEER_INACTIVITY_MS: u64 = 35_000;
/// No consumption progress while application data is outstanding.
pub const CONSUMER_STALL_MS: u64 = 35_000;
/// Maximum delay to send accumulated consumption credit.
pub const ACK_MAX_DELAY_MS: u64 = 50;
/// Send credit after this many newly consumed frames.
pub const ACK_FRAME_THRESHOLD: u64 = 16;
/// Maximum unconsumed, published application frames.
pub const WINDOW_FRAMES: u64 = 64;
/// Maximum body bytes of those frames.
pub const WINDOW_BYTES: u64 = 1_048_576;
/// Reserve for NATS headers when deriving the negotiated body limit.
pub const PROTOCOL_HEADER_RESERVE_BYTES: u64 = 4_096;
/// Limit for the complete opening envelope, including input.
pub const MAX_OPEN_BODY_BYTES: usize = 262_144;
/// Limit for offer, control, challenge and terminal JSON bodies.
pub const MAX_CONTROL_BODY_BYTES: usize = 8_192;
/// Grace for owned producer cleanup before reporting incomplete cleanup.
pub const CLEANUP_GRACE_MS: u64 = 2_000;
/// Total best-effort remote-close exchange budget.
pub const CLOSE_EXCHANGE_MS: u64 = 5_000;
/// Retry interval for close/end acknowledgement.
pub const CLOSE_RETRY_MS: u64 = 1_000;
/// Retain small closed-session receipts for idempotence.
pub const TOMBSTONE_MS: u64 = 60_000;
/// Bounded per-manager tombstone LRU size.
pub const MAX_TOMBSTONES: usize = 256;
/// Bounded concurrent verification of new opens.
pub const MAX_OPEN_VERIFY_INFLIGHT: usize = 8;
/// Bounded additional raw opening requests awaiting verification.
pub const MAX_OPEN_VERIFY_QUEUE: usize = 16;
/// Reserved verification capacity for established-session controls.
pub const MAX_CONTROL_VERIFY_INFLIGHT: usize = 8;
/// Bounded additional raw controls awaiting verification.
pub const MAX_CONTROL_VERIFY_QUEUE: usize = 64;
/// Combined offered/activating/active provider reservations.
pub const MAX_PROVIDER_SESSIONS: usize = 64;
/// Same bound by `(C, consumerSessionKey)`.
pub const MAX_PROVIDER_SESSIONS_PER_CALLER: usize = 16;
/// Combined prepared/activating/active local subscription ownership.
pub const MAX_CONSUMER_SESSIONS: usize = 64;
/// Smallest negotiated data body accepted by the protocol.
pub const MIN_DATA_BODY_BYTES: u64 = 1_024;

const NONCE_BYTES: usize = 16;
const NONCE_TEXT_LENGTH: usize = 22;
const MAXIMUM_REASON_MESSAGE_BYTES: usize = 2_048;
const MAXIMUM_REASON_MESSAGE_SCALARS: usize = 512;

/// Stable failure categories for live protocol and proof validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveProtocolErrorCode {
    /// A value has the wrong protocol format or strict object shape.
    InvalidFormat,
    /// A counter is not a canonical unsigned decimal string.
    InvalidCounter,
    /// A nonce is not the exact canonical random-token encoding.
    InvalidNonce,
    /// A subject token is empty, non-canonical, or injects a wildcard.
    InvalidSubjectToken,
    /// A body exceeds its protocol limit.
    BodyTooLarge,
    /// A negotiated data body limit is absent, invalid, or below the minimum.
    InvalidDataBodyLimit,
    /// A proof signature is malformed or cryptographically invalid.
    InvalidSignature,
    /// A required security-sensitive header is missing or duplicated.
    InvalidHeaders,
}

/// Errors produced while validating live protocol values.
#[derive(Debug, thiserror::Error)]
pub enum LiveProtocolError {
    /// A live protocol value failed validation.
    #[error("live protocol validation failed at '{path}' ({code:?}): {message}")]
    Invalid {
        /// Stable failure category.
        code: LiveProtocolErrorCode,
        /// Exact RFC 6901 path of the invalid value.
        path: Box<PointerBuf>,
        /// Safe diagnostic that omits secrets and payloads.
        message: String,
    },
}

impl From<LiveProtocolError> for ProtocolError {
    fn from(error: LiveProtocolError) -> Self {
        match error {
            LiveProtocolError::Invalid {
                code,
                path,
                message,
            } => Self::Live {
                code,
                path,
                message,
            },
        }
    }
}

/// Wire error taxonomy for live protocol messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveErrorCode {
    /// The peer does not implement this protocol version.
    UnsupportedProtocol,
    /// The opening or control body is not a valid protocol request.
    InvalidRequest,
    /// A subject is not the canonical derivation for this session.
    InvalidSubject,
    /// The caller lacks the exact required permission.
    PermissionDenied,
    /// The authorization context has expired.
    AuthorizationExpired,
    /// The authorization context was explicitly revoked.
    AuthorizationRevoked,
    /// Authorization coverage is currently unavailable.
    AuthorizationUnavailable,
    /// The installed API binding generation changed.
    BindingChanged,
    /// An admission quota is exhausted.
    ResourceExhausted,
    /// A duplicate `openId` for the same caller and route carried different inputs.
    OpenConflict,
    /// The session is unknown, expired, or evicted.
    SessionNotFound,
    /// The setup deadline elapsed before activation.
    SetupTimeout,
    /// A control sequence was reused with a different body.
    ControlConflict,
    /// A control sequence skipped a value.
    ControlGap,
    /// A reported cursor is impossible or regressing.
    InvalidCursor,
    /// A challenge response does not match the outstanding challenge.
    InvalidChallenge,
    /// A lower control sequence arrived after a newer one was processed.
    StaleControl,
    /// A frame exceeds the negotiated body limit.
    PayloadTooLarge,
    /// A second emit was attempted concurrently with an outstanding one.
    ConcurrentEmit,
    /// The session is already closed.
    Closed,
    /// The pinned peer stopped responding on the delivery path.
    PeerLost,
    /// The local transport epoch was lost.
    Disconnected,
    /// The consumer did not advance consumption progress.
    ConsumerSlow,
    /// A verified data sequence skipped a value.
    DeliveryGap,
    /// An application callback failed.
    CallbackFailed,
    /// The domain source failed.
    SourceFailed,
    /// A protocol rule was violated beyond the more specific codes.
    ProtocolError,
    /// The observation was cancelled locally.
    Cancelled,
    /// The runtime or connection shut down.
    LocalShutdown,
    /// Owned source cleanup did not settle within the shared grace.
    CleanupIncomplete,
}

impl LiveErrorCode {
    /// Return the exact wire suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidSubject => "invalid_subject",
            Self::PermissionDenied => "permission_denied",
            Self::AuthorizationExpired => "authorization_expired",
            Self::AuthorizationRevoked => "authorization_revoked",
            Self::AuthorizationUnavailable => "authorization_unavailable",
            Self::BindingChanged => "binding_changed",
            Self::ResourceExhausted => "resource_exhausted",
            Self::OpenConflict => "open_conflict",
            Self::SessionNotFound => "session_not_found",
            Self::SetupTimeout => "setup_timeout",
            Self::ControlConflict => "control_conflict",
            Self::ControlGap => "control_gap",
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidChallenge => "invalid_challenge",
            Self::StaleControl => "stale_control",
            Self::PayloadTooLarge => "payload_too_large",
            Self::ConcurrentEmit => "concurrent_emit",
            Self::Closed => "closed",
            Self::PeerLost => "peer_lost",
            Self::Disconnected => "disconnected",
            Self::ConsumerSlow => "consumer_slow",
            Self::DeliveryGap => "delivery_gap",
            Self::CallbackFailed => "callback_failed",
            Self::SourceFailed => "source_failed",
            Self::ProtocolError => "protocol_error",
            Self::Cancelled => "cancelled",
            Self::LocalShutdown => "local_shutdown",
            Self::CleanupIncomplete => "cleanup_incomplete",
        }
    }
}

/// Committed local terminal reason for one live session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveEndReason {
    /// The source returned normally and the full sequence was delivered.
    Complete,
    /// The owning application cancelled the observation.
    Cancelled,
    /// The runtime or connection shut down.
    LocalShutdown,
    /// Activation did not complete within the setup deadline.
    SetupTimeout,
    /// The pinned peer stopped responding on the delivery path.
    PeerLost,
    /// The local transport attachment was lost.
    Disconnected,
    /// Current authorization is no longer valid.
    AuthorizationLost,
    /// The installed API binding generation changed.
    BindingChanged,
    /// The consumer did not advance consumption progress.
    ConsumerSlow,
    /// A verified data sequence skipped a value.
    DeliveryGap,
    /// The domain source failed.
    SourceError,
    /// A protocol rule was violated.
    ProtocolError,
    /// An admission quota or ingress bound was exceeded.
    ResourceExhausted,
}

impl LiveEndReason {
    /// Return the exact wire suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::LocalShutdown => "local_shutdown",
            Self::SetupTimeout => "setup_timeout",
            Self::PeerLost => "peer_lost",
            Self::Disconnected => "disconnected",
            Self::AuthorizationLost => "authorization_lost",
            Self::BindingChanged => "binding_changed",
            Self::ConsumerSlow => "consumer_slow",
            Self::DeliveryGap => "delivery_gap",
            Self::SourceError => "source_error",
            Self::ProtocolError => "protocol_error",
            Self::ResourceExhausted => "resource_exhausted",
        }
    }
}

/// Reason a consumer includes in an explicit close control.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsumerCloseReason {
    /// The application cancelled the observation.
    Cancelled,
    /// The runtime or connection shut down.
    LocalShutdown,
    /// The application did not consume outstanding data in time.
    ConsumerSlow,
    /// A verified sequence gap ended the observation.
    DeliveryGap,
    /// A protocol rule was violated.
    ProtocolError,
}

impl ConsumerCloseReason {
    /// Return the exact wire suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::LocalShutdown => "local_shutdown",
            Self::ConsumerSlow => "consumer_slow",
            Self::DeliveryGap => "delivery_gap",
            Self::ProtocolError => "protocol_error",
        }
    }
}

fn live_error<'a>(
    code: LiveProtocolErrorCode,
    tokens: impl IntoIterator<Item = &'a str>,
    message: impl Into<String>,
) -> ProtocolError {
    ProtocolError::Live {
        code,
        path: Box::new(PointerBuf::from_tokens(tokens)),
        message: message.into(),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn decode_base64url<const N: usize>(
    encoded: &str,
    tokens: &[&'static str],
    code: LiveProtocolErrorCode,
) -> Result<[u8; N], ProtocolError> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        live_error(
            code,
            tokens.iter().copied(),
            "value is not canonical unpadded base64url",
        )
    })?;
    let bytes: [u8; N] = bytes.try_into().map_err(|_| {
        live_error(
            code,
            tokens.iter().copied(),
            "decoded value has the wrong length",
        )
    })?;
    Ok(bytes)
}

/// Return whether one decoded token re-encodes to its exact input bytes.
fn is_canonical_token(value: &str) -> bool {
    !value.is_empty()
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| URL_SAFE_NO_PAD.encode(&bytes) == value)
}

/// Validate one subject token encoding a UTF-8 logical identity.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the token is empty, non-canonical, not
/// valid UTF-8, or contains a NATS wildcard.
pub fn validate_subject_token(
    value: &str,
    tokens: impl IntoIterator<Item = &'static str> + Clone,
) -> Result<&str, ProtocolError> {
    if !is_canonical_token(value) {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidSubjectToken,
            tokens,
            "subject token must be non-empty canonical unpadded base64url",
        ));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .expect("canonical token decodes");
    if String::from_utf8(decoded).is_err() {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidSubjectToken,
            tokens,
            "subject token must encode UTF-8",
        ));
    }
    Ok(value)
}

/// Parse and validate one 16-byte random nonce in its canonical encoding.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] unless the value is exactly the canonical
/// 22-character base64url encoding of 16 random bytes.
pub fn parse_nonce(
    value: &str,
    tokens: impl IntoIterator<Item = &'static str> + Clone,
) -> Result<&str, ProtocolError> {
    if value.len() != NONCE_TEXT_LENGTH
        || !is_canonical_token(value)
        || URL_SAFE_NO_PAD.decode(value).map_or(true, |bytes| {
            bytes.len() != NONCE_BYTES || URL_SAFE_NO_PAD.encode(&bytes) != value
        })
    {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidNonce,
            tokens,
            "nonce must be the canonical 22-character encoding of 16 random bytes",
        ));
    }
    Ok(value)
}

/// Generate one canonical random nonce from the operating-system RNG.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the operating system RNG fails.
pub fn generate_nonce() -> Result<String, ProtocolError> {
    let mut bytes = [0u8; NONCE_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| {
        live_error(
            LiveProtocolErrorCode::InvalidNonce,
            ["nonce"],
            "operating system random number generator failed",
        )
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Parse one canonical unsigned 64-bit wire counter.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for a sign, whitespace, leading zero,
/// fraction, overflow, or any non-decimal form.
pub fn parse_u64s(value: &str, tokens: &[&'static str]) -> Result<u64, ProtocolError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidCounter,
            tokens.iter().copied(),
            "counter must be a canonical unsigned decimal string",
        ));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidCounter,
            tokens.iter().copied(),
            "counter must not have a leading zero",
        ));
    }
    value.parse::<u64>().map_err(|_| {
        live_error(
            LiveProtocolErrorCode::InvalidCounter,
            tokens.iter().copied(),
            "counter exceeds the unsigned 64-bit range",
        )
    })
}

/// Render one unsigned 64-bit counter as its canonical wire string.
#[must_use]
pub fn u64s(value: u64) -> String {
    value.to_string()
}

/// A checked unsigned 64-bit counter in its canonical decimal wire encoding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct U64s(u64);

impl U64s {
    /// Construct a canonical counter value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the numeric value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Serialize for U64s {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for U64s {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let value = parse_u64s(&text, &["counter"]).map_err(serde::de::Error::custom)?;
        Ok(Self(value))
    }
}

/// Validate one complete opening envelope body against its protocol limit.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the body is empty or exceeds
/// [`MAX_OPEN_BODY_BYTES`].
pub fn validate_open_body(raw: &[u8]) -> Result<(), ProtocolError> {
    if raw.is_empty() || raw.len() > MAX_OPEN_BODY_BYTES {
        return Err(live_error(
            LiveProtocolErrorCode::BodyTooLarge,
            ["body"],
            "opening body must be non-empty and within the protocol limit",
        ));
    }
    Ok(())
}

/// Validate one control/offer/terminal body against its protocol limit.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the body is empty or exceeds
/// [`MAX_CONTROL_BODY_BYTES`].
pub fn validate_control_body(raw: &[u8]) -> Result<(), ProtocolError> {
    if raw.is_empty() || raw.len() > MAX_CONTROL_BODY_BYTES {
        return Err(live_error(
            LiveProtocolErrorCode::BodyTooLarge,
            ["body"],
            "control body must be non-empty and within the protocol limit",
        ));
    }
    Ok(())
}

/// Compute the negotiated application-data body limit.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when either advertised payload limit is
/// absent, invalid, or the negotiated result is below [`MIN_DATA_BODY_BYTES`].
pub fn negotiate_max_data_body_bytes(
    consumer_max_payload_bytes: u64,
    provider_max_payload_bytes: u64,
) -> Result<u64, ProtocolError> {
    let limits = [consumer_max_payload_bytes, provider_max_payload_bytes];
    if limits
        .iter()
        .any(|limit| *limit <= PROTOCOL_HEADER_RESERVE_BYTES)
    {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidDataBodyLimit,
            ["receiveMaxPayloadBytes"],
            "advertised NATS payload limit is absent or too small for the header reserve",
        ));
    }
    let negotiated = WINDOW_BYTES.min(
        limits
            .iter()
            .map(|limit| limit - PROTOCOL_HEADER_RESERVE_BYTES)
            .min()
            .expect("two limits"),
    );
    if negotiated < MIN_DATA_BODY_BYTES {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidDataBodyLimit,
            ["receiveMaxPayloadBytes"],
            "negotiated data body limit is below the protocol minimum",
        ));
    }
    Ok(negotiated)
}

/// Live opening request sent to the unchanged bound Live base subject.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveOpen {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `open` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOpenKind,
    /// Consumer-generated nonce binding this logical open attempt.
    pub open_id: String,
    /// Consumer's actual connected NATS `max_payload`.
    pub receive_max_payload_bytes: u64,
    /// Native domain input decoded by its generated codec.
    pub input: Value,
}

/// Discriminant for [`LiveOpen`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveOpenKind {
    /// Literal `open`.
    Open,
}

/// Observation envelope embedded in an Operation watch control request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OperationObservationOpen {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `open` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOpenKind,
    /// Consumer-generated nonce binding this logical open attempt.
    pub open_id: String,
    /// Consumer's actual connected NATS `max_payload`.
    pub receive_max_payload_bytes: u64,
}

/// Operation watch opening control request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OperationWatchOpen {
    /// Literal `watch` action.
    pub action: OperationWatchAction,
    /// Existing durable operation identifier.
    pub operation_id: String,
    /// Whether the caller requests transient update envelopes.
    #[serde(default)]
    pub include_updates: Option<bool>,
    /// Live observation envelope.
    pub observation: OperationObservationOpen,
}

/// Discriminant for [`OperationWatchOpen`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationWatchAction {
    /// Literal `watch`.
    Watch,
}

/// Pinned provider identity carried by a signed offer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveOfferProvider {
    /// Provider logical runtime connection id from its signed context.
    pub connection_id: String,
    /// Provider runtime signing public key.
    pub session_key: String,
    /// Provider principal id.
    pub principal_id: String,
    /// Provider participant id.
    pub participant_id: String,
    /// Provider deployment id.
    pub deployment_id: String,
    /// Provider instance id.
    pub instance_id: String,
}

/// Pinned consumer identity carried by a signed offer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveOfferConsumer {
    /// Consumer logical runtime connection id.
    pub connection_id: String,
    /// Consumer runtime signing public key.
    pub session_key: String,
    /// Consumer principal id.
    pub principal_id: String,
    /// Consumer participant id.
    pub participant_id: String,
}

/// Fixed v1 session limits carried by a signed offer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveOfferLimits {
    /// Negotiated maximum encoded data-frame body size.
    pub max_data_body_bytes: u64,
    /// Maximum unconsumed application frames.
    pub window_frames: u64,
    /// Maximum body bytes of those frames.
    pub window_bytes: u64,
    /// Reservation deadline in milliseconds.
    pub reservation_ms: u64,
    /// Heartbeat interval in milliseconds.
    pub heartbeat_interval_ms: u64,
    /// Peer inactivity deadline in milliseconds.
    pub peer_inactivity_ms: u64,
    /// Consumer stall deadline in milliseconds.
    pub consumer_stall_ms: u64,
}

/// Signed provider session offer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveOffer {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `offer` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Live or Operation watch observation.
    #[serde(rename = "kind")]
    pub session_kind: LiveSessionKind,
    /// Consumer nonce this offer answers.
    pub open_id: String,
    /// Exact request-id header of the opening request.
    pub request_id: String,
    /// Provider-generated session identifier.
    pub session_id: String,
    /// Descriptor/binding-derived base subject.
    pub base_subject: String,
    /// Exact live delivery subject.
    pub data_subject: String,
    /// Exact owner-directed control subject.
    pub control_subject: String,
    /// Pinned provider identity.
    pub provider: LiveOfferProvider,
    /// Pinned consumer identity.
    pub consumer: LiveOfferConsumer,
    /// Fixed v1 session limits.
    pub limits: LiveOfferLimits,
}

/// Discriminant for provider-origin bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveOfferKind {
    /// Literal `offer`.
    Offer,
    /// Literal `open-error`.
    OpenError,
    /// Literal `control-ack`.
    ControlAck,
    /// Literal `control-error`.
    ControlError,
    /// Literal `data`.
    Data,
    /// Literal `challenge`.
    Challenge,
    /// Literal `end`.
    End,
}

/// Kind of live observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveSessionKind {
    /// Standalone live observation.
    Standalone,
    /// Operation live observation.
    Operation,
}

impl LiveSessionKind {
    /// Return the exact wire suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Operation => "operation",
        }
    }
}

/// Shared prefix of every consumer control body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlBase {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveControlKind,
    /// Session identifier this control addresses.
    pub session_id: String,
    /// Logical idempotence sequence.
    pub control_seq: U64s,
}

/// Discriminant for consumer control bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveControlKind {
    /// Literal `control`.
    Control,
}

/// Consumer control activating the prepared subscription.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlActivate {
    /// Literal `activate` action.
    pub action: ActivateAction,
    /// Starting received cursor, always zero.
    pub received_seq: U64s,
    /// Starting consumed cursor, always zero.
    pub consumed_seq: U64s,
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveControlKind,
    /// Session identifier this control addresses.
    pub session_id: String,
    /// Logical idempotence sequence.
    pub control_seq: U64s,
}

/// Discriminant for `activate`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivateAction {
    /// Literal `activate`.
    Activate,
}

/// Consumer control answering an outstanding delivery challenge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlPulse {
    /// Literal `pulse` action.
    pub action: PulseAction,
    /// Challenge identifier being answered.
    pub challenge_id: String,
    /// Received cursor.
    pub received_seq: U64s,
    /// Consumed cursor.
    pub consumed_seq: U64s,
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveControlKind,
    /// Session identifier this control addresses.
    pub session_id: String,
    /// Logical idempotence sequence.
    pub control_seq: U64s,
}

/// Discriminant for `pulse`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PulseAction {
    /// Literal `pulse`.
    Pulse,
}

/// Consumer control conveying accumulated consumption credit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlCredit {
    /// Literal `ack` action.
    pub action: AckAction,
    /// Received cursor.
    pub received_seq: U64s,
    /// Consumed cursor.
    pub consumed_seq: U64s,
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveControlKind,
    /// Session identifier this control addresses.
    pub session_id: String,
    /// Logical idempotence sequence.
    pub control_seq: U64s,
}

/// Discriminant for `ack`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AckAction {
    /// Literal `ack`.
    Ack,
}

/// Consumer control explicitly closing the observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlClose {
    /// Literal `close` action.
    pub action: CloseAction,
    /// Local close reason.
    pub reason: ConsumerCloseReason,
    /// Received cursor.
    pub received_seq: U64s,
    /// Consumed cursor.
    pub consumed_seq: U64s,
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveControlKind,
    /// Session identifier this control addresses.
    pub session_id: String,
    /// Logical idempotence sequence.
    pub control_seq: U64s,
}

/// Discriminant for `close`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseAction {
    /// Literal `close`.
    Close,
}

/// Consumer control acknowledging a verified terminal sequence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlEndAck {
    /// Literal `end-ack` action.
    pub action: EndAckAction,
    /// Final sequence acknowledged.
    pub final_seq: U64s,
    /// Received cursor.
    pub received_seq: U64s,
    /// Consumed cursor.
    pub consumed_seq: U64s,
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveControlKind,
    /// Session identifier this control addresses.
    pub session_id: String,
    /// Logical idempotence sequence.
    pub control_seq: U64s,
}

/// Discriminant for `end-ack`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndAckAction {
    /// Literal `end-ack`.
    EndAck,
}

/// One validated consumer control.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveControl {
    /// Activate the prepared subscription.
    Activate(LiveControlActivate),
    /// Answer an outstanding challenge.
    Pulse(LiveControlPulse),
    /// Convey accumulated credit.
    Ack(LiveControlCredit),
    /// Explicitly close the observation.
    Close(LiveControlClose),
    /// Acknowledge a verified terminal sequence.
    EndAck(LiveControlEndAck),
}

impl LiveControl {
    /// Return the logical idempotence sequence.
    #[must_use]
    pub const fn control_seq(&self) -> U64s {
        match self {
            Self::Activate(control) => control.control_seq,
            Self::Pulse(control) => control.control_seq,
            Self::Ack(control) => control.control_seq,
            Self::Close(control) => control.control_seq,
            Self::EndAck(control) => control.control_seq,
        }
    }

    /// Return the session identifier.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Activate(control) => &control.session_id,
            Self::Pulse(control) => &control.session_id,
            Self::Ack(control) => &control.session_id,
            Self::Close(control) => &control.session_id,
            Self::EndAck(control) => &control.session_id,
        }
    }

    /// Return the exact wire action name.
    #[must_use]
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Activate(_) => "activate",
            Self::Pulse(_) => "pulse",
            Self::Ack(_) => "ack",
            Self::Close(_) => "close",
            Self::EndAck(_) => "end-ack",
        }
    }

    /// Return the received cursor this control reports, when it carries one.
    #[must_use]
    pub const fn received_seq(&self) -> Option<U64s> {
        match self {
            Self::Activate(_) => None,
            Self::Pulse(control) => Some(control.received_seq),
            Self::Ack(control) => Some(control.received_seq),
            Self::Close(control) => Some(control.received_seq),
            Self::EndAck(control) => Some(control.received_seq),
        }
    }

    /// Return the consumed cursor this control reports, when it carries one.
    #[must_use]
    pub const fn consumed_seq(&self) -> Option<U64s> {
        match self {
            Self::Activate(_) => None,
            Self::Pulse(control) => Some(control.consumed_seq),
            Self::Ack(control) => Some(control.consumed_seq),
            Self::Close(control) => Some(control.consumed_seq),
            Self::EndAck(control) => Some(control.consumed_seq),
        }
    }

    /// Validate every counter and nonce in this control.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Live`] for an invalid cursor relationship,
    /// session id, challenge id, or activate invariant.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        parse_nonce(self.session_id(), ["sessionId"])?;
        match self {
            Self::Activate(control) => {
                if control.received_seq.get() != 0 || control.consumed_seq.get() != 0 {
                    return Err(live_error(
                        LiveProtocolErrorCode::InvalidCounter,
                        ["activate"],
                        "activation cursors must both be zero",
                    ));
                }
            }
            Self::Pulse(control) => {
                parse_nonce(&control.challenge_id, ["challengeId"])?;
                validate_cursors(control.received_seq, control.consumed_seq)?;
            }
            Self::Ack(control) => {
                validate_cursors(control.received_seq, control.consumed_seq)?;
            }
            Self::Close(control) => {
                validate_cursors(control.received_seq, control.consumed_seq)?;
            }
            Self::EndAck(control) => {
                validate_cursors(control.received_seq, control.consumed_seq)?;
                if control.final_seq.get() < control.received_seq.get() {
                    return Err(live_error(
                        LiveProtocolErrorCode::InvalidCounter,
                        ["finalSeq"],
                        "final sequence cannot be below the received cursor",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_logical_identity(value: &str, field: &'static str) -> Result<(), ProtocolError> {
    if value.is_empty() || value.trim() != value {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidFormat,
            [field],
            "identity must be a non-empty logical identifier without surrounding whitespace",
        ));
    }
    Ok(())
}

fn validate_cursors(received: U64s, consumed: U64s) -> Result<(), ProtocolError> {
    if consumed.get() > received.get() {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidCounter,
            ["consumedSeq"],
            "consumed cursor cannot exceed the received cursor",
        ));
    }
    Ok(())
}

/// Parse and validate one strict JSON control body.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for malformed JSON, unknown fields, an
/// unsupported format, or an invalid counter/nonce.
pub fn parse_live_control(raw: &[u8]) -> Result<LiveControl, ProtocolError> {
    validate_control_body(raw)?;
    let value: Value = serde_json::from_slice(raw)?;
    let format = value
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if format != LIVE_VERSION {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidFormat,
            ["format"],
            "control body must carry the exact live protocol discriminator",
        ));
    }
    let action = value
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let control = match action.as_str() {
        "activate" => LiveControl::Activate(serde_json::from_value(value)?),
        "pulse" => LiveControl::Pulse(serde_json::from_value(value)?),
        "ack" => LiveControl::Ack(serde_json::from_value(value)?),
        "close" => LiveControl::Close(serde_json::from_value(value)?),
        "end-ack" => LiveControl::EndAck(serde_json::from_value(value)?),
        _ => {
            return Err(live_error(
                LiveProtocolErrorCode::InvalidFormat,
                ["action"],
                "control body carries an unknown action",
            ));
        }
    };
    control.validate()?;
    Ok(control)
}

/// Signed provider control response or terminal acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlAck {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control-ack` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Session identifier this response addresses.
    pub session_id: String,
    /// Logical idempotence sequence being answered.
    pub control_seq: U64s,
    /// Exact request-id header of the control request.
    pub request_id: String,
    /// Action being acknowledged.
    pub action: LiveControlAckAction,
    /// Current provider session state.
    pub state: LiveSessionState,
    /// Highest received cursor accepted so far.
    pub accepted_received_seq: U64s,
    /// Highest consumed cursor accepted so far.
    pub accepted_consumed_seq: U64s,
    /// Terminal record when the session is closed.
    pub terminal: Option<WireTerminal>,
    /// Owned producer cleanup result, when known.
    pub cleanup: Option<CleanupStatus>,
}

/// Action field of a control acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveControlAckAction {
    /// Activation acknowledged.
    Activate,
    /// Challenge response acknowledged.
    Pulse,
    /// Credit acknowledged.
    Ack,
    /// Close acknowledged.
    Close,
    /// Terminal sequence acknowledged.
    #[serde(rename = "end-ack")]
    EndAck,
}

/// Observable provider session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveSessionState {
    /// Reserved and challenged, not yet delivery-verified.
    Activating,
    /// Delivery path verified and source running.
    Active,
    /// Closed.
    Closed,
}

/// Reported result of owned producer cleanup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupStatus {
    /// Every owned resource settled.
    Complete,
    /// The cleanup grace elapsed with owned work unsettled.
    Incomplete,
}

/// Signed provider error response to a control.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveControlError {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `control-error` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Session identifier this response addresses.
    pub session_id: String,
    /// Logical idempotence sequence being answered.
    pub control_seq: U64s,
    /// Exact request-id header of the control request.
    pub request_id: String,
    /// Stable wire error code.
    pub code: LiveErrorCode,
}

/// Bounded, signed opening error envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveOpenError {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `open-error` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Consumer nonce this error answers, when recognizable.
    pub open_id: Option<String>,
    /// Exact request-id header of the opening request.
    pub request_id: String,
    /// Stable wire error code.
    pub code: LiveErrorCode,
    /// Sanitized diagnostic.
    pub message: String,
}

/// Bounded error envelope carried by a terminal frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WireTerminalError {
    /// Stable wire error code.
    pub code: LiveErrorCode,
    /// Sanitized diagnostic.
    pub message: String,
    /// Trace id when one is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// Terminal record describing why a session ended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WireTerminal {
    /// Committed terminal reason.
    pub reason: LiveEndReason,
    /// Bounded error envelope, absent for normal completion.
    pub error: Option<WireTerminalError>,
}

impl WireTerminal {
    /// Construct one sanitized normal-completion terminal record.
    #[must_use]
    pub const fn complete() -> Self {
        Self {
            reason: LiveEndReason::Complete,
            error: None,
        }
    }

    /// Validate the reason/error pairing and message bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Live`] when a normal reason carries an error, a
    /// failure reason omits one, or a message exceeds its redaction bounds.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let normal = matches!(
            self.reason,
            LiveEndReason::Complete | LiveEndReason::Cancelled | LiveEndReason::LocalShutdown
        );
        match (normal, self.error.as_ref()) {
            (true, None) => Ok(()),
            (false, Some(error)) => {
                if error.message.len() > MAXIMUM_REASON_MESSAGE_BYTES
                    || error.message.chars().count() > MAXIMUM_REASON_MESSAGE_SCALARS
                    || error.message.contains('\0')
                {
                    return Err(live_error(
                        LiveProtocolErrorCode::InvalidFormat,
                        ["terminal", "error", "message"],
                        "terminal message exceeds its redaction bounds",
                    ));
                }
                Ok(())
            }
            _ => Err(live_error(
                LiveProtocolErrorCode::InvalidFormat,
                ["terminal"],
                "normal reasons carry no error and failure reasons require one",
            )),
        }
    }
}

/// One signed provider data-channel frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveFrame {
    /// One application-domain value.
    Data(LiveDataFrame),
    /// Delivery-path challenge.
    Challenge(LiveChallengeFrame),
    /// Terminal frame after the final application sequence.
    End(LiveEndFrame),
}

impl LiveFrame {
    /// Return the session identifier this frame addresses.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Data(frame) => &frame.session_id,
            Self::Challenge(frame) => &frame.session_id,
            Self::End(frame) => &frame.session_id,
        }
    }
}

/// One application-domain data frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveDataFrame {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `data` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Session identifier.
    pub session_id: String,
    /// Contiguous application sequence starting at one.
    pub seq: U64s,
    /// Native domain value or validated Operation envelope.
    pub value: Value,
}

/// One delivery-path challenge frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveChallengeFrame {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `challenge` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Session identifier.
    pub session_id: String,
    /// Challenge identifier being posed.
    pub challenge_id: String,
    /// Highest application sequence sent so far.
    pub last_sent_seq: U64s,
}

/// One terminal frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LiveEndFrame {
    /// Exact protocol discriminator.
    pub format: String,
    /// Literal `end` discriminant.
    #[serde(rename = "type")]
    pub kind: LiveOfferKind,
    /// Session identifier.
    pub session_id: String,
    /// Final application sequence.
    pub final_seq: U64s,
    /// Committed terminal record.
    pub terminal: WireTerminal,
}

/// Parse one strict JSON provider data-channel frame.
///
/// Raw input is pre-bounded by the larger of the negotiated application-data
/// limit and the protocol-control limit, then the tighter control limit is
/// applied to CHALLENGE/END and the negotiated limit to DATA. Opening requests
/// are bounded separately by [`validate_open_body`]; a DATA body may legally
/// exceed [`MAX_OPEN_BODY_BYTES`] when the negotiated window permits it.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an oversize body, malformed JSON, unknown
/// fields, an unsupported format, or an invalid nonce/counter.
pub fn parse_live_frame(raw: &[u8], max_data_body_bytes: u64) -> Result<LiveFrame, ProtocolError> {
    let pre_bound = max_data_body_bytes.max(MAX_CONTROL_BODY_BYTES as u64);
    if raw.is_empty() || raw.len() as u64 > pre_bound {
        return Err(body_too_large());
    }
    let value: Value = serde_json::from_slice(raw)?;
    let format = value
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if format != LIVE_VERSION {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidFormat,
            ["format"],
            "frame must carry the exact live protocol discriminator",
        ));
    }
    let frame = match value.get("type").and_then(Value::as_str) {
        Some("data") => {
            if raw.len() as u64 > max_data_body_bytes {
                return Err(body_too_large());
            }
            LiveFrame::Data(serde_json::from_value(value)?)
        }
        Some("challenge") => {
            if raw.len() > MAX_CONTROL_BODY_BYTES {
                return Err(body_too_large());
            }
            LiveFrame::Challenge(serde_json::from_value(value)?)
        }
        Some("end") => {
            if raw.len() > MAX_CONTROL_BODY_BYTES {
                return Err(body_too_large());
            }
            LiveFrame::End(serde_json::from_value(value)?)
        }
        _ => {
            return Err(live_error(
                LiveProtocolErrorCode::InvalidFormat,
                ["type"],
                "frame carries an unknown discriminant",
            ));
        }
    };
    match &frame {
        LiveFrame::Data(frame) => {
            parse_nonce(&frame.session_id, ["sessionId"])?;
        }
        LiveFrame::Challenge(frame) => {
            parse_nonce(&frame.session_id, ["sessionId"])?;
            parse_nonce(&frame.challenge_id, ["challengeId"])?;
        }
        LiveFrame::End(frame) => {
            parse_nonce(&frame.session_id, ["sessionId"])?;
            frame.terminal.validate()?;
        }
    }
    Ok(frame)
}

/// Build one bounded frame-body error.
fn body_too_large() -> ProtocolError {
    live_error(
        LiveProtocolErrorCode::BodyTooLarge,
        ["body"],
        "frame body must be non-empty and within its protocol limit",
    )
}

/// Derive the exact live delivery subject for one observation.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when either connection id is not the
/// canonical subject-token encoding or the session id is not a random nonce.
pub fn derive_live_data_subject(
    provider_connection_id: &str,
    consumer_connection_id: &str,
    session_id: &str,
) -> Result<String, ProtocolError> {
    validate_logical_identity(provider_connection_id, "providerConnectionId")?;
    validate_logical_identity(consumer_connection_id, "consumerConnectionId")?;
    let provider = subjects::encode_subject_token(provider_connection_id);
    let consumer = subjects::encode_subject_token(consumer_connection_id);
    let session = parse_nonce(session_id, ["sessionId"])?;
    Ok(format!("live.v1.data.{provider}.{consumer}.{session}"))
}

/// Derive the exact owner-directed control subject for one session.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an invalid base subject, connection id,
/// or session id.
pub fn derive_live_observe_subject(
    base_subject: &str,
    provider_connection_id: &str,
    session_id: &str,
) -> Result<String, ProtocolError> {
    validate_base_subject(base_subject, ["baseSubject"])?;
    validate_logical_identity(provider_connection_id, "providerConnectionId")?;
    let provider = subjects::encode_subject_token(provider_connection_id);
    let session = parse_nonce(session_id, ["sessionId"])?;
    Ok(format!("{base_subject}.observe.{provider}.{session}"))
}

/// Derive the nonqueued owner-control subscription for one provider connection.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an invalid base subject or connection id.
pub fn derive_live_observe_wildcard_subject(
    base_subject: &str,
    provider_connection_id: &str,
) -> Result<String, ProtocolError> {
    validate_base_subject(base_subject, ["baseSubject"])?;
    validate_logical_identity(provider_connection_id, "providerConnectionId")?;
    let provider = subjects::encode_subject_token(provider_connection_id);
    Ok(format!("{base_subject}.observe.{provider}.*"))
}

/// Validate that one base subject is a deployment-bound standalone live or
/// Operation route.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] unless the subject is exactly
/// `live.v1.route.<token>.<token>.<action>` or
/// `operation.v1.<token>.<token>.<action>` with a valid logical action.
pub fn validate_base_subject(
    base_subject: &str,
    tokens: impl IntoIterator<Item = &'static str> + Clone,
) -> Result<&str, ProtocolError> {
    let invalid = || {
        live_error(
            LiveProtocolErrorCode::InvalidSubjectToken,
            tokens.clone(),
            "base subject must be a canonical deployment-bound live or Operation route",
        )
    };
    let parts = base_subject.split('.').collect::<Vec<_>>();
    let prefix_len = match parts.first() {
        Some(&"live") if parts.get(1) == Some(&"v1") && parts.get(2) == Some(&"route") => 3,
        Some(&"operation") if parts.get(1) == Some(&"v1") => 2,
        _ => return Err(invalid()),
    };
    if parts.len() < prefix_len + 3 {
        return Err(invalid());
    }
    let api_token = parts[prefix_len];
    let deployment_token = parts[prefix_len + 1];
    let action = parts[prefix_len + 2..].join(".");
    validate_subject_token(api_token, ["baseSubject"])?;
    validate_subject_token(deployment_token, ["baseSubject"])?;
    validate_logical_name(&action).map_err(|_| invalid())?;
    Ok(base_subject)
}

/// Validate that one subject is an exact live delivery or owner-control route.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the subject is not one of the two
/// canonical live families with valid tokens.
pub fn validate_live_subject(subject: &str) -> Result<(), ProtocolError> {
    let invalid = |field: &'static str| {
        live_error(
            LiveProtocolErrorCode::InvalidSubjectToken,
            [field],
            "subject is not a canonical live-session route",
        )
    };
    let tokens = subject.split('.').collect::<Vec<_>>();
    if tokens.len() == 6
        && tokens[0] == "live"
        && tokens[1] == "v1"
        && tokens[2] == "data"
        && is_canonical_token(tokens[3])
        && is_canonical_token(tokens[4])
        && parse_nonce(tokens[5], ["sessionId"]).is_ok()
    {
        return Ok(());
    }
    match tokens.first() {
        Some(&"live") | Some(&"operation") => {
            // A base action may itself contain dots, so locate the observe
            // marker relative to the fixed trailing provider/session tokens.
            let Some(observe_index) = tokens.len().checked_sub(3) else {
                return Err(invalid("subject"));
            };
            if tokens[observe_index] != "observe" {
                return Err(invalid("subject"));
            }
            let base = tokens[..observe_index].join(".");
            validate_base_subject(&base, ["subject"])?;
            if parse_nonce(tokens[observe_index + 2], ["sessionId"]).is_ok()
                && validate_subject_token(tokens[observe_index + 1], ["subject"]).is_ok()
            {
                Ok(())
            } else {
                Err(invalid("subject"))
            }
        }
        _ => Err(invalid("subject")),
    }
}

fn push_length_prefixed(bytes: &mut Vec<u8>, component: &[u8]) -> Result<(), ProtocolError> {
    let length = u32::try_from(component.len()).map_err(|_| {
        live_error(
            LiveProtocolErrorCode::InvalidFormat,
            ["proof"],
            "proof component exceeds the unsigned 32-bit length-prefix range",
        )
    })?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(component);
    Ok(())
}

/// Canonical provider server-message proof input and digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveServerProofInput {
    bytes: Vec<u8>,
    digest: [u8; 32],
}

impl LiveServerProofInput {
    /// Return the exact length-prefixed proof input bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the SHA-256 proof digest signed by the provider key.
    #[must_use]
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// One unpadded base64url Ed25519 provider server-message proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveServerProof(String);

impl LiveServerProof {
    /// Parse and strictly validate an encoded provider proof.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Live`] unless the value canonically encodes one
    /// Ed25519 signature.
    pub fn parse(encoded: impl Into<String>) -> Result<Self, ProtocolError> {
        let encoded = encoded.into();
        decode_base64url::<64>(
            &encoded,
            &["trellis-live-proof"],
            LiveProtocolErrorCode::InvalidSignature,
        )?;
        Ok(Self(encoded))
    }

    /// Return the encoded proof.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Build provider server-message proof input from the exact received values.
///
/// The body hash is computed internally from `raw_body`.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the context digest is not canonical
/// base64url or a component exceeds the length-prefix range.
pub fn build_live_server_proof_input(
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
) -> Result<LiveServerProofInput, ProtocolError> {
    let context_digest = decode_base64url::<32>(
        context_digest,
        &["authorization-context"],
        LiveProtocolErrorCode::InvalidFormat,
    )?;
    let mut bytes = Vec::new();
    push_length_prefixed(&mut bytes, LIVE_SERVER_PROOF_DOMAIN_V1.as_bytes())?;
    push_length_prefixed(&mut bytes, &context_digest)?;
    push_length_prefixed(&mut bytes, subject.as_bytes())?;
    push_length_prefixed(&mut bytes, &sha256(raw_body))?;
    let digest = sha256(&bytes);
    Ok(LiveServerProofInput { bytes, digest })
}

/// Sign one provider-origin message with the provider runtime key.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an invalid context digest or oversized
/// proof component.
pub fn sign_live_server_proof(
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
    provider_key: &SigningKey,
) -> Result<LiveServerProof, ProtocolError> {
    let input = build_live_server_proof_input(context_digest, subject, raw_body)?;
    Ok(LiveServerProof(
        URL_SAFE_NO_PAD.encode(provider_key.sign(input.digest()).to_bytes()),
    ))
}

/// Verify one provider-origin message against the pinned provider key.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an invalid context digest or a signature
/// that does not bind the exact subject and raw body bytes.
pub fn verify_live_server_proof(
    proof: &LiveServerProof,
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
    provider_key: &VerifyingKey,
) -> Result<(), ProtocolError> {
    let input = build_live_server_proof_input(context_digest, subject, raw_body)?;
    let proof_bytes = decode_base64url::<64>(
        proof.as_str(),
        &["trellis-live-proof"],
        LiveProtocolErrorCode::InvalidSignature,
    )?;
    provider_key
        .verify_strict(input.digest(), &Signature::from_bytes(&proof_bytes))
        .map_err(|_| {
            live_error(
                LiveProtocolErrorCode::InvalidSignature,
                ["trellis-live-proof"],
                "provider server-message signature verification failed",
            )
        })
}

/// Verify one provider-origin message with an encoded provider public key.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an invalid key encoding, context
/// digest, or signature that does not bind the exact subject and body bytes.
pub fn verify_live_server_proof_encoded(
    proof: &LiveServerProof,
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
    provider_key: &str,
) -> Result<(), ProtocolError> {
    let key_bytes = decode_base64url::<32>(
        provider_key,
        &["session-key"],
        LiveProtocolErrorCode::InvalidSignature,
    )?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| {
        live_error(
            LiveProtocolErrorCode::InvalidSignature,
            ["session-key"],
            "provider runtime public key is malformed",
        )
    })?;
    verify_live_server_proof(proof, context_digest, subject, raw_body, &key)
}

/// Return the canonical encoded proof digest for exact subject and body bytes.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for an invalid context digest or oversized
/// proof component.
pub fn live_server_proof_digest(
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
) -> Result<String, ProtocolError> {
    let input = build_live_server_proof_input(context_digest, subject, raw_body)?;
    Ok(URL_SAFE_NO_PAD.encode(input.digest()))
}

/// Inputs identifying one logical open attempt for duplicate detection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalOpenIdentity {
    /// Live or Operation watch observation.
    pub kind: LiveSessionKind,
    /// Descriptor/binding-derived base subject.
    pub base_subject: String,
    /// Consumer nonce binding this logical open attempt.
    pub open_id: String,
    /// Consumer logical connection id.
    pub consumer_connection_id: String,
    /// Consumer runtime signing public key.
    pub consumer_session_key: String,
    /// Consumer principal id.
    pub consumer_principal_id: String,
    /// Consumer participant id.
    pub consumer_participant_id: String,
    /// Consumer's advertised NATS payload limit.
    pub receive_max_payload_bytes: u64,
    /// Native Live input, when this is a Live open.
    pub live_input: Option<Value>,
    /// Durable Operation id, when this is an Operation watch open.
    pub operation_id: Option<String>,
    /// Whether transient updates were requested, when this is a watch open.
    pub include_updates: Option<bool>,
}

/// Compute the canonical logical-open hash.
///
/// The hash excludes the transport request id and reply subject so a duplicate
/// open attempt with fresh transport metadata returns the same reservation.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] for invalid identities or JSON encoding.
pub fn logical_open_hash(identity: &LogicalOpenIdentity) -> Result<String, ProtocolError> {
    parse_nonce(&identity.open_id, ["openId"])?;
    validate_logical_identity(&identity.consumer_connection_id, "consumerConnectionId")?;
    validate_subject_token(&identity.consumer_session_key, ["consumerSessionKey"])?;
    validate_logical_identity(&identity.consumer_principal_id, "consumerPrincipalId")?;
    validate_logical_identity(&identity.consumer_participant_id, "consumerParticipantId")?;
    if identity.kind == LiveSessionKind::Standalone && identity.live_input.is_none() {
        return Err(live_error(
            LiveProtocolErrorCode::InvalidFormat,
            ["input"],
            "Live open identity requires the native input value",
        ));
    }
    if identity.kind == LiveSessionKind::Operation {
        let operation_id = identity.operation_id.as_deref().unwrap_or_default();
        if operation_id.is_empty() {
            return Err(live_error(
                LiveProtocolErrorCode::InvalidFormat,
                ["operationId"],
                "Operation watch identity requires a durable operation id",
            ));
        }
    }
    let value = serde_json::json!({
        "kind": identity.kind.as_str(),
        "baseSubject": identity.base_subject,
        "openId": identity.open_id,
        "receiveMaxPayloadBytes": identity.receive_max_payload_bytes,
        "consumer": {
            "connectionId": identity.consumer_connection_id,
            "sessionKey": identity.consumer_session_key,
            "principalId": identity.consumer_principal_id,
            "participantId": identity.consumer_participant_id,
        },
        "input": identity.live_input,
        "operationId": identity.operation_id,
        "includeUpdates": identity.include_updates.unwrap_or(false),
    });
    crate::digest_json(&value)
}

/// Compute the canonical logical-control hash used for idempotence.
///
/// # Errors
///
/// Returns [`ProtocolError::Live`] when the control body cannot be represented
/// as canonical JSON.
pub fn logical_control_hash(control: &LiveControl) -> Result<String, ProtocolError> {
    let value = match control {
        LiveControl::Activate(body) => serde_json::to_value(body)?,
        LiveControl::Pulse(body) => serde_json::to_value(body)?,
        LiveControl::Ack(body) => serde_json::to_value(body)?,
        LiveControl::Close(body) => serde_json::to_value(body)?,
        LiveControl::EndAck(body) => serde_json::to_value(body)?,
    };
    crate::digest_json(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode_subject_token;

    fn provider_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn context_digest() -> String {
        URL_SAFE_NO_PAD.encode([9u8; 32])
    }

    #[test]
    fn nonces_are_exactly_22_canonical_characters() {
        let nonce = generate_nonce().unwrap();
        assert_eq!(nonce.len(), 22);
        assert_eq!(parse_nonce(&nonce, ["sessionId"]).unwrap(), nonce);
        assert!(parse_nonce("short", ["sessionId"]).is_err());
        assert!(parse_nonce(&format!("{nonce}="), ["sessionId"]).is_err());
        assert!(parse_nonce(&"a".repeat(22), ["sessionId"]).is_err());
    }

    #[test]
    fn counters_reject_non_canonical_forms() {
        assert_eq!(parse_u64s("0", &["seq"]).unwrap(), 0);
        assert_eq!(parse_u64s("1", &["seq"]).unwrap(), 1);
        assert_eq!(
            parse_u64s("18446744073709551615", &["seq"]).unwrap(),
            u64::MAX
        );
        for invalid in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1.0",
            "0x1",
            "18446744073709551616",
        ] {
            assert!(parse_u64s(invalid, &["seq"]).is_err(), "{invalid}");
        }
    }

    #[test]
    fn subject_derivation_matches_the_wire_grammar() {
        let session = URL_SAFE_NO_PAD.encode([3u8; 16]);
        let provider = "01JYPROVIDER0000000000000";
        let consumer = "01JYCONSUMER0000000000000";
        let provider_token = encode_subject_token(provider);
        let consumer_token = encode_subject_token(consumer);
        assert_eq!(
            derive_live_data_subject(provider, consumer, &session).unwrap(),
            format!("live.v1.data.{provider_token}.{consumer_token}.{session}")
        );
        let base = "live.v1.route.YWNtZS5oZWFsdGhAdjE.ZGVwLTAx.Watch";
        assert_eq!(
            derive_live_observe_subject(base, provider, &session).unwrap(),
            format!("{base}.observe.{provider_token}.{session}")
        );
        assert_eq!(
            derive_live_observe_wildcard_subject(base, provider).unwrap(),
            format!("{base}.observe.{provider_token}.*")
        );
        assert!(derive_live_data_subject("", consumer, &session).is_err());
        assert!(derive_live_observe_subject("events.v1.X.Watch", provider, &session).is_err());
    }

    #[test]
    fn live_subject_validation_accepts_only_canonical_routes() {
        let session = URL_SAFE_NO_PAD.encode([4u8; 16]);
        let provider_token = encode_subject_token("provider-connection");
        let consumer_token = encode_subject_token("consumer-connection");
        assert!(validate_live_subject(
            &derive_live_data_subject("provider-connection", "consumer-connection", &session)
                .unwrap()
        )
        .is_ok());
        let base = "operation.v1.YXBpQHYx.ZGVwLTAx.Refund.Start";
        assert!(validate_live_subject(
            &derive_live_observe_subject(base, "provider-connection", &session).unwrap()
        )
        .is_ok());
        assert!(validate_live_subject("live.v1.data.*.consumer.session").is_err());
        assert!(validate_live_subject("events.v1.X.Y").is_err());
        assert!(validate_live_subject(&format!(
            "live.v1.data.{provider_token}.{consumer_token}.short"
        ))
        .is_err());
    }

    #[test]
    fn matches_pinned_cross_language_proof_vectors() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use std::fs;

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../conformance/live-protocol/vectors.json");
        let fixtures: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let key = SigningKey::from_bytes(&[42u8; 32]);
        for fixture in fixtures {
            let name = fixture["name"].as_str().unwrap();
            let context_digest = fixture["contextDigest"].as_str().unwrap();
            let subject = fixture["subject"].as_str().unwrap();
            let body = fixture["bodyUtf8"].as_str().unwrap().as_bytes();
            let expected = fixture["proof"].as_str().unwrap();
            let proof = sign_live_server_proof(context_digest, subject, body, &key).unwrap();
            assert_eq!(proof.as_str(), expected, "{name}");
            verify_live_server_proof(&proof, context_digest, subject, body, &key.verifying_key())
                .unwrap();
            verify_live_server_proof_encoded(
                &proof,
                context_digest,
                subject,
                body,
                &URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes()),
            )
            .unwrap();
            assert!(
                verify_live_server_proof(
                    &proof,
                    context_digest,
                    "other",
                    body,
                    &key.verifying_key()
                )
                .is_err(),
                "{name}: subject must bind"
            );
        }
    }

    #[test]
    fn provider_proof_binds_subject_body_and_context() {
        let proof = sign_live_server_proof(
            &context_digest(),
            "live.v1.data.a.b.c",
            b"{}",
            &provider_key(),
        )
        .unwrap();
        let key = provider_key().verifying_key();
        assert!(verify_live_server_proof(
            &proof,
            &context_digest(),
            "live.v1.data.a.b.c",
            b"{}",
            &key
        )
        .is_ok());
        assert!(verify_live_server_proof(
            &proof,
            &context_digest(),
            "live.v1.data.a.b.other",
            b"{}",
            &key
        )
        .is_err());
        assert!(verify_live_server_proof(
            &proof,
            &context_digest(),
            "live.v1.data.a.b.c",
            b"{\"x\":1}",
            &key
        )
        .is_err());
        assert!(verify_live_server_proof(
            &proof,
            &URL_SAFE_NO_PAD.encode([8u8; 32]),
            "live.v1.data.a.b.c",
            b"{}",
            &key
        )
        .is_err());
        let other = SigningKey::from_bytes(&[11; 32]).verifying_key();
        assert!(verify_live_server_proof(
            &proof,
            &context_digest(),
            "live.v1.data.a.b.c",
            b"{}",
            &other
        )
        .is_err());
    }

    #[test]
    fn control_parsing_is_strict_and_ordered() {
        let session = URL_SAFE_NO_PAD.encode([5u8; 16]);
        let body = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"control","sessionId":"{session}","controlSeq":"2","action":"ack","receivedSeq":"4","consumedSeq":"4"}}"#
        );
        let control = parse_live_control(body.as_bytes()).unwrap();
        assert_eq!(control.control_seq(), U64s::new(2));
        assert_eq!(control.received_seq(), Some(U64s::new(4)));
        assert_eq!(control.action(), "ack");

        for invalid in [
            body.replace("\"2\"", "\"02\""),
            body.replace("\"4\"", "\"-1\""),
            body.replace("\"4\",\"consumedSeq\":\"4\"", "\"4\",\"consumedSeq\":\"5\""),
            body.replace("trellis.live.v1", "trellis.live.v2"),
            format!("{body},\"extra\":true"),
            body.replace("\"ack\"", "\"unknown\""),
        ] {
            assert!(parse_live_control(invalid.as_bytes()).is_err(), "{invalid}");
        }
        assert!(parse_live_control(&vec![b' '; MAX_CONTROL_BODY_BYTES + 1]).is_err());
    }

    #[test]
    fn frame_parsing_rejects_unknown_fields_and_types() {
        let session = URL_SAFE_NO_PAD.encode([6u8; 16]);
        let challenge = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"challenge","sessionId":"{session}","challengeId":"{session}","lastSentSeq":"0"}}"#
        );
        assert!(matches!(
            parse_live_frame(challenge.as_bytes(), WINDOW_BYTES).unwrap(),
            LiveFrame::Challenge(_)
        ));
        let end = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"end","sessionId":"{session}","finalSeq":"3","terminal":{{"reason":"complete","error":null}}}}"#
        );
        assert!(matches!(
            parse_live_frame(end.as_bytes(), WINDOW_BYTES).unwrap(),
            LiveFrame::End(_)
        ));
        let invalid = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"end","sessionId":"{session}","finalSeq":"3","terminal":{{"reason":"peer_lost","error":null}}}}"#
        );
        assert!(parse_live_frame(invalid.as_bytes(), WINDOW_BYTES).is_err());
    }

    #[test]
    fn data_frames_use_the_negotiated_limit_not_the_opening_limit() {
        let session = URL_SAFE_NO_PAD.encode([7u8; 16]);
        // A legal DATA body above MAX_OPEN_BODY_BYTES must parse when the
        // negotiated window permits it, while CHALLENGE/END stay at the
        // protocol-control limit.
        let big = "x".repeat(MAX_OPEN_BODY_BYTES + 512);
        let data = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"data","sessionId":"{session}","seq":"1","value":"{big}"}}"#
        );
        assert!(data.len() > MAX_OPEN_BODY_BYTES);
        assert!(matches!(
            parse_live_frame(data.as_bytes(), WINDOW_BYTES).unwrap(),
            LiveFrame::Data(_)
        ));
        assert!(parse_live_frame(data.as_bytes(), (data.len() - 1) as u64).is_err());
        assert_eq!(
            parse_live_frame(data.as_bytes(), data.len() as u64)
                .unwrap()
                .session_id(),
            session
        );
        let oversized_challenge = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"challenge","sessionId":"{session}","challengeId":"{session}","lastSentSeq":"0","pad":"{}"}}"#,
            "x".repeat(MAX_CONTROL_BODY_BYTES)
        );
        assert!(parse_live_frame(oversized_challenge.as_bytes(), WINDOW_BYTES).is_err());
    }

    #[test]
    fn negotiated_body_limit_is_bounded_by_both_peers() {
        assert_eq!(
            negotiate_max_data_body_bytes(1_048_576, 1_048_576).unwrap(),
            WINDOW_BYTES.min(1_048_576 - PROTOCOL_HEADER_RESERVE_BYTES)
        );
        assert_eq!(
            negotiate_max_data_body_bytes(64 * 1024, 128 * 1024).unwrap(),
            64 * 1024 - PROTOCOL_HEADER_RESERVE_BYTES
        );
        assert!(negotiate_max_data_body_bytes(2_000, 1_048_576).is_err());
        assert!(negotiate_max_data_body_bytes(1_048_576, 0).is_err());
    }

    #[test]
    fn logical_open_hash_is_stable_and_input_sensitive() {
        let identity = LogicalOpenIdentity {
            kind: LiveSessionKind::Standalone,
            base_subject: "live.v1.route.YXBpQHYx.ZGVwLTAx.Watch".into(),
            open_id: URL_SAFE_NO_PAD.encode([1u8; 16]),
            consumer_connection_id: "01JYCONNECTION0000000000".into(),
            consumer_session_key: URL_SAFE_NO_PAD.encode([2u8; 32]),
            consumer_principal_id: "01JYPRINCIPAL00000000000".into(),
            consumer_participant_id: "app.console@v1".into(),
            receive_max_payload_bytes: 1_048_576,
            live_input: Some(serde_json::json!({ "site": "north" })),
            operation_id: None,
            include_updates: None,
        };
        let first = logical_open_hash(&identity).unwrap();
        let second = logical_open_hash(&identity).unwrap();
        assert_eq!(first, second);

        let mut changed = identity.clone();
        changed.receive_max_payload_bytes = 512 * 1024;
        assert_ne!(first, logical_open_hash(&changed).unwrap());

        let mut changed = identity.clone();
        changed.consumer_connection_id = "01JYOTHER00000000000000".into();
        assert_ne!(first, logical_open_hash(&changed).unwrap());

        let mut missing_input = identity.clone();
        missing_input.live_input = None;
        assert!(logical_open_hash(&missing_input).is_err());
    }

    #[test]
    fn logical_control_hash_distinguishes_body_not_transport_metadata() {
        let session = URL_SAFE_NO_PAD.encode([7u8; 16]);
        let body = format!(
            r#"{{"format":"{LIVE_VERSION}","type":"control","sessionId":"{session}","controlSeq":"1","action":"activate","receivedSeq":"0","consumedSeq":"0"}}"#
        );
        let first = parse_live_control(body.as_bytes()).unwrap();
        let second = parse_live_control(body.as_bytes()).unwrap();
        assert_eq!(
            logical_control_hash(&first).unwrap(),
            logical_control_hash(&second).unwrap()
        );
    }

    #[test]
    fn offer_round_trips_with_strict_unknown_field_rejection() {
        let offer = LiveOffer {
            format: LIVE_VERSION.into(),
            kind: LiveOfferKind::Offer,
            session_kind: LiveSessionKind::Standalone,
            open_id: URL_SAFE_NO_PAD.encode([1u8; 16]),
            request_id: "req-1".into(),
            session_id: URL_SAFE_NO_PAD.encode([2u8; 16]),
            base_subject: "live.v1.route.YXBpQHYx.ZGVwLTAx.Watch".into(),
            data_subject: "live.v1.data.A.B.C".into(),
            control_subject: "live.v1.route.YXBpQHYx.ZGVwLTAx.Watch.observe.A.C".into(),
            provider: LiveOfferProvider {
                connection_id: "P".into(),
                session_key: "K".into(),
                principal_id: "pr".into(),
                participant_id: "pa".into(),
                deployment_id: "dep".into(),
                instance_id: "inst".into(),
            },
            consumer: LiveOfferConsumer {
                connection_id: "C".into(),
                session_key: "KC".into(),
                principal_id: "pru".into(),
                participant_id: "pau".into(),
            },
            limits: LiveOfferLimits {
                max_data_body_bytes: 1_044_480,
                window_frames: WINDOW_FRAMES,
                window_bytes: WINDOW_BYTES,
                reservation_ms: OPEN_RESERVATION_MS,
                heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
                peer_inactivity_ms: PEER_INACTIVITY_MS,
                consumer_stall_ms: CONSUMER_STALL_MS,
            },
        };
        let encoded = serde_json::to_string(&offer).unwrap();
        let decoded: LiveOffer = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, offer);
        let with_unknown = encoded.replace(
            "\"kind\":\"standalone\"",
            "\"kind\":\"standalone\",\"extra\":1",
        );
        assert!(with_unknown.contains("extra"));
        assert!(serde_json::from_str::<LiveOffer>(&with_unknown).is_err());
    }
}
