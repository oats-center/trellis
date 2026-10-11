//! Provider responses bind exact bytes to an authenticated opening request.

use crate::authorization::{
    authorization_error, decode_base64url, push_component, validate_id, validate_subject,
    validate_text,
};
use crate::{
    AuthorityIssuerKey, AuthorizationErrorCode, CatalogActionIdentity, ProtocolError,
    SessionAuthorityVerificationPolicy, SessionBinding, SessionRequest, SignedProviderCertificate,
    U64s, VerifiedSessionAuthority, MESSAGE_RESPONSE_DOMAIN_V1,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Authenticated result classification, independent of broker status headers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionResponseKind {
    /// A provider's successful application result.
    Success,
    /// A provider's declared or authenticated framework error.
    Error,
}

/// Exact provider response identity and the opening request it answers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SessionResponse {
    /// Pinned installation identity.
    pub trellis_instance_id: String,
    /// Pinned public NATS account.
    pub audience_nats_account: String,
    /// Immutable provider authority digest.
    pub provider_authority_digest: String,
    /// Auth-issued provider action certificate digest.
    pub provider_certificate_digest: String,
    /// Requested version-qualified API.
    pub api_id: String,
    /// Requested API generation.
    pub api_generation: U64s,
    /// Exact registered action.
    pub action: String,
    /// Opening caller authority digest.
    pub request_authority_digest: String,
    /// Opening request ULID.
    pub request_id: String,
    /// Canonical opening request transcript digest, not a client-supplied hash.
    pub request_proof_digest: String,
    /// Actual received response subject.
    pub subject: String,
    /// Signed success/error classification.
    pub kind: SessionResponseKind,
    /// Provider issue time in Unix seconds.
    pub issued_at: i64,
}

/// Canonically encoded Ed25519 provider response signature.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionResponseProof(String);

impl SessionResponseProof {
    /// Reject noncanonical or incorrectly sized signature headers.
    pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        decode_base64url::<64>(&value)?;
        Ok(Self(value))
    }

    /// Canonical unpadded base64url header value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Hash the exact response payload once into its domain-separated transcript.
pub fn session_response_signing_digest(
    response: &SessionResponse,
    raw_payload: &[u8],
) -> Result<[u8; 32], ProtocolError> {
    validate_text(&response.trellis_instance_id, &["trellisInstanceId"])?;
    validate_text(&response.audience_nats_account, &["audienceNatsAccount"])?;
    crate::validate_api_id(&response.api_id)?;
    validate_text(&response.action, &["action"])?;
    validate_subject(&response.subject)?;
    validate_id(&response.request_id, "requestId")?;
    if response.api_generation.get() == 0 || response.issued_at < 0 {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [],
            "invalid response generation/time",
        ));
    }
    let provider = decode_base64url::<32>(&response.provider_authority_digest)?;
    let certificate = decode_base64url::<32>(&response.provider_certificate_digest)?;
    let caller = decode_base64url::<32>(&response.request_authority_digest)?;
    let request = decode_base64url::<32>(&response.request_proof_digest)?;
    let generation = response.api_generation.get().to_be_bytes();
    let issued_at = response.issued_at.to_be_bytes();
    let payload_hash: [u8; 32] = Sha256::digest(raw_payload).into();
    let kind = match response.kind {
        SessionResponseKind::Success => "success",
        SessionResponseKind::Error => "error",
    };
    let mut transcript = Vec::new();
    for component in [
        MESSAGE_RESPONSE_DOMAIN_V1.as_bytes(),
        response.trellis_instance_id.as_bytes(),
        response.audience_nats_account.as_bytes(),
        provider.as_slice(),
        certificate.as_slice(),
        response.api_id.as_bytes(),
        generation.as_slice(),
        response.action.as_bytes(),
        caller.as_slice(),
        response.request_id.as_bytes(),
        request.as_slice(),
        response.subject.as_bytes(),
        kind.as_bytes(),
        payload_hash.as_slice(),
        issued_at.as_slice(),
    ] {
        push_component(&mut transcript, component)?;
    }
    Ok(Sha256::digest(transcript).into())
}

/// Sign response bytes using the provider runtime's ephemeral key.
///
/// Reply owners must derive the response identity from their verified opening
/// request and installed provider authority, never arbitrary request headers.
pub fn sign_session_response(
    response: &SessionResponse,
    raw_payload: &[u8],
    key: &SigningKey,
) -> Result<SessionResponseProof, ProtocolError> {
    let digest = session_response_signing_digest(response, raw_payload)?;
    Ok(SessionResponseProof(
        URL_SAFE_NO_PAD.encode(key.sign(&digest).to_bytes()),
    ))
}

