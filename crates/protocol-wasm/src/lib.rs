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
    verify_session_proof as verify_session_proof_protocol, AuthorityIssuerKey,
    DeviceEnrollmentSessionProofInput, ProtocolError, SessionAuthorityPurpose,
    SessionAuthorityVerificationInput, SessionAuthorityVerificationPolicy, SessionProof,
    SessionProofInput, SessionProofPolicy, SessionRequest, TransportAuthorizationV1,
    VerifiedSessionAuthority,
};
use trellis_protocol::{
    verify_session_event as verify_session_event_protocol, OriginalEventPublication, SessionEvent,
    SessionEventProof, SessionEventVerificationInput,
};
#[cfg(feature = "provider")]
use trellis_protocol::{
    verify_session_request as verify_session_request_protocol, SessionRequestProof,
    SessionRequestVerificationInput,
};
use trellis_protocol::{
    verify_session_response as verify_session_response_protocol, CatalogActionIdentity,
    SessionResponse, SessionResponseProof, SessionResponseVerificationInput,
};
use wasm_bindgen::prelude::*;

/// Validate and normalize framework-private Operation state with the native codec.
#[cfg(feature = "provider")]
#[wasm_bindgen]
pub fn normalize_operation_record(record_json: &str) -> Result<String, JsError> {
    use trellis_protocol::operation_record::{encode_operation_record, DurableOperationRecord};
    let record: DurableOperationRecord =
        serde_json::from_str(record_json).map_err(|error| JsError::new(&error.to_string()))?;
    let bytes =
        encode_operation_record(&record).map_err(|error| JsError::new(&error.to_string()))?;
    String::from_utf8(bytes[9..].to_vec()).map_err(|error| JsError::new(&error.to_string()))
}

/// Sign the native own-admission request with the ephemeral session seed.
#[wasm_bindgen]
pub fn sign_admission_get(
    request_json: &str,
    subject: &str,
    reply: &str,
    seed: &[u8],
) -> Result<String, JsError> {
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| JsError::new("invalid session seed"))?;
    let request =
        serde_json::from_str(request_json).map_err(|error| JsError::new(&error.to_string()))?;
    let signed = trellis_protocol::sign_admission_get(request, subject, reply, &seed.into())
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&signed).map_err(|error| JsError::new(&error.to_string()))
}

