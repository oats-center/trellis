use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use jsonptr::PointerBuf;
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{canonicalize_json, ProtocolError, SessionProofErrorCode};

/// Strict device-enrollment signature envelope.
pub const SESSION_PROOF_FORMAT_V1: &str = "trellis.session-proof.v1";

const MAXIMUM_SAFE_JSON_INTEGER: i64 = 9_007_199_254_740_991;
const MAXIMUM_REQUEST_ID_BYTES: usize = 256;
const MAXIMUM_TEXT_BYTES: usize = 16 * 1024;
const MAXIMUM_PROOF_WINDOW_MS: i64 = 5 * 60 * 1_000;

/// One fixed signature domain within [`SESSION_PROOF_FORMAT_V1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionProofPurpose {
    /// Request device enrollment without runtime authority or a claimed assignment.
    DeviceEnrollment,
}

impl SessionProofPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::DeviceEnrollment => "deviceEnrollment",
        }
    }
}

impl fmt::Display for SessionProofPurpose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Validated purpose-specific input to one session proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionProofInput {
    purpose: SessionProofPurpose,
    request_id: String,
    issued_at: i64,
    signer_key_id: String,
    transcript_fields: Vec<Vec<u8>>,
}

/// Owned fields for an identity-signed device-enrollment request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceEnrollmentSessionProofInput {
    /// Exact configured Trellis origin, independently supplied by both peers.
    pub origin: String,
    /// Complete canonical request input, with the entire `proof` field omitted.
    pub unsigned_request: Value,
}

impl SessionProofInput {
    /// Build a device-enrollment proof bound to the selected origin, enrollment
    /// route, identity key, runtime key, and complete unsigned request.
    ///
    /// # Errors
    /// Returns [`ProtocolError::SessionProof`] for malformed or noncanonical input.
    pub fn device_enrollment(
        input: DeviceEnrollmentSessionProofInput,
    ) -> Result<Self, ProtocolError> {
        let DeviceEnrollmentSessionProofInput {
            origin,
            unsigned_request,
        } = input;
        validate_safe_json_integers(&unsigned_request, &mut Vec::new())?;
        let request = unsigned_request.as_object().ok_or_else(|| {
            proof_error(
                SessionProofErrorCode::InvalidFormat,
                ["request"],
                "unsigned request must be an object",
            )
        })?;
        if request.contains_key("proof") {
            return Err(proof_error(
                SessionProofErrorCode::InvalidFormat,
                ["proof"],
                "unsigned request must omit proof",
            ));
        }
        let required_text = |field: &str| -> Result<String, ProtocolError> {
            request
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    proof_error(
                        SessionProofErrorCode::InvalidFormat,
                        [field],
                        "required string is missing",
                    )
                })
        };
        let request_id = required_text("requestId")?;
        let identity_key_id = required_text("identityKeyId")?;
        let session_key = required_text("sessionKey")?;
        let connection_id = required_text("connectionId")?;
        text(&required_text("participantId")?, &["participantId"])?;
        let issued_at = request.get("iat").and_then(Value::as_i64).ok_or_else(|| {
            proof_error(
                SessionProofErrorCode::InvalidFormat,
                ["iat"],
                "iat must be an integer",
            )
        })?;
        if let Some(name) = request.get("name") {
            if name.as_str().is_none_or(|name| name.chars().count() > 128) {
                return Err(proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["name"],
                    "name must be a string of at most 128 characters",
                ));
            }
        }
        validate_key_id(&identity_key_id, &["identityKeyId"])?;
        let session_key = decode_public_key(&session_key, &["sessionKey"])?;
        let request_digest = Sha256::digest(canonicalize_json(&unsigned_request)?.as_bytes());
        validate_request_id(&request_id)?;
        validate_safe_integer(issued_at, &["issuedAt"])?;
        Ok(Self {
            purpose: SessionProofPurpose::DeviceEnrollment,
            request_id,
            issued_at,
            signer_key_id: identity_key_id.clone(),
            transcript_fields: vec![
                text(&origin, &["origin"])?,
                b"/auth/device/enroll".to_vec(),
                digest(&identity_key_id, &["identityKeyId"])?,
                session_key.as_bytes().to_vec(),
                text(&connection_id, &["connectionId"])?,
                request_digest.to_vec(),
            ],
        })
    }

    /// Return the fixed signature purpose.
    #[must_use]
    pub fn purpose(&self) -> SessionProofPurpose {
        self.purpose
    }

    /// Return the caller-generated request identifier.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Return the claimed Unix issue time in milliseconds.
    #[must_use]
    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    /// Return the expected signing-key identifier.
    #[must_use]
    pub fn signer_key_id(&self) -> &str {
        &self.signer_key_id
    }

    fn digest(&self) -> Result<[u8; 32], ProtocolError> {
        let mut transcript = Vec::new();
        push_length_prefixed(&mut transcript, SESSION_PROOF_FORMAT_V1.as_bytes())?;
        push_length_prefixed(&mut transcript, self.purpose.as_str().as_bytes())?;
        push_length_prefixed(&mut transcript, self.request_id.as_bytes())?;
        push_length_prefixed(&mut transcript, self.issued_at.to_string().as_bytes())?;
        for field in &self.transcript_fields {
            push_length_prefixed(&mut transcript, field)?;
        }
        Ok(sha256(&transcript))
    }

    fn validate_signer(&self, key: &VerifyingKey) -> Result<(), ProtocolError> {
        if self.signer_key_id != derived_key_id(key) {
            return Err(proof_error(
                SessionProofErrorCode::InvalidKeyId,
                ["signerKeyId"],
                "signer key id does not match the verification key",
            ));
        }
        Ok(())
    }
}

