//! Reusable Trellis auth/session helpers for Rust clients and the Trellis agent.

mod browser_login;
mod client;
mod device_activation;
mod device_identity;
mod error;
mod models;
mod portal;
mod protocol;
mod session_store;

pub use crate::service::payload_hash_base64url;
pub use browser_login::{
    generate_session_keypair, poll_agent_flow_until_ready,
    poll_agent_flow_until_ready_with_insecure_origin, start_admin_reauth, start_agent_login,
};
pub use client::{connect_admin_client_async, session_public_key};
pub use device_activation::{
    check_device_activation, derive_device_confirmation_code, wait_for_device_activation,
    DeviceActivationError, DeviceActivationOptions, DeviceActivationPending,
    DeviceActivationSession, DeviceActivationStatus,
};
pub use device_identity::{
    derive_device_identity, derive_device_user_companion,
    derive_device_user_companion_with_insecure_origin,
};
pub use error::TrellisAuthError;
pub use models::{
    AdminLoginOutcome, AdminReauthOutcome, AdminSessionState, AgentLoginChallenge, BoundSession,
    DeviceCompanionIdentity, DeviceIdentity, StartAgentLoginOpts,
};
pub use portal::{
    approve_local_login, begin_local_login, complete_local_login, create_portal_binding,
    flow_id_from_url, perform_local_login, LocalLoginStep, PortalBinding, PortalConsentSummary,
    PORTAL_BINDING_HEADER,
};
pub use protocol::AuthenticatedUser;
pub use session_store::{clear_admin_session, load_admin_session, save_admin_session};
