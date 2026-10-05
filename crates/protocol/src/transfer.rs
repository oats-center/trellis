//! Storage-neutral Transfer v2 wire values, exact subjects and compact proofs.
//!
//! DATA is raw bytes. Its descriptor travels in headers; EOF has an empty body
//! and a signed terminal descriptor. Controls and signals are bounded strict JSON.
//! Counters are canonical decimal strings, including values above JavaScript's
//! safe integer range. Transport, lifetime and durable commit belong to runtimes.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{canonicalize_json, encode_subject_token, live, ProtocolError};

/// Exact Transfer protocol discriminator.
pub const TRANSFER_VERSION: &str = "trellis.transfer.v2";
/// Compact caller frame digest domain, distinct from request-proof signing.
pub const TRANSFER_FRAME_DOMAIN_V2: &str = "trellis.transfer-frame.v2";
/// Provider proof domain, deliberately distinct from Live.
pub const TRANSFER_SERVER_PROOF_DOMAIN_V2: &str = "trellis.transfer-server-proof.v2";
/// Maximum raw DATA body size.
pub const MAX_TRANSFER_FRAME_BYTES: u64 = 1_048_576;
/// Maximum outstanding DATA frames.
pub const TRANSFER_WINDOW_FRAMES: u64 = 16;
/// Maximum outstanding raw DATA bytes.
pub const TRANSFER_WINDOW_BYTES: u64 = 4_194_304;
/// Coalesce credit until this many frames have been consumed.
pub const TRANSFER_CREDIT_FRAME_STEP: u64 = 8;
/// Coalesce credit until this many bytes have been consumed.
pub const TRANSFER_CREDIT_BYTE_STEP: u64 = 1_048_576;
/// Maximum pending credit age.
pub const TRANSFER_CREDIT_MAX_DELAY_MS: u64 = 25;
/// NATS payload capacity reserved for proof and descriptor headers.
pub const TRANSFER_HEADER_RESERVE: u64 = 4_096;
/// Maximum JSON control, signal or terminal descriptor size.
pub const MAX_TRANSFER_CONTROL_BYTES: usize = 8_192;
/// Header carrying the canonical DATA sequence.
pub const TRANSFER_SEQUENCE_HEADER: &str = "trellis-transfer-seq";
/// Header carrying the frame discriminator (`data`, `complete`, `eof`).
pub const TRANSFER_CONTROL_HEADER: &str = "trellis-transfer-control";
/// Header carrying a strict EOF terminal JSON descriptor.
pub const TRANSFER_TERMINAL_HEADER: &str = "trellis-transfer-terminal";
/// Header carrying the provider Ed25519 proof.
pub const TRANSFER_PROOF_HEADER: &str = "trellis-transfer-proof";

/// Decimal-string counter, shared with Live without changing Live wire semantics.
pub use crate::live::U64s;

fn invalid(field: &'static str, message: &'static str) -> ProtocolError {
    ProtocolError::Transfer { field, message }
}

/// Parse an unsigned decimal wire counter without precision loss.
pub fn parse_transfer_counter(value: &str) -> Result<u64, ProtocolError> {
    live::parse_u64s(value, &["counter"]).map_err(|_| {
        invalid(
            "counter",
            "expected canonical unsigned 64-bit decimal string",
        )
    })
}

/// Generate a high-entropy canonical 128-bit transfer ID.
pub fn generate_transfer_id() -> Result<String, ProtocolError> {
    live::generate_nonce().map_err(|_| invalid("transferId", "random number generation failed"))
}

/// Validate a transfer ID as a canonical 128-bit random token.
pub fn validate_transfer_id(value: &str) -> Result<(), ProtocolError> {
    live::parse_nonce(value, ["transferId"])
        .map(|_| ())
        .map_err(|_| invalid("transferId", "expected canonical 128-bit base64url token"))
}

/// Caller-facing transfer direction, not a backend storage operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferDirection {
    /// Caller sends to the provider.
    Send,
    /// Caller receives from the provider.
    Receive,
}

/// Canonical Transfer subject family.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferSubjectKind {
    /// Caller-to-provider raw DATA and ordered completion.
    UploadData,
    /// Provider-to-caller raw DATA and ordered EOF.
    DownloadData,
    /// Caller-to-provider controls.
    Control,
    /// Provider-to-caller signals.
    Signal,
}

impl TransferSubjectKind {
    /// Exact family prefix, also used to compile transport permissions.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::UploadData => "transfer.v2.upload.data",
            Self::DownloadData => "transfer.v2.download.data",
            Self::Control => "transfer.v2.control",
            Self::Signal => "transfer.v2.signal",
        }
    }
}

/// Derive an exact session subject using Live's canonical identity encoding.
pub fn derive_transfer_subject(
    kind: TransferSubjectKind,
    provider_connection_id: &str,
    consumer_connection_id: &str,
    transfer_id: &str,
) -> Result<String, ProtocolError> {
    for identity in [provider_connection_id, consumer_connection_id] {
        if identity.is_empty() || identity.trim() != identity {
            return Err(invalid(
                "connectionId",
                "expected non-empty unpadded logical identity",
            ));
        }
    }
    validate_transfer_id(transfer_id)?;
    Ok(format!(
        "{}.{}.{}.{}",
        kind.prefix(),
        encode_subject_token(provider_connection_id),
        encode_subject_token(consumer_connection_id),
        transfer_id
    ))
}

/// Derive a permission pattern scoped to either logical endpoint.
///
/// At least one identity must be supplied. This is for permission compilation,
/// never a runtime session subscription.
fn derive_transfer_permission_subject(
    kind: TransferSubjectKind,
    provider_connection_id: Option<&str>,
    consumer_connection_id: Option<&str>,
) -> Result<String, ProtocolError> {
    if provider_connection_id.is_none() && consumer_connection_id.is_none() {
        return Err(invalid(
            "connectionId",
            "permission must pin at least one endpoint",
        ));
    }
    let mut tokens = Vec::with_capacity(2);
    for identity in [provider_connection_id, consumer_connection_id] {
        tokens.push(match identity {
            None => "*".to_owned(),
            Some(value) if !value.is_empty() && value.trim() == value => {
                encode_subject_token(value)
            }
            Some(_) => {
                return Err(invalid(
                    "connectionId",
                    "expected non-empty unpadded logical identity",
                ))
            }
        });
    }
    Ok(format!("{}.{}.{}.*", kind.prefix(), tokens[0], tokens[1]))
}

/// All exact session subjects derived from one pair of logical identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferSubjects {
    /// Ordered upload DATA and completion.
    pub upload_data_subject: String,
    /// Ordered download DATA and EOF.
    pub download_data_subject: String,
    /// Caller controls.
    pub control_subject: String,
    /// Provider signals.
    pub signal_subject: String,
}

/// Derive all exact subjects together, never backend addresses or wildcards.
pub fn derive_transfer_subjects(
    provider: &str,
    consumer: &str,
    transfer_id: &str,
) -> Result<TransferSubjects, ProtocolError> {
    Ok(TransferSubjects {
        upload_data_subject: derive_transfer_subject(
            TransferSubjectKind::UploadData,
            provider,
            consumer,
            transfer_id,
        )?,
        download_data_subject: derive_transfer_subject(
            TransferSubjectKind::DownloadData,
            provider,
            consumer,
            transfer_id,
        )?,
        control_subject: derive_transfer_subject(
            TransferSubjectKind::Control,
            provider,
            consumer,
            transfer_id,
        )?,
        signal_subject: derive_transfer_subject(
            TransferSubjectKind::Signal,
            provider,
            consumer,
            transfer_id,
        )?,
    })
}

