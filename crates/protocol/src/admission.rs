//! Short-lived OAuth-to-NATS credentials. These authenticate a runtime key;
//! current application authority is compiled by Auth at connection admission.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::authorization::{authorization_error, signed_json_digest};
use crate::{AuthorityIssuerKey, AuthorityIssuerState, AuthorizationErrorCode, ProtocolError};

/// Signature domain for the OAuth token-response admission extension.
pub const OAUTH_ADMISSION_FORMAT_V1: &str = "trellis.oauth-admission.v1";

/// Provisioned identities authenticate CONNECT independently of OAuth and HTTP.
pub const PROVISIONED_ADMISSION_FORMAT_V1: &str = "trellis.provisioned-admission.v1";

/// One generated API implementation bound by a provisioned CONNECT proof.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionedImplementation {
    /// Exact version-qualified API identity.
    pub api_id: String,
    /// Accepted implementation digest from the deployed participant.
    pub implementation_digest: String,
}

/// Direct service/device admission, signed by the installed identity key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionedAdmission {
    /// Exact protocol format and signature domain.
    pub format: String,
    /// Selected Trellis installation.
    pub trellis_instance_id: String,
    /// Selected application NATS account.
    pub audience_nats_account: String,
    /// Installed identity-key identifier, never asserted public-key bytes.
    pub credential_id: String,
    /// Exact provisioned service/device binding.
    pub binding: crate::SessionBinding,
    /// Implementations in ascending API-ID order, without duplicate IDs.
    pub implementations: Vec<ProvisionedImplementation>,
    /// Fresh process runtime nonce.
    pub runtime_id: String,
    /// Fresh ephemeral user NKey that signs the broker's CONNECT nonce.
    pub ephemeral_nkey: String,
    /// Unix issuance time.
    pub issued_at: i64,
    /// Short Unix expiry, at most sixty seconds after issuance.
    pub expires_at: i64,
}

/// Signed direct CONNECT credential; never a reusable bearer credential.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedProvisionedAdmission {
    /// Authenticated CONNECT binding.
    pub credential: ProvisionedAdmission,
    /// Canonical Ed25519 signature by the installed identity key.
    pub signature: String,
}

impl ProvisionedAdmission {
    fn digest(&self) -> Result<[u8; 32], ProtocolError> {
        let (deployment, instance, participant) = match &self.binding {
            crate::SessionBinding::Service {
                deployment_id,
                instance_id,
                participant_id,
            }
            | crate::SessionBinding::Device {
                deployment_id,
                instance_id,
                participant_id,
            } => (deployment_id, instance_id, participant_id),
            _ => return Err(invalid()),
        };
        if self.format != PROVISIONED_ADMISSION_FORMAT_V1
            || self.audience_nats_account.is_empty()
            || self.audience_nats_account.len() > 256
            || participant.is_empty()
            || participant.len() > 2048
            || self.issued_at < 0
            || self.expires_at <= self.issued_at
            || self.expires_at.saturating_sub(self.issued_at) > 60
            || self.implementations.len() > 256
            || self
                .implementations
                .windows(2)
                .any(|pair| pair[0].api_id >= pair[1].api_id)
            || self.implementations.iter().any(|implementation| {
                implementation.api_id.is_empty()
                    || implementation.api_id.len() > 2048
                    || implementation.implementation_digest.is_empty()
                    || implementation.implementation_digest.len() > 256
            })
        {
            return Err(invalid());
        }
        for id in [&self.trellis_instance_id, deployment, instance] {
            if ulid::Ulid::from_string(id)
                .map_err(|_| invalid())?
                .to_string()
                != *id
            {
                return Err(invalid());
            }
        }
        canonical::<32>(&self.credential_id)?;
        canonical::<16>(&self.runtime_id)?;
        let key = nkeys::KeyPair::from_public_key(&self.ephemeral_nkey).map_err(|_| invalid())?;
        if key.key_pair_type() != nkeys::KeyPairType::User
            || key.public_key() != self.ephemeral_nkey
        {
            return Err(invalid());
        }
        signed_json_digest(PROVISIONED_ADMISSION_FORMAT_V1, self)
    }
}

