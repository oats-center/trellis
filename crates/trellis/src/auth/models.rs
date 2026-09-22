use serde::{Deserialize, Serialize};

use super::AuthenticatedUser;
use crate::client::SessionAuth;

/// Persisted admin session details for the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[doc = concat!("Public Trellis data type `", stringify!(AdminSessionState), "`.")]
pub struct AdminSessionState {
    /// Generated participant identity used by this login.
    pub participant_id: String,
    /// Durable user-only login identifier.
    pub login_session_id: String,
    /// Base URL for the Trellis deployment.
    #[doc = concat!("The `", stringify!(trellis_url), "` value.")]
    pub trellis_url: String,
    /// Session-key seed used to sign subsequent Trellis requests.
    #[doc = concat!("The `", stringify!(session_seed), "` value.")]
    pub session_seed: String,
    /// Session expiry in Unix milliseconds, when bounded.
    #[doc = concat!("The `", stringify!(expires_at), "` value.")]
    pub expires_at: Option<i64>,
    /// Whether this login explicitly accepted a non-loopback HTTP origin.
    #[serde(default)]
    pub allow_insecure_origin: bool,
}

impl AdminSessionState {
    /// Derive the public session key from the persisted private seed.
    pub fn session_key(&self) -> Result<String, crate::client::TrellisClientError> {
        Ok(crate::client::SessionAuth::from_seed_base64url(&self.session_seed)?.session_key)
    }
}

/// A successfully bound user session.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[doc = concat!("Public Trellis data type `", stringify!(BoundSession), "`.")]
pub struct BoundSession {
    /// Durable login shared by subsequent CLI connections.
    pub login_session_id: String,
    /// Session expiry in Unix milliseconds, when bounded.
    #[doc = concat!("The `", stringify!(expires_at), "` value.")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(BindResponseBound), "`.")]
pub struct BindResponseBound {
    /// Server time in Unix milliseconds.
    pub server_now: i64,
    #[doc = concat!("The `", stringify!(session), "` value.")]
    pub session: BoundSessionRecord,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[doc = concat!("Public Trellis data type `", stringify!(BoundSessionRecord), "`.")]
pub struct BoundSessionRecord {
    #[doc = concat!("The `", stringify!(session_id), "` value.")]
    pub session_id: String,
    /// Installed participant selected by the authenticated login flow.
    pub participant_id: String,
    /// Installation public key whose possession was proven during binding.
    pub session_key: String,
    #[doc = concat!("The `", stringify!(expires_at), "` value.")]
    pub expires_at: Option<i64>,
}

/// An in-progress agent login flow waiting for completion.
#[doc = concat!("Public Trellis data type `", stringify!(AgentLoginChallenge), "`.")]
pub struct AgentLoginChallenge {
    #[doc = concat!("The `", stringify!(flow_id), "` value.")]
    pub flow_id: String,
    #[doc = concat!("The `", stringify!(login_url), "` value.")]
    pub login_url: String,
    #[doc = concat!("The `", stringify!(session_seed), "` value.")]
    pub session_seed: String,
    pub(crate) participant_id: String,
    /// Whether this flow explicitly accepted a non-loopback HTTP origin.
    pub allow_insecure_origin: bool,
    #[doc = concat!("The `", stringify!(auth), "` value.")]
    pub auth: SessionAuth,
}

/// Options for starting an agent login flow.
pub struct StartAgentLoginOpts<'a> {
    /// Base URL for the Trellis deployment.
    #[doc = concat!("The `", stringify!(trellis_url), "` value.")]
    pub trellis_url: &'a str,
    /// Generated participant identity requesting the user session.
    pub participant_id: &'a str,
    /// Accept a non-loopback HTTP Trellis origin explicitly allow-listed by the
    /// operator. Defaults to strict HTTPS-or-loopback validation.
    pub allow_insecure_origin: bool,
}

/// Successful agent-login result after the admin user has been verified.
#[doc = concat!("Public Trellis data type `", stringify!(AdminLoginOutcome), "`.")]
pub struct AdminLoginOutcome {
    /// Persistable admin session state for later CLI reuse.
    #[doc = concat!("The `", stringify!(state), "` value.")]
    pub state: AdminSessionState,
    /// Authenticated user returned by `Auth.Sessions.Me` after bind succeeds.
    #[doc = concat!("The `", stringify!(user), "` value.")]
    pub user: AuthenticatedUser,
}

/// Result of starting admin reauthentication for a changed contract.
#[doc = concat!("Public Trellis value set `", stringify!(AdminReauthOutcome), "`.")]
pub enum AdminReauthOutcome {
    /// Contract change was auto-approved and the session was rebound immediately.
    Bound(Box<AdminLoginOutcome>),
    /// External interaction is still required to finish the agent auth flow.
    Flow(Box<AgentLoginChallenge>),
}

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
