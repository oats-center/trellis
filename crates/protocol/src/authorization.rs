//! Pinned-issuer session authority and independently authenticated messages.
//!
//! Verification is pure: trust anchors, expected instance/account, time, and
//! known logical-session revocation are explicit inputs. A key's content-derived
//! identifier is not evidence that the key is trusted. Signed extensions are
//! preserved; unsupported critical extensions fail closed.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use jsonptr::PointerBuf;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::{canonicalize_json, AuthorizationErrorCode, PlatformPrivilege, ProtocolError, U64s};

/// The sole session-authority format and issuer-signature domain.
pub const SESSION_AUTHORITY_FORMAT_V1: &str = "trellis.session-authority.v1";
/// Caller request proof domain.
pub const MESSAGE_REQUEST_DOMAIN_V1: &str = "trellis.message.request.v1";
/// Provider response proof domain.
pub const MESSAGE_RESPONSE_DOMAIN_V1: &str = "trellis.message.response.v1";
/// Event publication proof domain.
pub const MESSAGE_EVENT_DOMAIN_V1: &str = "trellis.message.event.v1";
/// Live proof domain; transcripts also identify direction and control kind.
pub const LIVE_PROOF_DOMAIN_V1: &str = "trellis.live.proof.v1";
/// Transfer proof domain; transcripts also identify direction and control kind.
pub const TRANSFER_PROOF_DOMAIN_V1: &str = "trellis.transfer.proof.v1";
/// Initial own-authority retrieval proof domain.
pub const ADMISSION_GET_DOMAIN_V1: &str = "trellis.admission.get.v1";
/// Issuer-signed public metadata response domain.
pub const ISSUER_METADATA_DOMAIN_V1: &str = "trellis.issuer.metadata.v1";

pub(crate) fn authorization_error<'a>(
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

pub(crate) fn validate_text(value: &str, path: &[&str]) -> Result<(), ProtocolError> {
    if value.is_empty()
        || value.len() > 1024
        || value.trim() != value
        || value.chars().any(|character| character.is_ascii_control())
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            path.iter().copied(),
            "value must be bounded nonempty protocol-safe text",
        ));
    }
    Ok(())
}

pub(crate) fn is_utf16_strictly_sorted(values: &[String]) -> bool {
    values
        .windows(2)
        .all(|pair| pair[0].encode_utf16().cmp(pair[1].encode_utf16()).is_lt())
}

fn decode_base64url<const N: usize>(value: &str) -> Result<[u8; N], ProtocolError> {
    if value.len() != (N * 8).div_ceil(6) {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidEncoding,
            [],
            "invalid encoded length",
        ));
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidEncoding,
            [],
            "invalid base64url",
        )
    })?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidEncoding,
            [],
            "noncanonical base64url",
        ));
    }
    bytes.try_into().map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidEncoding,
            [],
            "invalid decoded length",
        )
    })
}

fn verifying_key(value: &str) -> Result<VerifyingKey, ProtocolError> {
    let key = VerifyingKey::from_bytes(&decode_base64url(value)?).map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidPublicKey,
            [],
            "invalid Ed25519 key",
        )
    })?;
    if key.is_weak() {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidPublicKey,
            [],
            "weak Ed25519 key",
        ));
    }
    Ok(key)
}

fn validate_id(value: &str, field: &str) -> Result<(), ProtocolError> {
    if value.len() != 26
        || ulid::Ulid::from_string(value).is_err()
        || value != value.to_ascii_uppercase()
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [field],
            "identifier must be a canonical ULID",
        ));
    }
    Ok(())
}

fn validate_subject(value: &str) -> Result<(), ProtocolError> {
    validate_text(value, &["subject"])?;
    if value.split('.').any(str::is_empty)
        || value.contains(['*', '>'])
        || value.chars().any(char::is_whitespace)
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["subject"],
            "literal NATS subject required",
        ));
    }
    Ok(())
}

fn push_component(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), ProtocolError> {
    let length = u32::try_from(value.len()).map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [],
            "proof component too large",
        )
    })?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

pub(crate) fn signed_json_digest(
    domain: &str,
    value: &impl Serialize,
) -> Result<[u8; 32], ProtocolError> {
    let canonical = canonicalize_json(&serde_json::to_value(value)?)?;
    let mut bytes = Vec::with_capacity(domain.len() + canonical.len() + 8);
    push_component(&mut bytes, domain.as_bytes())?;
    push_component(&mut bytes, canonical.as_bytes())?;
    Ok(Sha256::digest(bytes).into())
}

