//! WASM boundary for deterministic Trellis protocol proof operations.

#![deny(missing_docs)]

use serde::Deserialize;
use serde_json::{json, Value};
use trellis_protocol::{
    decode_pagination_cursor as decode_pagination_cursor_protocol,
    derive_event_subject as derive_event_subject_protocol,
    encode_pagination_cursor as encode_pagination_cursor_protocol,
    pagination_query_digest as pagination_query_digest_protocol, parse_session_authority,
    session_proof_request_digest as session_proof_request_digest_protocol,
    session_proof_signing_digest as session_proof_signing_digest_protocol,
    verify_session_authority as verify_session_authority_protocol,
    verify_session_proof as verify_session_proof_protocol,
    verify_session_request as verify_session_request_protocol, AuthorityIssuerKey,
    AuthorizationContextRefreshSessionProofInput, NativeBootstrapSessionProofInput, ProtocolError,
    SessionAuthorityPurpose, SessionAuthorityVerificationInput, SessionAuthorityVerificationPolicy,
    SessionProof, SessionProofInput, SessionProofPolicy, SessionRequest, SessionRequestProof,
    SessionRequestVerificationInput, TransportAuthorizationV1, UserAuthBindSessionProofInput,
    UserAuthRequestSessionProofInput, VerifiedSessionAuthority,
};
use trellis_protocol::{
    verify_session_event as verify_session_event_protocol, OriginalEventPublication, SessionEvent,
    SessionEventProof, SessionEventVerificationInput,
};
use wasm_bindgen::prelude::*;