/// Upload DATA permission addressed to this provider logical connection.
pub fn derive_transfer_provider_upload_data_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::UploadData, Some(connection_id), None)
}
/// Download DATA permission from this provider logical connection.
pub fn derive_transfer_provider_download_data_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::DownloadData, Some(connection_id), None)
}
/// Control permission addressed to this provider logical connection.
pub fn derive_transfer_provider_control_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::Control, Some(connection_id), None)
}
/// Signal permission from this provider logical connection.
pub fn derive_transfer_provider_signal_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::Signal, Some(connection_id), None)
}
/// Upload DATA permission from this consumer logical connection.
pub fn derive_transfer_consumer_upload_data_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::UploadData, None, Some(connection_id))
}
/// Download DATA permission addressed to this consumer logical connection.
pub fn derive_transfer_consumer_download_data_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::DownloadData, None, Some(connection_id))
}
/// Control permission from this consumer logical connection.
pub fn derive_transfer_consumer_control_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::Control, None, Some(connection_id))
}
/// Signal permission addressed to this consumer logical connection.
pub fn derive_transfer_consumer_signal_wildcard_subject(
    connection_id: &str,
) -> Result<String, ProtocolError> {
    derive_transfer_permission_subject(TransferSubjectKind::Signal, None, Some(connection_id))
}

/// Validate an exact subject; wildcard permission patterns are rejected.
pub fn validate_transfer_subject(subject: &str) -> Result<TransferSubjectKind, ProtocolError> {
    for kind in [
        TransferSubjectKind::UploadData,
        TransferSubjectKind::DownloadData,
        TransferSubjectKind::Control,
        TransferSubjectKind::Signal,
    ] {
        if let Some(tail) = subject.strip_prefix(&format!("{}.", kind.prefix())) {
            let tokens: Vec<_> = tail.split('.').collect();
            if tokens.len() != 3 {
                break;
            }
            for token in &tokens[..2] {
                live::validate_subject_token(token, ["subject"])
                    .map_err(|_| invalid("subject", "invalid logical identity token"))?;
                let bytes = URL_SAFE_NO_PAD
                    .decode(token)
                    .map_err(|_| invalid("subject", "invalid identity encoding"))?;
                let identity = std::str::from_utf8(&bytes)
                    .map_err(|_| invalid("subject", "identity must encode UTF-8"))?;
                if identity.is_empty() || identity.trim() != identity {
                    return Err(invalid("subject", "invalid logical identity"));
                }
            }
            validate_transfer_id(tokens[2])?;
            return Ok(kind);
        }
    }
    Err(invalid(
        "subject",
        "not an exact canonical Transfer v2 subject",
    ))
}

/// Negotiate raw DATA capacity from both peers' actual NATS payload limits.
pub fn negotiate_transfer_max_frame_bytes(
    consumer: u64,
    provider: u64,
) -> Result<u64, ProtocolError> {
    let usable = consumer
        .min(provider)
        .checked_sub(TRANSFER_HEADER_RESERVE)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            invalid(
                "maxFrameBytes",
                "NATS payload limit cannot accommodate headers and DATA",
            )
        })?;
    Ok(usable.min(MAX_TRANSFER_FRAME_BYTES))
}

/// Exact format discriminant; unsupported versions fail deserialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransferFormat {
    /// Transfer v2.
    #[serde(rename = "trellis.transfer.v2")]
    V2,
}

/// Literal control discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferControlKind {
    /// Caller control.
    Control,
}

/// Strict caller controls. Every control has a nonzero idempotence sequence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TransferControl {
    /// Activate after exact subscriptions exist; both cursors must be zero.
    Activate {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Literal control.
        #[serde(rename = "type")]
        kind: TransferControlKind,
        /// Transfer ID.
        transfer_id: String,
        /// Idempotence sequence (must be 1).
        control_seq: U64s,
        /// Zero received cursor.
        received_seq: U64s,
        /// Zero consumed cursor.
        consumed_seq: U64s,
        /// Consumer's usable raw frame capacity.
        receive_max_frame_bytes: u64,
    },
    /// Cumulative download consumption credit.
    Credit {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Literal control.
        #[serde(rename = "type")]
        kind: TransferControlKind,
        /// Transfer ID.
        transfer_id: String,
        /// Idempotence sequence.
        control_seq: U64s,
        /// Highest contiguous authenticated frame.
        received_seq: U64s,
        /// Highest contiguous consumed frame.
        consumed_seq: U64s,
        /// Total bytes consumed.
        consumed_bytes: U64s,
    },
    /// Cancel; cannot roll back an already durable commit.
    Cancel {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Literal control.
        #[serde(rename = "type")]
        kind: TransferControlKind,
        /// Transfer ID.
        transfer_id: String,
        /// Idempotence sequence.
        control_seq: U64s,
        /// Highest contiguous authenticated frame.
        received_seq: U64s,
        /// Highest contiguous consumed frame.
        consumed_seq: U64s,
        /// Total bytes consumed.
        consumed_bytes: U64s,
    },
    /// Acknowledge verified EOF after all DATA has been consumed.
    EndAck {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Literal control.
        #[serde(rename = "type")]
        kind: TransferControlKind,
        /// Transfer ID.
        transfer_id: String,
        /// Idempotence sequence.
        control_seq: U64s,
        /// Highest contiguous authenticated frame.
        received_seq: U64s,
        /// Highest contiguous consumed frame.
        consumed_seq: U64s,
        /// Total bytes consumed.
        consumed_bytes: U64s,
        /// Final DATA sequence, equal to both cursors.
        final_seq: U64s,
    },
}