fn validate_extension_numbers(value: &Value) -> Result<(), ProtocolError> {
    match value {
        Value::Number(number) => {
            let unsafe_integer = number
                .as_i64()
                .is_some_and(|value| value.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|value| value > 9_007_199_254_740_991)
                || number.as_f64().is_some_and(|value| {
                    value.fract() == 0.0 && value.abs() > 9_007_199_254_740_991.0
                });
            if unsafe_integer {
                return Err(authorization_error(
                    AuthorizationErrorCode::UnsafeJsonInteger,
                    ["extensions"],
                    "extension integer outside interoperable range",
                ));
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_extension_numbers(value)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_extension_numbers(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Principal identities never represent applications.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PrincipalKind {
    /// User account.
    User,
    /// Provisioned service.
    Service,
    /// Provisioned device.
    Device,
}

/// Credential record discriminator, independent of public-client kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CredentialKind {
    /// A user OAuth grant bound to a durable public-client key.
    OAuthGrant,
    /// A provisioned public identity key.
    ProvisionedIdentity,
}

/// Authority binding authenticated by Auth, not asserted by a message header.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionBinding {
    /// Browser origin and OAuth client.
    Browser {
        /// Registered ID or validated CIMD URL.
        client_id: String,
        /// Canonical scheme/host/port origin.
        origin: String,
    },
    /// Native OAuth client and its durable key thumbprint.
    Native {
        /// Public OAuth client ID.
        client_id: String,
        /// Canonical RFC 7638 thumbprint.
        durable_dpop_jkt: String,
    },
    /// Service deployment identity.
    Service {
        /// Deployment identity.
        deployment_id: String,
        /// Provisioned instance identity.
        instance_id: String,
        /// Verified generated participant identity.
        participant_id: String,
    },
    /// Device deployment identity.
    Device {
        /// Deployment identity.
        deployment_id: String,
        /// Provisioned instance identity.
        instance_id: String,
        /// Verified generated participant identity.
        participant_id: String,
    },
}

/// Whole capability authority; counters are canonical decimal strings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CapabilityAuthority {
    /// Stable human-facing capability identity.
    pub capability_id: String,
    /// Auth-owned identity generation.
    pub identity_generation: U64s,
    /// Approved authored consent meaning.
    pub consent_revision: U64s,
}

/// Accepted API metadata captured in a signed authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorityApi {
    /// Version-qualified API identity.
    pub api_id: String,
    /// Auth-owned incompatible-replacement generation.
    pub generation: U64s,
    /// Auth-assigned accepted revision.
    pub accepted_revision: U64s,
    /// Digest of the corresponding Auth-signed catalog material.
    pub catalog_snapshot_digest: String,
}

/// Certified provider association; caller capabilities do not grant this right.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorityProvider {
    /// Version-qualified API identity.
    pub api_id: String,
    /// Accepted provider generation.
    pub generation: U64s,
    /// Deployment identity.
    pub deployment_id: String,
    /// Generated implementation semantics digest.
    pub implementation_digest: String,
    /// Auth-signed implemented-action certificate digest.
    pub provider_certificate_digest: String,
}

/// Authenticated request identity and captured whole-capability metadata.
///
/// This projection proves caller identity, not permission to execute an action.
/// Dispatch must additionally check the current signed catalog and retirement
/// state; captured API revisions never imply authority over later membership.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthenticatedCaller {
    /// Content address of the complete signed authority.
    pub authority_digest: String,
    /// Stable user/service/device identity.
    pub principal_id: String,
    /// Stable principal class.
    pub principal_kind: PrincipalKind,
    /// Auth-verified client or deployment binding.
    pub binding: SessionBinding,
    /// Irreversible logical-session retirement identity.
    pub authorization_session_id: String,
    /// Fresh runtime nonce, distinct from the durable session identity.
    pub runtime_id: String,
    /// Canonical ephemeral Ed25519 public key.
    pub session_public_key: String,
    /// Signed reply inbox prefix.
    pub inbox_prefix: String,
    /// Durable user login association, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_session_id: Option<String>,
    /// Durable user OAuth grant association, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_grant_id: Option<String>,
    /// Approved whole-capability identities and consent counters.
    pub capabilities: Vec<CapabilityAuthority>,
    /// Accepted API stamps captured by this authority.
    pub apis: Vec<AuthorityApi>,
    /// Explicit finite administrative subset.
    pub platform_privileges: Vec<PlatformPrivilege>,
}

/// Complete issuer-authenticated authority for one logical runtime session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnsignedSessionAuthority {
    /// Sole accepted format.
    pub format: String,
    /// Pinned issuer lookup identity.
    pub issuer_key_id: String,
    /// Prevents substitution from another installation.
    pub trellis_instance_id: String,
    /// Prevents substitution across broker accounts.
    pub audience_nats_account: String,
    /// Stable user/service/device identity.
    pub principal_id: String,
    /// Stable principal class.
    pub principal_kind: PrincipalKind,
    /// Auth-verified logical client/deployment binding.
    pub binding: SessionBinding,
    /// Irreversibly retired by reductions or hard revocation.
    pub authorization_session_id: String,
    /// Canonically encoded 128-bit fresh runtime nonce.
    pub runtime_id: String,
    /// Canonical base64url ephemeral Ed25519 public key.
    pub session_public_key: String,
    /// Literal caller reply prefix.
    pub inbox_prefix: String,
    /// User login association only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_session_id: Option<String>,
    /// User OAuth authorization association only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_grant_id: Option<String>,
    /// Whole approved capabilities, sorted by identity.
    pub capabilities: Vec<CapabilityAuthority>,
    /// Accepted API stamps, sorted by identity.
    pub apis: Vec<AuthorityApi>,
    /// Certified provider rights, sorted by API identity.
    pub provider_bindings: Vec<AuthorityProvider>,
    /// Principal-owned resource bindings, never app resource approvals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_bindings_digest: Option<String>,
    /// Explicitly authorized finite administrative subset.
    pub platform_privileges: Vec<PlatformPrivilege>,
    /// Unix seconds when Auth issued the authority.
    pub issued_at: i64,
    /// Inclusive Unix-second lower bound.
    pub not_before: i64,
    /// Exclusive Unix-second upper bound.
    pub expires_at: i64,
    /// Integrity-bound optional extensions.
    pub extensions: Map<String, Value>,
    /// Sorted extension names whose interpretation is required.
    pub critical: Vec<String>,
}