/// Sign a short-lived CONNECT proof with the provisioned identity key.
pub fn sign_provisioned_admission(
    credential: ProvisionedAdmission,
    identity_key: &SigningKey,
) -> Result<SignedProvisionedAdmission, ProtocolError> {
    let digest = credential.digest()?;
    Ok(SignedProvisionedAdmission {
        credential,
        signature: URL_SAFE_NO_PAD.encode(identity_key.sign(&digest).to_bytes()),
    })
}

/// Verify installed identity possession, selected scope, runtime key and time.
pub fn verify_provisioned_admission(
    bytes: &[u8],
    identity_public_key: &str,
    instance: &str,
    account: &str,
    ephemeral_nkey: &str,
    now: i64,
) -> Result<SignedProvisionedAdmission, ProtocolError> {
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        return Err(invalid());
    }
    let signed: SignedProvisionedAdmission = serde_json::from_slice(bytes)?;
    let body = &signed.credential;
    let digest = body.digest()?;
    if body.trellis_instance_id != instance
        || body.audience_nats_account != account
        || body.ephemeral_nkey != ephemeral_nkey
        || body.issued_at > now.saturating_add(5)
        || body.expires_at <= now
    {
        return Err(invalid());
    }
    VerifyingKey::from_bytes(&canonical::<32>(identity_public_key)?)
        .map_err(|_| invalid())?
        .verify_strict(
            &digest,
            &Signature::from_bytes(&canonical::<64>(&signed.signature)?),
        )
        .map_err(|_| invalid())?;
    Ok(signed)
}

/// Private inbox namespace derived from the ephemeral session key.
pub fn admission_inbox_prefix(session_public_key: &str) -> Result<String, ProtocolError> {
    let key = canonical::<32>(session_public_key)?;
    Ok(format!(
        "_INBOX.trellis.{}",
        URL_SAFE_NO_PAD.encode(Sha256::digest(key))
    ))
}

/// Maximum signed own-context reply, including exact context and public registry.
pub const MAX_ADMISSION_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Ephemeral-key proof for the already admitted runtime's first context lookup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionGet {
    /// Selected installation, authenticated again in the issuer reply.
    pub trellis_instance_id: String,
    /// Exact runtime nonce certified during admission.
    pub runtime_id: String,
    /// Canonical ephemeral Ed25519 public key, not a durable identity assertion.
    pub session_public_key: String,
    /// Fresh canonical random 128-bit reply-correlation nonce.
    pub request_nonce: String,
    /// Unix signing time in seconds.
    pub issued_at: i64,
}

/// Bound request proof. Actual NATS subject and reply are transcript inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedAdmissionGet {
    /// Exact requested runtime and nonce.
    pub request: AdmissionGet,
    /// Canonical Ed25519 signature over the domain-separated request transcript.
    pub signature: String,
}

/// Issuer-signed context installation response; no responder identity is trusted.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionContext {
    /// Exact proof domain and wire format.
    pub format: String,
    /// Pinned issuer lookup identity.
    pub issuer_key_id: String,
    /// Selected installation.
    pub trellis_instance_id: String,
    /// Selected public broker account.
    pub audience_nats_account: String,
    /// Ephemeral runtime owning the admitted attachment.
    pub runtime_id: String,
    /// Authenticated request nonce.
    pub request_nonce: String,
    /// Actual request subject.
    pub request_subject: String,
    /// Actual request reply inbox.
    pub reply_subject: String,
    /// Exact immutable UTF-8 signed context bytes, not a reconstructed projection.
    pub session_authority: String,
    /// Public signed catalog/provider/issuer material needed for installation.
    pub registry: serde_json::Value,
    /// Unix issuance seconds.
    pub issued_at: i64,
    /// Exclusive bounded installation-response deadline.
    pub expires_at: i64,
}

/// Signed own-context reply, verified before ordinary application I/O can resume.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedAdmissionContext {
    /// Exact reply contents.
    pub response: AdmissionContext,
    /// Canonical issuer Ed25519 signature.
    pub signature: String,
}