/// One strict session-proof signature envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProof {
    format: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSessionProof {
    format: String,
    signature: String,
}

impl<'de> Deserialize<'de> for SessionProof {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireSessionProof::deserialize(deserializer)?;
        parse_wire_proof(wire).map_err(de::Error::custom)
    }
}

impl SessionProof {
    /// Return the proof format identifier.
    #[must_use]
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Return the canonical unpadded base64url Ed25519 signature.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }
}

/// Freshness limits applied while verifying session proofs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionProofPolicy {
    maximum_age_ms: i64,
    maximum_future_skew_ms: i64,
}

impl SessionProofPolicy {
    /// Construct bounded proof freshness policy.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] when either limit is negative,
    /// exceeds five minutes, or their sum overflows.
    pub fn new(maximum_age_ms: i64, maximum_future_skew_ms: i64) -> Result<Self, ProtocolError> {
        if !(0..=MAXIMUM_PROOF_WINDOW_MS).contains(&maximum_age_ms)
            || !(0..=MAXIMUM_PROOF_WINDOW_MS).contains(&maximum_future_skew_ms)
        {
            return Err(proof_error(
                SessionProofErrorCode::InvalidFormat,
                std::iter::empty::<&str>(),
                "proof age and future skew must be between zero and five minutes",
            ));
        }
        maximum_age_ms
            .checked_add(maximum_future_skew_ms)
            .ok_or_else(|| {
                proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    std::iter::empty::<&str>(),
                    "proof validity window overflows",
                )
            })?;
        Ok(Self {
            maximum_age_ms,
            maximum_future_skew_ms,
        })
    }

    /// Return the maximum accepted proof age in milliseconds.
    #[must_use]
    pub fn maximum_age_ms(self) -> i64 {
        self.maximum_age_ms
    }

    /// Return the maximum accepted future clock skew in milliseconds.
    #[must_use]
    pub fn maximum_future_skew_ms(self) -> i64 {
        self.maximum_future_skew_ms
    }
}

impl Default for SessionProofPolicy {
    fn default() -> Self {
        Self {
            maximum_age_ms: 30_000,
            maximum_future_skew_ms: 30_000,
        }
    }
}

/// Parse and structurally validate one session-proof envelope.
///
/// # Errors
///
/// Returns [`ProtocolError::SessionProof`] when the value has a wrong format
/// or noncanonical signature encoding.
pub fn parse_session_proof(value: &Value) -> Result<SessionProof, ProtocolError> {
    let wire: WireSessionProof = serde_json::from_value(value.clone()).map_err(|error| {
        proof_error(
            SessionProofErrorCode::InvalidFormat,
            std::iter::empty::<&str>(),
            error.to_string(),
        )
    })?;
    parse_wire_proof(wire)
}