/// Immutable signed session authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SignedSessionAuthority {
    /// Complete authenticated fields.
    #[serde(flatten)]
    pub unsigned: UnsignedSessionAuthority,
    /// Canonical base64url issuer signature.
    pub signature: String,
}

impl SignedSessionAuthority {
    /// Content address of the complete canonical signed bytes.
    pub fn digest(&self) -> Result<String, ProtocolError> {
        let canonical = canonicalize_json(&serde_json::to_value(self)?)?;
        Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())))
    }
}

/// Lifecycle supplied from authenticated issuer trust material.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorityIssuerState {
    /// Eligible for current authority.
    Active,
    /// Historical material only.
    Retired,
    /// Compromised or explicitly revoked, including for history.
    Revoked,
}

/// A caller-supplied pinned/authenticated issuer, never self-authenticated by ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorityIssuerKey {
    /// SHA-256 lookup identity of the raw public key.
    pub key_id: String,
    /// Canonical base64url Ed25519 key.
    pub public_key: String,
    /// Authenticated lifecycle state.
    pub state: AuthorityIssuerState,
}

impl AuthorityIssuerKey {
    /// Validate canonical key bytes and their content address.
    ///
    /// This validates metadata integrity, not trust: callers must obtain this
    /// record from the instance's pinned issuer boundary before accepting it.
    pub fn verifying_key(&self) -> Result<VerifyingKey, ProtocolError> {
        let key = verifying_key(&self.public_key)?;
        if self.key_id != URL_SAFE_NO_PAD.encode(Sha256::digest(key.as_bytes())) {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidKeyId,
                [],
                "issuer content address mismatch",
            ));
        }
        Ok(key)
    }
}

/// Explicit time and allocation limits shared by native and WASM verification.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionAuthorityVerificationPolicy {
    /// Caller-controlled verification clock.
    pub now_unix_seconds: i64,
    /// Maximum symmetric message-clock skew.
    pub allowed_clock_skew_seconds: u32,
    /// Maximum authority lease duration.
    pub maximum_authority_lifetime_seconds: u32,
    /// Maximum authority JSON bytes.
    pub maximum_authority_bytes: usize,
    /// Maximum aggregate capability/API/provider entries.
    pub maximum_entries: usize,
}

/// Historical authority must never authorize a current request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionAuthorityPurpose {
    /// Current application/transport use.
    Live,
    /// Event-time eligibility is checked at the historical event boundary.
    HistoricalEvent,
}

/// Required trust scope and sticky revocation input.
#[derive(Clone, Debug)]
pub struct SessionAuthorityVerificationInput<'a> {
    /// Pinned issuer or key authenticated by its pinned rotation chain.
    pub issuer: &'a AuthorityIssuerKey,
    /// Expected installed Trellis identity.
    pub trellis_instance_id: &'a str,
    /// Expected NATS account.
    pub audience_nats_account: &'a str,
    /// Signed material being checked.
    pub authority: &'a SignedSessionAuthority,
    /// Bounded acceptance policy.
    pub policy: &'a SessionAuthorityVerificationPolicy,
    /// Live or historical-only use.
    pub purpose: SessionAuthorityPurpose,
    /// Authenticated cutoff for this exact logical session, if known.
    pub revocation_cutoff: Option<i64>,
}

/// Signature-checked immutable authority; policy authorization is separate.
#[derive(Clone, Debug)]
pub struct VerifiedSessionAuthority {
    signed: SignedSessionAuthority,
    digest: String,
    session_key: VerifyingKey,
    purpose: SessionAuthorityPurpose,
    revocation_cutoff: Option<i64>,
}

impl VerifiedSessionAuthority {
    /// Authenticated fields, never reconstructed from message headers.
    pub fn authority(&self) -> &UnsignedSessionAuthority {
        &self.signed.unsigned
    }
    /// Exact content address bound by every message proof.
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// Original immutable signed material.
    pub fn signed(&self) -> &SignedSessionAuthority {
        &self.signed
    }
    /// Recheck live time and sticky logical-session revocation after an async wait.
    pub fn assert_current(
        &self,
        policy: &SessionAuthorityVerificationPolicy,
        known_revoked: bool,
    ) -> Result<(), ProtocolError> {
        if self.purpose != SessionAuthorityPurpose::Live {
            return Err(authorization_error(
                AuthorizationErrorCode::HistoricalContext,
                [],
                "historical-only authority",
            ));
        }
        if known_revoked || self.revocation_cutoff.is_some() {
            return Err(authorization_error(
                AuthorizationErrorCode::SessionRevoked,
                [],
                "logical session revoked",
            ));
        }
        let body = self.authority();
        if policy.now_unix_seconds < body.not_before {
            return Err(authorization_error(
                AuthorizationErrorCode::ContextNotYetValid,
                [],
                "authority not yet valid",
            ));
        }
        if policy.now_unix_seconds >= body.expires_at {
            return Err(authorization_error(
                AuthorizationErrorCode::ContextExpired,
                [],
                "authority expired",
            ));
        }
        Ok(())
    }
}

