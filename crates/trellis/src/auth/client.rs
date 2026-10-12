use super::TrellisAuthError;
use crate::client::SessionAuth;

/// Derive the public session key for a base64url-encoded session seed.
pub fn session_public_key(seed_base64url: &str) -> Result<String, TrellisAuthError> {
    Ok(SessionAuth::from_seed_base64url(seed_base64url)?.session_key)
}