/// Compute the canonical request digest used by HTTP session proofs.
///
/// The input must contain a top-level `proof` object with the exact format and a
/// `signature` member. Only that signature member is removed before Trellis JSON
/// canonicalization and SHA-256 hashing.
///
/// # Errors
///
/// Returns [`ProtocolError::SessionProof`] when the request/proof shape is wrong,
/// the proof format differs, or canonicalization fails.
pub fn session_proof_request_digest(request: &Value) -> Result<String, ProtocolError> {
    let mut unsigned = request.clone();
    let object = unsigned.as_object_mut().ok_or_else(|| {
        proof_error(
            SessionProofErrorCode::InvalidFormat,
            std::iter::empty::<&str>(),
            "proof-bearing request must be an object",
        )
    })?;
    let proof = object
        .get_mut("proof")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            proof_error(
                SessionProofErrorCode::InvalidFormat,
                ["proof"],
                "proof must be an object",
            )
        })?;
    if proof.get("format").and_then(Value::as_str) != Some(SESSION_PROOF_FORMAT_V1) {
        return Err(proof_error(
            SessionProofErrorCode::InvalidFormat,
            ["proof", "format"],
            format!("format must equal '{SESSION_PROOF_FORMAT_V1}'"),
        ));
    }
    if proof.remove("signature").is_none() {
        return Err(proof_error(
            SessionProofErrorCode::InvalidFormat,
            ["proof", "signature"],
            "signature member is required",
        ));
    }
    validate_safe_json_integers(&unsigned, &mut Vec::new())?;
    let canonical = canonicalize_json(&unsigned)?;
    Ok(encode_base64url(&sha256(canonical.as_bytes())))
}

/// Compute the canonical SHA-256 signing digest for one validated proof input.
///
/// This is the cross-language boundary used by WebCrypto clients and WASM
/// bindings. Signatures are Ed25519 signatures over the decoded 32 digest bytes.
///
/// # Errors
///
/// Returns [`ProtocolError::SessionProof`] if a length-prefixed transcript
/// component cannot be encoded.
pub fn session_proof_signing_digest(input: &SessionProofInput) -> Result<String, ProtocolError> {
    Ok(encode_base64url(&input.digest()?))
}

/// Sign one validated purpose-specific session-proof input.
///
/// # Errors
///
/// Returns [`ProtocolError::SessionProof`] when the supplied signing key does not
/// match the input's signer identity or a transcript component cannot be encoded.
pub fn sign_session_proof(
    input: &SessionProofInput,
    signing_key: &SigningKey,
) -> Result<SessionProof, ProtocolError> {
    input.validate_signer(&signing_key.verifying_key())?;
    let digest = input.digest()?;
    Ok(SessionProof {
        format: SESSION_PROOF_FORMAT_V1.to_owned(),
        signature: encode_base64url(&signing_key.sign(&digest).to_bytes()),
    })
}

/// Verify one session proof against its expected signer and freshness policy.
///
/// # Errors
///
/// Returns [`ProtocolError::SessionProof`] when signer identity, proof format,
/// freshness, transcript encoding, or signature verification fails.
pub fn verify_session_proof(
    input: &SessionProofInput,
    proof: &SessionProof,
    expected_signer_public_key: &str,
    now_ms: i64,
    policy: SessionProofPolicy,
) -> Result<(), ProtocolError> {
    validate_safe_integer(now_ms, &["now"])?;
    if proof.format != SESSION_PROOF_FORMAT_V1 {
        return Err(proof_error(
            SessionProofErrorCode::InvalidFormat,
            ["proof", "format"],
            format!("format must equal '{SESSION_PROOF_FORMAT_V1}'"),
        ));
    }
    let oldest = now_ms.checked_sub(policy.maximum_age_ms).ok_or_else(|| {
        proof_error(
            SessionProofErrorCode::ProofIatOutOfRange,
            ["issuedAt"],
            "proof age calculation underflowed",
        )
    })?;
    let newest = now_ms
        .checked_add(policy.maximum_future_skew_ms)
        .ok_or_else(|| {
            proof_error(
                SessionProofErrorCode::ProofIatOutOfRange,
                ["issuedAt"],
                "proof future-skew calculation overflowed",
            )
        })?;
    if !(oldest..=newest).contains(&input.issued_at) {
        return Err(proof_error(
            SessionProofErrorCode::ProofIatOutOfRange,
            ["issuedAt"],
            "proof issue time is outside the accepted policy window",
        ));
    }

    let key = decode_public_key(expected_signer_public_key, &["signerPublicKey"])?;
    input.validate_signer(&key)?;
    let digest = input.digest()?;
    let signature = decode_base64url::<64>(
        &proof.signature,
        &["proof", "signature"],
        SessionProofErrorCode::InvalidSignature,
    )?;
    key.verify_strict(&digest, &Signature::from_bytes(&signature))
        .map_err(|_| {
            proof_error(
                SessionProofErrorCode::InvalidSignature,
                ["proof", "signature"],
                "signature verification failed",
            )
        })?;

    Ok(())
}