fn validate_authority(body: &UnsignedSessionAuthority) -> Result<(), ProtocolError> {
    if body.format != SESSION_AUTHORITY_FORMAT_V1 {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            ["format"],
            "unsupported authority format",
        ));
    }
    for (field, value) in [
        ("trellisInstanceId", body.trellis_instance_id.as_str()),
        ("principalId", body.principal_id.as_str()),
        (
            "authorizationSessionId",
            body.authorization_session_id.as_str(),
        ),
    ] {
        validate_id(value, field)?;
    }
    validate_text(&body.audience_nats_account, &["audienceNatsAccount"])?;
    decode_base64url::<32>(&body.issuer_key_id)?;
    decode_base64url::<16>(&body.runtime_id)?;
    verifying_key(&body.session_public_key)?;
    validate_subject(&body.inbox_prefix)?;
    if body.not_before < 0 || body.not_before > body.issued_at || body.issued_at >= body.expires_at
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidValidityWindow,
            [],
            "invalid authority interval",
        ));
    }
    if body.expires_at > 9_007_199_254_740_991 {
        return Err(authorization_error(
            AuthorizationErrorCode::UnsafeJsonInteger,
            [],
            "time outside JSON safe range",
        ));
    }
    match (&body.binding, body.principal_kind) {
        (SessionBinding::Browser { client_id, origin }, PrincipalKind::User) => {
            validate_text(client_id, &["binding", "clientId"])?;
            let url = url::Url::parse(origin).map_err(|_| {
                authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    ["binding", "origin"],
                    "invalid origin",
                )
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.origin().ascii_serialization() != *origin
            {
                return Err(authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    ["binding", "origin"],
                    "origin must be canonical scheme/host/port",
                ));
            }
        }
        (
            SessionBinding::Native {
                client_id,
                durable_dpop_jkt,
            },
            PrincipalKind::User,
        ) => {
            validate_text(client_id, &["binding", "clientId"])?;
            decode_base64url::<32>(durable_dpop_jkt)?;
        }
        (
            SessionBinding::Service {
                deployment_id,
                instance_id,
                participant_id,
            },
            PrincipalKind::Service,
        )
        | (
            SessionBinding::Device {
                deployment_id,
                instance_id,
                participant_id,
            },
            PrincipalKind::Device,
        ) => {
            validate_id(deployment_id, "deploymentId")?;
            validate_id(instance_id, "instanceId")?;
            validate_text(participant_id, &["binding", "participantId"])?;
        }
        _ => {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                ["binding"],
                "binding/principal mismatch",
            ))
        }
    }
    match (
        body.principal_kind,
        &body.login_session_id,
        &body.oauth_grant_id,
    ) {
        (PrincipalKind::User, Some(login), Some(grant)) => {
            validate_id(login, "loginSessionId")?;
            validate_id(grant, "oauthGrantId")?;
            if body.resource_bindings_digest.is_some() || !body.provider_bindings.is_empty() {
                return Err(authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    [],
                    "user binding cannot own deployment resources or provider rights",
                ));
            }
        }
        (PrincipalKind::Service | PrincipalKind::Device, None, None) => {}
        _ => {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                [],
                "invalid credential association",
            ))
        }
    }
    for capability in &body.capabilities {
        validate_text(&capability.capability_id, &["capabilityId"])?;
        if capability.identity_generation.get() == 0 || capability.consent_revision.get() == 0 {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                ["capabilities"],
                "capability counters must be positive",
            ));
        }
    }
    for api in &body.apis {
        crate::validate_api_id(&api.api_id)?;
        if api.generation.get() == 0 || api.accepted_revision.get() == 0 {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                ["apis"],
                "API counters must be positive",
            ));
        }
        decode_base64url::<32>(&api.catalog_snapshot_digest)?;
    }
    for provider in &body.provider_bindings {
        crate::validate_api_id(&provider.api_id)?;
        validate_id(&provider.deployment_id, "deploymentId")?;
        decode_base64url::<32>(&provider.implementation_digest)?;
        decode_base64url::<32>(&provider.provider_certificate_digest)?;
        let deployment = match &body.binding {
            SessionBinding::Service { deployment_id, .. }
            | SessionBinding::Device { deployment_id, .. } => deployment_id,
            _ => {
                return Err(authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    ["providerBindings"],
                    "deployment binding required",
                ))
            }
        };
        if &provider.deployment_id != deployment
            || !body
                .apis
                .iter()
                .any(|api| api.api_id == provider.api_id && api.generation == provider.generation)
        {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                ["providerBindings"],
                "provider must match the bound deployment and API generation",
            ));
        }
    }
    for ids in [
        body.capabilities
            .iter()
            .map(|cap| cap.capability_id.clone())
            .collect::<Vec<_>>(),
        body.apis.iter().map(|api| api.api_id.clone()).collect(),
        body.provider_bindings
            .iter()
            .map(|provider| provider.api_id.clone())
            .collect(),
    ] {
        if !is_utf16_strictly_sorted(&ids) {
            return Err(authorization_error(
                AuthorizationErrorCode::NonCanonicalSet,
                [],
                "authority identities must be sorted and unique",
            ));
        }
    }
    let privilege_names = body
        .platform_privileges
        .iter()
        .map(|privilege| {
            serde_json::to_value(privilege)
                .map(|value| value.as_str().unwrap_or_default().to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !is_utf16_strictly_sorted(&privilege_names) || !is_utf16_strictly_sorted(&body.critical) {
        return Err(authorization_error(
            AuthorizationErrorCode::NonCanonicalSet,
            [],
            "privileges and critical names must be sorted and unique",
        ));
    }
    if !body.critical.is_empty() {
        return Err(authorization_error(
            AuthorizationErrorCode::UnknownCriticalExtension,
            ["critical"],
            "unsupported critical extension",
        ));
    }
    if let Some(digest) = &body.resource_bindings_digest {
        decode_base64url::<32>(digest)?;
    }
    for value in body.extensions.values() {
        validate_extension_numbers(value)?;
    }
    Ok(())
}

/// Sign target-format authority using an Auth-owned Ed25519 issuer.
pub fn sign_session_authority(
    body: UnsignedSessionAuthority,
    issuer: &SigningKey,
) -> Result<SignedSessionAuthority, ProtocolError> {
    validate_authority(&body)?;
    if body.issuer_key_id
        != URL_SAFE_NO_PAD.encode(Sha256::digest(issuer.verifying_key().as_bytes()))
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidKeyId,
            ["issuerKeyId"],
            "issuer key mismatch",
        ));
    }
    let digest = signed_json_digest(SESSION_AUTHORITY_FORMAT_V1, &body)?;
    Ok(SignedSessionAuthority {
        unsigned: body,
        signature: URL_SAFE_NO_PAD.encode(issuer.sign(&digest).to_bytes()),
    })
}

