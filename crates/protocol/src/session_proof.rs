use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use jsonptr::PointerBuf;
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::{canonicalize_json, ProtocolError, SessionProofErrorCode};

/// Strict wire format for auth, bootstrap, and authorization-context refresh proofs.
pub const SESSION_PROOF_FORMAT_V1: &str = "trellis.session-proof.v1";

const MAXIMUM_SAFE_JSON_INTEGER: i64 = 9_007_199_254_740_991;
const MAXIMUM_REQUEST_ID_BYTES: usize = 256;
const MAXIMUM_TEXT_BYTES: usize = 16 * 1024;
const MAXIMUM_PROOF_WINDOW_MS: i64 = 5 * 60 * 1_000;

/// One fixed signature domain within [`SESSION_PROOF_FORMAT_V1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionProofPurpose {
    /// Start a user app or agent browser-auth request.
    UserAuthRequest,
    /// Claim an approved browser-auth flow with its enrolled session key.
    UserAuthBind,
    /// Bootstrap a provisioned service instance.
    ServiceBootstrap,
    /// Bootstrap a provisioned or activated device.
    DeviceBootstrap,
    /// Request device enrollment without runtime authority or a claimed assignment.
    DeviceEnrollment,
    /// Refresh an authorization context using the durable session key.
    AuthorizationContextRefresh,
}

impl SessionProofPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserAuthRequest => "userAuthRequest",
            Self::UserAuthBind => "userAuthBind",
            Self::ServiceBootstrap => "serviceBootstrap",
            Self::DeviceBootstrap => "deviceBootstrap",
            Self::DeviceEnrollment => "deviceEnrollment",
            Self::AuthorizationContextRefresh => "authorizationContextRefresh",
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

/// Owned fields for a user browser-auth initiation proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserAuthRequestSessionProofInput {
    /// Exact configured Trellis origin, independently supplied by both peers.
    pub origin: String,
    /// Complete canonical request input, with the entire `proof` field omitted.
    pub unsigned_request: Value,
}

/// Owned fields for claiming an approved browser-auth flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserAuthBindSessionProofInput {
    /// Exact configured Trellis origin, independently supplied by both peers.
    pub origin: String,
    /// Immutable browser flow identifier.
    pub flow_id: String,
    /// Enrolled unpadded base64url Ed25519 session public key.
    pub session_public_key: String,
    /// Complete canonical request input, with the entire `proof` field omitted.
    pub unsigned_request: Value,
}

/// Proof inputs shared by native service and device bootstrap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeBootstrapSessionProofInput {
    /// Exact configured Trellis origin, independently supplied by both peers.
    pub origin: String,
    /// Complete canonical request input, with the entire `proof` field omitted.
    pub unsigned_request: Value,
}

/// Owned fields for an authorization-context refresh proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationContextRefreshSessionProofInput {
    /// Exact configured Trellis origin, never taken from a forwarded Host header.
    pub origin: String,
    /// Installation public key loaded from the authenticated login.
    pub session_public_key: String,
    /// Complete request object with the proof field omitted.
    pub unsigned_request: Value,
}

