use std::time::SystemTime;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::{dpop, jwt::JwsVerifiers};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// This value is constructed only after the engine verifies the entire proof.
/// Its replay identifier must be claimed by the engine's token transaction.
pub(crate) struct TokenBinding {
    pub(crate) persistent_dpop_jkt: String,
    pub(crate) public_jwk: serde_json::Value,
    pub(crate) jti: String,
    pub(crate) replay_until: SystemTime,
    pub(crate) runtime_id: String,
    pub(crate) ephemeral_nkey: String,
    pub(crate) ephemeral_public_key: String,
}

// Deserialize all security-relevant standard claims as well as the extension:
// serde rejects duplicate recognized fields instead of silently choosing one.
#[derive(Deserialize)]
struct BindingClaims {
    #[serde(rename = "htm")]
    _method: String,
    #[serde(rename = "htu")]
    _uri: String,
    #[serde(rename = "iat")]
    _issued_at: i64,
    #[serde(rename = "jti")]
    _request_id: String,
    trellis_runtime_id: String,
    trellis_nats_jkt: String,
}

impl TokenBinding {
    /// Check the deployment-specific claims on exactly the JWT verified by the
    /// library, not on client-provided JSON or another proof. The engine still
    /// validates and claims the same proof during token processing; this pure
    /// precheck never consumes a token or a replay identifier.
    pub(crate) fn verify(
        verifiers: &JwsVerifiers,
        proof: &str,
        token_uri: &str,
        runtime_id: &str,
        ephemeral_nkey: &str,
        now: SystemTime,
    ) -> Result<Self, &'static str> {
        let verified = dpop::verify_proof(verifiers, proof, "POST", token_uri, now)
            .map_err(|_| "invalid_dpop_proof")?;
        let payload = proof.split('.').nth(1).ok_or("invalid_dpop_proof")?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| "invalid_dpop_proof")?;
        let claims: BindingClaims =
            serde_json::from_slice(&payload).map_err(|_| "invalid_dpop_proof")?;
        // A runtime nonce is a canonical random 128-bit value, not a durable
        // credential or a database identifier.
        let nonce = URL_SAFE_NO_PAD
            .decode(runtime_id)
            .map_err(|_| "invalid_request")?;
        if nonce.len() != 16
            || URL_SAFE_NO_PAD.encode(&nonce) != runtime_id
            || claims.trellis_runtime_id != runtime_id
        {
            return Err("invalid_dpop_proof");
        }
        let key = nkeys::KeyPair::from_public_key(ephemeral_nkey).map_err(|_| "invalid_request")?;
        if key.key_pair_type() != nkeys::KeyPairType::User || key.public_key() != ephemeral_nkey {
            return Err("invalid_request");
        }
        let (_, public_key) =
            nkeys::from_public_key(ephemeral_nkey).map_err(|_| "invalid_request")?;
        let ephemeral_public_key = URL_SAFE_NO_PAD.encode(public_key);
        // RFC 7638 requires precisely these members in lexicographic order.
        let jwk =
            format!("{{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"{ephemeral_public_key}\"}}");
        let thumbprint = URL_SAFE_NO_PAD.encode(Sha256::digest(jwk.as_bytes()));
        if claims.trellis_nats_jkt != thumbprint {
            return Err("invalid_dpop_proof");
        }
        Ok(Self {
            persistent_dpop_jkt: verified.jkt,
            public_jwk: oauth_as::jwt::CompactJws::parse(proof)
                .map_err(|_| "DPoP header is invalid")?
                .header
                .get("jwk")
                .cloned()
                .ok_or("DPoP public key is missing")?,
            jti: verified.jti,
            replay_until: verified.replay_until,
            runtime_id: runtime_id.into(),
            ephemeral_nkey: ephemeral_nkey.into(),
            ephemeral_public_key,
        })
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