/// Bound allocation before parsing the strict, single accepted authority format.
pub fn parse_session_authority(
    bytes: &[u8],
    maximum_bytes: usize,
) -> Result<SignedSessionAuthority, ProtocolError> {
    if maximum_bytes == 0 || bytes.len() > maximum_bytes {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextTooLarge,
            [],
            "authority exceeds byte budget",
        ));
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        #[serde(flatten)]
        unsigned: UnsignedSessionAuthority,
        signature: String,
    }
    let wire: Wire = serde_json::from_slice(bytes)?;
    validate_authority(&wire.unsigned)?;
    decode_base64url::<64>(&wire.signature)?;
    Ok(SignedSessionAuthority {
        unsigned: wire.unsigned,
        signature: wire.signature,
    })
}

/// Verify pinned issuer, installation/account, bounds, and logical-session state.
pub fn verify_session_authority(
    input: SessionAuthorityVerificationInput<'_>,
) -> Result<VerifiedSessionAuthority, ProtocolError> {
    validate_authority(&input.authority.unsigned)?;
    let policy = input.policy;
    if policy.maximum_authority_bytes == 0
        || policy.maximum_entries == 0
        || policy.maximum_authority_lifetime_seconds == 0
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [],
            "verification limits must be nonzero",
        ));
    }
    let body = &input.authority.unsigned;
    if serde_json::to_vec(input.authority)?.len() > policy.maximum_authority_bytes
        || body
            .capabilities
            .len()
            .saturating_add(body.apis.len())
            .saturating_add(body.provider_bindings.len())
            > policy.maximum_entries
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextTooLarge,
            [],
            "authority exceeds configured bounds",
        ));
    }
    if body.expires_at - body.not_before > i64::from(policy.maximum_authority_lifetime_seconds) {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextLifetimeExceeded,
            [],
            "authority lease exceeds policy",
        ));
    }
    if body.trellis_instance_id != input.trellis_instance_id
        || body.audience_nats_account != input.audience_nats_account
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ScopeMismatch,
            [],
            "authority installation/account mismatch",
        ));
    }
    let key = input.issuer.verifying_key()?;
    if body.issuer_key_id != input.issuer.key_id {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidKeyId,
            [],
            "pinned issuer mismatch",
        ));
    }
    if input.issuer.state == AuthorityIssuerState::Revoked {
        return Err(authorization_error(
            AuthorizationErrorCode::IssuerRevoked,
            [],
            "issuer revoked",
        ));
    }
    if input.purpose == SessionAuthorityPurpose::Live
        && input.issuer.state == AuthorityIssuerState::Retired
    {
        return Err(authorization_error(
            AuthorizationErrorCode::IssuerRetired,
            [],
            "issuer historical-only",
        ));
    }
    let signature = Signature::from_bytes(&decode_base64url(&input.authority.signature)?);
    key.verify_strict(
        &signed_json_digest(SESSION_AUTHORITY_FORMAT_V1, body)?,
        &signature,
    )
    .map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidSignature,
            [],
            "invalid issuer signature",
        )
    })?;
    let verified = VerifiedSessionAuthority {
        signed: input.authority.clone(),
        digest: input.authority.digest()?,
        session_key: verifying_key(&body.session_public_key)?,
        purpose: input.purpose,
        revocation_cutoff: input.revocation_cutoff,
    };
    if input.purpose == SessionAuthorityPurpose::Live {
        verified.assert_current(policy, false)?;
    }
    Ok(verified)
}

