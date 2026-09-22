//! Pure signed authorization-context and request-proof protocol.
//!
//! The configured authenticated Trellis origin supplies an [`AuthorizationIssuerKey`].
//! That online issuer signs contexts from current server-owned grant bindings.
//! The connection key bound into a verified context signs each exact request.
//! Verification requires no network, storage, wall clock, or async runtime:
//! issuer entries, policy, time, and raw
//! request bytes are explicit inputs.
//!
//! Signed objects have a strict top-level shape. Forward-compatible signed data
//! belongs in `extensions`; names in `critical` fail closed unless understood.
//! Signatures cover RFC 8785 canonical JSON for the complete object with only
//! its top-level `signature` member omitted. Every signed integer, recursively
//! including extension values, is restricted to the interoperable JSON safe
//! integer range `-(2^53 - 1)..=(2^53 - 1)`.
//!
//! # Complete local decision
//!
//! The following constructs and verifies an online issuer entry, context,
//! exact permission, and context-bound request proof:
//!
//! ```
//! use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
//! use ed25519_dalek::SigningKey;
//! use serde_json::Map;
//! use trellis_protocol::{
//!     sign_authorization_context, sign_authorization_event, sign_authorization_request,
//!     verify_authorization_context, verify_authorization_event,
//!     verify_authorization_request,
//!     ApiSurfaceKind, AuthorizationContextPurpose, AuthorizationIssuerKey,
//!     AuthorizationIssuerState, AuthorizationPrincipalKind, GrantOwnerKind,
//!     AuthorizationEventVerificationInput, AuthorizationRequestVerificationInput,
//!     AuthorizationVerificationPolicy, GrantSet, PermissionAction, PermissionAtom,
//!     PermissionTarget, UnsignedAuthorizationContext, encode_event_descriptor_identity,
//!     AUTHORIZATION_CONTEXT_FORMAT_V1,
//! };
//! use sha2::{Digest as _, Sha256};
//!
//! let issuer_key = SigningKey::from_bytes(&[2; 32]);
//! let session_key = SigningKey::from_bytes(&[3; 32]);
//! let encode_key = |key: &SigningKey| URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes());
//! let key_id = |key: &SigningKey| {
//!     URL_SAFE_NO_PAD.encode(Sha256::digest(key.verifying_key().as_bytes()))
//! };
//! let issuer = AuthorizationIssuerKey {
//!     key_id: key_id(&issuer_key), public_key: encode_key(&issuer_key),
//!     state: AuthorizationIssuerState::Active,
//! };
//! let policy = AuthorizationVerificationPolicy::new(1_100, 30, 300, 16_384, 16)?;
//! let permission = PermissionAtom::new(
//!     PermissionTarget::api_surface(
//!         "documents@v1",
//!         ApiSurfaceKind::Rpc,
//!         "Documents.Get",
//!     )?,
//!     PermissionAction::Call,
//! )?;
//! let event_permission = PermissionAtom::new(
//!     PermissionTarget::api_surface(
//!         "documents@v1",
//!         ApiSurfaceKind::Event,
//!         "Documents.Changed",
//!     )?,
//!     PermissionAction::Publish,
//! )?;
//! let context = sign_authorization_context(
//!     UnsignedAuthorizationContext {
//!         format: AUTHORIZATION_CONTEXT_FORMAT_V1.into(),
//!         issuer_key_id: key_id(&issuer_key),
//!         connection_id: "01JY0000000000000000000001".into(),
//!         session_key: encode_key(&session_key),
//!         principal_kind: AuthorizationPrincipalKind::User,
//!         principal_id: "01JY0000000000000000000002".into(),
//!         participant_id: "documents-web".into(),
//!         owner_kind: GrantOwnerKind::User,
//!         owner_id: "01JY0000000000000000000002".into(),
//!         grant_revision: 1,
//!         identity_key_id: None,
//!         login_session_id: Some("01JY0000000000000000000003".into()),
//!         deployment_id: None,
//!         instance_id: None,
//!         inbox_prefix: "_INBOX.example".into(),
//!         issued_at: 1_100,
//!         not_before: 1_100,
//!         expires_at: 1_300,
//!         grants: GrantSet::new(vec![event_permission, permission.clone()]),
//!         platform_privileges: vec![],
//!         extensions: Map::new(),
//!         critical: vec![],
//!     },
//!     &issuer_key,
//! )?;
//! let context = verify_authorization_context(&issuer, &context, &policy, AuthorizationContextPurpose::Live)?;
//! assert!(context.allows(&permission));
//! let proof = sign_authorization_request(
//!     context.context_digest(),
//!     "rpc.v1.Documents.Get",
//!     Some("_INBOX.example.reply"),
//!     br#"{"id":"doc-1"}"#,
//!     1_100,
//!     "req_example",
//!     &session_key,
//! )?;
//! let request_permissions = [permission.clone()];
//! let request = verify_authorization_request(AuthorizationRequestVerificationInput {
//!     context: &context,
//!     subject: "rpc.v1.Documents.Get",
//!     reply_subject: Some("_INBOX.example.reply"),
//!     raw_payload: br#"{"id":"doc-1"}"#,
//!     iat: 1_100,
//!     request_id: "req_example",
//!     proof: &proof,
//!     policy: &policy,
//!     required_permissions: &request_permissions,
//! })?;
//! assert_eq!(request.context().principal_id(), "01JY0000000000000000000002");
//! let event_proof = sign_authorization_event(
//!     context.context_digest(),
//!     &encode_event_descriptor_identity("documents@v1", "Documents.Changed", 1)?,
//!     "events.v1.ZG9jdW1lbnRzQHYx.Documents.Changed.doc-1",
//!     br#"{"id":"doc-1"}"#,
//!     "evt_example",
//!     "1970-01-01T00:19:10Z",
//!     &session_key,
//! )?;
//! let event = verify_authorization_event(AuthorizationEventVerificationInput {
//!     context: &context,
//!     descriptor_identity: &encode_event_descriptor_identity("documents@v1", "Documents.Changed", 1)?,
//!     subject: "events.v1.ZG9jdW1lbnRzQHYx.Documents.Changed.doc-1",
//!     raw_payload: br#"{"id":"doc-1"}"#,
//!     event_id: "evt_example",
//!     event_time: "1970-01-01T00:19:10Z",
//!     proof: &event_proof,
//!     policy: &policy,
//!     revoked_at: None,
//! })?;
//! assert_eq!(event.publisher().participant_id, "documents-web");
//! # Ok::<(), trellis_protocol::ProtocolError>(())
//! ```

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use jsonptr::PointerBuf;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::{
    canonicalize_json, ApiSurfaceKind, AuthorizationErrorCode, GrantOwnerKind, GrantSet,
    PermissionAction, PermissionAtom, PermissionTarget, PlatformPrivilege, ProtocolError,
};

/// Issuer-signed authorization-context wire format and signature domain.
pub const AUTHORIZATION_CONTEXT_FORMAT_V1: &str = "trellis.authorization-context.v1";
/// Context-bound request-proof signature domain.
pub const AUTHORIZATION_REQUEST_PROOF_DOMAIN_V1: &str = "trellis.authorization-request-proof.v1";
/// Context-bound event-proof signature domain.
pub const AUTHORIZATION_EVENT_PROOF_DOMAIN_V1: &str = "trellis.authorization-event-proof.v1";

const MAXIMUM_REQUEST_ID_BYTES: usize = 256;
const MAXIMUM_EVENT_ID_BYTES: usize = 256;
const MAXIMUM_SAFE_JSON_INTEGER: i64 = 9_007_199_254_740_991;

fn authorization_error<'a>(
    code: AuthorizationErrorCode,
    tokens: impl IntoIterator<Item = &'a str>,
    message: impl Into<String>,
) -> ProtocolError {
    ProtocolError::Authorization {
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
    code: AuthorizationErrorCode,
) -> Result<[u8; N], ProtocolError> {
    if encoded.contains('=') {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidEncoding,
            path.iter().copied(),
            "padded base64url is not accepted",
        ));
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidEncoding,
            path.iter().copied(),
            "value is not unpadded base64url",
        )
    })?;
    if decoded.len() != N || encode_base64url(&decoded) != encoded {
        return Err(authorization_error(
            code,
            path.iter().copied(),
            format!("value must canonically encode exactly {N} bytes"),
        ));
    }
    decoded.try_into().map_err(|_| {
        authorization_error(
            code,
            path.iter().copied(),
            format!("value must encode exactly {N} bytes"),
        )
    })
}