impl TransferControl {
    /// Transfer addressed by this control.
    pub fn transfer_id(&self) -> &str {
        match self {
            Self::Activate { transfer_id, .. }
            | Self::Credit { transfer_id, .. }
            | Self::Cancel { transfer_id, .. }
            | Self::EndAck { transfer_id, .. } => transfer_id,
        }
    }
    /// Logical idempotence sequence.
    pub fn control_seq(&self) -> U64s {
        match self {
            Self::Activate { control_seq, .. }
            | Self::Credit { control_seq, .. }
            | Self::Cancel { control_seq, .. }
            | Self::EndAck { control_seq, .. } => *control_seq,
        }
    }
    /// Validate cursor relationships and activation requirements.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_transfer_id(self.transfer_id())?;
        if self.control_seq().get() == 0 {
            return Err(invalid("controlSeq", "control sequence must be nonzero"));
        }
        match self {
            Self::Activate {
                control_seq,
                received_seq,
                consumed_seq,
                receive_max_frame_bytes,
                ..
            } => {
                if control_seq.get() != 1 || received_seq.get() != 0 || consumed_seq.get() != 0 {
                    return Err(invalid(
                        "activate",
                        "activation requires sequence 1 and zero cursors",
                    ));
                }
                validate_frame_limit(*receive_max_frame_bytes)?;
            }
            Self::Credit {
                received_seq,
                consumed_seq,
                consumed_bytes,
                ..
            }
            | Self::Cancel {
                received_seq,
                consumed_seq,
                consumed_bytes,
                ..
            }
            | Self::EndAck {
                received_seq,
                consumed_seq,
                consumed_bytes,
                ..
            } => {
                validate_credit(*received_seq, *consumed_seq, *consumed_bytes)?;
                if let Self::EndAck { final_seq, .. } = self {
                    if final_seq != received_seq || final_seq != consumed_seq {
                        return Err(invalid(
                            "finalSeq",
                            "EOF acknowledgement requires all DATA consumed",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_frame_limit(limit: u64) -> Result<(), ProtocolError> {
    if limit == 0 || limit > MAX_TRANSFER_FRAME_BYTES {
        return Err(invalid(
            "maxFrameBytes",
            "frame limit outside protocol bounds",
        ));
    }
    Ok(())
}

fn validate_credit(received: U64s, consumed: U64s, bytes: U64s) -> Result<(), ProtocolError> {
    if consumed > received || (consumed.get() == 0) != (bytes.get() == 0) {
        return Err(invalid(
            "consumedSeq",
            "impossible consumption cursor or byte total",
        ));
    }
    Ok(())
}

/// Strict upload completion body published after DATA on the upload subject.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferComplete {
    /// Exact protocol discriminator.
    pub format: TransferFormat,
    /// Literal complete.
    #[serde(rename = "type")]
    pub kind: TransferCompleteKind,
    /// Session being completed.
    pub transfer_id: String,
    /// Final DATA sequence; zero for an empty object.
    pub final_seq: U64s,
    /// Complete object size, an interoperable JSON integer.
    pub size: u64,
    /// Canonical SHA-256 logical object digest.
    pub digest: String,
}

/// Literal upload completion discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferCompleteKind {
    /// Complete.
    Complete,
}

/// Signed EOF metadata; the EOF raw payload remains zero bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferTerminal {
    /// Last DATA sequence, not an additional DATA sequence.
    pub final_seq: U64s,
    /// Full object byte count, an interoperable JSON integer.
    pub size: u64,
    /// Independently verified SHA-256 object digest.
    pub digest: String,
}

impl TransferTerminal {
    /// Validate an object's terminal byte count, sequence and digest encoding.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.size > 9_007_199_254_740_991
            || (self.final_seq.get() == 0) != (self.size == 0)
            || self.final_seq.get() > self.size
        {
            return Err(invalid(
                "size",
                "invalid final sequence or unsafe object size",
            ));
        }
        validate_transfer_digest(&self.digest)
    }
}

/// Validate the logical digest (`SHA-256=` plus canonical unpadded base64url).
pub fn validate_transfer_digest(value: &str) -> Result<(), ProtocolError> {
    let encoded = value
        .strip_prefix("SHA-256=")
        .ok_or_else(|| invalid("digest", "expected SHA-256 object digest"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| invalid("digest", "invalid SHA-256 encoding"))?;
    if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(invalid(
            "digest",
            "digest must encode exactly 32 bytes canonically",
        ));
    }
    Ok(())
}

/// Signed provider signals. Credit is consumption, never durable completion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TransferSignal {
    /// Authenticated activation response; correlates the activating request.
    Activated {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Transfer ID.
        transfer_id: String,
        /// Activation sequence, always 1.
        control_seq: U64s,
        /// Exact activating request ID.
        request_id: String,
        /// Negotiated raw DATA limit.
        max_frame_bytes: u64,
        /// Outstanding frame bound.
        window_frames: u64,
        /// Outstanding byte bound.
        window_bytes: u64,
    },
    /// Cumulative upload consumption credit.
    Credit {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Transfer ID.
        transfer_id: String,
        /// Authenticated/admitted cursor.
        received_seq: U64s,
        /// Backend-consumed cursor.
        consumed_seq: U64s,
        /// Backend-consumed byte total.
        consumed_bytes: U64s,
    },
    /// Required backend and Operation durability have completed.
    Committed {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Transfer ID.
        transfer_id: String,
        /// Final DATA sequence.
        final_seq: U64s,
        /// Logical file metadata; runtime decodes its existing FileInfo type.
        info: TransferFileInfo,
    },
    /// Cancellation settled without a durable commit.
    Cancelled {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Transfer ID.
        transfer_id: String,
    },
    /// Terminal transfer failure.
    Error {
        /// Protocol discriminator.
        format: TransferFormat,
        /// Transfer ID.
        transfer_id: String,
        /// Stable protocol error category.
        code: TransferErrorCode,
    },
}

/// Logical file metadata only; no physical storage identifiers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferFileInfo {
    /// Logical file key.
    pub key: String,
    /// Object size.
    pub size: u64,
    /// Commit timestamp.
    pub updated_at: String,
    /// SHA-256 digest.
    pub digest: String,
    /// Optional media type.
    #[serde(
        default,
        deserialize_with = "present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_type: Option<String>,
    /// Application metadata.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Identity pinned for the complete logical transfer session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferGrantIdentity {
    /// Logical runtime connection ID, not a physical socket ID.
    pub connection_id: String,
    /// Canonical unpadded base64url Ed25519 session public key.
    pub session_key: String,
}

/// Literal public grant discriminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransferGrantKind {
    /// Runtime-issued transfer grant.
    TransferGrant,
}

/// Caller-to-provider grant payload; direction is owned by [`TransferGrant`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferSendGrant {
    /// Exact protocol version.
    pub format: TransferFormat,
    /// Literal TransferGrant.
    #[serde(rename = "type")]
    pub kind: TransferGrantKind,
    /// Logical owning service.
    pub service: String,
    /// Canonical random transfer ID.
    pub transfer_id: String,
    /// Absolute RFC3339 expiry, checked against current time by the runtime.
    pub expires_at: String,
    /// Pinned provider identity.
    pub provider: TransferGrantIdentity,
    /// Pinned consumer identity.
    pub consumer: TransferGrantIdentity,
    /// Exact upload DATA subject.
    pub data_subject: String,
    /// Exact caller control subject.
    pub control_subject: String,
    /// Exact provider signal subject.
    pub signal_subject: String,
    /// Negotiated maximum raw frame bytes.
    pub max_frame_bytes: u64,
    /// Outstanding frame window.
    pub window_frames: u64,
    /// Outstanding byte window.
    pub window_bytes: u64,
    /// Optional full-object size cap.
    #[serde(
        default,
        deserialize_with = "present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_bytes: Option<u64>,
    /// Optional retained media type.
    #[serde(
        default,
        deserialize_with = "present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_type: Option<String>,
    /// Optional application metadata; omitted is an empty set at runtime.
    #[serde(
        default,
        deserialize_with = "present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<std::collections::BTreeMap<String, String>>,
}