/// Derive the only own-introspection subject allowed to this ephemeral key.
pub fn admission_get_subject(session_public_key: &str) -> Result<String, ProtocolError> {
    let key = canonical::<32>(session_public_key)?;
    Ok(format!(
        "trellis.auth.admission.{}.get",
        URL_SAFE_NO_PAD.encode(Sha256::digest(key))
    ))
}

fn admission_get_digest(
    request: &AdmissionGet,
    subject: &str,
    reply: &str,
) -> Result<[u8; 32], ProtocolError> {
    canonical::<16>(&request.runtime_id)?;
    canonical::<16>(&request.request_nonce)?;
    if request.trellis_instance_id.is_empty()
        || request.trellis_instance_id.len() > 256
        || request.issued_at < 0
        || admission_get_subject(&request.session_public_key)? != subject
        || !reply.starts_with(&format!(
            "{}.",
            admission_inbox_prefix(&request.session_public_key)?
        ))
        || reply.is_empty()
        || reply.len() > 512
        || reply.split('.').any(|token| {
            token.is_empty() || token.contains(['*', '>']) || token.chars().any(char::is_whitespace)
        })
    {
        return Err(invalid());
    }
    signed_json_digest(
        crate::ADMISSION_GET_DOMAIN_V1,
        &serde_json::to_value((subject, reply, request)).map_err(|_| invalid())?,
    )
}

/// Sign a first-context request with the same key that proved the NATS nonce.
pub fn sign_admission_get(
    request: AdmissionGet,
    subject: &str,
    reply: &str,
    signer: &SigningKey,
) -> Result<SignedAdmissionGet, ProtocolError> {
    if URL_SAFE_NO_PAD.encode(signer.verifying_key().as_bytes()) != request.session_public_key {
        return Err(invalid());
    }
    let digest = admission_get_digest(&request, subject, reply)?;
    Ok(SignedAdmissionGet {
        request,
        signature: URL_SAFE_NO_PAD.encode(signer.sign(&digest).to_bytes()),
    })
}

/// Verify actual subject/reply, selected runtime, possession, and a 30-second window.
pub fn verify_admission_get(
    request: &SignedAdmissionGet,
    subject: &str,
    reply: &str,
    instance: &str,
    now: i64,
) -> Result<(), ProtocolError> {
    if request.request.trellis_instance_id != instance
        || request.request.issued_at > now.saturating_add(5)
        || now.saturating_sub(request.request.issued_at) > 30
    {
        return Err(invalid());
    }
    let digest = admission_get_digest(&request.request, subject, reply)?;
    let key = VerifyingKey::from_bytes(&canonical::<32>(&request.request.session_public_key)?)
        .map_err(|_| invalid())?;
    key.verify_strict(
        &digest,
        &Signature::from_bytes(&canonical::<64>(&request.signature)?),
    )
    .map_err(|_| invalid())
}

fn admission_context_digest(response: &AdmissionContext) -> Result<[u8; 32], ProtocolError> {
    canonical::<16>(&response.runtime_id)?;
    canonical::<16>(&response.request_nonce)?;
    if response.format != crate::ADMISSION_GET_DOMAIN_V1
        || response.issuer_key_id.is_empty()
        || response.trellis_instance_id.is_empty()
        || response.audience_nats_account.is_empty()
        || response.issued_at < 0
        || response.expires_at <= response.issued_at
        || response.expires_at - response.issued_at > 30
        || response.session_authority.is_empty()
        || response.session_authority.len() > 64 * 1024
    {
        return Err(invalid());
    }
    let value = serde_json::to_value(response).map_err(|_| invalid())?;
    if serde_json::to_vec(&value).map_err(|_| invalid())?.len() > MAX_ADMISSION_RESPONSE_BYTES {
        return Err(invalid());
    }
    signed_json_digest(crate::ADMISSION_GET_DOMAIN_V1, &value)
}

/// Sign exact committed context bytes and public registry for a proven request.
pub fn sign_admission_context(
    response: AdmissionContext,
    signer: &SigningKey,
) -> Result<SignedAdmissionContext, ProtocolError> {
    if response.issuer_key_id
        != URL_SAFE_NO_PAD.encode(Sha256::digest(signer.verifying_key().as_bytes()))
    {
        return Err(invalid());
    }
    let digest = admission_context_digest(&response)?;
    Ok(SignedAdmissionContext {
        response,
        signature: URL_SAFE_NO_PAD.encode(signer.sign(&digest).to_bytes()),
    })
}

