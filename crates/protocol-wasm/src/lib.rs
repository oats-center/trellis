//! WASM boundary for deterministic Trellis protocol proof operations.

#![deny(missing_docs)]

use serde::Deserialize;
use serde_json::{json, Value};
use trellis_protocol::{
    decode_pagination_cursor as decode_pagination_cursor_protocol,
    derive_event_subject as derive_event_subject_protocol,
    encode_pagination_cursor as encode_pagination_cursor_protocol,
    pagination_query_digest as pagination_query_digest_protocol, parse_authorization_context,
    session_proof_request_digest as session_proof_request_digest_protocol,
    session_proof_signing_digest as session_proof_signing_digest_protocol,
    verify_authorization_context as verify_authorization_context_protocol,
    verify_authorization_event as verify_authorization_event_protocol,
    verify_authorization_request as verify_authorization_request_protocol,
    verify_session_proof as verify_session_proof_protocol, AuthorizationContextPurpose,
    AuthorizationContextRefreshSessionProofInput, AuthorizationEventProof,
    AuthorizationEventPublisher, AuthorizationEventVerificationInput, AuthorizationIssuerKey,
    AuthorizationRequestProof, AuthorizationRequestVerificationInput,
    AuthorizationVerificationPolicy, NativeBootstrapSessionProofInput, PermissionAtom,
    ProtocolError, SessionProof, SessionProofInput, SessionProofPolicy,
    UserAuthBindSessionProofInput, UserAuthRequestSessionProofInput, VerifiedAuthorizationContext,
};
use wasm_bindgen::prelude::*;

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
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