fn present_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Provider-to-caller grant payload; direction is owned by [`TransferGrant`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferReceiveGrant {
    /// Exact protocol version.
    pub format: TransferFormat,
    /// Literal TransferGrant.
    #[serde(rename = "type")]
    pub kind: TransferGrantKind,
    /// Logical owning service.
    pub service: String,
    /// Canonical random transfer ID.
    pub transfer_id: String,
    /// Absolute RFC3339 expiry, checked against current time by the runtime.
    pub expires_at: String,
    /// Pinned provider identity.
    pub provider: TransferGrantIdentity,
    /// Pinned consumer identity.
    pub consumer: TransferGrantIdentity,
    /// Exact download DATA subject.
    pub data_subject: String,
    /// Exact caller control subject.
    pub control_subject: String,
    /// Exact provider signal subject.
    pub signal_subject: String,
    /// Negotiated maximum raw frame bytes.
    pub max_frame_bytes: u64,
    /// Outstanding frame window.
    pub window_frames: u64,
    /// Outstanding byte window.
    pub window_bytes: u64,
    /// Logical metadata authenticated independently at EOF.
    pub info: TransferFileInfo,
}

/// Direction-tagged public v2 grant, with no physical storage identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "direction", rename_all = "lowercase")]
pub enum TransferGrant {
    /// Prepared caller-to-provider session.
    Send(TransferSendGrant),
    /// Prepared provider-to-caller session.
    Receive(TransferReceiveGrant),
}

impl TransferGrant {
    /// Validate exact session coordinates and logical transfer bounds.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let (
            service,
            id,
            expiry,
            provider,
            consumer,
            data,
            control,
            signal,
            frame,
            frames,
            bytes,
            data_kind,
        ) = match self {
            Self::Send(grant) => (
                &grant.service,
                &grant.transfer_id,
                &grant.expires_at,
                &grant.provider,
                &grant.consumer,
                &grant.data_subject,
                &grant.control_subject,
                &grant.signal_subject,
                grant.max_frame_bytes,
                grant.window_frames,
                grant.window_bytes,
                TransferSubjectKind::UploadData,
            ),
            Self::Receive(grant) => (
                &grant.service,
                &grant.transfer_id,
                &grant.expires_at,
                &grant.provider,
                &grant.consumer,
                &grant.data_subject,
                &grant.control_subject,
                &grant.signal_subject,
                grant.max_frame_bytes,
                grant.window_frames,
                grant.window_bytes,
                TransferSubjectKind::DownloadData,
            ),
        };
        if service.is_empty() || service.trim() != service {
            return Err(invalid(
                "service",
                "expected non-empty logical service name",
            ));
        }
        time::OffsetDateTime::parse(expiry, &time::format_description::well_known::Rfc3339)
            .map_err(|_| invalid("expiresAt", "expected RFC3339 absolute expiry"))?;
        for identity in [provider, consumer] {
            let key = decode_fixed::<32>(&identity.session_key, "sessionKey")?;
            VerifyingKey::from_bytes(&key)
                .map_err(|_| invalid("sessionKey", "invalid Ed25519 public key"))?;
        }
        validate_frame_limit(frame)?;
        if frames != TRANSFER_WINDOW_FRAMES || bytes != TRANSFER_WINDOW_BYTES || frame > bytes {
            return Err(invalid(
                "window",
                "grant must use the protocol frame and byte windows",
            ));
        }
        for (kind, actual) in [
            (data_kind, data),
            (TransferSubjectKind::Control, control),
            (TransferSubjectKind::Signal, signal),
        ] {
            let expected = derive_transfer_subject(
                kind,
                &provider.connection_id,
                &consumer.connection_id,
                id,
            )?;
            if *actual != expected {
                return Err(invalid(
                    "subject",
                    "grant subject does not match direction and pinned identities",
                ));
            }
        }
        match self {
            Self::Send(grant) => {
                if grant
                    .max_bytes
                    .is_some_and(|value| value == 0 || value > 9_007_199_254_740_991)
                {
                    return Err(invalid(
                        "maxBytes",
                        "expected positive interoperable JSON integer",
                    ));
                }
                if grant.content_type.as_ref().is_some_and(String::is_empty)
                    || grant
                        .metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.keys().any(String::is_empty))
                {
                    return Err(invalid(
                        "metadata",
                        "media type and metadata keys must be non-empty",
                    ));
                }
            }
            Self::Receive(grant) => grant.info.validate()?,
        }
        Ok(())
    }
}

impl TransferFileInfo {
    /// Validate logical metadata used to admit a download or report a commit.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.key.is_empty()
            || self.size > 9_007_199_254_740_991
            || self.content_type.as_ref().is_some_and(String::is_empty)
            || self.metadata.keys().any(String::is_empty)
        {
            return Err(invalid(
                "info",
                "invalid logical file metadata or unsafe size",
            ));
        }
        time::OffsetDateTime::parse(
            &self.updated_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| invalid("updatedAt", "expected RFC3339 commit timestamp"))?;
        validate_transfer_digest(&self.digest)
    }
}

/// Parse and validate a strict direction-specific public Transfer v2 grant.
///
/// This admission boundary performs no transport work. Current-time expiry and
/// matching the consumer to the local authenticated identity remain runtime checks.
pub fn parse_transfer_grant(raw: &[u8]) -> Result<TransferGrant, ProtocolError> {
    let grant: TransferGrant = parse_body(raw)?;
    grant.validate()?;
    Ok(grant)
}

/// Stable transfer failures, independent of backend errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferErrorCode {
    /// Invalid body, frame, proof binding or sequence.
    InvalidRequest,
    /// Missing transfer authority.
    PermissionDenied,
    /// Transfer grant expired.
    Expired,
    /// Current authorization is no longer usable.
    AuthorizationLost,
    /// Pinned physical transport lost.
    Disconnected,
    /// Verified DATA sequence skipped.
    DeliveryGap,
    /// Sender exceeded negotiated bounds.
    WindowExceeded,
    /// Frame exceeded negotiated maximum.
    PayloadTooLarge,
    /// Final bytes or digest differ from declared object.
    IntegrityFailed,
    /// Storage read/write did not finish successfully.
    StorageFailed,
    /// Durable Operation commit failed.
    CommitFailed,
    /// Session was already closed.
    Closed,
    /// Session is absent.
    NotFound,
    /// Runtime admission bound exhausted.
    ResourceExhausted,
    /// Control sequence reused with different content.
    ControlConflict,
    /// Control sequence skipped or regressed.
    ControlGap,
    /// Session cancelled.
    Cancelled,
}

impl TransferSignal {
    /// Transfer addressed by this signal.
    pub fn transfer_id(&self) -> &str {
        match self {
            Self::Activated { transfer_id, .. }
            | Self::Credit { transfer_id, .. }
            | Self::Committed { transfer_id, .. }
            | Self::Cancelled { transfer_id, .. }
            | Self::Error { transfer_id, .. } => transfer_id,
        }
    }
    /// Validate negotiated bounds, cursors and terminal metadata.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_transfer_id(self.transfer_id())?;
        match self {
            Self::Activated {
                control_seq,
                request_id,
                max_frame_bytes,
                window_frames,
                window_bytes,
                ..
            } => {
                validate_frame_limit(*max_frame_bytes)?;
                if control_seq.get() != 1
                    || request_id.is_empty()
                    || *window_frames == 0
                    || *window_frames > TRANSFER_WINDOW_FRAMES
                    || *window_bytes < *max_frame_bytes
                    || *window_bytes > TRANSFER_WINDOW_BYTES
                {
                    return Err(invalid(
                        "activated",
                        "invalid activation correlation or window limits",
                    ));
                }
            }
            Self::Credit {
                received_seq,
                consumed_seq,
                consumed_bytes,
                ..
            } => validate_credit(*received_seq, *consumed_seq, *consumed_bytes)?,
            Self::Committed {
                final_seq, info, ..
            } => TransferTerminal {
                final_seq: *final_seq,
                size: info.size,
                digest: info.digest.clone(),
            }
            .validate()?,
            Self::Cancelled { .. } | Self::Error { .. } => {}
        }
        Ok(())
    }
}