fn parse_wire_proof(wire: WireSessionProof) -> Result<SessionProof, ProtocolError> {
    if wire.format != SESSION_PROOF_FORMAT_V1 {
        return Err(proof_error(
            SessionProofErrorCode::InvalidFormat,
            ["format"],
            format!("format must equal '{SESSION_PROOF_FORMAT_V1}'"),
        ));
    }
    decode_base64url::<64>(
        &wire.signature,
        &["signature"],
        SessionProofErrorCode::InvalidSignature,
    )?;
    Ok(SessionProof {
        format: wire.format,
        signature: wire.signature,
    })
}

fn proof_error<'a>(
    code: SessionProofErrorCode,
    tokens: impl IntoIterator<Item = &'a str>,
    message: impl Into<String>,
) -> ProtocolError {
    ProtocolError::SessionProof {
        code,
        path: Box::new(PointerBuf::from_tokens(tokens)),
        message: message.into(),
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn encode_base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_base64url<const N: usize>(
    encoded: &str,
    path: &[&str],
    code: SessionProofErrorCode,
) -> Result<[u8; N], ProtocolError> {
    if encoded.contains('=') {
        return Err(proof_error(
            SessionProofErrorCode::InvalidEncoding,
            path.iter().copied(),
            "padded base64url is not accepted",
        ));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        proof_error(
            SessionProofErrorCode::InvalidEncoding,
            path.iter().copied(),
            "value is not unpadded base64url",
        )
    })?;
    if decoded.len() != N || encode_base64url(&decoded) != encoded {
        return Err(proof_error(
            code,
            path.iter().copied(),
            format!("value must canonically encode exactly {N} bytes"),
        ));
    }
    decoded.try_into().map_err(|_| {
        proof_error(
            code,
            path.iter().copied(),
            format!("value must encode exactly {N} bytes"),
        )
    })
}

fn decode_public_key(encoded: &str, path: &[&str]) -> Result<VerifyingKey, ProtocolError> {
    let key = VerifyingKey::from_bytes(&decode_base64url::<32>(
        encoded,
        path,
        SessionProofErrorCode::InvalidPublicKey,
    )?)
    .map_err(|_| {
        proof_error(
            SessionProofErrorCode::InvalidPublicKey,
            path.iter().copied(),
            "value is not a valid Ed25519 public key",
        )
    })?;
    if key.is_weak() {
        return Err(proof_error(
            SessionProofErrorCode::InvalidPublicKey,
            path.iter().copied(),
            "weak Ed25519 public keys are not accepted",
        ));
    }
    Ok(key)
}

fn derived_key_id(key: &VerifyingKey) -> String {
    encode_base64url(&sha256(key.as_bytes()))
}

fn validate_key_id(value: &str, path: &[&str]) -> Result<(), ProtocolError> {
    decode_base64url::<32>(value, path, SessionProofErrorCode::InvalidKeyId).map(|_| ())
}

fn validate_safe_json_integers(value: &Value, path: &mut Vec<String>) -> Result<(), ProtocolError> {
    match value {
        Value::Number(number) => {
            let unsafe_integer = if let Some(value) = number.as_i64() {
                value.unsigned_abs() > MAXIMUM_SAFE_JSON_INTEGER as u64
            } else if let Some(value) = number.as_u64() {
                value > MAXIMUM_SAFE_JSON_INTEGER as u64
            } else {
                number.as_f64().is_some_and(|value| {
                    value.fract() == 0.0 && value.abs() > MAXIMUM_SAFE_JSON_INTEGER as f64
                })
            };
            if unsafe_integer {
                return Err(ProtocolError::SessionProof {
                    code: SessionProofErrorCode::UnsafeJsonInteger,
                    path: Box::new(PointerBuf::from_tokens(path.iter().map(String::as_str))),
                    message: "integer must be within the interoperable JSON safe-integer range"
                        .to_owned(),
                });
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(index.to_string());
                validate_safe_json_integers(value, path)?;
                path.pop();
            }
        }
        Value::Object(values) => {
            for (name, value) in values {
                path.push(name.clone());
                validate_safe_json_integers(value, path)?;
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn validate_safe_integer(value: i64, path: &[&str]) -> Result<(), ProtocolError> {
    if (-MAXIMUM_SAFE_JSON_INTEGER..=MAXIMUM_SAFE_JSON_INTEGER).contains(&value) {
        Ok(())
    } else {
        Err(proof_error(
            SessionProofErrorCode::UnsafeJsonInteger,
            path.iter().copied(),
            "integer must be within the interoperable JSON safe-integer range",
        ))
    }
}

fn validate_request_id(value: &str) -> Result<(), ProtocolError> {
    validate_text(value, &["requestId"])?;
    if value.len() <= MAXIMUM_REQUEST_ID_BYTES {
        Ok(())
    } else {
        Err(proof_error(
            SessionProofErrorCode::InvalidFormat,
            ["requestId"],
            "request ID exceeds 256 UTF-8 bytes",
        ))
    }
}

fn validate_text(value: &str, path: &[&str]) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > MAXIMUM_TEXT_BYTES
        || value.trim() != value
        || value.chars().any(|character| character.is_ascii_control())
    {
        return Err(proof_error(
            SessionProofErrorCode::InvalidFormat,
            path.iter().copied(),
            "value must be bounded, nonempty protocol-safe text",
        ));
    }
    Ok(())
}

fn text(value: &str, path: &[&str]) -> Result<Vec<u8>, ProtocolError> {
    validate_text(value, path)?;
    Ok(value.as_bytes().to_vec())
}

fn digest(value: &str, path: &[&str]) -> Result<Vec<u8>, ProtocolError> {
    Ok(decode_base64url::<32>(value, path, SessionProofErrorCode::InvalidEncoding)?.to_vec())
}

fn push_length_prefixed(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ProtocolError> {
    let length = u32::try_from(bytes.len()).map_err(|_| {
        proof_error(
            SessionProofErrorCode::InvalidFormat,
            std::iter::empty::<&str>(),
            "signature input component exceeds u32 length",
        )
    })?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_request_digest_removes_only_signature() -> Result<(), ProtocolError> {
        let first = json!({"requestId":"req_1","proof":{"format":SESSION_PROOF_FORMAT_V1,"signature":"first"},"nullable":null});
        let second = json!({"nullable":null,"proof":{"signature":"second","format":SESSION_PROOF_FORMAT_V1},"requestId":"req_1"});
        assert_eq!(
            session_proof_request_digest(&first)?,
            session_proof_request_digest(&second)?
        );
        let mut changed = second;
        changed["nullable"] = json!("changed");
        assert_ne!(
            session_proof_request_digest(&first)?,
            session_proof_request_digest(&changed)?
        );
        Ok(())
    }

    #[test]
    fn rejects_unsafe_request_integers_and_weak_keys() {
        let unsafe_request = json!({"proof":{"format":SESSION_PROOF_FORMAT_V1,"signature":encode_base64url(&[0;64])},"nested":{"counter":9_007_199_254_740_992_u64}});
        assert!(matches!(
            session_proof_request_digest(&unsafe_request),
            Err(ProtocolError::SessionProof {
                code: SessionProofErrorCode::UnsafeJsonInteger,
                ..
            })
        ));
        assert!(matches!(
            decode_public_key(&encode_base64url(&[0; 32]), &["sessionPublicKey"]),
            Err(ProtocolError::SessionProof {
                code: SessionProofErrorCode::InvalidPublicKey,
                ..
            })
        ));
    }

    fn enrollment_input() -> (SigningKey, DeviceEnrollmentSessionProofInput) {
        let identity = SigningKey::from_bytes(&[41; 32]);
        let session = SigningKey::from_bytes(&[42; 32]);
        let input = DeviceEnrollmentSessionProofInput {
            origin: "https://trellis.example".into(),
            unsigned_request: json!({
                "identityKeyId":derived_key_id(&identity.verifying_key()),
                "sessionKey":encode_base64url(session.verifying_key().as_bytes()),
                "connectionId":"01JY0000000000000000000001",
                "requestId":"01JY0000000000000000000002",
                "participantId":"example.device",
                "iat":1_750_000_000_000_i64,
                "name":"device one", "extension":{"value":1}
            }),
        };
        (identity, input)
    }

    #[test]
    fn device_enrollment_binds_origin_identity_and_complete_unsigned_request(
    ) -> Result<(), ProtocolError> {
        let (identity, input) = enrollment_input();
        let enrollment = SessionProofInput::device_enrollment(input.clone())?;
        let proof = sign_session_proof(&enrollment, &identity)?;
        let key = encode_base64url(identity.verifying_key().as_bytes());
        let policy = SessionProofPolicy::default();
        verify_session_proof(&enrollment, &proof, &key, enrollment.issued_at(), policy)?;
        let mut other_origin = input.clone();
        other_origin.origin = "https://another.example".into();
        assert!(verify_session_proof(
            &SessionProofInput::device_enrollment(other_origin)?,
            &proof,
            &key,
            enrollment.issued_at(),
            policy
        )
        .is_err());
        for (field, value) in [
            ("connectionId", json!("01JY0000000000000000000003")),
            ("requestId", json!("01JY0000000000000000000004")),
            ("iat", json!(enrollment.issued_at() + 1)),
            ("participantId", json!("example.another-device")),
            ("name", json!("changed name")),
            ("extension", json!({"value":2})),
            (
                "sessionKey",
                json!(encode_base64url(
                    SigningKey::from_bytes(&[43; 32]).verifying_key().as_bytes()
                )),
            ),
            (
                "identityKeyId",
                json!(derived_key_id(
                    &SigningKey::from_bytes(&[44; 32]).verifying_key()
                )),
            ),
        ] {
            let mut modified = input.clone();
            modified.unsigned_request[field] = value;
            let modified = SessionProofInput::device_enrollment(modified)?;
            assert!(
                verify_session_proof(&modified, &proof, &key, enrollment.issued_at(), policy)
                    .is_err(),
                "{field} must be signature-bound"
            );
        }
        assert!(sign_session_proof(&enrollment, &SigningKey::from_bytes(&[42; 32])).is_err());
        Ok(())
    }

    #[test]
    fn device_enrollment_rejects_invalid_unsigned_inputs_and_stale_proofs(
    ) -> Result<(), ProtocolError> {
        let (identity, input) = enrollment_input();
        let enrollment = SessionProofInput::device_enrollment(input.clone())?;
        let proof = sign_session_proof(&enrollment, &identity)?;
        let key = encode_base64url(identity.verifying_key().as_bytes());
        for now in [
            enrollment.issued_at() - 30_001,
            enrollment.issued_at() + 30_001,
        ] {
            assert!(verify_session_proof(
                &enrollment,
                &proof,
                &key,
                now,
                SessionProofPolicy::default()
            )
            .is_err());
        }
        for (field, value) in [
            ("proof", json!({"format":SESSION_PROOF_FORMAT_V1})),
            ("name", json!("x".repeat(129))),
            ("name", Value::Null),
            ("iat", json!(9_007_199_254_740_992_u64)),
            ("iat", json!(0.5)),
            ("requestId", json!("")),
            ("connectionId", json!("")),
            ("participantId", json!("")),
            ("identityKeyId", json!("invalid")),
            ("sessionKey", json!(encode_base64url(&[0; 32]))),
            ("extension", json!({"value":9_007_199_254_740_992_u64})),
        ] {
            let mut modified = input.clone();
            modified.unsigned_request[field] = value;
            assert!(
                SessionProofInput::device_enrollment(modified).is_err(),
                "{field} must fail validation"
            );
        }
        for field in [
            "identityKeyId",
            "sessionKey",
            "connectionId",
            "requestId",
            "participantId",
            "iat",
        ] {
            let mut modified = input.clone();
            modified
                .unsigned_request
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(SessionProofInput::device_enrollment(modified).is_err());
        }
        Ok(())
    }
}