fn decode_verifying_key(
    encoded: &str,
    path: &[&str],
    code: AuthorizationErrorCode,
) -> Result<VerifyingKey, ProtocolError> {
    VerifyingKey::from_bytes(&decode_base64url::<32>(encoded, path, code)?).map_err(|_| {
        authorization_error(
            code,
            path.iter().copied(),
            "value is not a valid Ed25519 public key",
        )
    })
}

fn derived_key_id(key: &VerifyingKey) -> String {
    encode_base64url(&sha256(key.as_bytes()))
}

fn check_key_id(declared: &str, key: &VerifyingKey, path: &[&str]) -> Result<(), ProtocolError> {
    decode_base64url::<32>(declared, path, AuthorizationErrorCode::InvalidKeyId)?;
    if declared == derived_key_id(key) {
        Ok(())
    } else {
        Err(authorization_error(
            AuthorizationErrorCode::InvalidKeyId,
            path.iter().copied(),
            "declared key id does not match the public key",
        ))
    }
}

fn validate_safe_i64(value: i64, path: &[&str]) -> Result<(), ProtocolError> {
    if (-MAXIMUM_SAFE_JSON_INTEGER..=MAXIMUM_SAFE_JSON_INTEGER).contains(&value) {
        Ok(())
    } else {
        Err(authorization_error(
            AuthorizationErrorCode::UnsafeJsonInteger,
            path.iter().copied(),
            "integer must be within the interoperable JSON safe-integer range",
        ))
    }
}

fn validate_safe_u64(value: u64, path: &[&str]) -> Result<(), ProtocolError> {
    if value <= MAXIMUM_SAFE_JSON_INTEGER as u64 {
        Ok(())
    } else {
        Err(authorization_error(
            AuthorizationErrorCode::UnsafeJsonInteger,
            path.iter().copied(),
            "integer must be within the interoperable JSON safe-integer range",
        ))
    }
}

fn validate_safe_extension_integers(
    value: &Value,
    path: &mut Vec<String>,
) -> Result<(), ProtocolError> {
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
                return Err(ProtocolError::Authorization {
                    code: AuthorizationErrorCode::UnsafeJsonInteger,
                    path: Box::new(PointerBuf::from_tokens(path.iter().map(String::as_str))),
                    message: "integer must be within the interoperable JSON safe-integer range"
                        .to_owned(),
                });
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(index.to_string());
                validate_safe_extension_integers(value, path)?;
                path.pop();
            }
        }
        Value::Object(values) => {
            for (name, value) in values {
                path.push(name.clone());
                validate_safe_extension_integers(value, path)?;
                path.pop();
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

fn push_length_prefixed(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ProtocolError> {
    let length = u32::try_from(bytes.len()).map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            std::iter::empty::<&str>(),
            "signature input component exceeds u32 length",
        )
    })?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

fn signed_json_digest(domain: &str, unsigned: &Value) -> Result<[u8; 32], ProtocolError> {
    let canonical = canonicalize_json(unsigned)?;
    let mut input = Vec::with_capacity(domain.len() + canonical.len() + 8);
    push_length_prefixed(&mut input, domain.as_bytes())?;
    push_length_prefixed(&mut input, canonical.as_bytes())?;
    Ok(sha256(&input))
}

fn complete_digest<T: Serialize>(value: &T) -> Result<String, ProtocolError> {
    let canonical = canonicalize_json(&serde_json::to_value(value)?)?;
    Ok(encode_base64url(&sha256(canonical.as_bytes())))
}

fn validate_text(value: &str, path: &[&str]) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().any(|character| character.is_ascii_control())
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            path.iter().copied(),
            "value must be nonempty protocol-safe text",
        ));
    }
    Ok(())
}

fn validate_inbox_prefix(value: &str) -> Result<(), ProtocolError> {
    validate_text(value, &["inboxPrefix"])?;
    if value.starts_with('.')
        || value.ends_with('.')
        || value.split('.').any(str::is_empty)
        || value.contains('*')
        || value.contains('>')
        || value.chars().any(char::is_whitespace)
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["inboxPrefix"],
            "inbox prefix must contain only safe literal NATS tokens",
        ));
    }
    Ok(())
}

fn is_utf16_strictly_sorted(values: &[String]) -> bool {
    values
        .windows(2)
        .all(|pair| pair[0].encode_utf16().cmp(pair[1].encode_utf16()).is_lt())
}

fn validate_extensions(
    extensions: &Map<String, Value>,
    critical: &[String],
) -> Result<(), ProtocolError> {
    for (index, name) in critical.iter().enumerate() {
        validate_text(name, &["critical", &index.to_string()])?;
    }
    if !is_utf16_strictly_sorted(critical) {
        return Err(authorization_error(
            AuthorizationErrorCode::NonCanonicalSet,
            ["critical"],
            "critical extension names must be UTF-16 sorted and unique",
        ));
    }
    if let Some((index, _name)) = critical
        .iter()
        .enumerate()
        .find(|(_, name)| !extensions.contains_key(name.as_str()))
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["critical", &index.to_string()],
            "critical extension is absent from extensions",
        ));
    }
    if let Some(name) = critical.first() {
        return Err(authorization_error(
            AuthorizationErrorCode::UnknownCriticalExtension,
            ["extensions", name],
            "critical extension is not understood by this protocol version",
        ));
    }
    validate_safe_extension_integers(
        &Value::Object(extensions.clone()),
        &mut vec!["extensions".to_owned()],
    )?;
    Ok(())
}

fn validate_window(
    issued_at: i64,
    not_before: i64,
    expires_at: i64,
    prefix: &[&str],
) -> Result<(), ProtocolError> {
    validate_safe_i64(
        issued_at,
        &prefix
            .iter()
            .copied()
            .chain(["issuedAt"])
            .collect::<Vec<_>>(),
    )?;
    validate_safe_i64(
        not_before,
        &prefix
            .iter()
            .copied()
            .chain(["notBefore"])
            .collect::<Vec<_>>(),
    )?;
    validate_safe_i64(
        expires_at,
        &prefix
            .iter()
            .copied()
            .chain(["expiresAt"])
            .collect::<Vec<_>>(),
    )?;
    if not_before <= issued_at && issued_at <= expires_at && not_before < expires_at {
        Ok(())
    } else {
        Err(authorization_error(
            AuthorizationErrorCode::InvalidValidityWindow,
            prefix.iter().copied().chain(["expiresAt"]),
            "validity must satisfy notBefore <= issuedAt <= expiresAt and be nonempty",
        ))
    }
}

fn verify_signature(
    key: &VerifyingKey,
    digest: &[u8; 32],
    encoded: &str,
    code: AuthorizationErrorCode,
) -> Result<(), ProtocolError> {
    let bytes = decode_base64url::<64>(encoded, &["signature"], code)?;
    key.verify_strict(digest, &Signature::from_bytes(&bytes))
        .map_err(|_| authorization_error(code, ["signature"], "signature verification failed"))
}

/// Explicit security policy supplied to pure authorization verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationVerificationPolicy {
    /// Verification time supplied by the caller.
    pub now_unix_seconds: i64,
    /// Allowed symmetric time skew.
    pub allowed_clock_skew_seconds: u32,
    /// Maximum authorization-context lease duration.
    pub maximum_context_lifetime_seconds: u32,
    /// Maximum canonical signed-context JSON size in UTF-8 bytes.
    pub maximum_context_bytes: usize,
    /// Maximum exact permissions in one context.
    pub maximum_permissions: usize,
}

impl AuthorizationVerificationPolicy {
    /// Construct an explicit verification policy.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Authorization`] if any security limit is zero.
    pub fn new(
        now_unix_seconds: i64,
        allowed_clock_skew_seconds: u32,
        maximum_context_lifetime_seconds: u32,
        maximum_context_bytes: usize,
        maximum_permissions: usize,
    ) -> Result<Self, ProtocolError> {
        let policy = Self {
            now_unix_seconds,
            allowed_clock_skew_seconds,
            maximum_context_lifetime_seconds,
            maximum_context_bytes,
            maximum_permissions,
        };
        validate_policy(&policy)?;
        Ok(policy)
    }
}