fn parse_body<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, ProtocolError> {
    if raw.is_empty() || raw.len() > MAX_TRANSFER_CONTROL_BYTES {
        return Err(invalid(
            "body",
            "control or signal body exceeds protocol bound",
        ));
    }
    Ok(serde_json::from_slice(raw)?)
}

/// Parse strict JSON caller controls, rejecting unknown and duplicate fields.
pub fn parse_transfer_control(raw: &[u8]) -> Result<TransferControl, ProtocolError> {
    let body: TransferControl = parse_body(raw)?;
    body.validate()?;
    Ok(body)
}

/// Parse strict JSON provider signals and validate semantic relationships.
pub fn parse_transfer_signal(raw: &[u8]) -> Result<TransferSignal, ProtocolError> {
    let body: TransferSignal = parse_body(raw)?;
    body.validate()?;
    Ok(body)
}

/// Parse ordered upload completion and validate its terminal declaration.
pub fn parse_transfer_complete(raw: &[u8]) -> Result<TransferComplete, ProtocolError> {
    let body: TransferComplete = parse_body(raw)?;
    validate_transfer_id(&body.transfer_id)?;
    TransferTerminal {
        final_seq: body.final_seq,
        size: body.size,
        digest: body.digest.clone(),
    }
    .validate()?;
    Ok(body)
}

/// Parse a strict authenticated EOF descriptor from its header bytes.
pub fn parse_transfer_terminal(raw: &[u8]) -> Result<TransferTerminal, ProtocolError> {
    let body: TransferTerminal = parse_body(raw)?;
    body.validate()?;
    Ok(body)
}

/// Authenticated frame discriminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferFrameKind {
    /// Nonempty raw DATA, sequence starts at 1.
    Data,
    /// Upload completion JSON, sequence is finalSeq.
    Complete,
    /// Zero-byte download EOF with signed terminal metadata.
    Eof,
    /// Caller control JSON, sequence is controlSeq.
    Control,
    /// Provider signal JSON, sequence is 0.
    Signal,
}

/// Compact frame identity carried by headers and reconstructed by the verifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TransferFrameDescriptor {
    /// Exact transfer session ID.
    pub transfer_id: String,
    /// Caller-facing direction.
    pub direction: TransferDirection,
    /// DATA sequence, completion finalSeq, controlSeq or signal 0.
    pub sequence: U64s,
    /// Frame/control discriminator.
    pub kind: TransferFrameKind,
    /// Present only for zero-byte EOF; authenticated in the compact digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TransferTerminal>,
}

impl TransferFrameDescriptor {
    /// Validate a descriptor and its actual payload before proof construction.
    pub fn validate(&self, payload: &[u8]) -> Result<(), ProtocolError> {
        validate_transfer_id(&self.transfer_id)?;
        match self.kind {
            TransferFrameKind::Data => {
                if self.sequence.get() == 0
                    || payload.is_empty()
                    || payload.len() as u64 > MAX_TRANSFER_FRAME_BYTES
                    || self.terminal.is_some()
                {
                    return Err(invalid(
                        "data",
                        "invalid DATA sequence, size or terminal metadata",
                    ));
                }
            }
            TransferFrameKind::Eof => {
                let terminal = self
                    .terminal
                    .as_ref()
                    .ok_or_else(|| invalid("terminal", "EOF requires terminal metadata"))?;
                terminal.validate()?;
                if !payload.is_empty()
                    || self.direction != TransferDirection::Receive
                    || terminal.final_seq != self.sequence
                {
                    return Err(invalid(
                        "eof",
                        "EOF must be empty and bind the final download sequence",
                    ));
                }
            }
            TransferFrameKind::Complete => {
                let complete = parse_transfer_complete(payload)?;
                if self.direction != TransferDirection::Send
                    || self.sequence != complete.final_seq
                    || self.transfer_id != complete.transfer_id
                    || self.terminal.is_some()
                {
                    return Err(invalid(
                        "complete",
                        "completion does not match frame identity",
                    ));
                }
            }
            TransferFrameKind::Control => {
                let control = parse_transfer_control(payload)?;
                if control.control_seq() != self.sequence
                    || control.transfer_id() != self.transfer_id
                    || self.terminal.is_some()
                {
                    return Err(invalid("control", "control does not match frame identity"));
                }
            }
            TransferFrameKind::Signal => {
                let signal = parse_transfer_signal(payload)?;
                if self.sequence.get() != 0
                    || signal.transfer_id() != self.transfer_id
                    || self.terminal.is_some()
                {
                    return Err(invalid("signal", "signal does not match frame identity"));
                }
            }
        }
        Ok(())
    }
}

fn hash_components(components: &[&[u8]]) -> Result<[u8; 32], ProtocolError> {
    let mut hash = Sha256::new();
    for component in components {
        let length = u32::try_from(component.len())
            .map_err(|_| invalid("proof", "proof component exceeds length-prefix range"))?;
        hash.update(length.to_be_bytes());
        hash.update(component);
    }
    Ok(hash.finalize().into())
}

/// SHA-256 of length-prefixed domain, canonical descriptor and raw payload hash.
///
/// Only the small descriptor is serialized; the raw payload is hashed in place.
/// Request proofs authenticate these 32 bytes instead of copying a whole frame.
pub fn transfer_frame_digest(
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
) -> Result<[u8; 32], ProtocolError> {
    descriptor.validate(payload)?;
    let descriptor_bytes = canonicalize_json(&serde_json::to_value(descriptor)?)?;
    hash_components(&[
        TRANSFER_FRAME_DOMAIN_V2.as_bytes(),
        descriptor_bytes.as_bytes(),
        &Sha256::digest(payload),
    ])
}

/// Canonical base64url compact frame digest for WASM/TypeScript signing.
pub fn transfer_frame_digest_encoded(
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
) -> Result<String, ProtocolError> {
    Ok(URL_SAFE_NO_PAD.encode(transfer_frame_digest(descriptor, payload)?))
}

/// Provider proof digest binds its own domain, context, actual subject and frame.
pub fn transfer_server_proof_digest(
    context_digest: &str,
    subject: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
) -> Result<[u8; 32], ProtocolError> {
    let kind = validate_transfer_subject(subject)?;
    if subject.rsplit('.').next() != Some(descriptor.transfer_id.as_str())
        || !matches!(
            (kind, descriptor.kind, descriptor.direction),
            (
                TransferSubjectKind::DownloadData,
                TransferFrameKind::Data | TransferFrameKind::Eof,
                TransferDirection::Receive
            ) | (TransferSubjectKind::Signal, TransferFrameKind::Signal, _)
        )
    {
        return Err(invalid(
            "subject",
            "provider proof must address this transfer's download DATA or signal subject",
        ));
    }
    let context = decode_fixed::<32>(context_digest, "contextDigest")?;
    let frame_digest = transfer_frame_digest(descriptor, payload)?;
    hash_components(&[
        TRANSFER_SERVER_PROOF_DOMAIN_V2.as_bytes(),
        &context,
        subject.as_bytes(),
        &frame_digest,
    ])
}

