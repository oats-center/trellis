use ed25519_dalek::{Signature, Signer, SigningKey};
use nkeys::{KeyPair, KeyPairType};
use trellis_protocol::{
    sign_authorization_event, sign_authorization_request, sign_session_proof,
    AuthorizationEventProof, AuthorizationRequestProof, SessionProof, SessionProofInput,
};

use crate::client::proof::{base64url_decode, base64url_encode, sha256};
use crate::client::TrellisClientError;

/// Session-scoped signing material used for Trellis auth and RPC proofs.
#[doc = concat!("Public Trellis data type `", stringify!(SessionAuth), "`.")]
pub struct SessionAuth {
    /// Public session key in base64url form.
    #[doc = concat!("The `", stringify!(session_key), "` value.")]
    pub session_key: String,
    signing_key: SigningKey,
}

impl SessionAuth {
    /// Construct a session authenticator from a base64url-encoded Ed25519 seed.
    #[doc = concat!("Trellis API operation `", stringify!(from_seed_base64url), "`.")]
    pub fn from_seed_base64url(seed_b64url: &str) -> Result<Self, TrellisClientError> {
        let seed = base64url_decode(seed_b64url)?;
        if seed.len() != 32 {
            return Err(TrellisClientError::InvalidSeedLen(seed.len()));
        }
        let mut seed32 = [0u8; 32];
        seed32.copy_from_slice(&seed);
        let signing_key = SigningKey::from_bytes(&seed32);
        let public = signing_key.verifying_key().to_bytes();
        let session_key = base64url_encode(&public);
        Ok(Self {
            session_key,
            signing_key,
        })
    }

    #[doc = concat!("Trellis API operation `", stringify!(sign_sha256_bytes), "`.")]
    pub fn sign_sha256_bytes(&self, bytes: &[u8]) -> String {
        let digest = sha256(bytes);
        let signature: Signature = self.signing_key.sign(&digest);
        base64url_encode(&signature.to_bytes())
    }

    pub(crate) fn sign_bytes(&self, bytes: &[u8]) -> String {
        let signature: Signature = self.signing_key.sign(bytes);
        base64url_encode(&signature.to_bytes())
    }

    pub(crate) fn key_id(&self) -> String {
        base64url_encode(&sha256(self.signing_key.verifying_key().as_bytes()))
    }

    pub(crate) fn nkey_pair(&self) -> Result<KeyPair, TrellisClientError> {
        KeyPair::new_from_raw(KeyPairType::User, self.signing_key.to_bytes())
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }

    /// Sign one canonical protocol-owned session proof input.
    pub fn sign_session_proof(
        &self,
        input: &SessionProofInput,
    ) -> Result<SessionProof, TrellisClientError> {
        sign_session_proof(input, &self.signing_key)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }

    /// Return the session public key encoded as a NATS User NKey.
    pub fn session_nkey(&self) -> Result<String, TrellisClientError> {
        Ok(self.nkey_pair()?.public_key())
    }

    /// Return the inbox prefix derived from the session key.
    #[doc = concat!("Trellis API operation `", stringify!(inbox_prefix), "`.")]
    pub fn inbox_prefix(&self) -> String {
        format!(
            "_INBOX.{}",
            &self.session_key[..16.min(self.session_key.len())]
        )
    }

    /// Return the private signing key used for live-session server proofs.
    ///
    /// The key stays inside the crate so provider publications authenticate
    /// with the same runtime identity as ordinary context-bound requests.
    pub(crate) fn live_signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.signing_key
    }

    /// Create the context-bound v1 `proof` header for a signed RPC request.
    ///
    /// The exact NATS reply inbox must be created before signing and used for
    /// both the proof input and the publish reply.
    #[doc = concat!("Trellis API operation `", stringify!(create_request_proof), "`.")]
    pub fn create_request_proof(
        &self,
        context_digest: &str,
        subject: &str,
        reply_subject: &str,
        payload: &[u8],
        iat: i64,
        request_id: &str,
    ) -> Result<AuthorizationRequestProof, TrellisClientError> {
        if reply_subject.is_empty() {
            return Err(TrellisClientError::Bootstrap(
                "request reply subject must not be empty".into(),
            ));
        }
        sign_authorization_request(
            context_digest,
            subject,
            Some(reply_subject),
            payload,
            iat,
            request_id,
            &self.signing_key,
        )
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }

    /// Create the context-bound v1 `proof` header for a signed event.
    #[doc = concat!("Trellis API operation `", stringify!(create_event_proof), "`.")]
    pub fn create_event_proof(
        &self,
        context_digest: &str,
        descriptor_identity: &str,
        subject: &str,
        payload: &[u8],
        event_id: &str,
        event_time: &str,
    ) -> Result<AuthorizationEventProof, TrellisClientError> {
        sign_authorization_event(
            context_digest,
            descriptor_identity,
            subject,
            payload,
            event_id,
            event_time,
            &self.signing_key,
        )
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }
}
