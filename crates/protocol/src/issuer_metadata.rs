//! Issuer-authenticated public-material reads, without trusting a NATS responder.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::authorization::{
    authorization_error, decode_base64url, signed_json_digest, validate_id, validate_subject,
};
use crate::{
    AuthorityIssuerKey, AuthorityIssuerState, AuthorizationErrorCode, ProtocolError,
    ISSUER_METADATA_DOMAIN_V1, MAX_ADMISSION_RESPONSE_BYTES,
};

/// Read-only issuer metadata bootstrap route for already admitted connections.
/// Delivery is not trust: callers verify the retained signed exchange.
pub const ISSUER_METADATA_SUBJECT: &str = "trellis.auth.metadata.get";

/// Exact identifiers requested from the public Auth registry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IssuerMetadataQuery {
    /// Resolve immutable context bytes and their public verification material.
    AuthorityResolve {
        /// Content digest of the requested immutable context.
        #[serde(rename = "contextDigest")]
        context_digest: String,
    },
    /// Obtain current session state and any signed historical revocation cutoff.
    AuthorityStatus {
        /// Logical session identity, not a context digest.
        #[serde(rename = "authorizationSessionId")]
        authorization_session_id: String,
    },
    /// Fetch rotations linking a configured trust anchor to a selected issuer.
    IssuerChain {
        /// Previously trusted issuer key digest.
        #[serde(rename = "pinnedKeyId")]
        pinned_key_id: String,
        /// Requested issuer key digest; it is not itself a trust anchor.
        #[serde(rename = "targetKeyId")]
        target_key_id: String,
    },
}

/// Retained request binding for one bounded metadata exchange.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssuerMetadataRequest {
    /// Configured installation, not responder-selected scope.
    pub trellis_instance_id: String,
    /// Configured target broker account.
    pub audience_nats_account: String,
    /// Fresh canonical random 128-bit correlation nonce.
    pub request_nonce: String,
    /// Exact public objects requested.
    pub query: IssuerMetadataQuery,
}

/// Authenticated read outcome. Unavailability never proves absence or revocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum IssuerMetadataResult {
    /// Exact UTF-8 generated response bytes, decoded only after verification.
    Value {
        /// Public material only; no tokens, credentials or private keys.
        payload: String,
    },
    /// Auth confirms the exact requested object is absent at issuance time.
    Missing,
    /// Auth could not resolve it; previously learned evidence remains unchanged.
    Unavailable,
}

/// Public issuer statement bound to the retained exchange, including absence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssuerMetadata {
    /// Accepted protocol format and signature domain.
    pub format: String,
    /// Lookup identity of an already authenticated issuer key.
    pub issuer_key_id: String,
    /// Exact request identity and configured trust scope.
    pub request: IssuerMetadataRequest,
    /// Actual NATS request subject, not a routing field supplied by the body.
    pub request_subject: String,
    /// Actual received reply destination.
    pub reply_subject: String,
    /// Authenticated data or read failure, never an application service assertion.
    pub result: IssuerMetadataResult,
    /// Unix issuance seconds.
    pub issued_at: i64,
    /// Exclusive, short reply-admission deadline.
    pub expires_at: i64,
}

/// Untrusted wire envelope until checked against the configured issuer chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedIssuerMetadata {
    /// Complete issuer statement.
    pub response: IssuerMetadata,
    /// Canonical base64url Ed25519 signature.
    pub signature: String,
}

/// Metadata authenticated for one exact retained exchange.
#[derive(Debug)]
pub struct VerifiedIssuerMetadata(SignedIssuerMetadata);

impl VerifiedIssuerMetadata {
    /// Verified public result; callers must not turn unavailability into absence.
    pub fn result(&self) -> &IssuerMetadataResult {
        &self.0.response.result
    }

    /// Authenticated complete statement for public-material decoding and caching.
    pub fn response(&self) -> &IssuerMetadata {
        &self.0.response
    }
}

