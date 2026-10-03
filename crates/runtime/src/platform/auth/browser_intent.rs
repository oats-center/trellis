//! Server-validated sign-in requests. These carry no authentication or authority.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::AuthorizationStateError;

const FORMAT: &str = "trellis.browser-sign-in-intent.v1";

/// Immutable request fields carried by the browser until authentication starts.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BrowserSignInIntent {
    pub(crate) format: String,
    pub(crate) intent_id: String,
    pub(crate) participant_id: String,
    pub(crate) session_public_key: String,
    pub(crate) origin: String,
    pub(crate) redirect_target: String,
    pub(crate) portal_id: String,
    pub(crate) issued_at: i64,
    pub(crate) issuer_key_id: String,
}

impl BrowserSignInIntent {
    /// Sign immutable request data using the existing online issuer.
    pub(crate) fn sign(mut self, key: &SigningKey) -> Result<String, AuthorizationStateError> {
        self.format = FORMAT.to_owned();
        let body = trellis_protocol::canonicalize_json(
            &serde_json::to_value(&self)
                .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?,
        )
        .map_err(|error| AuthorizationStateError::InvalidRecord(error.to_string()))?;
        // The format inside these signed bytes separates this signature domain
        // from contexts and other objects signed with the same online issuer.
        let signature = key.sign(body.as_bytes());
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(body),
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }

    /// Verify request integrity without applying an authentication-attempt TTL.
    pub(crate) fn verify(token: &str, key: &VerifyingKey) -> Result<Self, AuthorizationStateError> {
        let invalid =
            || AuthorizationStateError::InvalidRecord("invalid browser sign-in intent".to_owned());
        if token.len() > 16_384 {
            return Err(invalid());
        }
        let (body, signature) = token.split_once('.').ok_or_else(invalid)?;
        let body = URL_SAFE_NO_PAD.decode(body).map_err(|_| invalid())?;
        let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| invalid())?;
        let signature = Signature::from_slice(&signature).map_err(|_| invalid())?;
        key.verify_strict(&body, &signature)
            .map_err(|_| invalid())?;
        let intent: Self = serde_json::from_slice(&body).map_err(|_| invalid())?;
        if intent.format != FORMAT {
            return Err(invalid());
        }
        Ok(intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_request_cannot_substitute_participant_return_target_or_key() {
        let key = SigningKey::from_bytes(&[71; 32]);
        let token = BrowserSignInIntent {
            format: FORMAT.to_owned(),
            intent_id: "intent".to_owned(),
            participant_id: "example.App".to_owned(),
            session_public_key: "app-key".to_owned(),
            origin: "https://app.example".to_owned(),
            redirect_target: "https://app.example/return".to_owned(),
            portal_id: "builtin".to_owned(),
            issued_at: 1,
            issuer_key_id: "issuer".to_owned(),
        }
        .sign(&key)
        .unwrap();
        assert_eq!(
            BrowserSignInIntent::verify(&token, &key.verifying_key())
                .unwrap()
                .intent_id,
            "intent"
        );
        let (body, signature) = token.split_once('.').unwrap();
        let original: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).unwrap()).unwrap();
        for (field, value) in [
            ("participantId", "foreign.App"),
            ("redirectTarget", "https://foreign.example"),
            ("sessionPublicKey", "foreign-key"),
            ("format", "other.v1"),
        ] {
            let mut changed = original.clone();
            changed[field] = value.into();
            let altered = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).unwrap()),
                signature
            );
            assert!(BrowserSignInIntent::verify(&altered, &key.verifying_key()).is_err());
        }
        let mut wrong_domain = original;
        wrong_domain["format"] = "trellis.authorization-context.v1".into();
        let bytes = serde_json::to_vec(&wrong_domain).unwrap();
        let signed_other = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&bytes),
            URL_SAFE_NO_PAD.encode(key.sign(&bytes).to_bytes())
        );
        assert!(BrowserSignInIntent::verify(&signed_other, &key.verifying_key()).is_err());
    }
}
