use std::sync::Arc;

use serde::{Deserialize, Serialize};
use trellis_protocol::{SignedSessionAuthority, VerifiedSessionAuthority};

/// Native broker endpoints selected by the application or provisioning record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationNativeTransport {
    /// Native NATS endpoints.
    pub nats_servers: Vec<String>,
}

/// Available physical transports for one logical runtime.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationRuntimeTransports {
    /// Native NATS endpoints.
    pub native: Option<AuthorizationNativeTransport>,
    /// WebSocket endpoints.
    pub websocket: Option<AuthorizationNativeTransport>,
}

/// Application-selected transport location and ephemeral runtime identity.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizationRuntimeBinding {
    /// Random process runtime nonce certified at CONNECT.
    pub runtime_id: String,
    /// Session-key-derived private reply namespace.
    pub inbox_prefix: String,
    /// Explicit transport endpoints; no HTTP bootstrap is performed.
    pub transports: AuthorizationRuntimeTransports,
}

/// Exact verified authority exposed by the logical installation controller.
#[derive(Clone, Debug)]
pub struct AuthorizationContextBundle {
    /// Complete issuer-signed logical-session authority.
    pub context: SignedSessionAuthority,
}

/// CONNECT credentials are independent of short-lived application authority.
pub(crate) enum AuthorizationCredential {
    OAuth(trellis_protocol::SignedOAuthAdmission),
    Provisioned {
        admission: trellis_protocol::ProvisionedAdmission,
        identity: Arc<super::super::SessionAuth>,
    },
}

/// One prepared logical-authority installation. Only verified values enter it.
#[derive(Clone, Debug)]
pub struct AuthorizationInstallation {
    pub(crate) authorization: Option<serde_json::Value>,
    /// Privately verified replacement, owned by the shared authority owner.
    pub authority: Arc<VerifiedSessionAuthority>,
    /// Exact runtime-compiled ACL bound by the signed admission response.
    pub transport_authorization: trellis_protocol::TransportAuthorizationV1,
}

/// One correlated immutable snapshot used by attachment staging.
#[derive(Clone, Debug)]
pub(crate) struct OwnTransportSnapshot {
    pub(crate) context_digest: String,
    pub(crate) authorization_session_id: String,
    pub(crate) policy: trellis_protocol::TransportAuthorizationV1,
    pub(crate) runtime: AuthorizationRuntimeBinding,
}