/// Exact request identity; route metadata must come from the actual registered route.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionRequest {
    /// Immutable signed authority content address.
    pub authority_digest: String,
    /// Actual API identity.
    pub api_id: String,
    /// Caller API generation.
    pub api_generation: U64s,
    /// Caller accepted revision.
    pub accepted_revision: U64s,
    /// Exact registered action identity.
    pub action: String,
    /// Actual received NATS subject.
    pub subject: String,
    /// Actual received reply destination.
    pub reply_subject: Option<String>,
    /// Canonical request ULID.
    pub request_id: String,
    /// Signed Unix-second issue time.
    pub issued_at: i64,
}

/// Compact proof of the immutable request transcript.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionRequestProof(String);

impl SessionRequestProof {
    /// Validate canonical signature encoding before accepting a header.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        decode_base64url::<64>(&value)?;
        Ok(Self(value))
    }
    /// Canonical base64url header value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Hash the exact payload once and construct a domain-separated request digest.
pub fn session_request_signing_digest(
    request: &SessionRequest,
    raw_payload: &[u8],
) -> Result<[u8; 32], ProtocolError> {
    crate::validate_api_id(&request.api_id)?;
    validate_text(&request.action, &["action"])?;
    validate_subject(&request.subject)?;
    if let Some(reply) = &request.reply_subject {
        validate_subject(reply)?;
    }
    validate_id(&request.request_id, "requestId")?;
    if request.api_generation.get() == 0
        || request.accepted_revision.get() == 0
        || request.issued_at < 0
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [],
            "request counters/time must be valid",
        ));
    }
    let authority_digest = decode_base64url::<32>(&request.authority_digest)?;
    let payload_hash: [u8; 32] = Sha256::digest(raw_payload).into();
    let generation = request.api_generation.get().to_be_bytes();
    let revision = request.accepted_revision.get().to_be_bytes();
    let time = request.issued_at.to_be_bytes();
    let mut bytes = Vec::new();
    for component in [
        MESSAGE_REQUEST_DOMAIN_V1.as_bytes(),
        authority_digest.as_slice(),
        request.api_id.as_bytes(),
        generation.as_slice(),
        revision.as_slice(),
        request.action.as_bytes(),
        request.subject.as_bytes(),
        request.reply_subject.as_deref().unwrap_or("").as_bytes(),
        request.request_id.as_bytes(),
        time.as_slice(),
        payload_hash.as_slice(),
    ] {
        push_component(&mut bytes, component)?;
    }
    Ok(Sha256::digest(bytes).into())
}

/// Sign exact target-format request bytes with the ephemeral runtime key.
pub fn sign_session_request(
    request: &SessionRequest,
    raw_payload: &[u8],
    key: &SigningKey,
) -> Result<SessionRequestProof, ProtocolError> {
    let digest = session_request_signing_digest(request, raw_payload)?;
    Ok(SessionRequestProof(
        URL_SAFE_NO_PAD.encode(key.sign(&digest).to_bytes()),
    ))
}

/// Pure request authentication input; payload hashes from headers are not inputs.
#[derive(Debug)]
pub struct SessionRequestVerificationInput<'a> {
    /// Previously signature-checked authority.
    pub authority: &'a VerifiedSessionAuthority,
    /// Actual route/header identity to authenticate.
    pub request: &'a SessionRequest,
    /// Exact received bytes retained for codec dispatch.
    pub raw_payload: &'a [u8],
    /// Received proof.
    pub proof: &'a SessionRequestProof,
    /// Current bounded policy.
    pub policy: &'a SessionAuthorityVerificationPolicy,
    /// Sticky known revocation of this logical session.
    pub known_revoked: bool,
}

/// Authenticated request binding, before dispatcher capability/provider checks.
#[derive(Debug)]
pub struct VerifiedSessionRequest<'a> {
    authority: &'a VerifiedSessionAuthority,
    request: &'a SessionRequest,
    raw_payload: &'a [u8],
    digest: [u8; 32],
}

impl<'a> VerifiedSessionRequest<'a> {
    /// Project the authenticated caller without granting action authorization.
    pub fn caller(&self) -> AuthenticatedCaller {
        let authority = self.authority.authority();
        AuthenticatedCaller {
            authority_digest: self.authority.digest().to_owned(),
            principal_id: authority.principal_id.clone(),
            principal_kind: authority.principal_kind,
            binding: authority.binding.clone(),
            authorization_session_id: authority.authorization_session_id.clone(),
            runtime_id: authority.runtime_id.clone(),
            session_public_key: authority.session_public_key.clone(),
            inbox_prefix: authority.inbox_prefix.clone(),
            login_session_id: authority.login_session_id.clone(),
            oauth_grant_id: authority.oauth_grant_id.clone(),
            capabilities: authority.capabilities.clone(),
            apis: authority.apis.clone(),
            platform_privileges: authority.platform_privileges.clone(),
        }
    }

