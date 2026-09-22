//! File-backed online issuer loading.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ed25519_dalek::SigningKey;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use trellis_protocol::{
    AuthorizationIssuerKey, AuthorizationIssuerState, AuthorizationVerificationPolicy,
};

use crate::config::AuthorizationConfig;

/// Configured online signing key and its public verification material.
pub(crate) struct VerifiedTrustMaterial {
    pub(crate) issuer: AuthorizationIssuerKey,
    pub(crate) issuer_signing_key: SigningKey,
    pub(crate) policy: AuthorizationVerificationPolicy,
}

impl std::fmt::Debug for VerifiedTrustMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedTrustMaterial")
            .field("issuerKeyId", &self.issuer.key_id)
            .finish_non_exhaustive()
    }
}

impl VerifiedTrustMaterial {
    /// Load the configured issuer without any offline root or manifest.
    pub(crate) fn load(
        config: &AuthorizationConfig,
        now_unix_seconds: i64,
    ) -> Result<Self, TrustMaterialError> {
        let policy = AuthorizationVerificationPolicy::new(
            now_unix_seconds,
            config
                .allowed_clock_skew_seconds
                .try_into()
                .map_err(|_| TrustMaterialError::InvalidPolicy)?,
            config
                .context_lifetime_seconds
                .try_into()
                .map_err(|_| TrustMaterialError::InvalidPolicy)?,
            config.maximum_context_bytes,
            config.maximum_permissions,
        )
        .map_err(|_| TrustMaterialError::InvalidPolicy)?;

        let mut seed_bytes = read_signing_seed(&config.issuer_signing_seed_file)?;
        let issuer_signing_key = SigningKey::from_bytes(&seed_bytes);
        seed_bytes.fill(0);
        let issuer_public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(issuer_signing_key.verifying_key().to_bytes());
        let issuer_key_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            Sha256::digest(issuer_signing_key.verifying_key().to_bytes()),
        );
        Ok(Self {
            issuer: AuthorizationIssuerKey {
                key_id: issuer_key_id,
                public_key: issuer_public_key,
                state: AuthorizationIssuerState::Active,
            },
            issuer_signing_key,
            policy,
        })
    }
}

/// Safe file-backed trust loading failure.
#[derive(Debug, Error)]
pub(crate) enum TrustMaterialError {
    #[error("authorization issuer seed could not be read: {path}")]
    Read { path: PathBuf },
    #[error("authorization issuer seed file is invalid")]
    InvalidIssuerSeed,
    #[error("authorization verification policy is invalid")]
    InvalidPolicy,
}

fn read_signing_seed(path: &Path) -> Result<[u8; 32], TrustMaterialError> {
    warn_if_secret_permissions_are_open(path);
    let text = fs::read_to_string(path).map_err(|_| TrustMaterialError::Read {
        path: path.to_path_buf(),
    })?;
    let encoded = text.trim();
    let mut decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| TrustMaterialError::InvalidIssuerSeed)?;
    if base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        decoded.fill(0);
        return Err(TrustMaterialError::InvalidIssuerSeed);
    }
    let result = <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| {
        decoded.fill(0);
        TrustMaterialError::InvalidIssuerSeed
    })?;
    decoded.fill(0);
    Ok(result)
}

#[cfg(unix)]
fn warn_if_secret_permissions_are_open(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    if let Ok(metadata) = fs::metadata(path) {
        if metadata.permissions().mode() & 0o077 != 0 {
            tracing::warn!(path = %path.display(), "authorization issuer seed permissions allow group or other access");
        }
    }
}

#[cfg(not(unix))]
fn warn_if_secret_permissions_are_open(_path: &Path) {}
