//! Reusable Trellis auth/session helpers for Rust clients and the Trellis agent.

mod browser_login;
mod client;
#[cfg(feature = "device")]
mod device_activation;
#[cfg(feature = "device")]
mod device_identity;
mod error;
mod models;
#[cfg(feature = "native-oauth")]
mod native_oauth;
#[cfg(feature = "native-oauth")]
mod oauth_store;
mod protocol;

pub use crate::service::payload_hash_base64url;
pub use browser_login::generate_session_keypair;
pub use client::session_public_key;
#[cfg(feature = "device")]
pub use device_activation::{
    check_device_activation, derive_device_confirmation_code, wait_for_device_activation,
    DeviceActivationError, DeviceActivationOptions, DeviceActivationPending,
    DeviceActivationSession, DeviceActivationStatus,
};
#[cfg(feature = "device")]
pub use device_identity::{derive_device_identity, derive_device_user_companion};
pub use error::TrellisAuthError;
pub use models::{DeviceCompanionIdentity, DeviceIdentity};
#[cfg(feature = "native-oauth")]
pub use native_oauth::{
    discover_native_oauth, NativeDeviceAuthorization, NativeOAuthClient, NativeOAuthError,
    NativeOAuthServer, NativeOAuthSession,
};
#[cfg(feature = "native-oauth")]
pub use oauth_store::{NativeCredentialLease, NativeCredentialScope, NativeOAuthGrant};
pub use protocol::AuthenticatedUser;