    /// Signature-checked caller authority.
    pub fn authority(&self) -> &'a VerifiedSessionAuthority {
        self.authority
    }
    /// Exact opening request identity for response binding.
    pub fn request(&self) -> &'a SessionRequest {
        self.request
    }
    /// Exact verified payload, not a reconstructed JSON value.
    pub fn raw_payload(&self) -> &'a [u8] {
        self.raw_payload
    }
    /// Canonical opening transcript digest for a provider response.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Authenticate the actual request and captured API stamp without a replay cache.
///
/// A dispatcher must additionally apply its verified catalog capability and
/// action/membership introduction checks before producing a verified caller.
pub fn verify_session_request(
    input: SessionRequestVerificationInput<'_>,
) -> Result<VerifiedSessionRequest<'_>, ProtocolError> {
    input
        .authority
        .assert_current(input.policy, input.known_revoked)?;
    let body = input.authority.authority();
    let request = input.request;
    if request.authority_digest != input.authority.digest() {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidRequestProof,
            [],
            "request authority mismatch",
        ));
    }
    if !body.apis.iter().any(|api| {
        api.api_id == request.api_id
            && api.generation == request.api_generation
            && api.accepted_revision == request.accepted_revision
    }) {
        return Err(authorization_error(
            AuthorizationErrorCode::PermissionDenied,
            [],
            "request API stamp is not in the authority",
        ));
    }
    if request.issued_at.abs_diff(input.policy.now_unix_seconds)
        > u64::from(input.policy.allowed_clock_skew_seconds)
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ProofIatOutOfRange,
            [],
            "request clock skew exceeded",
        ));
    }
    if let Some(reply) = &request.reply_subject {
        if !reply.starts_with(&format!("{}.", body.inbox_prefix)) {
            return Err(authorization_error(
                AuthorizationErrorCode::ReplySubjectMismatch,
                [],
                "reply outside authenticated caller inbox",
            ));
        }
    }
    let digest = session_request_signing_digest(request, input.raw_payload)?;
    let signature = Signature::from_bytes(&decode_base64url(input.proof.as_str())?);
    input
        .authority
        .session_key
        .verify_strict(&digest, &signature)
        .map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidRequestProof,
                [],
                "request signature mismatch",
            )
        })?;
    Ok(VerifiedSessionRequest {
        authority: input.authority,
        request,
        raw_payload: input.raw_payload,
        digest,
    })
}

/// Exact event publication identity, independent of the consumer's read time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionEvent {
    /// Immutable publisher authority content address.
    pub authority_digest: String,
    /// Actual event API identity.
    pub api_id: String,
    /// Publisher API generation.
    pub api_generation: U64s,
    /// Publisher accepted revision.
    pub accepted_revision: U64s,
    /// Exact generated event identity, never inferred from a dotted subject.
    pub action: String,
    /// Actual publication subject.
    pub subject: String,
    /// Canonical event ULID.
    pub event_id: String,
    /// Signed RFC 3339 publisher timestamp.
    pub event_time: String,
}

/// Compact signature of the event identity and exact payload bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionEventProof(String);

impl SessionEventProof {
    /// Validate canonical signature encoding before accepting a header.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        decode_base64url::<64>(&value)?;
        Ok(Self(value))
    }

    /// Canonical base64url proof header.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Trusted original JetStream metadata, not a sender-supplied timestamp.
///
/// The projector supplies `Message::info()` values. Other historical readers
/// must receive those preserved original values through a trusted boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OriginalEventPublication {
    /// Original server-assigned publication timestamp in RFC 3339 form.
    pub published_at: String,
    /// Original nonzero stream sequence, not a redelivery sequence.
    pub stream_sequence: U64s,
}

/// Construct the domain-separated event transcript from exact payload bytes.
pub fn session_event_signing_digest(
    event: &SessionEvent,
    raw_payload: &[u8],
) -> Result<[u8; 32], ProtocolError> {
    crate::validate_api_id(&event.api_id)?;
    validate_text(&event.action, &["action"])?;
    validate_subject(&event.subject)?;
    validate_id(&event.event_id, "eventId")?;
    validate_text(&event.event_time, &["eventTime"])?;
    if event.api_generation.get() == 0 || event.accepted_revision.get() == 0 {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [],
            "event API counters must be nonzero",
        ));
    }
    let authority_digest = decode_base64url::<32>(&event.authority_digest)?;
    let payload_hash: [u8; 32] = Sha256::digest(raw_payload).into();
    let generation = event.api_generation.get().to_be_bytes();
    let revision = event.accepted_revision.get().to_be_bytes();
    let mut bytes = Vec::new();
    for component in [
        MESSAGE_EVENT_DOMAIN_V1.as_bytes(),
        authority_digest.as_slice(),
        event.api_id.as_bytes(),
        generation.as_slice(),
        revision.as_slice(),
        event.action.as_bytes(),
        event.subject.as_bytes(),
        event.event_id.as_bytes(),
        event.event_time.as_bytes(),
        payload_hash.as_slice(),
    ] {
        push_component(&mut bytes, component)?;
    }
    Ok(Sha256::digest(bytes).into())
}