fn validate_policy(policy: &AuthorizationVerificationPolicy) -> Result<(), ProtocolError> {
    validate_safe_i64(policy.now_unix_seconds, &["nowUnixSeconds"])?;
    if policy.maximum_context_lifetime_seconds == 0
        || policy.maximum_context_bytes == 0
        || policy.maximum_permissions == 0
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            std::iter::empty::<&str>(),
            "verification limits must be nonzero",
        ));
    }
    Ok(())
}

/// Lifecycle of an online issuer key obtained from the configured Trellis origin.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorizationIssuerState {
    /// Eligible for current contexts; rotation keeps the previous key active until expiry.
    Active,
    /// Retained solely for historically eligible context and event verification.
    Retired,
    /// Explicitly revoked and unusable for both live and historical authority.
    Revoked,
}

/// Public issuer entry authenticated by the configured server's TLS origin.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorizationIssuerKey {
    /// SHA-256 identifier of the raw Ed25519 public key.
    pub key_id: String,
    /// Canonically encoded, unpadded base64url Ed25519 public key.
    pub public_key: String,
    /// Current server-owned lifecycle state.
    pub state: AuthorizationIssuerState,
}

impl AuthorizationIssuerKey {
    /// Validate key encoding, strength, and its content-derived identifier.
    ///
    /// # Errors
    /// Returns an authorization error for malformed or mismatching key material.
    pub fn verifying_key(&self) -> Result<VerifyingKey, ProtocolError> {
        let key = decode_verifying_key(
            &self.public_key,
            &["issuer", "publicKey"],
            AuthorizationErrorCode::InvalidPublicKey,
        )?;
        check_key_id(&self.key_id, &key, &["issuer", "keyId"])?;
        Ok(key)
    }
}

/// Intended use of a verified context; historical handles cannot authorize requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationContextPurpose {
    /// A current request, connection, or handler registration.
    Live,
    /// A retained event whose signed time is checked independently of ordinary expiry.
    HistoricalEvent,
}

/// Stable principal classes represented by an authorization context.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorizationPrincipalKind {
    /// A Trellis user account.
    User,
    /// A hosted service runtime.
    Service,
    /// A device runtime.
    Device,
}

/// Complete unsigned authorization-context fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnsignedAuthorizationContext {
    /// Wire format.
    pub format: String,
    /// Content-derived signing issuer key id from the configured online authority.
    pub issuer_key_id: String,
    /// SDK-owned runtime connection identity.
    pub connection_id: String,
    /// Session Ed25519 public key.
    pub session_key: String,
    /// Stable caller principal, assigned by the server.
    pub principal_id: String,
    /// Kind of the authenticated principal.
    pub principal_kind: AuthorizationPrincipalKind,
    /// Installed participant assignment, not a client-supplied digest.
    pub participant_id: String,
    /// Owner of the current grant binding.
    pub owner_kind: GrantOwnerKind,
    /// Stable deployment or user identifier owning the grants.
    pub owner_id: String,
    /// Revision of the server-owned binding used for issuance.
    pub grant_revision: u64,
    /// Provisioned identity credential for native runtimes only.
    pub identity_key_id: Option<String>,
    /// Durable user login credential; native runtimes have no login session.
    pub login_session_id: Option<String>,
    /// Deployment id for service and applicable device contexts.
    pub deployment_id: Option<String>,
    /// Runtime instance id for service and applicable device contexts.
    pub instance_id: Option<String>,
    /// Caller reply-inbox prefix.
    pub inbox_prefix: String,
    /// Context issue time.
    pub issued_at: i64,
    /// Inclusive lower validity bound.
    pub not_before: i64,
    /// Inclusive upper validity bound.
    pub expires_at: i64,
    /// Exact machine permission authority.
    pub grants: GrantSet,
    /// Canonical orthogonal platform authority from the server-owned binding.
    pub platform_privileges: Vec<PlatformPrivilege>,
    /// Signed extension values.
    pub extensions: Map<String, Value>,
    /// Canonical critical extension names.
    pub critical: Vec<String>,
}

/// Issuer-signed authorization context.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignedAuthorizationContext {
    /// Unsigned context fields.
    #[serde(flatten)]
    pub unsigned: UnsignedAuthorizationContext,
    /// Issuer signature.
    pub signature: String,
}

impl SignedAuthorizationContext {
    /// Return the digest of complete signed canonical context JSON.
    ///
    /// # Errors
    ///
    /// Returns a JSON canonicalization error if serialization fails.
    pub fn digest(&self) -> Result<String, ProtocolError> {
        complete_digest(self)
    }
}

/// Return the domain-separated signing digest for an authorization context.
///
/// # Errors
///
/// Returns a JSON canonicalization error if serialization fails.
pub fn authorization_context_signing_digest(
    context: &UnsignedAuthorizationContext,
) -> Result<[u8; 32], ProtocolError> {
    validate_context_fields(context, None)?;
    signed_json_digest(
        AUTHORIZATION_CONTEXT_FORMAT_V1,
        &serde_json::to_value(context)?,
    )
}

/// Sign a short-lived authorization context with an issuer key.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for invalid context structure, or a
/// JSON canonicalization error.
pub fn sign_authorization_context(
    context: UnsignedAuthorizationContext,
    issuer_key: &SigningKey,
) -> Result<SignedAuthorizationContext, ProtocolError> {
    validate_context_fields(&context, None)?;
    decode_verifying_key(
        &context.session_key,
        &["sessionKey"],
        AuthorizationErrorCode::InvalidSessionKey,
    )?;
    check_key_id(
        &context.issuer_key_id,
        &issuer_key.verifying_key(),
        &["issuerKeyId"],
    )?;
    let digest = authorization_context_signing_digest(&context)?;
    Ok(SignedAuthorizationContext {
        unsigned: context,
        signature: encode_base64url(&issuer_key.sign(&digest).to_bytes()),
    })
}

/// Strictly parse an authorization context and reject noncanonical set identity.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for unknown top-level fields,
/// malformed keys/signatures, or noncanonical set-like arrays.
pub fn parse_authorization_context(
    value: &Value,
) -> Result<SignedAuthorizationContext, ProtocolError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct WireSignedAuthorizationContext {
        #[serde(flatten)]
        unsigned: UnsignedAuthorizationContext,
        signature: String,
    }

    let wire: WireSignedAuthorizationContext =
        serde_json::from_value(value.clone()).map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                std::iter::empty::<&str>(),
                "authorization context has an invalid strict object shape",
            )
        })?;
    let context = SignedAuthorizationContext {
        unsigned: wire.unsigned,
        signature: wire.signature,
    };
    validate_context_fields(&context.unsigned, Some(value))?;
    decode_verifying_key(
        &context.unsigned.session_key,
        &["sessionKey"],
        AuthorizationErrorCode::InvalidSessionKey,
    )?;
    decode_base64url::<64>(
        &context.signature,
        &["signature"],
        AuthorizationErrorCode::InvalidSignature,
    )?;
    Ok(context)
}

