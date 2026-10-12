//! Client authorization: own-context management, pre-NATS trust bootstrap,
//! and connected NATS-backed provider-side resolution.

mod bootstrap_http;
mod core;
mod own_context;
mod provider_cache;
mod refresh;
mod registry;
mod transport;
mod types;

pub use bootstrap_http::canonical_trellis_origin;

pub use core::{
    AuthorizationVerificationCore, AuthorizationVerificationError, EventVerificationInput,
    RequestVerificationInput, VerifiedAuthorizationEvent, VerifiedAuthorizationRequest,
};
pub use own_context::AuthorizationContextCache;
pub(crate) use own_context::{OwnTransitionGuard, PreparedOwnCandidate};
pub use provider_cache::{
    AuthorizationLease, AuthorizationOwner, HistoricalEventVerificationInput,
    RuntimeAuthorizationTrust, VerifiedAuthorizationResponse,
};
pub(crate) use refresh::install_prepared_authorization;
pub(crate) use refresh::spawn_authorization_context_refresh_task;
pub(crate) use refresh::AuthorizationRefreshRuntime;
pub use transport::read_own_admission;
pub use transport::resource_action_marker;
pub(crate) use transport::{policy_covers, ResourceTransportAction, ResourceTransportKind};
pub(super) use types::AuthorizationCredential;
pub(crate) use types::OwnTransportSnapshot;
pub use types::{
    AuthorizationContextBundle, AuthorizationInstallation, AuthorizationNativeTransport,
    AuthorizationRuntimeBinding, AuthorizationRuntimeTransports,
};