/// Generate a high-entropy canonical Transfer v2 session ID.
#[wasm_bindgen]
pub fn transfer_generate_id() -> Result<String, JsError> {
    trellis_protocol::transfer::generate_transfer_id()
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Derive an exact Transfer v2 subject from logical endpoint identities.
/// `kind` is upload-data, download-data, control or signal.
#[wasm_bindgen]
pub fn transfer_subject(
    kind: &str,
    provider: &str,
    consumer: &str,
    transfer_id: &str,
) -> Result<String, JsError> {
    let kind =
        serde_json::from_value::<trellis_protocol::transfer::TransferSubjectKind>(json!(kind))
            .map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::transfer::derive_transfer_subject(kind, provider, consumer, transfer_id)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Validate an exact subject, returning its canonical family name.
#[wasm_bindgen]
pub fn transfer_validate_subject(subject: &str) -> Result<String, JsError> {
    let kind = trellis_protocol::transfer::validate_transfer_subject(subject)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&kind).map_err(|error| JsError::new(&error.to_string()))
}

/// Validate an unsigned decimal-string counter without converting it to f64.
#[wasm_bindgen]
pub fn transfer_parse_counter(value: &str) -> Result<String, JsError> {
    trellis_protocol::transfer::parse_transfer_counter(value)
        .map(|value| value.to_string())
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Negotiate raw DATA size from exact safe-integer NATS payload limits.
#[wasm_bindgen]
pub fn transfer_negotiate_max_frame_bytes(consumer: f64, provider: f64) -> Result<f64, JsError> {
    let consumer = u64::try_from(safe_integer(consumer, "consumerMaxPayload")?)
        .map_err(|_| JsError::new("negative payload limit"))?;
    let provider = u64::try_from(safe_integer(provider, "providerMaxPayload")?)
        .map_err(|_| JsError::new("negative payload limit"))?;
    trellis_protocol::transfer::negotiate_transfer_max_frame_bytes(consumer, provider)
        .map(|value| value as f64)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Return canonical Transfer limits and protocol header names as JSON.
#[wasm_bindgen]
pub fn transfer_constants() -> String {
    use trellis_protocol::transfer::*;
    json!({
        "format": TRANSFER_VERSION,
        "maxFrameBytes": MAX_TRANSFER_FRAME_BYTES,
        "windowFrames": TRANSFER_WINDOW_FRAMES,
        "windowBytes": TRANSFER_WINDOW_BYTES,
        "creditFrameStep": TRANSFER_CREDIT_FRAME_STEP,
        "creditByteStep": TRANSFER_CREDIT_BYTE_STEP,
        "creditMaxDelayMs": TRANSFER_CREDIT_MAX_DELAY_MS,
        "headerReserve": TRANSFER_HEADER_RESERVE,
        "maxControlBytes": MAX_TRANSFER_CONTROL_BYTES,
        "sequenceHeader": TRANSFER_SEQUENCE_HEADER,
        "controlHeader": TRANSFER_CONTROL_HEADER,
        "terminalHeader": TRANSFER_TERMINAL_HEADER,
        "proofHeader": TRANSFER_PROOF_HEADER,
    })
    .to_string()
}

/// Parse a direction-specific grant, validating exact subjects and identities.
#[wasm_bindgen]
pub fn transfer_parse_grant(raw: &[u8]) -> Result<String, JsError> {
    let grant = trellis_protocol::transfer::parse_transfer_grant(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&grant).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse a strict Transfer caller control and return its normalized JSON.
#[wasm_bindgen]
pub fn transfer_parse_control(raw: &[u8]) -> Result<String, JsError> {
    let value = trellis_protocol::transfer::parse_transfer_control(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&value).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse a strict provider signal and return its normalized JSON.
#[wasm_bindgen]
pub fn transfer_parse_signal(raw: &[u8]) -> Result<String, JsError> {
    let value = trellis_protocol::transfer::parse_transfer_signal(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&value).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse ordered upload completion and return its normalized JSON.
#[wasm_bindgen]
pub fn transfer_parse_complete(raw: &[u8]) -> Result<String, JsError> {
    let value = trellis_protocol::transfer::parse_transfer_complete(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&value).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse signed zero-byte EOF's terminal header and return normalized JSON.
#[wasm_bindgen]
pub fn transfer_parse_terminal(raw: &[u8]) -> Result<String, JsError> {
    let value = trellis_protocol::transfer::parse_transfer_terminal(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&value).map_err(|error| JsError::new(&error.to_string()))
}

/// Compute a compact frame digest as base64url, hashing raw bytes in place.
#[wasm_bindgen]
pub fn transfer_frame_digest(descriptor_json: &str, payload: &[u8]) -> Result<String, JsError> {
    let descriptor =
        serde_json::from_str(descriptor_json).map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::transfer::transfer_frame_digest_encoded(&descriptor, payload)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Compute the Transfer-domain provider signature digest as base64url.
#[wasm_bindgen]
pub fn transfer_server_proof_digest(
    context_digest: &str,
    subject: &str,
    descriptor_json: &str,
    payload: &[u8],
) -> Result<String, JsError> {
    let descriptor =
        serde_json::from_str(descriptor_json).map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::transfer::transfer_server_proof_digest_encoded(
        context_digest,
        subject,
        &descriptor,
        payload,
    )
    .map_err(|error| JsError::new(&error.to_string()))
}

/// Verify a Transfer-only provider proof against the pinned session key.
#[wasm_bindgen]
pub fn transfer_verify_server_proof(
    proof: &str,
    context_digest: &str,
    subject: &str,
    descriptor_json: &str,
    payload: &[u8],
    provider_key: &str,
) -> Result<(), JsError> {
    let descriptor =
        serde_json::from_str(descriptor_json).map_err(|error| JsError::new(&error.to_string()))?;
    let proof = trellis_protocol::transfer::TransferServerProof::parse(proof)
        .map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::transfer::verify_transfer_server_proof_encoded(
        &proof,
        context_digest,
        subject,
        &descriptor,
        payload,
        provider_key,
    )
    .map_err(|error| JsError::new(&error.to_string()))
}

const MAXIMUM_SAFE_JSON_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Compute the shared query binding for an opaque pagination cursor.
#[wasm_bindgen]
pub fn pagination_query_digest(endpoint: &str, query_json: &str) -> Result<String, JsError> {
    let query: Value =
        serde_json::from_str(query_json).map_err(|error| JsError::new(&error.to_string()))?;
    pagination_query_digest_protocol(endpoint, &query)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Encode a JSON value as a shared versioned pagination cursor.
#[wasm_bindgen]
pub fn encode_pagination_cursor(query_digest: &str, after_json: &str) -> Result<String, JsError> {
    let after: Value =
        serde_json::from_str(after_json).map_err(|error| JsError::new(&error.to_string()))?;
    encode_pagination_cursor_protocol(query_digest, &after)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Decode and query-bind a shared versioned pagination cursor.
#[wasm_bindgen]
pub fn decode_pagination_cursor(encoded: &str, query_digest: &str) -> Result<String, JsError> {
    let after: Value = decode_pagination_cursor_protocol(encoded, query_digest)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&after).map_err(|error| JsError::new(&error.to_string()))
}

/// Derive an API-qualified event base subject.
#[wasm_bindgen]
pub fn event_subject(api_id: &str, action: &str) -> Result<String, JsError> {
    derive_event_subject_protocol(api_id, action).map_err(|error| JsError::new(&error.to_string()))
}

#[derive(Deserialize)]
#[serde(
    deny_unknown_fields,
    tag = "purpose",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum WireSessionProofInput {
    UserAuthProgress {
        origin: String,
        unsigned_request: Value,
    },
    UserAuthBind {
        origin: String,
        transaction_id: String,
        session_public_key: String,
        unsigned_request: Value,
    },
    UserAuthRequest {
        origin: String,
        unsigned_request: Value,
    },
    ServiceBootstrap {
        origin: String,
        unsigned_request: Value,
    },
    DeviceBootstrap {
        origin: String,
        unsigned_request: Value,
    },
    DeviceEnrollment {
        origin: String,
        unsigned_request: Value,
    },
    AuthorizationContextRefresh {
        origin: String,
        session_public_key: String,
        unsigned_request: Value,
    },
}

impl TryFrom<WireSessionProofInput> for SessionProofInput {
    type Error = ProtocolError;

    fn try_from(value: WireSessionProofInput) -> Result<Self, Self::Error> {
        match value {
            WireSessionProofInput::UserAuthProgress {
                origin,
                unsigned_request,
            } => Self::user_auth_progress(UserAuthRequestSessionProofInput {
                origin,
                unsigned_request,
            }),
            WireSessionProofInput::UserAuthBind {
                origin,
                transaction_id,
                session_public_key,
                unsigned_request,
            } => Self::user_auth_bind(UserAuthBindSessionProofInput {
                origin,
                transaction_id,
                session_public_key,
                unsigned_request,
            }),
            WireSessionProofInput::UserAuthRequest {
                origin,
                unsigned_request,
            } => Self::user_auth_request(UserAuthRequestSessionProofInput {
                origin,
                unsigned_request,
            }),
            WireSessionProofInput::ServiceBootstrap {
                origin,
                unsigned_request,
            } => Self::service_bootstrap(NativeBootstrapSessionProofInput {
                origin,
                unsigned_request,
            }),
            WireSessionProofInput::DeviceBootstrap {
                origin,
                unsigned_request,
            } => Self::device_bootstrap(NativeBootstrapSessionProofInput {
                origin,
                unsigned_request,
            }),
            WireSessionProofInput::DeviceEnrollment {
                origin,
                unsigned_request,
            } => Self::device_enrollment(NativeBootstrapSessionProofInput {
                origin,
                unsigned_request,
            }),
            WireSessionProofInput::AuthorizationContextRefresh {
                origin,
                session_public_key,
                unsigned_request,
            } => {
                Self::authorization_context_refresh(AuthorizationContextRefreshSessionProofInput {
                    origin,
                    session_public_key,
                    unsigned_request,
                })
            }
        }
    }
}

fn parse_input(input_json: &str) -> Result<SessionProofInput, JsError> {
    serde_json::from_str::<WireSessionProofInput>(input_json)
        .map_err(|error| JsError::new(&error.to_string()))?
        .try_into()
        .map_err(|error: ProtocolError| JsError::new(&error.to_string()))
}

fn safe_integer(value: f64, name: &str) -> Result<i64, JsError> {
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= MAXIMUM_SAFE_JSON_INTEGER {
        Ok(value as i64)
    } else {
        Err(JsError::new(&format!(
            "{name} must be an interoperable JSON safe integer"
        )))
    }
}

/// Return the canonical request digest for a JSON-encoded proof-bearing request.
#[wasm_bindgen]
pub fn session_proof_request_digest(request_json: &str) -> Result<String, JsError> {
    let request: Value =
        serde_json::from_str(request_json).map_err(|error| JsError::new(&error.to_string()))?;
    session_proof_request_digest_protocol(&request)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Return the canonical signing digest for a JSON-encoded purpose-specific input.
#[wasm_bindgen]
pub fn session_proof_signing_digest(input_json: &str) -> Result<String, JsError> {
    session_proof_signing_digest_protocol(&parse_input(input_json)?)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Parse and normalize one strict JSON-encoded session-proof envelope.
#[wasm_bindgen]
pub fn parse_session_proof(proof_json: &str) -> Result<String, JsError> {
    let value: Value =
        serde_json::from_str(proof_json).map_err(|error| JsError::new(&error.to_string()))?;
    let proof = trellis_protocol::parse_session_proof(&value)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&proof).map_err(|error| JsError::new(&error.to_string()))
}

/// Verify a JSON-encoded session proof.
#[wasm_bindgen]
pub fn verify_session_proof(
    input_json: &str,
    proof_json: &str,
    signer_public_key: &str,
    now_ms: f64,
    maximum_age_ms: f64,
    maximum_future_skew_ms: f64,
) -> Result<(), JsError> {
    let proof: SessionProof =
        serde_json::from_str(proof_json).map_err(|error| JsError::new(&error.to_string()))?;
    let now_ms = safe_integer(now_ms, "nowMs")?;
    let policy = SessionProofPolicy::new(
        safe_integer(maximum_age_ms, "maximumAgeMs")?,
        safe_integer(maximum_future_skew_ms, "maximumFutureSkewMs")?,
    )
    .map_err(|error| JsError::new(&error.to_string()))?;
    verify_session_proof_protocol(
        &parse_input(input_json)?,
        &proof,
        signer_public_key,
        now_ms,
        policy,
    )
    .map_err(|error| JsError::new(&error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireSessionAuthorityVerification {
    issuer: AuthorityIssuerKey,
    trellis_instance_id: String,
    audience_nats_account: String,
    policy: SessionAuthorityVerificationPolicy,
    purpose: SessionAuthorityPurpose,
    revocation_cutoff: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireSessionRequest {
    request: SessionRequest,
    proof: SessionRequestProof,
    policy: SessionAuthorityVerificationPolicy,
    known_revoked: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireSessionEvent {
    event: SessionEvent,
    proof: SessionEventProof,
    policy: SessionAuthorityVerificationPolicy,
    original_publication: Option<OriginalEventPublication>,
    known_revoked: bool,
}

/// Opaque verified authority retained inside the shared Rust/WASM implementation.
#[wasm_bindgen]
pub struct VerifiedSessionAuthorityHandle {
    authority: VerifiedSessionAuthority,
    projection: String,
}

/// Verify bounded target authority using explicit pinned trust and instance/account.
#[wasm_bindgen]
pub fn create_session_authority_handle(
    verification_json: &str,
    authority_bytes: &[u8],
) -> Result<VerifiedSessionAuthorityHandle, JsError> {
    if verification_json.len() > 16_384 {
        return Err(JsError::new("verification input exceeds byte budget"));
    }
    let input: WireSessionAuthorityVerification = serde_json::from_str(verification_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let signed = parse_session_authority(authority_bytes, input.policy.maximum_authority_bytes)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let authority = verify_session_authority_protocol(SessionAuthorityVerificationInput {
        issuer: &input.issuer,
        trellis_instance_id: &input.trellis_instance_id,
        audience_nats_account: &input.audience_nats_account,
        authority: &signed,
        policy: &input.policy,
        purpose: input.purpose,
        revocation_cutoff: input.revocation_cutoff,
    })
    .map_err(|error| JsError::new(&error.to_string()))?;
    let projection = serde_json::to_string(&json!({
        "authorityDigest": authority.digest(),
        "authority": authority.signed(),
    }))
    .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(VerifiedSessionAuthorityHandle {
        authority,
        projection,
    })
}

#[wasm_bindgen]
impl VerifiedSessionAuthorityHandle {
    /// Return immutable authenticated material and its content address.
    pub fn projection(&self) -> Result<String, JsError> {
        Ok(self.projection.clone())
    }

    /// Recheck current time and sticky logical-session revocation after waiting.
    pub fn assert_current(&self, policy_json: &str, known_revoked: bool) -> Result<(), JsError> {
        if policy_json.len() > 4096 {
            return Err(JsError::new("policy exceeds byte budget"));
        }
        let policy: SessionAuthorityVerificationPolicy =
            serde_json::from_str(policy_json).map_err(|error| JsError::new(&error.to_string()))?;
        self.authority
            .assert_current(&policy, known_revoked)
            .map_err(|error| JsError::new(&error.to_string()))
    }
}

/// Return the canonical digest of a signed transport-authorization policy.
#[wasm_bindgen]
pub fn transport_authorization_digest(policy_json: &str) -> Result<String, JsError> {
    let policy: TransportAuthorizationV1 =
        serde_json::from_str(policy_json).map_err(|error| JsError::new(&error.to_string()))?;
    policy
        .digest()
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Classify admitted transport policy `A` against currently allowed policy `D`.
///
/// Returns one of `current`, `upgrade_available`, or `reduction_required` as
/// JSON, using the same pure full-witness inclusion implementation as the Rust
/// runtime so both SDKs agree on wildcard containment.
#[wasm_bindgen]
pub fn classify_transport_authorization(
    admitted_json: &str,
    allowed_json: &str,
    now_unix_seconds: f64,
) -> Result<String, JsError> {
    if !now_unix_seconds.is_finite() {
        return Err(JsError::new("now must be a finite number"));
    }
    let admitted: TransportAuthorizationV1 =
        serde_json::from_str(admitted_json).map_err(|error| JsError::new(&error.to_string()))?;
    let allowed: TransportAuthorizationV1 =
        serde_json::from_str(allowed_json).map_err(|error| JsError::new(&error.to_string()))?;
    let class = admitted
        .classify(&allowed, now_unix_seconds as i64)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&class).map_err(|error| JsError::new(&error.to_string()))
}

/// Generate one canonical live-session nonce from the operating system RNG.
#[wasm_bindgen]
pub fn live_generate_nonce() -> Result<String, JsError> {
    trellis_protocol::generate_nonce().map_err(|error| JsError::new(&error.to_string()))
}

/// Parse one strict JSON live control and return its canonical projection.
#[wasm_bindgen]
pub fn live_parse_control(raw: &[u8]) -> Result<String, JsError> {
    let control = trellis_protocol::parse_live_control(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let projection = match &control {
        trellis_protocol::LiveControl::Activate(body) => serde_json::to_value(body),
        trellis_protocol::LiveControl::Pulse(body) => serde_json::to_value(body),
        trellis_protocol::LiveControl::Ack(body) => serde_json::to_value(body),
        trellis_protocol::LiveControl::Close(body) => serde_json::to_value(body),
        trellis_protocol::LiveControl::EndAck(body) => serde_json::to_value(body),
    }
    .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&projection).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse one strict JSON provider data-channel frame and return its projection.
///
/// `max_data_body_bytes` is the negotiated application-data body limit; DATA is
/// bounded by it while CHALLENGE/END use the tighter protocol-control limit.
#[wasm_bindgen]
pub fn live_parse_frame(raw: &[u8], max_data_body_bytes: f64) -> Result<String, JsError> {
    if !max_data_body_bytes.is_finite() || max_data_body_bytes < 0.0 {
        return Err(JsError::new(
            "max data body limit must be a finite nonnegative number",
        ));
    }
    let frame = trellis_protocol::parse_live_frame(raw, max_data_body_bytes as u64)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let projection = match &frame {
        trellis_protocol::LiveFrame::Data(body) => serde_json::to_value(body),
        trellis_protocol::LiveFrame::Challenge(body) => serde_json::to_value(body),
        trellis_protocol::LiveFrame::End(body) => serde_json::to_value(body),
    }
    .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&projection).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse one strict signed provider offer and return its canonical projection.
#[wasm_bindgen]
pub fn live_parse_offer(raw: &[u8]) -> Result<String, JsError> {
    trellis_protocol::validate_control_body(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|error| JsError::new(&error.to_string()))?;
    if value.get("type").and_then(|kind| kind.as_str()) != Some("offer") {
        return Err(JsError::new("live offer carries an unknown discriminant"));
    }
    let offer: trellis_protocol::LiveOffer =
        serde_json::from_value(value).map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&offer).map_err(|error| JsError::new(&error.to_string()))
}

/// Parse one strict signed control response and return its tagged projection.
#[wasm_bindgen]
pub fn live_parse_control_response(raw: &[u8]) -> Result<String, JsError> {
    trellis_protocol::validate_control_body(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|error| JsError::new(&error.to_string()))?;
    let projection = match value.get("type").and_then(|kind| kind.as_str()) {
        Some("control-ack") => {
            let ack: trellis_protocol::LiveControlAck =
                serde_json::from_value(value).map_err(|error| JsError::new(&error.to_string()))?;
            serde_json::json!({ "kind": "ack", "body": ack })
        }
        Some("control-error") => {
            let error: trellis_protocol::LiveControlError =
                serde_json::from_value(value).map_err(|error| JsError::new(&error.to_string()))?;
            serde_json::json!({ "kind": "error", "body": error })
        }
        _ => {
            return Err(JsError::new(
                "live control response carries an unknown discriminant",
            ));
        }
    };
    serde_json::to_string(&projection).map_err(|error| JsError::new(&error.to_string()))
}

/// Return the shared live timing, window and admission constants as JSON.
#[wasm_bindgen]
pub fn live_constants() -> Result<String, JsError> {
    use trellis_protocol::{
        ACK_FRAME_THRESHOLD, ACK_MAX_DELAY_MS, CHALLENGE_RETRY_MS, CLEANUP_GRACE_MS,
        CLOSE_EXCHANGE_MS, CLOSE_RETRY_MS, CONSUMER_STALL_MS, CONTROL_TIMEOUT_MS,
        HEARTBEAT_INTERVAL_MS, MAX_CONSUMER_SESSIONS, MAX_CONTROL_BODY_BYTES, MAX_OPEN_BODY_BYTES,
        MAX_PROVIDER_SESSIONS, MAX_PROVIDER_SESSIONS_PER_CALLER, MAX_TOMBSTONES,
        OPEN_RESERVATION_MS, PEER_INACTIVITY_MS, PROTOCOL_HEADER_RESERVE_BYTES, TOMBSTONE_MS,
        WINDOW_BYTES, WINDOW_FRAMES,
    };
    let value = serde_json::json!({
        "openReservationMs": OPEN_RESERVATION_MS,
        "controlTimeoutMs": CONTROL_TIMEOUT_MS,
        "heartbeatIntervalMs": HEARTBEAT_INTERVAL_MS,
        "challengeRetryMs": CHALLENGE_RETRY_MS,
        "peerInactivityMs": PEER_INACTIVITY_MS,
        "consumerStallMs": CONSUMER_STALL_MS,
        "ackMaxDelayMs": ACK_MAX_DELAY_MS,
        "ackFrameThreshold": ACK_FRAME_THRESHOLD,
        "windowFrames": WINDOW_FRAMES,
        "windowBytes": WINDOW_BYTES,
        "maxOpenBodyBytes": MAX_OPEN_BODY_BYTES,
        "maxControlBodyBytes": MAX_CONTROL_BODY_BYTES,
        "headerReserveBytes": PROTOCOL_HEADER_RESERVE_BYTES,
        "cleanupGraceMs": CLEANUP_GRACE_MS,
        "closeExchangeMs": CLOSE_EXCHANGE_MS,
        "closeRetryMs": CLOSE_RETRY_MS,
        "tombstoneMs": TOMBSTONE_MS,
        "maxProviderSessions": MAX_PROVIDER_SESSIONS,
        "maxProviderSessionsPerCaller": MAX_PROVIDER_SESSIONS_PER_CALLER,
        "maxConsumerSessions": MAX_CONSUMER_SESSIONS,
        "maxTombstones": MAX_TOMBSTONES,
    });
    serde_json::to_string(&value).map_err(|error| JsError::new(&error.to_string()))
}

/// Compute the negotiated DATA body limit from both peers' max payloads.
#[wasm_bindgen]
pub fn live_negotiate_max_data_body_bytes(consumer: f64, provider: f64) -> Result<f64, JsError> {
    if !consumer.is_finite() || !provider.is_finite() || consumer < 0.0 || provider < 0.0 {
        return Err(JsError::new(
            "max payloads must be finite nonnegative numbers",
        ));
    }
    trellis_protocol::negotiate_max_data_body_bytes(consumer as u64, provider as u64)
        .map(|value| value as f64)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Derive the exact live delivery subject for one observation.
#[wasm_bindgen]
pub fn live_data_subject(
    provider_connection_id: &str,
    consumer_connection_id: &str,
    session_id: &str,
) -> Result<String, JsError> {
    trellis_protocol::derive_live_data_subject(
        provider_connection_id,
        consumer_connection_id,
        session_id,
    )
    .map_err(|error| JsError::new(&error.to_string()))
}

/// Derive the exact owner-directed control subject for one session.
#[wasm_bindgen]
pub fn live_observe_subject(
    base_subject: &str,
    provider_connection_id: &str,
    session_id: &str,
) -> Result<String, JsError> {
    trellis_protocol::derive_live_observe_subject(base_subject, provider_connection_id, session_id)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Derive the nonqueued owner-control subscription for one provider connection.
#[wasm_bindgen]
pub fn live_observe_wildcard_subject(
    base_subject: &str,
    provider_connection_id: &str,
) -> Result<String, JsError> {
    trellis_protocol::derive_live_observe_wildcard_subject(base_subject, provider_connection_id)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Validate one subject as a canonical live-session route.
#[wasm_bindgen]
pub fn live_validate_subject(subject: &str) -> Result<(), JsError> {
    trellis_protocol::validate_live_subject(subject)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Parse one canonical unsigned 64-bit wire counter.
#[wasm_bindgen]
pub fn live_parse_u64s(value: &str) -> Result<f64, JsError> {
    trellis_protocol::parse_u64s(value, &["counter"])
        .map(|value| value as f64)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Compute the canonical logical-open hash from its JSON identity projection.
#[wasm_bindgen]
pub fn live_logical_open_hash(identity_json: &str) -> Result<String, JsError> {
    let wire: WireLogicalOpenIdentity =
        serde_json::from_str(identity_json).map_err(|error| JsError::new(&error.to_string()))?;
    let identity = trellis_protocol::LogicalOpenIdentity {
        kind: wire.kind,
        base_subject: wire.base_subject,
        open_id: wire.open_id,
        consumer_connection_id: wire.consumer_connection_id,
        consumer_session_key: wire.consumer_session_key,
        consumer_principal_id: wire.consumer_principal_id,
        consumer_participant_id: wire.consumer_participant_id,
        receive_max_payload_bytes: wire.receive_max_payload_bytes,
        live_input: wire.input,
        operation_id: wire.operation_id,
        include_updates: wire.include_updates,
    };
    trellis_protocol::logical_open_hash(&identity).map_err(|error| JsError::new(&error.to_string()))
}

/// Compute the canonical logical-control hash from its raw JSON body.
#[wasm_bindgen]
pub fn live_logical_control_hash(raw: &[u8]) -> Result<String, JsError> {
    let control = trellis_protocol::parse_live_control(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::logical_control_hash(&control)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Build the canonical provider server-message proof digest for exact bytes.
#[wasm_bindgen]
pub fn live_server_proof_digest(
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
) -> Result<String, JsError> {
    trellis_protocol::live_server_proof_digest(context_digest, subject, raw_body)
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Verify one provider server-message proof against the pinned provider key.
#[wasm_bindgen]
pub fn live_verify_server_proof(
    proof: &str,
    context_digest: &str,
    subject: &str,
    raw_body: &[u8],
    provider_key: &str,
) -> Result<(), JsError> {
    let proof = trellis_protocol::LiveServerProof::parse(proof)
        .map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::verify_live_server_proof_encoded(
        &proof,
        context_digest,
        subject,
        raw_body,
        provider_key,
    )
    .map_err(|error| JsError::new(&error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireLogicalOpenIdentity {
    kind: trellis_protocol::LiveSessionKind,
    base_subject: String,
    open_id: String,
    consumer_connection_id: String,
    consumer_session_key: String,
    consumer_principal_id: String,
    consumer_participant_id: String,
    receive_max_payload_bytes: u64,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    include_updates: Option<bool>,
}

fn protocol_error_result(error: &ProtocolError) -> String {
    let (code, path) = match error {
        ProtocolError::Authorization { code, path, .. } => (format!("{code:?}"), path.to_string()),
        ProtocolError::Live { code, path, .. } => (format!("{code:?}"), path.to_string()),
        _ => ("InvalidInput".to_owned(), String::new()),
    };
    json_result(json!({
        "ok": false,
        "error": {
            "code": code,
            "path": path,
        },
    }))
}

fn input_error_result(path: &str) -> String {
    json_result(json!({
        "ok": false,
        "error": {
            "code": "InvalidInput",
            "path": path,
        },
    }))
}

fn json_result(value: Value) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| {
        r#"{"ok":false,"error":{"code":"SerializationError","path":""}}"#.to_owned()
    })
}

/// Authenticate a target request with the same native transcript implementation.
///
/// This authenticates bytes and API stamps; dispatch subsequently requires the
/// verified catalog's whole-capability/action authorization.
#[wasm_bindgen]
pub fn verify_session_request(
    authority: &VerifiedSessionAuthorityHandle,
    request_json: &str,
    payload: &[u8],
) -> String {
    if request_json.len() > 16_384 {
        return input_error_result("");
    }
    let input: WireSessionRequest = match serde_json::from_str(request_json) {
        Ok(input) => input,
        Err(_) => return input_error_result(""),
    };
    let result = verify_session_request_protocol(SessionRequestVerificationInput {
        authority: &authority.authority,
        request: &input.request,
        raw_payload: payload,
        proof: &input.proof,
        policy: &input.policy,
        known_revoked: input.known_revoked,
    });
    match result {
        Ok(verified) => json_result(json!({
            "ok": true,
            "authorityDigest": verified.authority().digest(),
            "requestProofDigest": base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, verified.digest()),
            "caller": verified.caller(),
        })),
        Err(error) => protocol_error_result(&error),
    }
}

/// Authenticate an event using the same native publication-time checks.
///
/// Historical verification requires original metadata from a trusted broker or
/// preserved projector record, not fields supplied by the event publisher.
#[wasm_bindgen]
pub fn verify_session_event(
    authority: &VerifiedSessionAuthorityHandle,
    event_json: &str,
    payload: &[u8],
) -> String {
    if event_json.len() > 16_384 {
        return input_error_result("");
    }
    let input: WireSessionEvent = match serde_json::from_str(event_json) {
        Ok(input) => input,
        Err(_) => return input_error_result(""),
    };
    let result = verify_session_event_protocol(SessionEventVerificationInput {
        authority: &authority.authority,
        event: &input.event,
        raw_payload: payload,
        proof: &input.proof,
        policy: &input.policy,
        original_publication: input.original_publication.as_ref(),
        known_revoked: input.known_revoked,
    });
    match result {
        Ok(verified) => json_result(json!({
            "ok": true,
            "authorityDigest": verified.authority().digest(),
            "eventProofDigest": base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, verified.digest()),
            "publisherAuthority": verified.authority().authority(),
            "originalPublication": verified.original_publication(),
        })),
        Err(error) => protocol_error_result(&error),
    }
}
