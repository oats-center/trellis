use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;

use super::{DeviceCompanionIdentity, DeviceIdentity, TrellisAuthError};

const DEVICE_IDENTITY_HKDF_INFO: &str = "trellis/device-identity/v1";
const DEVICE_ACTIVATION_HKDF_INFO: &str = "trellis/device-activate/v1";
const DEVICE_USER_COMPANION_DOMAIN: &str = "trellis.device.user-companion.v1";

#[doc = concat!("Trellis API operation `", stringify!(derive_device_identity), "`.")]
pub fn derive_device_identity(
    device_root_secret: &[u8],
) -> Result<DeviceIdentity, TrellisAuthError> {
    if device_root_secret.len() != 32 {
        return Err(TrellisAuthError::InvalidArgument(format!(
            "invalid device root secret length: {} (expected 32)",
            device_root_secret.len()
        )));
    }

    let hkdf = Hkdf::<Sha256>::new(Some(&[]), device_root_secret);
    let mut identity_seed = [0u8; 32];
    hkdf.expand(DEVICE_IDENTITY_HKDF_INFO.as_bytes(), &mut identity_seed)
        .map_err(|error| {
            TrellisAuthError::InvalidArgument(format!(
                "failed to derive device identity seed: {error}"
            ))
        })?;
    let mut activation_key = [0u8; 32];
    hkdf.expand(DEVICE_ACTIVATION_HKDF_INFO.as_bytes(), &mut activation_key)
        .map_err(|error| {
            TrellisAuthError::InvalidArgument(format!(
                "failed to derive device activation key: {error}"
            ))
        })?;
    let public_identity_key = URL_SAFE_NO_PAD.encode(
        SigningKey::from_bytes(&identity_seed)
            .verifying_key()
            .to_bytes(),
    );

    Ok(DeviceIdentity {
        identity_seed_base64url: URL_SAFE_NO_PAD.encode(identity_seed),
        public_identity_key,
        activation_key_base64url: URL_SAFE_NO_PAD.encode(activation_key),
    })
}

/// Derive the installation credential for one exact origin-bound child participant.
///
/// # Errors
///
/// Returns an invalid-argument error for a non-32-byte root secret, invalid Trellis
/// URL, empty child identity, or an HKDF expansion failure.
pub fn derive_device_user_companion(
    device_root_secret: &[u8],
    trellis_url: &str,
    child_participant_id: &str,
) -> Result<DeviceCompanionIdentity, TrellisAuthError> {
    if device_root_secret.len() != 32 {
        return Err(TrellisAuthError::InvalidArgument(format!(
            "invalid device root secret length: {} (expected 32)",
            device_root_secret.len()
        )));
    }
    if child_participant_id.is_empty() {
        return Err(TrellisAuthError::InvalidArgument(
            "companion participant ID must not be empty".into(),
        ));
    }
    let origin = crate::client::canonical_trellis_origin(trellis_url)
        .map_err(|error| TrellisAuthError::InvalidArgument(error.to_string()))?;
    let mut info = Vec::new();
    for part in [
        DEVICE_USER_COMPANION_DOMAIN.as_bytes(),
        origin.as_bytes(),
        child_participant_id.as_bytes(),
    ] {
        info.extend_from_slice(&(part.len() as u64).to_be_bytes());
        info.extend_from_slice(part);
    }
    let hkdf = Hkdf::<Sha256>::new(Some(&[]), device_root_secret);
    let mut seed = [0u8; 32];
    hkdf.expand(&info, &mut seed).map_err(|error| {
        TrellisAuthError::InvalidArgument(format!(
            "failed to derive device companion installation key: {error}"
        ))
    })?;
    Ok(DeviceCompanionIdentity {
        installation_seed_base64url: URL_SAFE_NO_PAD.encode(seed),
        installation_public_key: URL_SAFE_NO_PAD
            .encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()),
    })
}

#[cfg(test)]
mod tests {
    use super::{derive_device_identity, derive_device_user_companion};

    #[test]
    fn identity_derivation_is_deterministic_and_validates_length() {
        let identity = derive_device_identity(&[7; 32]).expect("first identity");
        assert_eq!(
            identity,
            derive_device_identity(&[7; 32]).expect("second identity")
        );
        assert_eq!(
            identity.identity_seed_base64url,
            "ANrLNfV6eakMEoleiHoPuE9bQL1BkOE4VTDqAU3jvPQ"
        );
        assert_eq!(
            identity.public_identity_key,
            "PJOPafbG8Sq47Ra0sOSYmG2pJQj5FRgPrlwynA5Dq0I"
        );
        assert_eq!(
            identity.activation_key_base64url,
            "z89beQNUvhI08xF7ceiwvCD_kUF_RtBGcvDFsyiErgA"
        );
        assert!(derive_device_identity(&[7; 31]).is_err());
    }

    #[test]
    fn companion_derivation_binds_origin_and_exact_child() {
        let companion = derive_device_user_companion(
            &[7; 32],
            "https://example.com/path",
            "acme.Sensor.Companion",
        )
        .expect("companion identity");
        assert_eq!(
            companion.installation_seed_base64url,
            "vIo7nClzQFE0Ir1R7AQz3aqO4NJ5uU2-wXJLaqcyVss"
        );
        assert_eq!(
            companion.installation_public_key,
            "vGf_UmbZSDEhpK6sxpO1esqkNwuof8fwvxvaNG20xZw"
        );
        assert_ne!(
            companion,
            derive_device_user_companion(
                &[7; 32],
                "https://other.example.com",
                "acme.Sensor.Companion",
            )
            .expect("other origin")
        );
        assert_ne!(
            companion,
            derive_device_user_companion(&[7; 32], "https://example.com", "acme.Sensor.Other",)
                .expect("other child")
        );
    }
}