fn validate_context_fields(
    context: &UnsignedAuthorizationContext,
    authored: Option<&Value>,
) -> Result<(), ProtocolError> {
    if context.format != AUTHORIZATION_CONTEXT_FORMAT_V1 {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["format"],
            "unsupported authorization-context format",
        ));
    }
    for (value, field) in [
        (&context.connection_id, "connectionId"),
        (&context.principal_id, "principalId"),
        (&context.participant_id, "participantId"),
        (&context.owner_id, "ownerId"),
    ] {
        validate_text(value, &[field])?;
    }
    decode_base64url::<32>(
        &context.issuer_key_id,
        &["issuerKeyId"],
        AuthorizationErrorCode::InvalidKeyId,
    )?;
    decode_verifying_key(
        &context.session_key,
        &["sessionKey"],
        AuthorizationErrorCode::InvalidSessionKey,
    )?;
    validate_safe_u64(context.grant_revision, &["grantRevision"])?;
    if context.grant_revision == 0 {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["grantRevision"],
            "grant revision must be positive",
        ));
    }
    let valid_binding = match context.principal_kind {
        AuthorizationPrincipalKind::User => {
            context.owner_kind == GrantOwnerKind::User
                && context.owner_id == context.principal_id
                && context.login_session_id.is_some()
                && context.identity_key_id.is_none()
                && context.deployment_id.is_none()
                && context.instance_id.is_none()
        }
        AuthorizationPrincipalKind::Service | AuthorizationPrincipalKind::Device => {
            context.owner_kind == GrantOwnerKind::Deployment
                && context.deployment_id.as_deref() == Some(context.owner_id.as_str())
                && context.instance_id.is_some()
                && context.identity_key_id.is_some()
                && context.login_session_id.is_none()
        }
    };
    if !valid_binding {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["ownerKind"],
            "principal, owner, and credential bindings are inconsistent",
        ));
    }
    if let Some(key) = &context.identity_key_id {
        decode_base64url::<32>(
            key,
            &["identityKeyId"],
            AuthorizationErrorCode::InvalidKeyId,
        )?;
    }
    for (value, field) in [
        (&context.login_session_id, "loginSessionId"),
        (&context.deployment_id, "deploymentId"),
        (&context.instance_id, "instanceId"),
    ] {
        if let Some(value) = value {
            validate_text(value, &[field])?;
        }
    }
    validate_inbox_prefix(&context.inbox_prefix)?;
    validate_window(
        context.issued_at,
        context.not_before,
        context.expires_at,
        &[],
    )?;
    if !context
        .platform_privileges
        .windows(2)
        .all(|pair| pair[0] < pair[1])
    {
        return Err(authorization_error(
            AuthorizationErrorCode::NonCanonicalSet,
            ["platformPrivileges"],
            "platform privileges must be sorted and unique",
        ));
    }
    if let Some(authored_grants) = authored.and_then(|value| value.get("grants")) {
        if serde_json::to_value(&context.grants)? != *authored_grants {
            return Err(authorization_error(
                AuthorizationErrorCode::NonCanonicalSet,
                ["grants", "permissions"],
                "permission atoms must be canonical and unique",
            ));
        }
    }
    validate_extensions(&context.extensions, &context.critical)
}

/// Compute the deterministic refresh time for a signed authorization context.
///
/// Jitter is derived from the canonical context digest and can only move the
/// refresh earlier than the configured safety lead. The same context and policy
/// therefore produce the same schedule in every runtime.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] if the digest is malformed, the
/// signed time window is invalid, or the policy cannot schedule a refresh no
/// earlier than issuance and strictly before expiry.
pub fn authorization_context_refresh_at(
    context_digest: &str,
    issued_at: i64,
    not_before: i64,
    expires_at: i64,
    refresh_lead_seconds: u32,
    refresh_jitter_seconds: u32,
) -> Result<i64, ProtocolError> {
    validate_safe_i64(issued_at, &["issuedAt"])?;
    validate_safe_i64(not_before, &["notBefore"])?;
    validate_safe_i64(expires_at, &["expiresAt"])?;
    if not_before > issued_at || issued_at >= expires_at || refresh_lead_seconds == 0 {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["refreshAt"],
            "authorization context cannot be refreshed within its signed window",
        ));
    }
    let digest = decode_base64url::<32>(
        context_digest,
        &["contextDigest"],
        AuthorizationErrorCode::InvalidEncoding,
    )?;
    let jitter_range = u64::from(refresh_jitter_seconds) + 1;
    let jitter = i64::try_from(
        u64::from_be_bytes(digest[..8].try_into().map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidEncoding,
                ["contextDigest"],
                "authorization context digest is invalid",
            )
        })?) % jitter_range,
    )
    .map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["refreshAt"],
            "authorization refresh jitter overflows",
        )
    })?;
    let refresh_at = expires_at
        .checked_sub(i64::from(refresh_lead_seconds))
        .and_then(|value| value.checked_sub(jitter))
        .ok_or_else(|| {
            authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                ["refreshAt"],
                "authorization refresh schedule overflows",
            )
        })?;
    if refresh_at < issued_at || refresh_at >= expires_at {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["refreshAt"],
            "authorization refresh policy has no usable window",
        ));
    }
    Ok(refresh_at)
}

/// Authorization context whose complete trust chain and lease have verified.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationContext {
    context: SignedAuthorizationContext,
    session_key: VerifyingKey,
    context_digest: String,
    issuer_state: AuthorizationIssuerState,
    purpose: AuthorizationContextPurpose,
}

impl VerifiedAuthorizationContext {
    /// Require a live-purpose context from an active issuer at the supplied time.
    ///
    /// # Errors
    ///
    /// Returns an authorization error for historical-only, retired, or expired authority.
    pub fn assert_current(
        &self,
        policy: &AuthorizationVerificationPolicy,
    ) -> Result<(), ProtocolError> {
        validate_policy(policy)?;
        if self.issuer_state != AuthorizationIssuerState::Active
            || self.purpose != AuthorizationContextPurpose::Live
        {
            return Err(authorization_error(
                AuthorizationErrorCode::HistoricalContext,
                ["authorization-context"],
                "context is not eligible for live requests",
            ));
        }
        let now = i128::from(policy.now_unix_seconds);
        let skew = i128::from(policy.allowed_clock_skew_seconds);
        if now + skew < i128::from(self.not_before()) {
            return Err(authorization_error(
                AuthorizationErrorCode::ContextNotYetValid,
                ["authorization-context", "notBefore"],
                "context is not yet valid",
            ));
        }
        if now - skew > i128::from(self.expires_at()) {
            return Err(authorization_error(
                AuthorizationErrorCode::ContextExpired,
                ["authorization-context", "expiresAt"],
                "context has expired",
            ));
        }
        Ok(())
    }
    /// Return the server-assigned principal identifier.
    pub fn principal_id(&self) -> &str {
        &self.context.unsigned.principal_id
    }
    /// Return the authenticated principal kind.
    pub fn principal_kind(&self) -> AuthorizationPrincipalKind {
        self.context.unsigned.principal_kind
    }
    /// Return the installed participant assignment.
    pub fn participant_id(&self) -> &str {
        &self.context.unsigned.participant_id
    }
    /// Return the grant binding owner kind.
    pub fn owner_kind(&self) -> GrantOwnerKind {
        self.context.unsigned.owner_kind
    }
    /// Return the grant binding owner identifier.
    pub fn owner_id(&self) -> &str {
        &self.context.unsigned.owner_id
    }
    /// Return the authoritative grant revision at issuance.
    pub fn grant_revision(&self) -> u64 {
        self.context.unsigned.grant_revision
    }
    /// Return the provisioned native identity credential, if present.
    pub fn identity_key_id(&self) -> Option<&str> {
        self.context.unsigned.identity_key_id.as_deref()
    }
    /// Return the durable user login, if present.
    pub fn login_session_id(&self) -> Option<&str> {
        self.context.unsigned.login_session_id.as_deref()
    }
    /// Return the SDK-owned runtime connection identifier.
    pub fn connection_id(&self) -> &str {
        &self.context.unsigned.connection_id
    }
    /// Return server-derived deployment metadata.
    pub fn deployment_id(&self) -> Option<&str> {
        self.context.unsigned.deployment_id.as_deref()
    }
    /// Return server-derived stable instance metadata.
    pub fn instance_id(&self) -> Option<&str> {
        self.context.unsigned.instance_id.as_deref()
    }
    /// Return the proof-bound Ed25519 connection key.
    pub fn session_key(&self) -> &VerifyingKey {
        &self.session_key
    }
    /// Return the exact caller reply-inbox prefix.
    pub fn inbox_prefix(&self) -> &str {
        &self.context.unsigned.inbox_prefix
    }
    /// Return expanded exact permission atoms.
    pub fn grant_set(&self) -> &GrantSet {
        &self.context.unsigned.grants
    }
    /// Return validated, server-owned platform meta-authority.
    pub fn platform_privileges(&self) -> &[PlatformPrivilege] {
        &self.context.unsigned.platform_privileges
    }
    /// Test membership of one explicitly assigned platform privilege.
    pub fn has_platform_privilege(&self, privilege: PlatformPrivilege) -> bool {
        self.platform_privileges().contains(&privilege)
    }
    /// Return the complete signed object's content identity.
    pub fn context_digest(&self) -> &str {
        &self.context_digest
    }
    /// Return the context issuance time.
    pub fn issued_at(&self) -> i64 {
        self.context.unsigned.issued_at
    }
    /// Return the inclusive lower validity bound.
    pub fn not_before(&self) -> i64 {
        self.context.unsigned.not_before
    }
    /// Return the exclusive upper validity bound.
    pub fn expires_at(&self) -> i64 {
        self.context.unsigned.expires_at
    }
    /// Return the signed context and its retained verification payload.
    pub fn signed_context(&self) -> &SignedAuthorizationContext {
        &self.context
    }
    /// Test membership of one exact permission atom.
    pub fn allows(&self, permission: &PermissionAtom) -> bool {
        self.grant_set().permissions().contains(permission)
    }
    /// Test every receiver-owned required permission atom.
    pub fn allows_all(&self, permissions: &[PermissionAtom]) -> bool {
        permissions.iter().all(|permission| self.allows(permission))
    }
}