impl SessionProofInput {
    /// Build a user browser-auth initiation proof input.
    ///
    /// `request_digest` is the output of [`session_proof_request_digest`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] when a field is noncanonical,
    /// unsafe, empty, malformed, or the NKey does not encode `session_public_key`.
    pub fn user_auth_request(
        input: UserAuthRequestSessionProofInput,
    ) -> Result<Self, ProtocolError> {
        let UserAuthRequestSessionProofInput {
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
        let issued_at = request
            .get("issuedAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["issuedAt"],
                    "issuedAt must be an integer",
                )
            })?;
        let session_public_key = required_text("sessionPublicKey")?;
        let key = decode_public_key(&session_public_key, &["sessionPublicKey"])?;
        let signer_key_id = derived_key_id(&key);
        Self::new(
            SessionProofPurpose::UserAuthRequest,
            request_id,
            issued_at,
            signer_key_id,
            vec![
                text(&origin, &["origin"])?,
                text("/auth/requests", &["route"])?,
                key.as_bytes().to_vec(),
                Sha256::digest(canonicalize_json(&unsigned_request)?.as_bytes()).to_vec(),
            ],
        )
    }

    /// Build a proof input for claiming an approved browser-auth flow.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] when a field is noncanonical,
    /// unsafe, empty, or malformed.
    pub fn user_auth_bind(input: UserAuthBindSessionProofInput) -> Result<Self, ProtocolError> {
        let UserAuthBindSessionProofInput {
            origin,
            flow_id,
            session_public_key,
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
        let request_id = request
            .get("requestId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["requestId"],
                    "requestId must be a string",
                )
            })?
            .to_owned();
        let issued_at = request
            .get("issuedAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["issuedAt"],
                    "issuedAt must be an integer",
                )
            })?;
        if ulid::Ulid::from_string(&request_id)
            .map(|parsed| parsed.to_string() != request_id)
            .unwrap_or(true)
        {
            return Err(proof_error(
                SessionProofErrorCode::InvalidFormat,
                ["requestId"],
                "browser bind request ID must be a canonical ULID",
            ));
        }
        let key = decode_public_key(&session_public_key, &["sessionPublicKey"])?;
        let signer_key_id = derived_key_id(&key);
        Self::new(
            SessionProofPurpose::UserAuthBind,
            request_id,
            issued_at,
            signer_key_id,
            vec![
                text(&origin, &["origin"])?,
                text(&format!("/auth/flow/{flow_id}/bind"), &["route"])?,
                key.as_bytes().to_vec(),
                Sha256::digest(canonicalize_json(&unsigned_request)?.as_bytes()).to_vec(),
            ],
        )
    }

    /// Build a provisioned service-instance bootstrap proof input.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] when a field is noncanonical,
    /// unsafe, empty, or malformed.
    pub fn service_bootstrap(
        input: NativeBootstrapSessionProofInput,
    ) -> Result<Self, ProtocolError> {
        Self::native_bootstrap(
            input,
            SessionProofPurpose::ServiceBootstrap,
            "/bootstrap/service",
        )
    }

    /// Build a provisioned or activated device bootstrap proof input.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] when a field is noncanonical,
    /// unsafe, empty, or malformed.
    pub fn device_bootstrap(
        input: NativeBootstrapSessionProofInput,
    ) -> Result<Self, ProtocolError> {
        Self::native_bootstrap(
            input,
            SessionProofPurpose::DeviceBootstrap,
            "/bootstrap/device",
        )
    }

    /// Build a device enrollment proof bound to the enrollment route.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] for malformed or noncanonical input.
    pub fn device_enrollment(
        input: NativeBootstrapSessionProofInput,
    ) -> Result<Self, ProtocolError> {
        Self::native_bootstrap(
            input,
            SessionProofPurpose::DeviceEnrollment,
            "/auth/device/enroll",
        )
    }

    fn native_bootstrap(
        input: NativeBootstrapSessionProofInput,
        purpose: SessionProofPurpose,
        route: &str,
    ) -> Result<Self, ProtocolError> {
        let NativeBootstrapSessionProofInput {
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
        if purpose == SessionProofPurpose::DeviceEnrollment {
            text(&required_text("participantId")?, &["participantId"])?;
        }
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

        Self::new(
            purpose,
            request_id,
            issued_at,
            identity_key_id.clone(),
            vec![
                text(&origin, &["origin"])?,
                text(route, &["route"])?,
                digest(&identity_key_id, &["identityKeyId"])?,
                session_key.as_bytes().to_vec(),
                text(&connection_id, &["connectionId"])?,
                request_digest.to_vec(),
            ],
        )
    }

    /// Build a proof input for refreshing the current authorization context.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::SessionProof`] when a field is noncanonical,
    /// unsafe, empty, or malformed.
    pub fn authorization_context_refresh(
        input: AuthorizationContextRefreshSessionProofInput,
    ) -> Result<Self, ProtocolError> {
        let AuthorizationContextRefreshSessionProofInput {
            origin,
            session_public_key,
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
        let required_text = |field: &str| {
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
        let login_session_id = required_text("loginSessionId")?;
        let connection_id = required_text("connectionId")?;
        let issued_at = request
            .get("issuedAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["issuedAt"],
                    "issuedAt must be an integer",
                )
            })?;
        match request.get("currentContextDigest") {
            Some(Value::Null) => {}
            Some(Value::String(value)) => {
                digest(value, &["currentContextDigest"])?;
            }
            _ => {
                return Err(proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["currentContextDigest"],
                    "currentContextDigest must be present and nullable",
                ))
            }
        }
        if let Some(name) = request.get("name") {
            if name.as_str().is_none_or(|name| name.chars().count() > 128) {
                return Err(proof_error(
                    SessionProofErrorCode::InvalidFormat,
                    ["name"],
                    "name must be a string of at most 128 characters",
                ));
            }
        }
        let key = decode_public_key(&session_public_key, &["sessionPublicKey"])?;
        let request_digest = Sha256::digest(canonicalize_json(&unsigned_request)?.as_bytes());

        Self::new(
            SessionProofPurpose::AuthorizationContextRefresh,
            request_id,
            issued_at,
            derived_key_id(&key),
            vec![
                text(&origin, &["origin"])?,
                b"/auth/context/refresh".to_vec(),
                key.as_bytes().to_vec(),
                text(&login_session_id, &["loginSessionId"])?,
                text(&connection_id, &["connectionId"])?,
                request_digest.to_vec(),
            ],
        )
    }

    fn new(
        purpose: SessionProofPurpose,
        request_id: String,
        issued_at: i64,
        signer_key_id: String,
        transcript_fields: Vec<Vec<u8>>,
    ) -> Result<Self, ProtocolError> {
        validate_request_id(&request_id)?;
        validate_safe_integer(issued_at, &["issuedAt"])?;
        Ok(Self {
            purpose,
            request_id,
            issued_at,
            signer_key_id,
            transcript_fields,
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

    const DIGEST: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn user_auth_request_needs_no_nats_key() -> Result<(), ProtocolError> {
        let signing_key = SigningKey::from_bytes(&[6; 32]);
        let public_key = encode_base64url(signing_key.verifying_key().as_bytes());
        let input = SessionProofInput::user_auth_request(UserAuthRequestSessionProofInput {
            origin: "https://trellis.example".to_owned(),
            unsigned_request: json!({
                "requestId": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "issuedAt": 1_000,
                "sessionPublicKey": public_key,
                "participantId": "app.example@v1",
            }),
        })?;
        let proof = sign_session_proof(&input, &signing_key)?;
        verify_session_proof(
            &input,
            &proof,
            &encode_base64url(signing_key.verifying_key().as_bytes()),
            1_000,
            SessionProofPolicy::default(),
        )?;
        Ok(())
    }

    #[test]
    fn user_auth_bind_proof_is_bound_to_the_flow() -> Result<(), ProtocolError> {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let public_key = encode_base64url(signing_key.verifying_key().as_bytes());
        let bind = |flow_id: &str| {
            SessionProofInput::user_auth_bind(UserAuthBindSessionProofInput {
                origin: "https://trellis.example".to_owned(),
                flow_id: flow_id.to_owned(),
                session_public_key: public_key.clone(),
                unsigned_request: json!({
                    "requestId": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "issuedAt": 1_000,
                }),
            })
        };
        let proof = sign_session_proof(&bind("flow_1")?, &signing_key)?;
        verify_session_proof(
            &bind("flow_1")?,
            &proof,
            &public_key,
            1_000,
            SessionProofPolicy::default(),
        )?;
        assert!(verify_session_proof(
            &bind("flow_2")?,
            &proof,
            &public_key,
            1_000,
            SessionProofPolicy::default(),
        )
        .is_err());
        assert!(
            SessionProofInput::user_auth_bind(UserAuthBindSessionProofInput {
                origin: "https://trellis.example".to_owned(),
                flow_id: "flow_1".to_owned(),
                session_public_key: public_key,
                unsigned_request: json!({"requestId": "req_bind_1", "issuedAt": 1_000}),
            })
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn user_refresh_binds_login_connection_origin_and_complete_unsigned_request(
    ) -> Result<(), ProtocolError> {
        let key = SigningKey::from_bytes(&[0x37; 32]);
        let request = json!({
            "requestId": "01JY0000000000000000000001", "issuedAt": 1_735_689_600_000_i64,
            "loginSessionId": "01JY0000000000000000000002", "connectionId": "01JY0000000000000000000003",
            "currentContextDigest": null, "name": "test runtime", "future": {"bound": true}
        });
        let input = AuthorizationContextRefreshSessionProofInput {
            origin: "https://trellis.example".into(),
            session_public_key: encode_base64url(key.verifying_key().as_bytes()),
            unsigned_request: request,
        };
        let proof_input = SessionProofInput::authorization_context_refresh(input.clone())?;
        let proof = sign_session_proof(&proof_input, &key)?;
        let policy = SessionProofPolicy::new(30_000, 5_000)?;
        verify_session_proof(
            &proof_input,
            &proof,
            &input.session_public_key,
            1_735_689_600_000,
            policy,
        )?;
        for (field, value) in [
            ("requestId", json!("01JY0000000000000000000004")),
            ("issuedAt", json!(1_735_689_600_001_i64)),
            ("loginSessionId", json!("01JY0000000000000000000005")),
            ("connectionId", json!("01JY0000000000000000000006")),
            ("currentContextDigest", json!(DIGEST)),
            ("name", json!("changed name")),
            ("future", json!({"bound": false})),
        ] {
            let mut changed = input.clone();
            changed.unsigned_request[field] = value;
            let changed = SessionProofInput::authorization_context_refresh(changed)?;
            assert!(verify_session_proof(
                &changed,
                &proof,
                &input.session_public_key,
                1_735_689_600_000,
                policy
            )
            .is_err());
        }
        let mut changed = input.clone();
        changed.origin = "https://another.example".into();
        assert!(verify_session_proof(
            &SessionProofInput::authorization_context_refresh(changed)?,
            &proof,
            &input.session_public_key,
            1_735_689_600_000,
            policy
        )
        .is_err());
        let mut missing = input;
        missing
            .unsigned_request
            .as_object_mut()
            .unwrap()
            .remove("currentContextDigest");
        assert!(SessionProofInput::authorization_context_refresh(missing).is_err());
        Ok(())
    }

    #[test]
    fn canonical_request_digest_removes_only_signature() -> Result<(), ProtocolError> {
        let first = json!({
            "requestId": "req_1",
            "proof": {"format": SESSION_PROOF_FORMAT_V1, "signature": "first"},
            "nullable": null
        });
        let second = json!({
            "nullable": null,
            "proof": {"signature": "second", "format": SESSION_PROOF_FORMAT_V1},
            "requestId": "req_1"
        });
        assert_eq!(
            session_proof_request_digest(&first)?,
            session_proof_request_digest(&second)?
        );
        Ok(())
    }

    #[test]
    fn rejects_unsafe_request_integers_and_weak_keys() {
        let unsafe_request = json!({
            "proof": {
                "format": SESSION_PROOF_FORMAT_V1,
                "signature": encode_base64url(&[0; 64])
            },
            "nested": {"counter": 9_007_199_254_740_992_u64}
        });
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

    fn native_input() -> (SigningKey, NativeBootstrapSessionProofInput) {
        let identity = SigningKey::from_bytes(&[41; 32]);
        let session = SigningKey::from_bytes(&[42; 32]);
        let input = NativeBootstrapSessionProofInput {
            origin: "https://trellis.example".to_owned(),
            unsigned_request: json!({
                "identityKeyId": derived_key_id(&identity.verifying_key()),
                "sessionKey": encode_base64url(session.verifying_key().as_bytes()),
                "connectionId": "01JY0000000000000000000001",
                "requestId": "01JY0000000000000000000002",
                "iat": 1_750_000_000_000_i64,
                "name": "replica one",
                "extension": {"value": 1}
            }),
        };
        (identity, input)
    }

    #[test]
    fn native_bootstrap_binds_the_origin_route_identity_and_complete_unsigned_request(
    ) -> Result<(), ProtocolError> {
        let (identity, input) = native_input();
        let service = SessionProofInput::service_bootstrap(input.clone())?;
        let proof = sign_session_proof(&service, &identity)?;
        let key = encode_base64url(identity.verifying_key().as_bytes());
        verify_session_proof(
            &service,
            &proof,
            &key,
            service.issued_at(),
            SessionProofPolicy::default(),
        )?;
        let device = SessionProofInput::device_bootstrap(input.clone())?;
        assert!(verify_session_proof(
            &device,
            &proof,
            &key,
            service.issued_at(),
            SessionProofPolicy::default()
        )
        .is_err());
        let mut other_origin = input.clone();
        other_origin.origin = "https://another.example".to_owned();
        let other_origin = SessionProofInput::service_bootstrap(other_origin)?;
        assert!(verify_session_proof(
            &other_origin,
            &proof,
            &key,
            service.issued_at(),
            SessionProofPolicy::default()
        )
        .is_err());
        for (field, value) in [
            ("connectionId", json!("01JY0000000000000000000003")),
            ("requestId", json!("01JY0000000000000000000004")),
            ("iat", json!(service.issued_at() + 1)),
            ("name", json!("another replica")),
            ("extension", json!({"value": 2})),
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
            let modified = SessionProofInput::service_bootstrap(modified)?;
            assert!(
                verify_session_proof(
                    &modified,
                    &proof,
                    &key,
                    service.issued_at(),
                    SessionProofPolicy::default()
                )
                .is_err(),
                "{field} must be signature-bound"
            );
        }
        let session = SigningKey::from_bytes(&[42; 32]);
        assert!(sign_session_proof(&service, &session).is_err());
        assert!(SessionProofInput::device_enrollment(input.clone()).is_err());
        let mut enrollment_request = input;
        enrollment_request.unsigned_request["participantId"] = json!("example.device");
        let enrollment = SessionProofInput::device_enrollment(enrollment_request.clone())?;
        let enrollment_proof = sign_session_proof(&enrollment, &identity)?;
        verify_session_proof(
            &enrollment,
            &enrollment_proof,
            &key,
            service.issued_at(),
            SessionProofPolicy::default(),
        )?;
        let ready_device = SessionProofInput::device_bootstrap(enrollment_request)?;
        assert!(verify_session_proof(
            &ready_device,
            &enrollment_proof,
            &key,
            service.issued_at(),
            SessionProofPolicy::default()
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn native_bootstrap_rejects_invalid_unsigned_inputs_and_stale_proofs(
    ) -> Result<(), ProtocolError> {
        let (identity, input) = native_input();
        let service = SessionProofInput::service_bootstrap(input.clone())?;
        let proof = sign_session_proof(&service, &identity)?;
        let key = encode_base64url(identity.verifying_key().as_bytes());
        for now in [service.issued_at() - 30_001, service.issued_at() + 30_001] {
            assert!(verify_session_proof(
                &service,
                &proof,
                &key,
                now,
                SessionProofPolicy::default()
            )
            .is_err());
        }
        for (field, value) in [
            ("proof", json!({"format": SESSION_PROOF_FORMAT_V1})),
            ("name", json!("x".repeat(129))),
            ("name", Value::Null),
            ("iat", json!(9_007_199_254_740_992_u64)),
            ("iat", json!(0.5)),
            ("requestId", json!("")),
            ("connectionId", json!("")),
            ("identityKeyId", json!("invalid")),
            ("sessionKey", json!(encode_base64url(&[0; 32]))),
            ("extension", json!({"value": 9_007_199_254_740_992_u64})),
        ] {
            let mut modified = input.clone();
            modified.unsigned_request[field] = value;
            assert!(
                SessionProofInput::service_bootstrap(modified).is_err(),
                "{field} must fail validation"
            );
        }
        for field in [
            "identityKeyId",
            "sessionKey",
            "connectionId",
            "requestId",
            "iat",
        ] {
            let mut modified = input.clone();
            modified
                .unsigned_request
                .as_object_mut()
                .expect("request object")
                .remove(field);
            assert!(SessionProofInput::service_bootstrap(modified).is_err());
        }
        Ok(())
    }
}