fn decode_fixed<const N: usize>(
    value: &str,
    field: &'static str,
) -> Result<[u8; N], ProtocolError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid(field, "invalid base64url encoding"))?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(invalid(field, "noncanonical base64url encoding"));
    }
    bytes
        .try_into()
        .map_err(|_| invalid(field, "encoded value has wrong length"))
}

/// Canonical base64url provider proof digest for language bindings.
pub fn transfer_server_proof_digest_encoded(
    context_digest: &str,
    subject: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
) -> Result<String, ProtocolError> {
    Ok(URL_SAFE_NO_PAD.encode(transfer_server_proof_digest(
        context_digest,
        subject,
        descriptor,
        payload,
    )?))
}

/// Canonical Ed25519 provider proof, signed under the Transfer-only domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferServerProof(String);

impl TransferServerProof {
    /// Validate one unpadded base64url Ed25519 signature.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        decode_fixed::<64>(&value, "proof")?;
        Ok(Self(value))
    }
    /// Encoded signature for the provider proof header.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Sign exact provider-origin DATA, EOF or signal bytes.
pub fn sign_transfer_server_proof(
    context_digest: &str,
    subject: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
    key: &SigningKey,
) -> Result<TransferServerProof, ProtocolError> {
    let digest = transfer_server_proof_digest(context_digest, subject, descriptor, payload)?;
    Ok(TransferServerProof(
        URL_SAFE_NO_PAD.encode(key.sign(&digest).to_bytes()),
    ))
}

/// Verify provider-origin DATA, EOF or signal against the pinned session key.
pub fn verify_transfer_server_proof(
    proof: &TransferServerProof,
    context_digest: &str,
    subject: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
    key: &VerifyingKey,
) -> Result<(), ProtocolError> {
    let digest = transfer_server_proof_digest(context_digest, subject, descriptor, payload)?;
    let signature = Signature::from_bytes(&decode_fixed::<64>(proof.as_str(), "proof")?);
    key.verify_strict(&digest, &signature)
        .map_err(|_| invalid("proof", "provider signature verification failed"))
}