#[derive(Deserialize)]
#[serde(
    deny_unknown_fields,
    tag = "purpose",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum WireSessionProofInput {
    UserAuthBind {
        origin: String,
        flow_id: String,
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
            WireSessionProofInput::UserAuthBind {
                origin,
                flow_id,
                session_public_key,
                unsigned_request,
            } => Self::user_auth_bind(UserAuthBindSessionProofInput {
                origin,
                flow_id,
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
struct WireAuthorizationVerificationPolicy {
    now_unix_seconds: f64,
    allowed_clock_skew_seconds: u32,
    maximum_context_lifetime_seconds: u32,
    maximum_context_bytes: usize,
    maximum_permissions: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireAuthorizationRequest {
    subject: String,
    reply: RequiredNullable<String>,
    iat: i64,
    request_id: String,
    proof: String,
    required_permissions: Vec<PermissionAtom>,
    policy: WireAuthorizationVerificationPolicy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireAuthorizationEvent {
    subject: String,
    descriptor_identity: String,
    event_id: String,
    event_time: String,
    proof: String,
    #[serde(default)]
    revoked_at: Option<i64>,
    policy: WireAuthorizationVerificationPolicy,
}

fn authorization_verification_policy(
    policy_json: &str,
) -> Result<AuthorizationVerificationPolicy, JsError> {
    let wire: WireAuthorizationVerificationPolicy =
        serde_json::from_str(policy_json).map_err(|error| JsError::new(&error.to_string()))?;
    let policy = authorization_verification_policy_from_wire(&wire)?;
    Ok(policy)
}

fn authorization_verification_policy_from_wire(
    wire: &WireAuthorizationVerificationPolicy,
) -> Result<AuthorizationVerificationPolicy, JsError> {
    let policy = AuthorizationVerificationPolicy::new(
        safe_integer(wire.now_unix_seconds, "nowUnixSeconds")?,
        wire.allowed_clock_skew_seconds,
        wire.maximum_context_lifetime_seconds,
        wire.maximum_context_bytes,
        wire.maximum_permissions,
    )
    .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(policy)
}

/// Verify a live context with its authenticated online issuer entry.
#[wasm_bindgen]
pub fn verify_authorization_context(
    issuer_json: &str,
    context_json: &str,
    policy_json: &str,
) -> Result<String, JsError> {
    create_authorization_context_handle(issuer_json, context_json, policy_json, false)?.projection()
}

/// Opaque Rust-owned authorization context retained for repeated proof verification.
#[wasm_bindgen]
pub struct VerifiedAuthorizationContextHandle {
    context: VerifiedAuthorizationContext,
    projection: String,
}

/// Verify an online-issued context once and retain it inside WASM.
#[wasm_bindgen]
pub fn create_authorization_context_handle(
    issuer_json: &str,
    context_json: &str,
    policy_json: &str,
    historical: bool,
) -> Result<VerifiedAuthorizationContextHandle, JsError> {
    let policy = authorization_verification_policy(policy_json)?;
    let issuer: AuthorizationIssuerKey =
        serde_json::from_str(issuer_json).map_err(|error| JsError::new(&error.to_string()))?;
    let context_value: Value =
        serde_json::from_str(context_json).map_err(|error| JsError::new(&error.to_string()))?;
    let signed_context = parse_authorization_context(&context_value)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let purpose = if historical {
        AuthorizationContextPurpose::HistoricalEvent
    } else {
        AuthorizationContextPurpose::Live
    };
    let context = verify_authorization_context_protocol(&issuer, &signed_context, &policy, purpose)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let projection = serde_json::to_string(&json!({
        "issuer": issuer,
        "contextDigest": context.context_digest(),
        "context": context.signed_context(),
    }))
    .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(VerifiedAuthorizationContextHandle {
        context,
        projection,
    })
}

#[wasm_bindgen]
impl VerifiedAuthorizationContextHandle {
    /// Return the verified context projection used by the TypeScript cache.
    pub fn projection(&self) -> Result<String, JsError> {
        Ok(self.projection.clone())
    }

    /// Require the retained context to be eligible at the supplied current time.
    pub fn assert_current(&self, policy_json: &str) -> Result<(), JsError> {
        let policy = authorization_verification_policy(policy_json)?;
        self.context
            .assert_current(&policy)
            .map_err(|error| JsError::new(&error.to_string()))
    }
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

fn request_result(
    context: &VerifiedAuthorizationContext,
    input: WireAuthorizationRequest,
    payload: &[u8],
) -> String {
    let policy = match authorization_verification_policy_from_wire(&input.policy) {
        Ok(policy) => policy,
        Err(_) => return input_error_result("/policy"),
    };
    let proof = match AuthorizationRequestProof::parse(input.proof) {
        Ok(proof) => proof,
        Err(error) => return protocol_error_result(&error),
    };
    let verified =
        match verify_authorization_request_protocol(AuthorizationRequestVerificationInput {
            context,
            subject: &input.subject,
            reply_subject: input.reply.0.as_deref(),
            raw_payload: payload,
            iat: input.iat,
            request_id: &input.request_id,
            proof: &proof,
            policy: &policy,
            required_permissions: &input.required_permissions,
        }) {
            Ok(verified) => verified,
            Err(error) => return protocol_error_result(&error),
        };
    json_result(json!({
        "ok": true,
        "contextDigest": verified.context().context_digest(),
    }))
}

fn event_publisher_projection(publisher: &AuthorizationEventPublisher) -> Value {
    json!({
        "kind": publisher.kind,
        "deploymentId": publisher.deployment_id,
        "instanceId": publisher.instance_id,
        "participantId": publisher.participant_id,
        "connectionId": publisher.connection_id,
        "loginSessionId": publisher.login_session_id,
    })
}

fn event_result(
    context: &VerifiedAuthorizationContext,
    input: WireAuthorizationEvent,
    payload: &[u8],
) -> String {
    let policy = match authorization_verification_policy_from_wire(&input.policy) {
        Ok(policy) => policy,
        Err(_) => return input_error_result("/policy"),
    };
    let proof = match AuthorizationEventProof::parse(input.proof) {
        Ok(proof) => proof,
        Err(error) => return protocol_error_result(&error),
    };
    let verified = match verify_authorization_event_protocol(AuthorizationEventVerificationInput {
        context,
        subject: &input.subject,
        descriptor_identity: &input.descriptor_identity,
        raw_payload: payload,
        event_id: &input.event_id,
        event_time: &input.event_time,
        proof: &proof,
        policy: &policy,
        revoked_at: input.revoked_at,
    }) {
        Ok(verified) => verified,
        Err(error) => return protocol_error_result(&error),
    };
    json_result(json!({
        "ok": true,
        "contextDigest": verified.context().context_digest(),
        "publisher": event_publisher_projection(verified.publisher()),
    }))
}

/// Verify one context-bound authorization request proof from a JSON argument.
///
/// The result is always a JSON object. Successful results have `ok: true` and
/// contain the verified context digest; rejected inputs have `ok: false`
/// and a stable authorization error code and path.
#[wasm_bindgen]
pub fn verify_authorization_request(
    context: &VerifiedAuthorizationContextHandle,
    request_json: &str,
    payload: &[u8],
) -> String {
    let input: WireAuthorizationRequest = match serde_json::from_str(request_json) {
        Ok(input) => input,
        Err(_) => return input_error_result(""),
    };
    request_result(&context.context, input, payload)
}

/// Verify one context-bound authorization event proof from a JSON argument.
///
/// The result is always a JSON object. Successful results have `ok: true` and
/// contain verified publisher/context metadata; rejected inputs have `ok: false`
/// and a stable authorization error code and path. Event context chains are
/// checked at their signed historical boundary before the strict event-time
/// window is evaluated.
#[wasm_bindgen]
pub fn verify_authorization_event(
    context: &VerifiedAuthorizationContextHandle,
    event_json: &str,
    payload: &[u8],
) -> String {
    let input: WireAuthorizationEvent = match serde_json::from_str(event_json) {
        Ok(input) => input,
        Err(_) => return input_error_result(""),
    };
    event_result(&context.context, input, payload)
}
