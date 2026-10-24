use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;

/// Generate an ephemeral Ed25519 session seed and its canonical public key.
///
/// Returns `(seed_base64url, public_key_base64url)`; callers must keep the seed
/// private. User authorization is acquired through the native OAuth client.
pub fn generate_session_keypair() -> (String, String) {
    let seed = rand::random::<[u8; 32]>();
    let signing_key = SigningKey::from_bytes(&seed);
    (
        URL_SAFE_NO_PAD.encode(seed),
        URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes()),
    )
}
