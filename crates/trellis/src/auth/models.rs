use serde::{Deserialize, Serialize};

/// Derived device identity material used to provision and bootstrap a device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(DeviceIdentity), "`.")]
pub struct DeviceIdentity {
    #[serde(rename = "identitySeedBase64url")]
    #[doc = concat!("The `", stringify!(identity_seed_base64url), "` value.")]
    pub identity_seed_base64url: String,
    #[serde(rename = "publicIdentityKey")]
    #[doc = concat!("The `", stringify!(public_identity_key), "` value.")]
    pub public_identity_key: String,
    #[serde(rename = "activationKeyBase64url")]
    /// Secret used to derive activation confirmation codes. Keep it device-local.
    pub activation_key_base64url: String,
}

/// Device-local credential derived for one exact user companion installation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(DeviceCompanionIdentity), "`.")]
pub struct DeviceCompanionIdentity {
    /// Secret Ed25519 seed. Keep it device-local and never send it to Trellis.
    #[serde(rename = "installationSeedBase64url")]
    pub installation_seed_base64url: String,
    /// Public installation key sent in the proof-bound companion claim.
    #[serde(rename = "installationPublicKey")]
    pub installation_public_key: String,
}