/// Verification against retained request state and current certified provider rights.
#[derive(Debug)]
pub struct SessionResponseVerificationInput<'a> {
    /// Previously issuer-verified provider authority.
    pub provider: &'a VerifiedSessionAuthority,
    /// Provider action certificate resolved by its authenticated digest.
    pub certificate: &'a SignedProviderCertificate,
    /// Pinned issuer for that certificate.
    pub certificate_issuer: &'a AuthorityIssuerKey,
    /// Actual retained opening request, not response assertions.
    pub pending_request: &'a SessionRequest,
    /// Retained canonical opening request transcript digest.
    pub pending_request_digest: &'a [u8; 32],
    /// Exact route-owned action identity, including surface and direction.
    pub action: &'a CatalogActionIdentity,
    /// Auth-selected deployment when routing selects one.
    pub expected_deployment_id: Option<&'a str>,
    /// Actual transport response destination.
    pub received_subject: &'a str,
    /// Received signed response identity.
    pub response: &'a SessionResponse,
    /// Exact received bytes preserved for the application codec.
    pub raw_payload: &'a [u8],
    /// Received provider signature.
    pub proof: &'a SessionResponseProof,
    /// Current time and verification limits after any metadata resolution.
    pub policy: &'a SessionAuthorityVerificationPolicy,
    /// Sticky provider-session revocation.
    pub known_revoked: bool,
}

/// Authenticated provider result; only this boundary exposes typed result bytes.
#[derive(Debug)]
pub struct VerifiedSessionResponse<'a> {
    provider: &'a VerifiedSessionAuthority,
    response: &'a SessionResponse,
    raw_payload: &'a [u8],
}

impl<'a> VerifiedSessionResponse<'a> {
    /// Current certified provider authority.
    pub fn provider(&self) -> &'a VerifiedSessionAuthority {
        self.provider
    }
    /// Authenticated response classification and request binding.
    pub fn response(&self) -> &'a SessionResponse {
        self.response
    }
    /// Exact authenticated payload for decoding without reconstruction.
    pub fn raw_payload(&self) -> &'a [u8] {
        self.raw_payload
    }
}

/// Verify provider scope, request binding, current authority, and exact response bytes.
///
/// The opening caller need not remain live for previously accepted finite work
/// to complete. The provider must still have current signing authority. A failed
/// verification does not authenticate a result or consume a pending request.
pub fn verify_session_response(
    input: SessionResponseVerificationInput<'_>,
) -> Result<VerifiedSessionResponse<'_>, ProtocolError> {
    input
        .provider
        .assert_current(input.policy, input.known_revoked)?;
    let provider = input.provider.authority();
    let response = input.response;
    let request = input.pending_request;
    let certificate = input.certificate;
    certificate.verify(
        input.certificate_issuer,
        &provider.trellis_instance_id,
        &provider.audience_nats_account,
        input.policy.now_unix_seconds,
    )?;
    let certificate_digest = certificate.digest()?;
    let deployment_matches = match &provider.binding {
        SessionBinding::Service {
            deployment_id,
            instance_id,
            ..
        }
        | SessionBinding::Device {
            deployment_id,
            instance_id,
            ..
        } => deployment_id == &certificate.deployment_id && instance_id == &certificate.instance_id,
        SessionBinding::Browser { .. } | SessionBinding::Native { .. } => false,
    };
    if !deployment_matches
        || certificate.principal_id != provider.principal_id
        || certificate.session_public_key != provider.session_public_key
        || certificate.api_id != request.api_id
        || certificate.generation != request.api_generation
        || !certificate.implemented_actions.contains(input.action)
        || input.action.name != request.action
        || input
            .expected_deployment_id
            .is_some_and(|id| id != certificate.deployment_id)
        || !provider.provider_bindings.iter().any(|binding| {
            binding.api_id == certificate.api_id
                && binding.generation == certificate.generation
                && binding.deployment_id == certificate.deployment_id
                && binding.implementation_digest == certificate.implementation_digest
                && binding.provider_certificate_digest == certificate_digest
        })
    {
        return Err(authorization_error(
            AuthorizationErrorCode::PermissionDenied,
            [],
            "response provider is not certified for the requested action",
        ));
    }
    if response.trellis_instance_id != provider.trellis_instance_id
        || response.audience_nats_account != provider.audience_nats_account
        || response.provider_authority_digest != input.provider.digest()
        || response.provider_certificate_digest != certificate_digest
        || response.api_id != request.api_id
        || response.api_generation != request.api_generation
        || response.action != request.action
        || response.request_authority_digest != request.authority_digest
        || response.request_id != request.request_id
        || response.request_proof_digest != URL_SAFE_NO_PAD.encode(input.pending_request_digest)
        || request.reply_subject.as_deref() != Some(input.received_subject)
        || response.subject != input.received_subject
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidResponseProof,
            [],
            "response does not match the retained opening request",
        ));
    }
    if response.issued_at < provider.not_before
        || response.issued_at >= provider.expires_at
        || response.issued_at.abs_diff(input.policy.now_unix_seconds)
            > u64::from(input.policy.allowed_clock_skew_seconds)
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ProofIatOutOfRange,
            [],
            "response issue time is outside provider authority",
        ));
    }
    let digest = session_response_signing_digest(response, input.raw_payload)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&decode_base64url::<32>(
        &provider.session_public_key,
    )?)
    .map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidPublicKey,
            [],
            "invalid provider session key",
        )
    })?;
    key.verify_strict(
        &digest,
        &Signature::from_bytes(&decode_base64url::<64>(input.proof.as_str())?),
    )
    .map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidResponseProof,
            [],
            "invalid provider response signature",
        )
    })?;
    Ok(VerifiedSessionResponse {
        provider: input.provider,
        response,
        raw_payload: input.raw_payload,
    })
}