impl IssuerMetadata {
    /// Validate bounded, canonical identifiers and hash the complete statement.
    pub fn digest(&self) -> Result<[u8; 32], ProtocolError> {
        validate_id(&self.request.trellis_instance_id, "trellisInstanceId")?;
        decode_base64url::<32>(&self.issuer_key_id)?;
        decode_base64url::<16>(&self.request.request_nonce)?;
        validate_subject(&self.request_subject)?;
        validate_subject(&self.reply_subject)?;
        match &self.request.query {
            IssuerMetadataQuery::AuthorityResolve { context_digest } => {
                decode_base64url::<32>(context_digest)?;
            }
            IssuerMetadataQuery::AuthorityStatus {
                authorization_session_id,
            } => {
                validate_id(authorization_session_id, "authorizationSessionId")?;
            }
            IssuerMetadataQuery::IssuerChain {
                pinned_key_id,
                target_key_id,
            } => {
                decode_base64url::<32>(pinned_key_id)?;
                decode_base64url::<32>(target_key_id)?;
            }
        }
        if self.format != ISSUER_METADATA_DOMAIN_V1
            || self.request.audience_nats_account.is_empty()
            || self.request.audience_nats_account.len() > 1024
            || self.issued_at < 0
            || self.expires_at <= self.issued_at
            || self.expires_at > self.issued_at.saturating_add(30)
            || self.expires_at > 9_007_199_254_740_991
            || matches!(&self.result, IssuerMetadataResult::Value { payload } if payload.is_empty() || payload.len() > MAX_ADMISSION_RESPONSE_BYTES)
        {
            return Err(authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                [],
                "invalid issuer metadata statement",
            ));
        }
        let value = serde_json::to_value(self).map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidFormat,
                [],
                "invalid issuer metadata encoding",
            )
        })?;
        if serde_json::to_vec(&value)
            .map_err(|_| {
                authorization_error(
                    AuthorizationErrorCode::InvalidFormat,
                    [],
                    "invalid issuer metadata encoding",
                )
            })?
            .len()
            > MAX_ADMISSION_RESPONSE_BYTES
        {
            return Err(authorization_error(
                AuthorizationErrorCode::ContextTooLarge,
                [],
                "issuer metadata exceeds admission budget",
            ));
        }
        signed_json_digest(ISSUER_METADATA_DOMAIN_V1, &value)
    }
}

/// Sign public registry material and the actual metadata request/reply binding.
pub fn sign_issuer_metadata(
    response: IssuerMetadata,
    signer: &SigningKey,
) -> Result<SignedIssuerMetadata, ProtocolError> {
    if response.issuer_key_id
        != URL_SAFE_NO_PAD.encode(Sha256::digest(signer.verifying_key().as_bytes()))
    {
        return Err(authorization_error(
            AuthorizationErrorCode::InvalidKeyId,
            [],
            "issuer metadata signing key mismatch",
        ));
    }
    let digest = response.digest()?;
    let signed = SignedIssuerMetadata {
        response,
        signature: URL_SAFE_NO_PAD.encode(signer.sign(&digest).to_bytes()),
    };
    if serde_json::to_vec(&signed)?.len() > MAX_ADMISSION_RESPONSE_BYTES {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextTooLarge,
            [],
            "issuer metadata exceeds admission budget",
        ));
    }
    Ok(signed)
}

/// Authenticate a bounded reply without trusting its NATS responder or errors.
pub fn verify_issuer_metadata(
    bytes: &[u8],
    issuer: &AuthorityIssuerKey,
    request: &IssuerMetadataRequest,
    subject: &str,
    reply: &str,
    now: i64,
) -> Result<VerifiedIssuerMetadata, ProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_ADMISSION_RESPONSE_BYTES {
        return Err(authorization_error(
            AuthorizationErrorCode::ContextTooLarge,
            [],
            "issuer metadata exceeds admission budget",
        ));
    }
    let signed: SignedIssuerMetadata = serde_json::from_slice(bytes).map_err(|_| {
        authorization_error(
            AuthorizationErrorCode::InvalidFormat,
            [],
            "invalid issuer metadata encoding",
        )
    })?;
    let response = &signed.response;
    if issuer.state != AuthorityIssuerState::Active
        || response.issuer_key_id != issuer.key_id
        || &response.request != request
        || response.request_subject != subject
        || response.reply_subject != reply
        || response.issued_at > now.saturating_add(5)
        || response.expires_at <= now
    {
        return Err(authorization_error(
            AuthorizationErrorCode::ScopeMismatch,
            [],
            "issuer metadata does not match retained exchange",
        ));
    }
    issuer
        .verifying_key()?
        .verify_strict(
            &response.digest()?,
            &Signature::from_bytes(&decode_base64url::<64>(&signed.signature)?),
        )
        .map_err(|_| {
            authorization_error(
                AuthorizationErrorCode::InvalidSignature,
                [],
                "invalid issuer metadata signature",
            )
        })?;
    Ok(VerifiedIssuerMetadata(signed))
}
