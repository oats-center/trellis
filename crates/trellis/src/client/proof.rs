use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

#[doc = concat!("Trellis API operation `", stringify!(sha256), "`.")]
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.into()
}

#[doc = concat!("Trellis API operation `", stringify!(base64url_encode), "`.")]
pub fn base64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

#[doc = concat!("Trellis API operation `", stringify!(base64url_decode), "`.")]
pub fn base64url_decode(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(value)
}

#[doc = concat!("Trellis API operation `", stringify!(now_iat_seconds), "`.")]
pub fn now_iat_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[doc = concat!("Trellis API operation `", stringify!(new_request_id), "`.")]
pub fn new_request_id() -> String {
    ulid::Ulid::new().to_string()
}