/// Verify an installation reply against the pinned issuer and original request.
pub fn verify_admission_context(
    bytes: &[u8],
    issuer: &AuthorityIssuerKey,
    request: &AdmissionGet,
    subject: &str,
    reply: &str,
    account: &str,
    now: i64,
) -> Result<SignedAdmissionContext, ProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_ADMISSION_RESPONSE_BYTES {
        return Err(invalid());
    }
    let signed: SignedAdmissionContext = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let response = &signed.response;
    if issuer.state != AuthorityIssuerState::Active
        || response.issuer_key_id != issuer.key_id
        || response.trellis_instance_id != request.trellis_instance_id
        || response.audience_nats_account != account
        || response.runtime_id != request.runtime_id
        || response.request_nonce != request.request_nonce
        || response.request_subject != subject
        || response.reply_subject != reply
        || response.issued_at > now.saturating_add(5)
        || response.expires_at <= now
    {
        return Err(invalid());
    }
    let digest = admission_context_digest(response)?;
    issuer
        .verifying_key()?
        .verify_strict(
            &digest,
            &Signature::from_bytes(&canonical::<64>(&signed.signature)?),
        )
        .map_err(|_| invalid())?;
    let context = crate::parse_session_authority(response.session_authority.as_bytes(), 64 * 1024)?;
    if context.unsigned.runtime_id != request.runtime_id
        || context.unsigned.session_public_key != request.session_public_key
        || context.unsigned.trellis_instance_id != request.trellis_instance_id
        || context.unsigned.audience_nats_account != account
        || response.expires_at > context.unsigned.expires_at
    {
        return Err(invalid());
    }
    Ok(signed)
}

/// User app binding certified by Auth, never an asserted NATS permission set.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "bindingKind", rename_all = "camelCase", deny_unknown_fields)]
pub enum OAuthAdmissionBinding {
    /// Browser State and consent identity at the approved callback origin.
    Browser {
        /// Exact approved app origin.
        #[serde(rename = "browserOrigin")]
        browser_origin: String,
    },
    /// Native consent identity bound to the durable DPoP key.
    Native {
        /// RFC 7638 P-256 key thumbprint.
        #[serde(rename = "nativeDpopJkt")]
        native_dpop_jkt: String,
    },
}

/// Immutable Auth-certified association of an OAuth grant and an ephemeral key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuthAdmission {
    /// Exact accepted wire format and signature domain.
    pub format: String,
    /// Pinned issuer lookup identity, not a trust anchor.
    pub issuer_key_id: String,
    /// Selected installation.
    pub trellis_instance_id: String,
    /// Selected broker account.
    pub audience_nats_account: String,
    /// Authoritative OAuth authorization.
    pub oauth_grant_id: String,
    /// Engine-owned rotating refresh family.
    pub refresh_family_id: String,
    /// Authoritative user login.
    pub login_session_id: String,
    /// Stable user identity.
    pub principal_id: String,
    /// Approved client identity.
    pub client_id: String,
    /// Approved browser or native binding.
    pub binding: OAuthAdmissionBinding,
    /// RFC 7638 durable P-256 key thumbprint.
    pub persistent_dpop_jkt: String,
    /// Canonical 128-bit runtime nonce.
    pub runtime_id: String,
    /// Canonical ephemeral User NKey, separately proven against the NATS nonce.
    pub ephemeral_nkey: String,
    /// Unix issuance seconds.
    pub issued_at: i64,
    /// Inclusive Unix validity start.
    pub not_before: i64,
    /// Exclusive Unix validity end.
    pub expires_at: i64,
}

/// Signed OAuth admission credential, kept in process memory only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedOAuthAdmission {
    /// Authenticated fields.
    pub credential: OAuthAdmission,
    /// Canonical base64url Ed25519 signature.
    pub signature: String,
}

fn invalid() -> ProtocolError {
    authorization_error(
        AuthorizationErrorCode::InvalidFormat,
        [],
        "invalid OAuth admission",
    )
}