/// Sign a provisioned CONNECT credential without client-asserted deployment claims.
#[cfg(feature = "provider")]
#[wasm_bindgen]
pub fn sign_provisioned_admission(credential_json: &str, seed: &[u8]) -> Result<String, JsError> {
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| JsError::new("invalid identity seed"))?;
    let credential =
        serde_json::from_str(credential_json).map_err(|error| JsError::new(&error.to_string()))?;
    let signed = trellis_protocol::sign_provisioned_admission(credential, &seed.into())
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&signed).map_err(|error| JsError::new(&error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireAdmissionVerification {
    issuer: AuthorityIssuerKey,
    request: trellis_protocol::AdmissionGet,
    subject: String,
    reply: String,
    account: String,
    now_unix_seconds: i64,
}

/// Authenticate a signed admission reply against the exact pending request.
#[wasm_bindgen]
pub fn verify_admission_context(bytes: &[u8], verification_json: &str) -> Result<String, JsError> {
    let input: WireAdmissionVerification = serde_json::from_str(verification_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let signed = trellis_protocol::verify_admission_context(
        bytes,
        &input.issuer,
        &input.request,
        &input.subject,
        &input.reply,
        &input.account,
        input.now_unix_seconds,
    )
    .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&signed).map_err(|error| JsError::new(&error.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireOAuthAdmissionVerification {
    issuer: AuthorityIssuerKey,
    trellis_instance_id: String,
    audience_nats_account: String,
    ephemeral_nkey: String,
    now_unix_seconds: i64,
    maximum_lifetime_seconds: u32,
}

/// Authenticate a bounded OAuth admission against explicitly selected trust.
///
/// The caller must additionally match runtime, client, durable DPoP binding and
/// saved grant identity before installing credentials or connecting.
#[wasm_bindgen]
pub fn verify_oauth_admission(bytes: &[u8], verification_json: &str) -> String {
    if verification_json.len() > 16_384 {
        return input_error_result("");
    }
    let input: WireOAuthAdmissionVerification = match serde_json::from_str(verification_json) {
        Ok(input) => input,
        Err(_) => return input_error_result(""),
    };
    match trellis_protocol::verify_oauth_admission(
        bytes,
        &input.issuer,
        &input.trellis_instance_id,
        &input.audience_nats_account,
        &input.ephemeral_nkey,
        input.now_unix_seconds,
        input.maximum_lifetime_seconds,
    ) {
        Ok(admission) => json_result(json!({"ok": true, "admission": admission})),
        Err(error) => protocol_error_result(&error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireAuthorityMetadataVerification {
    issuer: AuthorityIssuerKey,
    trellis_instance_id: String,
    audience_nats_account: String,
    now_unix_seconds: i64,
    installed_sequence: Option<trellis_protocol::U64s>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireProviderPublication {
    catalog_issuer: AuthorityIssuerKey,
    certificate_issuer: AuthorityIssuerKey,
    trellis_instance_id: String,
    audience_nats_account: String,
    at: i64,
    published_at: Option<i64>,
}

/// A catalog snapshot authenticated once against its issuer and trust scope.
///
/// Each authorization rechecks only the issuer's key and revocation state
/// and the trust scope, never the signature or structure again.
#[wasm_bindgen]
pub struct AuthenticatedCatalogHandle {
    inner: trellis_protocol::AuthenticatedCatalog,
}

/// A provider certificate authenticated once against its issuer and scope.
///
/// Each use rechecks the issuer, scope and the certificate's validity time.
#[wasm_bindgen]
pub struct AuthenticatedProviderCertificateHandle {
    inner: trellis_protocol::AuthenticatedProviderCertificate,
}

#[wasm_bindgen]
impl AuthenticatedCatalogHandle {
    /// Content address and immutable authenticated record as JSON.
    pub fn projection(&self) -> String {
        json!({"digest": self.inner.digest(), "record": self.inner.record()}).to_string()
    }
}

#[wasm_bindgen]
impl AuthenticatedProviderCertificateHandle {
    /// Content address and immutable authenticated record as JSON.
    pub fn projection(&self) -> String {
        json!({"digest": self.inner.digest(), "record": self.inner.record()}).to_string()
    }
}

/// Authenticate a canonical catalog snapshot once.
#[wasm_bindgen]
pub fn create_catalog_handle(
    bytes: &[u8],
    verification_json: &str,
) -> Result<AuthenticatedCatalogHandle, JsError> {
    if bytes.len() > 2 * 1024 * 1024 || verification_json.len() > 16_384 {
        return Err(JsError::new("catalog byte budget exceeded"));
    }
    let input: WireAuthorityMetadataVerification = serde_json::from_str(verification_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let record = serde_json::from_slice(bytes).map_err(|error| JsError::new(&error.to_string()))?;
    let inner = trellis_protocol::AuthenticatedCatalog::authenticate(
        record,
        &input.issuer,
        &input.trellis_instance_id,
        &input.audience_nats_account,
    )
    .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(AuthenticatedCatalogHandle { inner })
}

/// Authenticate a canonical provider certificate once, valid at `nowUnixSeconds`.
#[wasm_bindgen]
pub fn create_provider_certificate_handle(
    bytes: &[u8],
    verification_json: &str,
) -> Result<AuthenticatedProviderCertificateHandle, JsError> {
    if bytes.len() > 2 * 1024 * 1024 || verification_json.len() > 16_384 {
        return Err(JsError::new("certificate byte budget exceeded"));
    }
    let input: WireAuthorityMetadataVerification = serde_json::from_str(verification_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let record = serde_json::from_slice(bytes).map_err(|error| JsError::new(&error.to_string()))?;
    let inner = trellis_protocol::AuthenticatedProviderCertificate::authenticate(
        record,
        &input.issuer,
        &input.trellis_instance_id,
        &input.audience_nats_account,
    )
    .and_then(|inner| {
        inner.check_at(
            &input.issuer,
            &input.trellis_instance_id,
            &input.audience_nats_account,
            input.now_unix_seconds,
        )?;
        Ok(inner)
    })
    .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(AuthenticatedProviderCertificateHandle { inner })
}

/// Authenticate one canonical revocation or issuer rotation; decoding alone never
/// establishes trust. Catalogs and certificates become handles instead.
#[wasm_bindgen]
pub fn verify_authority_metadata(
    kind: &str,
    bytes: &[u8],
    verification_json: &str,
) -> Result<String, JsError> {
    if bytes.len() > 2 * 1024 * 1024 || verification_json.len() > 16_384 {
        return Err(JsError::new("metadata byte budget exceeded"));
    }
    let input: WireAuthorityMetadataVerification = serde_json::from_str(verification_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let result = (|| -> Result<Value, JsError> {
        let issuer = &input.issuer;
        let instance = &input.trellis_instance_id;
        let account = &input.audience_nats_account;
        match kind {
            "revocation" => {
                let record: trellis_protocol::SignedSessionRevocation =
                    serde_json::from_slice(bytes)?;
                record.verify(issuer, instance, account)?;
                Ok(json!({"ok": true, "digest": record.digest()?, "record": record}))
            }
            "rotation" => {
                let record: trellis_protocol::SignedIssuerRotation = serde_json::from_slice(bytes)?;
                let successor = record.verify_successor(
                    issuer,
                    instance,
                    account,
                    input
                        .installed_sequence
                        .map_or(0, |sequence| sequence.get()),
                    input.now_unix_seconds,
                )?;
                Ok(
                    json!({"ok": true, "digest": record.digest()?, "record": record, "successor": successor}),
                )
            }
            _ => Err(JsError::new("unknown metadata kind")),
        }
    })()?;
    serde_json::to_string(&result).map_err(|error| JsError::new(&error.to_string()))
}

/// Check signed whole-capability membership against an already verified authority.
#[wasm_bindgen]
pub fn authorize_catalog_action(
    authority: &VerifiedSessionAuthorityHandle,
    catalog: &AuthenticatedCatalogHandle,
    verification_json: &str,
    action_json: &str,
) -> Result<String, JsError> {
    if verification_json.len() > 16_384 || action_json.len() > 4096 {
        return Err(JsError::new("catalog byte budget exceeded"));
    }
    let input: WireAuthorityMetadataVerification = serde_json::from_str(verification_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let action =
        serde_json::from_str(action_json).map_err(|error| JsError::new(&error.to_string()))?;
    catalog
        .inner
        .check(
            &input.issuer,
            &input.trellis_instance_id,
            &input.audience_nats_account,
        )
        .map_err(|error| JsError::new(&error.to_string()))?;
    let authorized = catalog
        .inner
        .authorizes(authority.authority.signed(), &action);
    Ok(json!({"ok": true, "authorized": authorized}).to_string())
}

/// Authorize a provider's own event publication by its certified implementation,
/// at the authenticated event time and, when retained, its publication time.
#[wasm_bindgen]
pub fn authorize_provider_event_publication(
    authority: &VerifiedSessionAuthorityHandle,
    catalog: &AuthenticatedCatalogHandle,
    certificate: &AuthenticatedProviderCertificateHandle,
    publication_json: &str,
    action_json: &str,
) -> Result<String, JsError> {
    if publication_json.len() > 16_384 || action_json.len() > 4096 {
        return Err(JsError::new("publication byte budget exceeded"));
    }
    let input: WireProviderPublication =
        serde_json::from_str(publication_json).map_err(|error| JsError::new(&error.to_string()))?;
    let action =
        serde_json::from_str(action_json).map_err(|error| JsError::new(&error.to_string()))?;
    let result = (|| -> Result<bool, trellis_protocol::ProtocolError> {
        catalog.inner.check(
            &input.catalog_issuer,
            &input.trellis_instance_id,
            &input.audience_nats_account,
        )?;
        let signed = authority.authority.signed();
        let mut authorized = certificate.inner.authorizes_event_publication(
            &input.certificate_issuer,
            signed,
            &catalog.inner,
            &action,
            input.at,
        )?;
        if let Some(published) = input.published_at {
            authorized &= certificate.inner.authorizes_event_publication(
                &input.certificate_issuer,
                signed,
                &catalog.inner,
                &action,
                published,
            )?;
        }
        Ok(authorized)
    })()
    .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(json!({"ok": true, "authorized": result}).to_string())
}

/// Generate a high-entropy canonical Transfer v1 session ID.
#[cfg(feature = "provider")]
#[wasm_bindgen]
pub fn transfer_generate_id() -> Result<String, JsError> {
    trellis_protocol::transfer::generate_transfer_id()
        .map_err(|error| JsError::new(&error.to_string()))
}

/// Derive an exact Transfer v1 subject from logical endpoint identities.
/// `kind` is upload-data, download-data, control or signal.
#[cfg(feature = "provider")]
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

/// Receive window `[frames, bytes]` to offer from `available` admission bytes,
/// or `null` when less than one frame fits. The one policy both SDKs share.
#[wasm_bindgen]
pub fn transfer_receive_window(available: f64, max_frame_bytes: f64) -> Result<String, JsError> {
    let available = u64::try_from(safe_integer(available, "available")?)
        .map_err(|_| JsError::new("negative admission bytes"))?;
    let max_frame_bytes = u64::try_from(safe_integer(max_frame_bytes, "maxFrameBytes")?)
        .map_err(|_| JsError::new("negative frame limit"))?;
    Ok(
        match trellis_protocol::transfer::transfer_receive_window(available, max_frame_bytes) {
            Some((frames, bytes)) => json!([frames, bytes]),
            None => serde_json::Value::Null,
        }
        .to_string(),
    )
}

/// Verify a provider's Ed25519 proof over a 32-byte domain-separated digest.
///
/// Transfer DATA and Live server proofs are digested natively by the caller;
/// signature verification keeps the shared strict rules.
#[wasm_bindgen]
pub fn verify_proof_digest(proof: &str, digest: &[u8], public_key: &str) -> Result<(), JsError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let decode = |value: &str, length: usize, field: &str| {
        URL_SAFE_NO_PAD
            .decode(value)
            .ok()
            .filter(|bytes| bytes.len() == length && URL_SAFE_NO_PAD.encode(bytes) == value)
            .ok_or_else(|| JsError::new(&format!("{field} must be canonical base64url")))
    };
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| JsError::new("proof digest must be 32 bytes"))?;
    let key: [u8; 32] = decode(public_key, 32, "public key")?
        .try_into()
        .map_err(|_| JsError::new("public key must be 32 bytes"))?;
    let signature: [u8; 64] = decode(proof, 64, "proof")?
        .try_into()
        .map_err(|_| JsError::new("proof must be 64 bytes"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&key)
        .map_err(|_| JsError::new("invalid Ed25519 public key"))?
        .verify_strict(&digest, &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| JsError::new("provider signature verification failed"))
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
        "maxWindowFrames": TRANSFER_MAX_WINDOW_FRAMES,
        "maxWindowBytes": TRANSFER_MAX_WINDOW_BYTES,
        "processReceiveBytes": TRANSFER_PROCESS_RECEIVE_BYTES,
        "creditFrameStep": TRANSFER_CREDIT_FRAME_STEP,
        "creditByteStep": TRANSFER_CREDIT_BYTE_STEP,
        "creditMaxDelayMs": TRANSFER_CREDIT_MAX_DELAY_MS,
        "headerReserve": TRANSFER_HEADER_RESERVE,
        "operationUploadLifetimeSeconds": TRANSFER_OPERATION_UPLOAD_LIFETIME_SECONDS,
        "operationUploadMaxBytes": TRANSFER_OPERATION_UPLOAD_MAX_BYTES,
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
#[cfg(feature = "provider")]
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
#[cfg(feature = "provider")]
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
    DeviceEnrollment {
        origin: String,
        unsigned_request: Value,
    },
}

impl TryFrom<WireSessionProofInput> for SessionProofInput {
    type Error = ProtocolError;
    fn try_from(value: WireSessionProofInput) -> Result<Self, Self::Error> {
        let WireSessionProofInput::DeviceEnrollment {
            origin,
            unsigned_request,
        } = value;
        Self::device_enrollment(DeviceEnrollmentSessionProofInput {
            origin,
            unsigned_request,
        })
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

#[cfg(feature = "provider")]
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireSessionResponse {
    response: SessionResponse,
    proof: SessionResponseProof,
    certificate_issuer: AuthorityIssuerKey,
    pending_request: SessionRequest,
    pending_request_digest: String,
    action: CatalogActionIdentity,
    expected_deployment_id: Option<String>,
    received_subject: String,
    policy: SessionAuthorityVerificationPolicy,
    known_revoked: bool,
}

/// Verify a provider response against a retained opening request and certified action.
#[wasm_bindgen]
pub fn verify_session_response(
    provider: &VerifiedSessionAuthorityHandle,
    certificate: &AuthenticatedProviderCertificateHandle,
    response_json: &str,
    raw_payload: &[u8],
) -> String {
    use base64::Engine as _;
    if response_json.len() > 1_048_576 {
        return input_error_result("response");
    }
    let input: WireSessionResponse = match serde_json::from_str(response_json) {
        Ok(input) => input,
        Err(_) => return input_error_result("response"),
    };
    let digest = match base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&input.pending_request_digest)
    {
        Ok(bytes)
            if bytes.len() == 32
                && base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes)
                    == input.pending_request_digest =>
        {
            bytes
        }
        _ => return input_error_result("pendingRequestDigest"),
    };
    let Ok(digest) = <[u8; 32]>::try_from(digest.as_slice()) else {
        return input_error_result("pendingRequestDigest");
    };
    match verify_session_response_protocol(SessionResponseVerificationInput {
        provider: &provider.authority,
        certificate: &certificate.inner,
        certificate_issuer: &input.certificate_issuer,
        pending_request: &input.pending_request,
        pending_request_digest: &digest,
        action: &input.action,
        expected_deployment_id: input.expected_deployment_id.as_deref(),
        received_subject: &input.received_subject,
        response: &input.response,
        raw_payload,
        proof: &input.proof,
        policy: &input.policy,
        known_revoked: input.known_revoked,
    }) {
        Ok(verified) => json_result(json!({
            "ok": true,
            "providerAuthorityDigest": verified.provider().digest(),
            "response": verified.response(),
        })),
        Err(error) => protocol_error_result(&error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WireIssuerMetadataVerification {
    issuer: AuthorityIssuerKey,
    request: trellis_protocol::IssuerMetadataRequest,
    request_subject: String,
    reply_subject: String,
    now_unix_seconds: i64,
}

/// Return the protocol-owned admitted-connection metadata bootstrap subject.
#[wasm_bindgen]
pub fn issuer_metadata_subject() -> String {
    trellis_protocol::ISSUER_METADATA_SUBJECT.into()
}

/// Authenticate public metadata against a retained nonce and configured issuer.
///
/// The responder's own context is deliberately unnecessary: the pinned Auth
/// signature authenticates the objects required to resolve that context.
#[wasm_bindgen]
pub fn verify_issuer_metadata(verification_json: &str, raw_response: &[u8]) -> String {
    if verification_json.len() > 16_384 {
        return input_error_result("metadata");
    }
    let input: WireIssuerMetadataVerification = match serde_json::from_str(verification_json) {
        Ok(input) => input,
        Err(_) => return input_error_result("metadata"),
    };
    match trellis_protocol::verify_issuer_metadata(
        raw_response,
        &input.issuer,
        &input.request,
        &input.request_subject,
        &input.reply_subject,
        input.now_unix_seconds,
    ) {
        Ok(verified) => json_result(json!({"ok": true, "response": verified.response()})),
        Err(error) => protocol_error_result(&error),
    }
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
        consumer_binding: wire.consumer_binding,
        receive_max_payload_bytes: wire.receive_max_payload_bytes,
        live_input: wire.input,
        operation_id: wire.operation_id,
        include_updates: wire.include_updates,
    };
    trellis_protocol::logical_open_hash(&identity).map_err(|error| JsError::new(&error.to_string()))
}

/// Compute the canonical logical-control hash from its raw JSON body.
#[cfg(feature = "provider")]
#[wasm_bindgen]
pub fn live_logical_control_hash(raw: &[u8]) -> Result<String, JsError> {
    let control = trellis_protocol::parse_live_control(raw)
        .map_err(|error| JsError::new(&error.to_string()))?;
    trellis_protocol::logical_control_hash(&control)
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
    consumer_binding: trellis_protocol::SessionBinding,
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
#[cfg(feature = "provider")]
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
