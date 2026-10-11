use ed25519_dalek::{Signature, Signer, SigningKey};
use nkeys::{KeyPair, KeyPairType};
use trellis_protocol::{
    session_request_signing_digest, sign_session_event, sign_session_proof, sign_session_response,
    SessionEvent, SessionEventProof, SessionProof, SessionProofInput, SessionRequest,
    SessionRequestProof, SessionResponse, SessionResponseProof,
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

    /// Sign the exact publication bytes and catalog-stamped request transcript.
    ///
    /// Returns the signature and retained request-transcript digest. The caller
    /// retains that digest for response correlation without hashing the whole
    /// publication buffer again.
    /// When a request has a reply, its actual inbox must be created before
    /// signing. One-way control traffic signs the absence of a reply instead.
    #[doc = concat!("Trellis API operation `", stringify!(create_request_proof), "`.")]
    pub fn create_request_proof(
        &self,
        request: &SessionRequest,
        payload: &[u8],
    ) -> Result<(SessionRequestProof, [u8; 32]), TrellisClientError> {
        let digest = session_request_signing_digest(request, payload)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let signature = self.signing_key.sign(&digest);
        let proof = SessionRequestProof::parse(base64url_encode(&signature.to_bytes()))
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        Ok((proof, digest))
    }

    /// Sign a catalog-stamped event without interpreting its subject as identity.
    #[doc = concat!("Trellis API operation `", stringify!(create_event_proof), "`.")]
    pub fn create_event_proof(
        &self,
        event: &SessionEvent,
        payload: &[u8],
    ) -> Result<SessionEventProof, TrellisClientError> {
        sign_session_event(event, payload, &self.signing_key)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }

    /// Authenticate provider result bytes against the retained caller opening.
    ///
    /// Provider certification and current authority are checked by the receiver;
    /// signing a result does not itself assert that this session is a provider.
    pub fn create_response_proof(
        &self,
        response: &SessionResponse,
        payload: &[u8],
    ) -> Result<SessionResponseProof, TrellisClientError> {
        sign_session_response(response, payload, &self.signing_key)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }
}