fn canonical<const N: usize>(value: &str) -> Result<[u8; N], ProtocolError> {
    let bytes: [u8; N] = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())?;
    if URL_SAFE_NO_PAD.encode(bytes) != value {
        return Err(invalid());
    }
    Ok(bytes)
}

impl OAuthAdmission {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.format != OAUTH_ADMISSION_FORMAT_V1
            || self.client_id.is_empty()
            || self.client_id.len() > 2048
            || self.refresh_family_id.is_empty()
            || self.refresh_family_id.len() > 256
            || self.audience_nats_account.is_empty()
            || self.issued_at < 0
            || self.not_before < self.issued_at
            || self.expires_at <= self.not_before
        {
            return Err(invalid());
        }
        for id in [
            &self.trellis_instance_id,
            &self.oauth_grant_id,
            &self.login_session_id,
            &self.principal_id,
        ] {
            if ulid::Ulid::from_string(id)
                .map_err(|_| invalid())?
                .to_string()
                != *id
            {
                return Err(invalid());
            }
        }
        canonical::<32>(&self.issuer_key_id)?;
        canonical::<32>(&self.persistent_dpop_jkt)?;
        canonical::<16>(&self.runtime_id)?;
        let key = nkeys::KeyPair::from_public_key(&self.ephemeral_nkey).map_err(|_| invalid())?;
        if key.key_pair_type() != nkeys::KeyPairType::User
            || key.public_key() != self.ephemeral_nkey
        {
            return Err(invalid());
        }
        match &self.binding {
            OAuthAdmissionBinding::Native { native_dpop_jkt }
                if native_dpop_jkt == &self.persistent_dpop_jkt => {}
            OAuthAdmissionBinding::Browser { browser_origin } => {
                let origin = url::Url::parse(browser_origin).map_err(|_| invalid())?;
                let loopback =
                    matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
                if origin.origin().ascii_serialization() != *browser_origin
                    || (origin.scheme() != "https" && !(loopback && origin.scheme() == "http"))
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
}

/// Sign a grant-to-runtime association using the current Auth issuer.
pub fn sign_oauth_admission(
    credential: OAuthAdmission,
    issuer: &SigningKey,
) -> Result<SignedOAuthAdmission, ProtocolError> {
    credential.validate()?;
    let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(issuer.verifying_key().as_bytes()));
    if credential.issuer_key_id != digest {
        return Err(invalid());
    }
    let signature = URL_SAFE_NO_PAD.encode(
        issuer
            .sign(&signed_json_digest(OAUTH_ADMISSION_FORMAT_V1, &credential)?)
            .to_bytes(),
    );
    Ok(SignedOAuthAdmission {
        credential,
        signature,
    })
}

/// Decode and verify an admission credential against selected, pinned trust.
/// Current login/grant/family policy must still be checked by Auth afterwards.
pub fn verify_oauth_admission(
    bytes: &[u8],
    issuer: &AuthorityIssuerKey,
    instance: &str,
    account: &str,
    ephemeral_nkey: &str,
    now: i64,
    maximum_lifetime_seconds: u32,
) -> Result<SignedOAuthAdmission, ProtocolError> {
    if bytes.len() > 16 * 1024 || maximum_lifetime_seconds == 0 {
        return Err(invalid());
    }
    let signed: SignedOAuthAdmission = serde_json::from_slice(bytes)?;
    let body = &signed.credential;
    body.validate()?;
    if issuer.state != AuthorityIssuerState::Active
        || body.issuer_key_id != issuer.key_id
        || body.trellis_instance_id != instance
        || body.audience_nats_account != account
        || body.ephemeral_nkey != ephemeral_nkey
        || now < body.not_before
        || now >= body.expires_at
        || body.expires_at - body.not_before > i64::from(maximum_lifetime_seconds)
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ScopeMismatch,
            [],
            "OAuth admission outside selected scope or validity",
        ));
    }
    issuer
        .verifying_key()?
        .verify_strict(
            &signed_json_digest(OAUTH_ADMISSION_FORMAT_V1, body)?,
            &Signature::from_bytes(&canonical::<64>(&signed.signature)?),
        )
        .map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidSignature,
                [],
                "invalid admission signature",
            )
        })?;
    Ok(signed)
}