/// Verify with a canonical base64url pinned provider public key.
pub fn verify_transfer_server_proof_encoded(
    proof: &TransferServerProof,
    context_digest: &str,
    subject: &str,
    descriptor: &TransferFrameDescriptor,
    payload: &[u8],
    provider_key: &str,
) -> Result<(), ProtocolError> {
    let key = VerifyingKey::from_bytes(&decode_fixed::<32>(provider_key, "sessionKey")?)
        .map_err(|_| invalid("sessionKey", "invalid Ed25519 public key"))?;
    verify_transfer_server_proof(proof, context_digest, subject, descriptor, payload, &key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id() -> String {
        URL_SAFE_NO_PAD.encode([7u8; 16])
    }
    fn object_digest(payload: &[u8]) -> String {
        format!(
            "SHA-256={}",
            URL_SAFE_NO_PAD.encode(Sha256::digest(payload))
        )
    }
    fn data_descriptor() -> TransferFrameDescriptor {
        TransferFrameDescriptor {
            transfer_id: id(),
            direction: TransferDirection::Receive,
            sequence: U64s::new(1),
            kind: TransferFrameKind::Data,
            terminal: None,
        }
    }

    // Admission must fail before a caller can use attacker-chosen subjects,
    // direction-specific fields, invalid pinned keys or unsafe transfer limits.
    #[test]
    fn grant_admission_rejects_cross_direction_and_invalid_coordinates() {
        let subjects = derive_transfer_subjects("provider", "consumer", &id()).unwrap();
        let provider_key =
            URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes());
        let consumer_key =
            URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes());
        let mut send = json!({
            "format": TRANSFER_VERSION, "type": "TransferGrant", "direction": "send", "service": "files",
            "transferId": id(), "expiresAt": "2026-10-04T12:00:00Z",
            "provider": {"connectionId": "provider", "sessionKey": provider_key},
            "consumer": {"connectionId": "consumer", "sessionKey": consumer_key},
            "dataSubject": subjects.upload_data_subject, "controlSubject": subjects.control_subject, "signalSubject": subjects.signal_subject,
            "maxFrameBytes": 4096, "windowFrames": TRANSFER_WINDOW_FRAMES, "windowBytes": TRANSFER_WINDOW_BYTES,
        });
        let parsed = parse_transfer_grant(&serde_json::to_vec(&send).unwrap()).unwrap();
        assert!(matches!(parsed, TransferGrant::Send(_)));
        // Both TypeScript's omitted metadata and Rust's populated metadata are
        // legitimate public grants and survive a canonical parse round trip.
        send["maxBytes"] = json!(8192);
        send["contentType"] = json!("application/octet-stream");
        send["metadata"] = json!({"label":"evidence"});
        let parsed = parse_transfer_grant(&serde_json::to_vec(&send).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), send);
        let info = json!({"key":"logical/evidence", "size":8192, "updatedAt":"2026-10-04T11:00:00Z", "digest":object_digest(&vec![42;8192]), "metadata":{}});
        let mut receive = send.clone();
        for field in ["maxBytes", "contentType", "metadata"] {
            receive.as_object_mut().unwrap().remove(field);
        }
        receive["direction"] = json!("receive");
        receive["dataSubject"] = json!(subjects.download_data_subject);
        receive["info"] = info.clone();
        let parsed = parse_transfer_grant(&serde_json::to_vec(&receive).unwrap()).unwrap();
        assert!(matches!(parsed, TransferGrant::Receive(_)));
        assert_eq!(serde_json::to_value(&parsed).unwrap(), receive);
        for (field, value) in [
            ("direction", json!("receive")),
            ("dataSubject", json!(subjects.download_data_subject)),
            (
                "signalSubject",
                json!(derive_transfer_subject(
                    TransferSubjectKind::Signal,
                    "foreign",
                    "consumer",
                    &id()
                )
                .unwrap()),
            ),
            ("controlSubject", json!("transfer.v2.control.*.*.*")),
            ("transferId", json!("not-a-random-session-id")),
            ("maxFrameBytes", json!(MAX_TRANSFER_FRAME_BYTES + 1)),
            ("windowFrames", json!(0)),
            ("windowBytes", json!(TRANSFER_WINDOW_BYTES + 1)),
            ("expiresAt", json!("invalid timestamp")),
            ("maxBytes", json!(9_007_199_254_740_992u64)),
            ("maxBytes", serde_json::Value::Null),
            ("info", info.clone()),
            ("storageBucket", json!("private backend")),
        ] {
            let mut invalid_grant = send.clone();
            invalid_grant[field] = value;
            assert!(
                parse_transfer_grant(&serde_json::to_vec(&invalid_grant).unwrap()).is_err(),
                "admitted invalid {field}"
            );
        }
        for identity in ["provider", "consumer"] {
            let mut invalid_grant = send.clone();
            invalid_grant[identity]["sessionKey"] = json!("padded-or-invalid-key=");
            assert!(parse_transfer_grant(&serde_json::to_vec(&invalid_grant).unwrap()).is_err());
            invalid_grant[identity]["sessionKey"] = send[identity]["sessionKey"].clone();
            invalid_grant[identity]["connectionId"] = json!("different-logical-identity");
            assert!(parse_transfer_grant(&serde_json::to_vec(&invalid_grant).unwrap()).is_err());
        }
        for (field, value) in [
            ("digest", json!("SHA-256=malformed")),
            ("size", json!(9_007_199_254_740_992u64)),
            ("updatedAt", json!("not a timestamp")),
            ("key", json!("")),
        ] {
            let mut invalid_grant = receive.clone();
            invalid_grant["info"][field] = value;
            assert!(
                parse_transfer_grant(&serde_json::to_vec(&invalid_grant).unwrap()).is_err(),
                "admitted invalid info.{field}"
            );
        }
        receive["maxBytes"] = json!(8192);
        assert!(parse_transfer_grant(&serde_json::to_vec(&receive).unwrap()).is_err());
        let duplicate_direction =
            serde_json::to_string(&send)
                .unwrap()
                .replacen("{", "{\"direction\":\"receive\",", 1);
        assert!(parse_transfer_grant(duplicate_direction.as_bytes()).is_err());
    }

    // Proves signatures cannot be replayed onto other bytes, cursors, sessions,
    // contexts, endpoints, provider identities or Live's signing domain.
    #[test]
    fn provider_signature_authenticates_exact_transfer_and_payload() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let context = URL_SAFE_NO_PAD.encode([4u8; 32]);
        let descriptor = data_descriptor();
        let subject = derive_transfer_subject(
            TransferSubjectKind::DownloadData,
            "provider",
            "consumer",
            &id(),
        )
        .unwrap();
        let payload = vec![42; 256 * 1024];
        let proof =
            sign_transfer_server_proof(&context, &subject, &descriptor, &payload, &key).unwrap();
        verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            &payload,
            &key.verifying_key(),
        )
        .unwrap();
        let mut corrupt_payload = payload.clone();
        let middle = corrupt_payload.len() / 2;
        corrupt_payload[middle] ^= 1;
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            &corrupt_payload,
            &key.verifying_key()
        )
        .is_err());
        let mut other_sequence = descriptor.clone();
        other_sequence.sequence = U64s::new(2);
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &other_sequence,
            &payload,
            &key.verifying_key()
        )
        .is_err());
        let other_subject = derive_transfer_subject(
            TransferSubjectKind::DownloadData,
            "provider",
            "another consumer",
            &id(),
        )
        .unwrap();
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &other_subject,
            &descriptor,
            &payload,
            &key.verifying_key()
        )
        .is_err());
        assert!(verify_transfer_server_proof(
            &proof,
            &URL_SAFE_NO_PAD.encode([5u8; 32]),
            &subject,
            &descriptor,
            &payload,
            &key.verifying_key()
        )
        .is_err());
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            &payload,
            &SigningKey::from_bytes(&[8; 32]).verifying_key()
        )
        .is_err());
        let live_proof = live::sign_live_server_proof(&context, &subject, &payload, &key).unwrap();
        let replay = TransferServerProof::parse(live_proof.as_str()).unwrap();
        assert!(verify_transfer_server_proof(
            &replay,
            &context,
            &subject,
            &descriptor,
            &payload,
            &key.verifying_key()
        )
        .is_err());
        let mut another_session = descriptor.clone();
        another_session.transfer_id = URL_SAFE_NO_PAD.encode([9; 16]);
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &another_session,
            &payload,
            &key.verifying_key()
        )
        .is_err());
    }

    // A zero-body EOF must authenticate its terminal digest and size. Otherwise
    // an attacker could alter unsigned headers after signing an empty payload.
    #[test]
    fn empty_eof_authenticates_final_metadata() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let context = URL_SAFE_NO_PAD.encode([4; 32]);
        let subject =
            derive_transfer_subject(TransferSubjectKind::DownloadData, "P", "C", &id()).unwrap();
        let mut descriptor = data_descriptor();
        descriptor.kind = TransferFrameKind::Eof;
        descriptor.terminal = Some(TransferTerminal {
            final_seq: U64s::new(1),
            size: 3,
            digest: object_digest(b"abc"),
        });
        let proof = sign_transfer_server_proof(&context, &subject, &descriptor, b"", &key).unwrap();
        verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            b"",
            &key.verifying_key(),
        )
        .unwrap();
        let terminal = descriptor.terminal.as_mut().unwrap();
        terminal.digest = object_digest(b"abd");
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            b"",
            &key.verifying_key()
        )
        .is_err());
        descriptor.terminal.as_mut().unwrap().size = 4;
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            b"",
            &key.verifying_key()
        )
        .is_err());
        assert!(transfer_frame_digest(&descriptor, b"not zero bytes").is_err());
        descriptor.terminal = None;
        assert!(transfer_frame_digest(&descriptor, b"").is_err());
        descriptor.sequence = U64s::new(0);
        descriptor.terminal = Some(TransferTerminal {
            final_seq: U64s::new(0),
            size: 0,
            digest: object_digest(b""),
        });
        let proof = sign_transfer_server_proof(&context, &subject, &descriptor, b"", &key).unwrap();
        verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            b"",
            &key.verifying_key(),
        )
        .unwrap();
    }

    // Caller controls must not admit ambiguous authorization/credit inputs.
    #[test]
    fn controls_round_trip_and_reject_invalid_credit_and_ambiguous_json() {
        let control = TransferControl::Credit {
            format: TransferFormat::V2,
            kind: TransferControlKind::Control,
            transfer_id: id(),
            control_seq: U64s::new(2),
            received_seq: U64s::new(u64::MAX),
            consumed_seq: U64s::new(u64::MAX - 1),
            consumed_bytes: U64s::new(u64::MAX),
        };
        let raw = serde_json::to_vec(&control).unwrap();
        assert_eq!(parse_transfer_control(&raw).unwrap(), control);
        let mut value = serde_json::to_value(&control).unwrap();
        value["consumedSeq"] = json!(u64::MAX.to_string());
        value["receivedSeq"] = json!((u64::MAX - 1).to_string());
        assert!(parse_transfer_control(&serde_json::to_vec(&value).unwrap()).is_err());
        value["receivedSeq"] = json!(u64::MAX.to_string());
        value["consumedSeq"] = json!("01");
        assert!(parse_transfer_control(&serde_json::to_vec(&value).unwrap()).is_err());
        value["consumedSeq"] = json!("1");
        value["storageBucket"] = json!("forbidden unknown field");
        assert!(parse_transfer_control(&serde_json::to_vec(&value).unwrap()).is_err());
        let duplicate = format!(
            r#"{{"action":"credit","format":"trellis.transfer.v2","type":"control","transferId":"{}","controlSeq":"2","receivedSeq":"1","consumedSeq":"0","consumedSeq":"1","consumedBytes":"1"}}"#,
            id()
        );
        assert!(parse_transfer_control(duplicate.as_bytes()).is_err());
        let activate = json!({"action":"activate","type":"control","format":TRANSFER_VERSION,"transferId":id(),"controlSeq":"1","receivedSeq":"0","consumedSeq":"0","receiveMaxFrameBytes":1024});
        parse_transfer_control(&serde_json::to_vec(&activate).unwrap()).unwrap();
        let mut bad = activate;
        bad["receivedSeq"] = json!("1");
        assert!(parse_transfer_control(&serde_json::to_vec(&bad).unwrap()).is_err());
        assert!(parse_transfer_control(&vec![b' '; MAX_TRANSFER_CONTROL_BYTES + 1]).is_err());
    }

    #[test]
    fn completion_and_end_ack_require_a_coherent_terminal_declaration() {
        let complete = TransferComplete {
            format: TransferFormat::V2,
            kind: TransferCompleteKind::Complete,
            transfer_id: id(),
            final_seq: U64s::new(2),
            size: 4096,
            digest: object_digest(&vec![42; 4096]),
        };
        let raw = serde_json::to_vec(&complete).unwrap();
        assert_eq!(parse_transfer_complete(&raw).unwrap(), complete);
        let mut descriptor = TransferFrameDescriptor {
            transfer_id: id(),
            direction: TransferDirection::Send,
            sequence: U64s::new(2),
            kind: TransferFrameKind::Complete,
            terminal: None,
        };
        transfer_frame_digest(&descriptor, &raw).unwrap();
        descriptor.sequence = U64s::new(3);
        assert!(transfer_frame_digest(&descriptor, &raw).is_err());
        let ack = json!({"action":"end-ack","format":TRANSFER_VERSION,"type":"control","transferId":id(),"controlSeq":"3","receivedSeq":"2","consumedSeq":"2","consumedBytes":"4096","finalSeq":"2"});
        parse_transfer_control(&serde_json::to_vec(&ack).unwrap()).unwrap();
        let mut missing_consumption = ack;
        missing_consumption["consumedSeq"] = json!("1");
        assert!(
            parse_transfer_control(&serde_json::to_vec(&missing_consumption).unwrap()).is_err()
        );
        let mut malformed = serde_json::to_value(&complete).unwrap();
        malformed["digest"] = json!("SHA-256=wrong");
        assert!(parse_transfer_complete(&serde_json::to_vec(&malformed).unwrap()).is_err());
        malformed["digest"] = json!(object_digest(b""));
        malformed["size"] = json!(0);
        assert!(parse_transfer_complete(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }

    #[test]
    fn signals_authenticate_activation_and_distinguish_credit_from_commit() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let context = URL_SAFE_NO_PAD.encode([4; 32]);
        let subject =
            derive_transfer_subject(TransferSubjectKind::Signal, "P", "C", &id()).unwrap();
        let signal = TransferSignal::Activated {
            format: TransferFormat::V2,
            transfer_id: id(),
            control_seq: U64s::new(1),
            request_id: "request-a".into(),
            max_frame_bytes: 4096,
            window_frames: TRANSFER_WINDOW_FRAMES,
            window_bytes: TRANSFER_WINDOW_BYTES,
        };
        let raw = serde_json::to_vec(&signal).unwrap();
        assert_eq!(parse_transfer_signal(&raw).unwrap(), signal);
        let descriptor = TransferFrameDescriptor {
            transfer_id: id(),
            direction: TransferDirection::Send,
            sequence: U64s::new(0),
            kind: TransferFrameKind::Signal,
            terminal: None,
        };
        let proof =
            sign_transfer_server_proof(&context, &subject, &descriptor, &raw, &key).unwrap();
        verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            &raw,
            &key.verifying_key(),
        )
        .unwrap();
        let mut changed = serde_json::to_value(&signal).unwrap();
        changed["requestId"] = json!("request-b");
        assert!(verify_transfer_server_proof(
            &proof,
            &context,
            &subject,
            &descriptor,
            &serde_json::to_vec(&changed).unwrap(),
            &key.verifying_key()
        )
        .is_err());
        changed["controlSeq"] = json!("0");
        assert!(parse_transfer_signal(&serde_json::to_vec(&changed).unwrap()).is_err());
        let credit = TransferSignal::Credit {
            format: TransferFormat::V2,
            transfer_id: id(),
            received_seq: U64s::new(2),
            consumed_seq: U64s::new(1),
            consumed_bytes: U64s::new(4096),
        };
        let raw_credit = serde_json::to_vec(&credit).unwrap();
        assert_eq!(parse_transfer_signal(&raw_credit).unwrap(), credit);
        let commit = TransferSignal::Committed {
            format: TransferFormat::V2,
            transfer_id: id(),
            final_seq: U64s::new(2),
            info: TransferFileInfo {
                key: "logical/path".into(),
                size: 8192,
                digest: object_digest(&vec![42; 8192]),
                updated_at: "2026-10-04T00:00:00Z".into(),
                content_type: None,
                metadata: Default::default(),
            },
        };
        let raw_commit = serde_json::to_vec(&commit).unwrap();
        assert_eq!(parse_transfer_signal(&raw_commit).unwrap(), commit);
        let credit_proof =
            sign_transfer_server_proof(&context, &subject, &descriptor, &raw_credit, &key).unwrap();
        assert!(verify_transfer_server_proof(
            &credit_proof,
            &context,
            &subject,
            &descriptor,
            &raw_commit,
            &key.verifying_key()
        )
        .is_err());
        let mut incomplete = serde_json::to_value(&commit).unwrap();
        incomplete["finalSeq"] = json!("0");
        assert!(parse_transfer_signal(&serde_json::to_vec(&incomplete).unwrap()).is_err());
    }

    #[test]
    fn subject_derivation_round_trips_without_wildcard_injection() {
        for kind in [
            TransferSubjectKind::UploadData,
            TransferSubjectKind::DownloadData,
            TransferSubjectKind::Control,
            TransferSubjectKind::Signal,
        ] {
            let subject =
                derive_transfer_subject(kind, "logical.provider.*", "consumer.>", &id()).unwrap();
            assert_eq!(validate_transfer_subject(&subject).unwrap(), kind);
            let wildcard =
                derive_transfer_permission_subject(kind, Some("logical.provider.*"), None).unwrap();
            assert!(validate_transfer_subject(&wildcard).is_err());
            assert!(derive_transfer_subject(kind, "provider", "consumer", "*").is_err());
            assert!(validate_transfer_subject(&format!("{subject}.extra")).is_err());
        }
        assert!(
            derive_transfer_permission_subject(TransferSubjectKind::Control, None, None).is_err()
        );
        assert!(negotiate_transfer_max_frame_bytes(4096, 1_048_576).is_err());
        let usable = negotiate_transfer_max_frame_bytes(8192, 1_048_576).unwrap();
        let mut descriptor = data_descriptor();
        transfer_frame_digest(&descriptor, &vec![42; usable as usize]).unwrap();
        descriptor.sequence = U64s::new(0);
        assert!(transfer_frame_digest(&descriptor, b"data").is_err());
        descriptor.sequence = U64s::new(1);
        assert!(transfer_frame_digest(
            &descriptor,
            &vec![42; MAX_TRANSFER_FRAME_BYTES as usize + 1]
        )
        .is_err());
    }
}