/// Verify one context against an issuer entry authenticated by the configured origin.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for an invalid issuer key or lifecycle
/// state, a validity or collection limit failure, or an invalid signature.
pub fn verify_authorization_context(
    issuer: &AuthorizationIssuerKey,
    context: &SignedAuthorizationContext,
    policy: &AuthorizationVerificationPolicy,
    purpose: AuthorizationContextPurpose,
) -> Result<VerifiedAuthorizationContext, ProtocolError> {
    validate_policy(policy)?;
    validate_context_fields(&context.unsigned, None)?;
    if canonicalize_json(&serde_json::to_value(context)?)?.len() > policy.maximum_context_bytes {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextTooLarge,
            std::iter::empty::<&str>(),
            "canonical signed context exceeds policy size",
        ));
    }
    if context.unsigned.grants.permissions().len() > policy.maximum_permissions {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["grants", "permissions"],
            "context exceeds the permission limit",
        ));
    }
    let key = issuer.verifying_key()?;
    if context.unsigned.issuer_key_id != issuer.key_id {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidKeyId,
            ["issuerKeyId"],
            "context does not belong to the supplied issuer",
        ));
    }
    match (issuer.state, purpose) {
        (AuthorizationIssuerState::Revoked, _) => {
            return Err(authorization_error(
                AuthorizationErrorCode::IssuerRevoked,
                ["issuerKeyId"],
                "issuer is explicitly revoked",
            ))
        }
        (AuthorizationIssuerState::Retired, AuthorizationContextPurpose::Live) => {
            return Err(authorization_error(
                AuthorizationErrorCode::IssuerRetired,
                ["issuerKeyId"],
                "retired issuers cannot authorize live contexts",
            ))
        }
        _ => {}
    }
    let lifetime =
        i128::from(context.unsigned.expires_at) - i128::from(context.unsigned.not_before);
    if lifetime > i128::from(policy.maximum_context_lifetime_seconds) {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextLifetimeExceeded,
            ["expiresAt"],
            "context lifetime exceeds policy",
        ));
    }
    let now = i128::from(policy.now_unix_seconds);
    let skew = i128::from(policy.allowed_clock_skew_seconds);
    if now + skew < i128::from(context.unsigned.not_before) {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextNotYetValid,
            ["notBefore"],
            "authorization context is not yet valid",
        ));
    }
    if purpose == AuthorizationContextPurpose::Live
        && now - skew > i128::from(context.unsigned.expires_at)
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextExpired,
            ["expiresAt"],
            "authorization context has expired",
        ));
    }
    verify_signature(
        &key,
        &authorization_context_signing_digest(&context.unsigned)?,
        &context.signature,
        AuthorizationErrorCode::InvalidSignature,
    )?;
    Ok(VerifiedAuthorizationContext {
        session_key: decode_verifying_key(
            &context.unsigned.session_key,
            &["sessionKey"],
            AuthorizationErrorCode::InvalidSessionKey,
        )?,
        context_digest: context.digest()?,
        context: context.clone(),
        issuer_state: issuer.state,
        purpose,
    })
}

/// Canonical context-bound request-proof input and digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRequestProofInput {
    bytes: Vec<u8>,
    digest: [u8; 32],
}

impl AuthorizationRequestProofInput {
    /// Return the exact length-prefixed proof input bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the SHA-256 proof digest signed by the session key.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// An unpadded base64url Ed25519 request proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRequestProof(String);

impl AuthorizationRequestProof {
    /// Parse and strictly validate an encoded request proof.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Authorization`] unless the value canonically
    /// encodes exactly one Ed25519 signature.
    pub fn parse(encoded: impl Into<String>) -> Result<Self, ProtocolError> {
        let encoded = encoded.into();
        decode_base64url::<64>(
            &encoded,
            &["proof"],
            AuthorizationErrorCode::InvalidRequestProof,
        )?;
        Ok(Self(encoded))
    }

    /// Return the encoded proof.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Build request-proof v1 input from the exact received request values.
///
/// The payload hash is computed internally from `raw_payload`.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] if a component exceeds the unsigned
/// 32-bit length-prefix range.
pub fn build_authorization_request_proof_input(
    context_digest: &[u8; 32],
    subject: &str,
    reply_subject: Option<&str>,
    raw_payload: &[u8],
    iat: i64,
    request_id: &str,
) -> Result<AuthorizationRequestProofInput, ProtocolError> {
    validate_safe_i64(iat, &["iat"])?;
    let payload_hash = sha256(raw_payload);
    let iat = iat.to_string();
    let mut bytes = Vec::new();
    for component in [
        AUTHORIZATION_REQUEST_PROOF_DOMAIN_V1.as_bytes(),
        context_digest,
        subject.as_bytes(),
        reply_subject.unwrap_or("").as_bytes(),
        payload_hash.as_slice(),
        iat.as_bytes(),
        request_id.as_bytes(),
    ] {
        push_length_prefixed(&mut bytes, component)?;
    }
    let digest = sha256(&bytes);
    Ok(AuthorizationRequestProofInput { bytes, digest })
}

/// Sign a context-bound request with the session private key.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for an invalid context digest or
/// oversized proof component.
pub fn sign_authorization_request(
    context_digest: &str,
    subject: &str,
    reply_subject: Option<&str>,
    raw_payload: &[u8],
    iat: i64,
    request_id: &str,
    session_key: &SigningKey,
) -> Result<AuthorizationRequestProof, ProtocolError> {
    validate_safe_i64(iat, &["iat"])?;
    validate_text(request_id, &["request-id"])?;
    validate_text(subject, &["subject"])?;
    if request_id.len() > MAXIMUM_REQUEST_ID_BYTES {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["request-id"],
            "request id exceeds the protocol limit",
        ));
    }
    let context_digest = decode_base64url::<32>(
        context_digest,
        &["authorization-context"],
        AuthorizationErrorCode::InvalidEncoding,
    )?;
    let input = build_authorization_request_proof_input(
        &context_digest,
        subject,
        reply_subject,
        raw_payload,
        iat,
        request_id,
    )?;
    Ok(AuthorizationRequestProof(encode_base64url(
        &session_key.sign(input.digest()).to_bytes(),
    )))
}

/// Verified local caller metadata.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationRequestProof {
    context: VerifiedAuthorizationContext,
}

/// Borrowed inputs for verifying one context-bound request proof.
///
/// Unlike [`AuthorizationRequestProofInput`], which contains the canonical bytes
/// and digest signed by a session key, this type groups the complete local
/// verification decision, including context, policy, and required authority.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizationRequestVerificationInput<'a> {
    /// Verified authorization context that binds the session key and authority.
    pub context: &'a VerifiedAuthorizationContext,
    /// Exact routed NATS subject covered by the proof.
    pub subject: &'a str,
    /// Actual NATS reply subject covered by the proof, when present.
    pub reply_subject: Option<&'a str>,
    /// Exact received payload bytes covered by the proof.
    pub raw_payload: &'a [u8],
    /// Signed proof issue time in Unix seconds.
    pub iat: i64,
    /// Signed request identifier.
    pub request_id: &'a str,
    /// Session-key signature to verify.
    pub proof: &'a AuthorizationRequestProof,
    /// Verification time and protocol limits.
    pub policy: &'a AuthorizationVerificationPolicy,
    /// Exact permissions required by the routed operation.
    pub required_permissions: &'a [PermissionAtom],
}