/// Sign an event with its originating ephemeral runtime key.
pub fn sign_session_event(
    event: &SessionEvent,
    raw_payload: &[u8],
    key: &SigningKey,
) -> Result<SessionEventProof, ProtocolError> {
    let digest = session_event_signing_digest(event, raw_payload)?;
    Ok(SessionEventProof(
        URL_SAFE_NO_PAD.encode(key.sign(&digest).to_bytes()),
    ))
}

/// Pure event-authentication inputs; read authority remains a separate check.
#[derive(Debug)]
pub struct SessionEventVerificationInput<'a> {
    /// Signature-checked live or historical publisher authority.
    pub authority: &'a VerifiedSessionAuthority,
    /// Actual descriptor, subject, and signed event identity.
    pub event: &'a SessionEvent,
    /// Exact received payload bytes.
    pub raw_payload: &'a [u8],
    /// Received event proof.
    pub proof: &'a SessionEventProof,
    /// Current verification clock and limits.
    pub policy: &'a SessionAuthorityVerificationPolicy,
    /// Original trusted broker metadata; mandatory for historical verification.
    pub original_publication: Option<&'a OriginalEventPublication>,
    /// Sticky live logical-session revocation.
    pub known_revoked: bool,
}

/// Authenticated event and original publication evidence, before read policy.
#[derive(Debug)]
pub struct VerifiedSessionEvent<'a> {
    authority: &'a VerifiedSessionAuthority,
    event: &'a SessionEvent,
    raw_payload: &'a [u8],
    original_publication: Option<&'a OriginalEventPublication>,
    digest: [u8; 32],
}

impl<'a> VerifiedSessionEvent<'a> {
    /// Authenticated publisher authority, not a message-header assertion.
    pub fn authority(&self) -> &'a VerifiedSessionAuthority {
        self.authority
    }
    /// Verified event identity.
    pub fn event(&self) -> &'a SessionEvent {
        self.event
    }
    /// Exact authenticated payload bytes.
    pub fn raw_payload(&self) -> &'a [u8] {
        self.raw_payload
    }
    /// Trusted original publication evidence used for historical eligibility.
    pub fn original_publication(&self) -> Option<&'a OriginalEventPublication> {
        self.original_publication
    }
    /// Canonical signed transcript digest.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Authenticate publication time as well as signed time for retained events.
///
/// Catalog checks for publish authority and current consumer read authority
/// remain mandatory at the dispatcher. Historical success never authorizes a
/// current request or treats a consumer's read time as publication evidence.
pub fn verify_session_event(
    input: SessionEventVerificationInput<'_>,
) -> Result<VerifiedSessionEvent<'_>, ProtocolError> {
    let body = input.authority.authority();
    let event = input.event;
    let reject = || {
        authorization_error(
            AuthorizationErrorCode::InvalidEventProof,
            [],
            "event identity or publication evidence rejected",
        )
    };
    if input.authority.purpose == SessionAuthorityPurpose::Live {
        input
            .authority
            .assert_current(input.policy, input.known_revoked)?;
    } else if input.original_publication.is_none()
        || (input.known_revoked && input.authority.revocation_cutoff.is_none())
    {
        return Err(reject());
    }
    if event.authority_digest != input.authority.digest()
        || !body.apis.iter().any(|api| {
            api.api_id == event.api_id
                && api.generation == event.api_generation
                && api.accepted_revision == event.accepted_revision
        })
    {
        return Err(reject());
    }
    let event_time = time::OffsetDateTime::parse(
        &event.event_time,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|_| reject())?;
    let not_before = i128::from(body.not_before) * 1_000_000_000;
    let expires_at = i128::from(body.expires_at) * 1_000_000_000;
    if event_time.unix_timestamp_nanos() < not_before
        || event_time.unix_timestamp_nanos() >= expires_at
        || input.authority.revocation_cutoff.is_some_and(|cutoff| {
            event_time.unix_timestamp_nanos() >= i128::from(cutoff) * 1_000_000_000
        })
        || (input.authority.purpose == SessionAuthorityPurpose::Live
            && event_time
                .unix_timestamp()
                .abs_diff(input.policy.now_unix_seconds)
                > u64::from(input.policy.allowed_clock_skew_seconds))
    {
        return Err(reject());
    }
    if let Some(publication) = input.original_publication {
        let published_at = time::OffsetDateTime::parse(
            &publication.published_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| reject())?
        .unix_timestamp_nanos();
        if publication.stream_sequence.get() == 0
            || published_at < not_before
            || published_at >= expires_at
            || input
                .authority
                .revocation_cutoff
                .is_some_and(|cutoff| published_at >= i128::from(cutoff) * 1_000_000_000)
        {
            return Err(reject());
        }
    }
    let digest = session_event_signing_digest(event, input.raw_payload)?;
    let signature = Signature::from_bytes(&decode_base64url(input.proof.as_str())?);
    input
        .authority
        .session_key
        .verify_strict(&digest, &signature)
        .map_err(|_| reject())?;
    Ok(VerifiedSessionEvent {
        authority: input.authority,
        event,
        raw_payload: input.raw_payload,
        original_publication: input.original_publication,
        digest,
    })
}