impl VerifiedAuthorizationRequestProof {
    /// Return verified caller context metadata.
    pub fn context(&self) -> &VerifiedAuthorizationContext {
        &self.context
    }
}

/// Verify request freshness, inbox binding, exact authority subsets, and session
/// key possession without storage access.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for an invalid request id, reply
/// subject, issue time, permission subset, or session-key signature.
pub fn verify_authorization_request(
    input: AuthorizationRequestVerificationInput<'_>,
) -> Result<VerifiedAuthorizationRequestProof, ProtocolError> {
    let AuthorizationRequestVerificationInput {
        context,
        subject,
        reply_subject,
        raw_payload,
        iat,
        request_id,
        proof,
        policy,
        required_permissions,
    } = input;
    context.assert_current(policy)?;
    validate_safe_i64(iat, &["iat"])?;
    validate_text(request_id, &["request-id"])?;
    validate_text(subject, &["subject"])?;
    if request_id.len() > MAXIMUM_REQUEST_ID_BYTES {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["request-id"],
            "request id exceeds the protocol limit",
        ));
    }
    if let Some(reply) = reply_subject {
        let mut required_prefix = context.inbox_prefix().to_owned();
        required_prefix.push('.');
        if !reply.starts_with(&required_prefix) {
            return Err(authorization_error(
                AuthorizationErrorCode::ReplySubjectMismatch,
                ["reply"],
                "reply subject is outside the verified caller inbox prefix",
            ));
        }
    }
    let difference = i128::from(policy.now_unix_seconds) - i128::from(iat);
    if difference.abs() > i128::from(policy.allowed_clock_skew_seconds) {
        return Err(authorization_error(
            AuthorizationErrorCode::ProofIatOutOfRange,
            ["iat"],
            "request proof issue time is outside policy skew",
        ));
    }
    if !context.allows_all(required_permissions) {
        return Err(authorization_error(
            AuthorizationErrorCode::PermissionDenied,
            ["grantSet", "permissions"],
            "verified context does not contain every required exact permission",
        ));
    }
    let context_digest = decode_base64url::<32>(
        context.context_digest(),
        &["authorization-context"],
        AuthorizationErrorCode::InvalidEncoding,
    )?;
    let input = build_authorization_request_proof_input(
        &context_digest,
        subject,
        reply_subject,
        raw_payload,
        iat,
        request_id,
    )?;
    let proof_bytes = decode_base64url::<64>(
        proof.as_str(),
        &["proof"],
        AuthorizationErrorCode::InvalidRequestProof,
    )?;
    context
        .session_key
        .verify_strict(input.digest(), &Signature::from_bytes(&proof_bytes))
        .map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidRequestProof,
                ["proof"],
                "context-bound request signature verification failed",
            )
        })?;
    Ok(VerifiedAuthorizationRequestProof {
        context: context.clone(),
    })
}

/// Canonical context-bound event-proof input and digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationEventProofInput {
    bytes: Vec<u8>,
    digest: [u8; 32],
}

/// Borrowed inputs for verifying one context-bound event proof.
///
/// Unlike [`AuthorizationEventProofInput`], which contains the canonical bytes
/// and digest signed by a session key, this type groups the complete historical
/// verification decision, including context, policy, authority, and revocation.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizationEventVerificationInput<'a> {
    /// Verified authorization context that binds the session key and authority.
    pub context: &'a VerifiedAuthorizationContext,
    /// Exact published NATS subject covered by the proof.
    pub subject: &'a str,
    /// Canonical generated descriptor identity covered by the proof.
    pub descriptor_identity: &'a str,
    /// Exact received payload bytes covered by the proof.
    pub raw_payload: &'a [u8],
    /// Signed event identifier.
    pub event_id: &'a str,
    /// Signed canonical RFC 3339 event time.
    pub event_time: &'a str,
    /// Session-key signature to verify.
    pub proof: &'a AuthorizationEventProof,
    /// Verification policy and protocol limits.
    pub policy: &'a AuthorizationVerificationPolicy,
    /// Context revocation time when revocation evidence exists.
    pub revoked_at: Option<i64>,
}

impl AuthorizationEventProofInput {
    /// Return the exact length-prefixed proof input bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the SHA-256 digest signed by the session key.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// An unpadded base64url Ed25519 event proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationEventProof(String);

impl AuthorizationEventProof {
    /// Parse and strictly validate an encoded event proof.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Authorization`] unless the value canonically
    /// encodes exactly one Ed25519 signature.
    pub fn parse(encoded: impl Into<String>) -> Result<Self, ProtocolError> {
        let encoded = encoded.into();
        decode_base64url::<64>(
            &encoded,
            &["proof"],
            AuthorizationErrorCode::InvalidEventProof,
        )?;
        Ok(Self(encoded))
    }

    /// Return the encoded proof.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed publisher projection for Event Log indexing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationEventPublisher {
    /// Principal kind of the verified publisher.
    pub kind: String,
    /// Deployment identity for deployed principals.
    pub deployment_id: Option<String>,
    /// Runtime instance identity for deployed principals.
    pub instance_id: Option<String>,
    /// Participant id bound into the verified context.
    pub participant_id: String,
    /// Stable authenticated principal.
    pub principal_id: String,
    /// Runtime connection owning the event proof key.
    pub connection_id: String,
    /// Durable user login, when the event publisher is a user.
    pub login_session_id: Option<String>,
}

/// Verified event metadata and publisher projection.
#[derive(Clone, Debug)]
pub struct VerifiedAuthorizationEventProof {
    context: VerifiedAuthorizationContext,
    publisher: AuthorizationEventPublisher,
}

impl VerifiedAuthorizationEventProof {
    /// Return verified publisher context metadata.
    pub fn context(&self) -> &VerifiedAuthorizationContext {
        &self.context
    }

    /// Return the typed publisher projection for Event Log indexing.
    pub fn publisher(&self) -> &AuthorizationEventPublisher {
        &self.publisher
    }
}

/// Parse a canonical RFC 3339 UTC event time into Unix seconds.
///
/// The accepted wire form is `YYYY-MM-DDTHH:MM:SS(.fraction)?Z` with an
/// uppercase `T` separator and `Z` UTC terminator.
fn canonical_event_time_seconds(event_time: &str, path: &[&str]) -> Result<i64, ProtocolError> {
    if !event_time.contains('T') || !event_time.ends_with('Z') {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidEventTime,
            path.iter().copied(),
            "event time is not canonical RFC 3339 UTC",
        ));
    }
    let parsed =
        time::OffsetDateTime::parse(event_time, &time::format_description::well_known::Rfc3339)
            .map_err(|_| {
                authorization_error(
                    AuthorizationErrorCode::InvalidEventTime,
                    path.iter().copied(),
                    "event time is not canonical RFC 3339 UTC",
                )
            })?;
    if parsed.offset() != time::UtcOffset::UTC {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidEventTime,
            path.iter().copied(),
            "event time must be expressed in UTC",
        ));
    }
    Ok(parsed.unix_timestamp())
}

/// Build event-proof v1 input from the exact published event values.
///
/// The payload hash is computed internally from `raw_payload`, and the exact
/// `event_time` string is signed without reformatting.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] if a component exceeds the unsigned
/// 32-bit length-prefix range.
pub fn build_authorization_event_proof_input(
    context_digest: &[u8; 32],
    descriptor_identity: &str,
    subject: &str,
    raw_payload: &[u8],
    event_id: &str,
    event_time: &str,
) -> Result<AuthorizationEventProofInput, ProtocolError> {
    let payload_hash = sha256(raw_payload);
    let mut bytes = Vec::new();
    for component in [
        AUTHORIZATION_EVENT_PROOF_DOMAIN_V1.as_bytes(),
        context_digest,
        descriptor_identity.as_bytes(),
        subject.as_bytes(),
        payload_hash.as_slice(),
        event_id.as_bytes(),
        event_time.as_bytes(),
    ] {
        push_length_prefixed(&mut bytes, component)?;
    }
    let digest = sha256(&bytes);
    Ok(AuthorizationEventProofInput { bytes, digest })
}

/// Sign a context-bound event with the session private key.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for an invalid context digest,
/// non-canonical event time, or oversized proof component.
pub fn sign_authorization_event(
    context_digest: &str,
    descriptor_identity: &str,
    subject: &str,
    raw_payload: &[u8],
    event_id: &str,
    event_time: &str,
    session_key: &SigningKey,
) -> Result<AuthorizationEventProof, ProtocolError> {
    validate_text(event_id, &["event-id"])?;
    validate_text(subject, &["subject"])?;
    if event_id.len() > MAXIMUM_EVENT_ID_BYTES {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["event-id"],
            "event id exceeds the protocol limit",
        ));
    }
    canonical_event_time_seconds(event_time, &["event-time"])?;
    let context_digest = decode_base64url::<32>(
        context_digest,
        &["authorization-context"],
        AuthorizationErrorCode::InvalidEncoding,
    )?;
    let input = build_authorization_event_proof_input(
        &context_digest,
        descriptor_identity,
        subject,
        raw_payload,
        event_id,
        event_time,
    )?;
    Ok(AuthorizationEventProof(encode_base64url(
        &session_key.sign(input.digest()).to_bytes(),
    )))
}

/// Verify event freshness, context binding, exact authority subsets, and
/// session-key possession without storage access.
///
/// The signed event time is evaluated against the verified context window.
/// Any revocation evidence invalidates every event proof from that context so
/// an old signed event cannot be replayed after authority changes.
///
/// # Errors
///
/// Returns [`ProtocolError::Authorization`] for a non-canonical event time,
/// an event outside the context window, a revoked event, missing
/// permission evidence, or an invalid session-key signature.
pub fn verify_authorization_event(
    input: AuthorizationEventVerificationInput<'_>,
) -> Result<VerifiedAuthorizationEventProof, ProtocolError> {
    let AuthorizationEventVerificationInput {
        context,
        subject,
        descriptor_identity,
        raw_payload,
        event_id,
        event_time,
        proof,
        policy,
        revoked_at,
    } = input;
    validate_policy(policy)?;
    validate_text(event_id, &["event-id"])?;
    validate_text(subject, &["subject"])?;
    validate_text(descriptor_identity, &["descriptor-identity"])?;
    let descriptor = crate::decode_event_descriptor_identity(descriptor_identity)?;
    crate::validate_event_descriptor_subject(&descriptor, subject)?;
    if event_id.len() > MAXIMUM_EVENT_ID_BYTES {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["event-id"],
            "event id exceeds the protocol limit",
        ));
    }
    if let Some(revoked_at) = revoked_at {
        validate_safe_i64(revoked_at, &["revoked-at"])?;
    }
    let event_time_seconds = canonical_event_time_seconds(event_time, &["event-time"])?;
    let event_time_unix = i128::from(event_time_seconds);
    // Historical eligibility is the strict signed-context window: no clock skew
    // is applied to notBefore/expiresAt and `expiresAt` itself is exclusive.
    if event_time_unix < i128::from(context.context.unsigned.not_before) {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextNotYetValid,
            ["event-time"],
            "event is before the signed context notBefore window",
        ));
    }
    if event_time_unix >= i128::from(context.context.unsigned.expires_at) {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextExpired,
            ["event-time"],
            "event is at or after the signed context expiresAt window",
        ));
    }
    if revoked_at.is_some() {
        return Err(authorization_error(
            AuthorizationErrorCode::EventRevoked,
            ["event-time"],
            "event authorization context is revoked",
        ));
    }
    let required_permission = PermissionAtom::new(
        PermissionTarget::api_surface(
            descriptor.api_id(),
            ApiSurfaceKind::Event,
            descriptor.event_name(),
        )?,
        PermissionAction::Publish,
    )?;
    if !context.allows_all(std::slice::from_ref(&required_permission)) {
        return Err(authorization_error(
            AuthorizationErrorCode::PermissionDenied,
            ["grantSet", "permissions"],
            "verified context does not contain every required exact permission",
        ));
    }
    let context_digest = decode_base64url::<32>(
        context.context_digest(),
        &["authorization-context"],
        AuthorizationErrorCode::InvalidEncoding,
    )?;
    let input = build_authorization_event_proof_input(
        &context_digest,
        descriptor_identity,
        subject,
        raw_payload,
        event_id,
        event_time,
    )?;
    let proof_bytes = decode_base64url::<64>(
        proof.as_str(),
        &["proof"],
        AuthorizationErrorCode::InvalidEventProof,
    )?;
    context
        .session_key
        .verify_strict(input.digest(), &Signature::from_bytes(&proof_bytes))
        .map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidEventProof,
                ["proof"],
                "context-bound event signature verification failed",
            )
        })?;
    let publisher = AuthorizationEventPublisher {
        kind: match context.principal_kind() {
            AuthorizationPrincipalKind::User => "user",
            AuthorizationPrincipalKind::Service => "service",
            AuthorizationPrincipalKind::Device => "device",
        }
        .to_owned(),
        deployment_id: context.deployment_id().map(str::to_owned),
        instance_id: context.instance_id().map(str::to_owned),
        participant_id: context.participant_id().to_owned(),
        principal_id: context.principal_id().to_owned(),
        connection_id: context.connection_id().to_owned(),
        login_session_id: context.login_session_id().map(str::to_owned),
    };
    Ok(VerifiedAuthorizationEventProof {
        context: context.clone(),
        publisher,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        encode_event_descriptor_identity, ApiSurfaceKind, PermissionAction, PermissionTarget,
    };
    use serde_json::json;

    fn permission() -> PermissionAtom {
        PermissionAtom::new(
            PermissionTarget::api_surface("documents@v1", ApiSurfaceKind::Rpc, "Documents.Get")
                .unwrap(),
            PermissionAction::Call,
        )
        .unwrap()
    }
    fn policy(now: i64) -> AuthorizationVerificationPolicy {
        AuthorizationVerificationPolicy::new(now, 30, 300, 16_384, 16).unwrap()
    }
    fn issued() -> (
        AuthorizationIssuerKey,
        SignedAuthorizationContext,
        SigningKey,
    ) {
        issued_with_event(None)
    }

    fn issued_with_event(
        event_name: Option<&str>,
    ) -> (
        AuthorizationIssuerKey,
        SignedAuthorizationContext,
        SigningKey,
    ) {
        let issuer_key = SigningKey::from_bytes(&[2; 32]);
        let session_key = SigningKey::from_bytes(&[3; 32]);
        let issuer = AuthorizationIssuerKey {
            key_id: derived_key_id(&issuer_key.verifying_key()),
            public_key: encode_base64url(issuer_key.verifying_key().as_bytes()),
            state: AuthorizationIssuerState::Active,
        };
        let mut permissions = vec![permission()];
        if let Some(event_name) = event_name {
            permissions.push(
                PermissionAtom::new(
                    PermissionTarget::api_surface(
                        "documents@v1",
                        ApiSurfaceKind::Event,
                        event_name,
                    )
                    .unwrap(),
                    PermissionAction::Publish,
                )
                .unwrap(),
            );
        }
        let context = sign_authorization_context(
            UnsignedAuthorizationContext {
                format: AUTHORIZATION_CONTEXT_FORMAT_V1.to_owned(),
                issuer_key_id: issuer.key_id.clone(),
                principal_id: "01JY0000000000000000000001".to_owned(),
                principal_kind: AuthorizationPrincipalKind::User,
                participant_id: "documents-web".to_owned(),
                owner_kind: GrantOwnerKind::User,
                owner_id: "01JY0000000000000000000001".to_owned(),
                grant_revision: 12,
                identity_key_id: None,
                login_session_id: Some("01JY0000000000000000000002".to_owned()),
                connection_id: "01JY0000000000000000000003".to_owned(),
                session_key: encode_base64url(session_key.verifying_key().as_bytes()),
                deployment_id: None,
                instance_id: None,
                inbox_prefix: "_INBOX.test".to_owned(),
                issued_at: 1_100,
                not_before: 1_100,
                expires_at: 1_300,
                grants: GrantSet::new(permissions),
                platform_privileges: vec![PlatformPrivilege::Admin],
                extensions: Map::new(),
                critical: vec![],
            },
            &issuer_key,
        )
        .unwrap();
        (issuer, context, session_key)
    }

    #[test]
    fn online_context_binds_grants_and_the_exact_request() {
        let (issuer, context, session_key) = issued();
        let policy = policy(1_100);
        let verified = verify_authorization_context(
            &issuer,
            &context,
            &policy,
            AuthorizationContextPurpose::Live,
        )
        .unwrap();
        let permissions = [permission()];
        let proof = sign_authorization_request(
            verified.context_digest(),
            "rpc.v1.Documents.Get",
            Some("_INBOX.test.reply"),
            b"payload",
            1_100,
            "01JY0000000000000000000004",
            &session_key,
        )
        .unwrap();
        let request = AuthorizationRequestVerificationInput {
            context: &verified,
            subject: "rpc.v1.Documents.Get",
            reply_subject: Some("_INBOX.test.reply"),
            raw_payload: b"payload",
            iat: 1_100,
            request_id: "01JY0000000000000000000004",
            proof: &proof,
            policy: &policy,
            required_permissions: &permissions,
        };
        verify_authorization_request(request).unwrap();
        for changed in [
            AuthorizationRequestVerificationInput {
                reply_subject: Some("_INBOX.other.reply"),
                ..request
            },
            AuthorizationRequestVerificationInput {
                raw_payload: b"tampered",
                ..request
            },
            AuthorizationRequestVerificationInput {
                subject: "rpc.v1.Other.Get",
                ..request
            },
        ] {
            assert!(verify_authorization_request(changed).is_err());
        }
        let missing = [PermissionAtom::new(
            PermissionTarget::api_surface("documents@v1", ApiSurfaceKind::Rpc, "Documents.Delete")
                .unwrap(),
            PermissionAction::Call,
        )
        .unwrap()];
        assert!(
            verify_authorization_request(AuthorizationRequestVerificationInput {
                required_permissions: &missing,
                ..request
            })
            .is_err(),
            "admin must not manufacture ordinary action grants"
        );
    }

    #[test]
    fn retired_keys_preserve_history_but_never_live_authority() {
        let (mut issuer, context, session_key) = issued_with_event(Some("Documents.Changed"));
        let current = policy(1_400);
        assert!(verify_authorization_context(
            &issuer,
            &context,
            &current,
            AuthorizationContextPurpose::Live
        )
        .is_err());
        issuer.state = AuthorizationIssuerState::Retired;
        let historical = verify_authorization_context(
            &issuer,
            &context,
            &current,
            AuthorizationContextPurpose::HistoricalEvent,
        )
        .unwrap();
        assert!(verify_authorization_context(
            &issuer,
            &context,
            &policy(1_100),
            AuthorizationContextPurpose::Live
        )
        .is_err());
        let descriptor_identity =
            encode_event_descriptor_identity("documents@v1", "Documents.Changed", 0).unwrap();
        let proof = sign_authorization_event(
            historical.context_digest(),
            &descriptor_identity,
            "events.v1.ZG9jdW1lbnRzQHYx.Documents.Changed",
            b"event",
            "01JY0000000000000000000005",
            "1970-01-01T00:19:10Z",
            &session_key,
        )
        .unwrap();
        let event = AuthorizationEventVerificationInput {
            context: &historical,
            subject: "events.v1.ZG9jdW1lbnRzQHYx.Documents.Changed",
            descriptor_identity: &descriptor_identity,
            raw_payload: b"event",
            event_id: "01JY0000000000000000000005",
            event_time: "1970-01-01T00:19:10Z",
            proof: &proof,
            policy: &current,
            revoked_at: None,
        };
        verify_authorization_event(event).unwrap();
        assert!(
            verify_authorization_event(AuthorizationEventVerificationInput {
                revoked_at: Some(1_350),
                ..event
            })
            .is_err()
        );
        let request_proof = sign_authorization_request(
            historical.context_digest(),
            "rpc.v1.Documents.Get",
            None,
            b"payload",
            1_100,
            "01JY0000000000000000000004",
            &session_key,
        )
        .unwrap();
        let permissions = [permission()];
        assert!(matches!(
            verify_authorization_request(AuthorizationRequestVerificationInput {
                context: &historical,
                subject: "rpc.v1.Documents.Get",
                reply_subject: None,
                raw_payload: b"payload",
                iat: 1_100,
                request_id: "01JY0000000000000000000004",
                proof: &request_proof,
                policy: &policy(1_100),
                required_permissions: &permissions
            }),
            Err(ProtocolError::Authorization {
                code: AuthorizationErrorCode::HistoricalContext,
                ..
            })
        ));
        issuer.state = AuthorizationIssuerState::Revoked;
        assert!(verify_authorization_context(
            &issuer,
            &context,
            &current,
            AuthorizationContextPurpose::HistoricalEvent
        )
        .is_err());
    }

    #[test]
    fn overlapping_dotted_event_names_require_the_exact_granted_descriptor() {
        let subject = "events.v1.ZG9jdW1lbnRzQHYx.Connections.Opened";
        let cases = [
            ("Connections", 1, "Connections.Opened", 0),
            ("Connections.Opened", 0, "Connections", 1),
        ];

        for (granted_name, granted_params, denied_name, denied_params) in cases {
            let (issuer, context, session_key) = issued_with_event(Some(granted_name));
            let context = verify_authorization_context(
                &issuer,
                &context,
                &policy(1_150),
                AuthorizationContextPurpose::Live,
            )
            .unwrap();

            for (event_name, parameter_count, accepted) in [
                (granted_name, granted_params, true),
                (denied_name, denied_params, false),
            ] {
                let descriptor_identity =
                    encode_event_descriptor_identity("documents@v1", event_name, parameter_count)
                        .unwrap();
                let proof = sign_authorization_event(
                    context.context_digest(),
                    &descriptor_identity,
                    subject,
                    b"event",
                    "01JY0000000000000000000005",
                    "1970-01-01T00:19:10Z",
                    &session_key,
                )
                .unwrap();
                let result = verify_authorization_event(AuthorizationEventVerificationInput {
                    context: &context,
                    subject,
                    descriptor_identity: &descriptor_identity,
                    raw_payload: b"event",
                    event_id: "01JY0000000000000000000005",
                    event_time: "1970-01-01T00:19:10Z",
                    proof: &proof,
                    policy: &policy(1_150),
                    revoked_at: None,
                });
                assert_eq!(
                    result.is_ok(),
                    accepted,
                    "event identity {event_name}: {result:?}"
                );
            }
        }
    }

    #[test]
    fn context_identity_and_platform_privileges_fail_closed_on_tampering() {
        let (issuer, context, _) = issued();
        let value = serde_json::to_value(&context).unwrap();
        assert_eq!(context.digest().unwrap().len(), 43);
        for (field, replacement) in [
            ("principalId", json!("01JY0000000000000000000009")),
            ("grantRevision", json!(0)),
            (
                "platformPrivileges",
                json!(["ordinary.participant-capability"]),
            ),
            (
                "platformPrivileges",
                json!(["trellis.auth::admin", "trellis.auth::admin"]),
            ),
        ] {
            let mut changed = value.clone();
            changed[field] = replacement;
            assert!(parse_authorization_context(&changed).is_err());
        }
        let mut changed = context.clone();
        changed.unsigned.grant_revision += 1;
        assert!(verify_authorization_context(
            &issuer,
            &changed,
            &policy(1_100),
            AuthorizationContextPurpose::Live
        )
        .is_err());
        let mut wrong_key = issuer.clone();
        wrong_key.public_key =
            encode_base64url(SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes());
        assert!(wrong_key.verifying_key().is_err());
        let mut unknown = value;
        unknown["unknownField"] = json!("not-an-identity-credential");
        assert!(parse_authorization_context(&unknown).is_err());
    }
}
