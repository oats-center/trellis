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
    VerifiedCaller,
};
pub use own_context::AuthorizationContextCache;
pub(crate) use provider_cache::AuthorizationContextLease;
pub use provider_cache::AuthorizationProviderCache;
#[cfg(feature = "runtime-internals")]
pub use provider_cache::{RuntimeAuthorizationIoCounters, RuntimeAuthorizationTrust};
pub(crate) use refresh::install_prepared_authorization;
pub(crate) use refresh::refresh_until_materialized;
pub(crate) use refresh::spawn_authorization_context_refresh_task;
pub(crate) use refresh::AuthorizationRefreshRuntime;
pub use transport::{
    read_own_admission, TransportAuthorizationState, TransportAuthorizationStatus,
};
pub use transport::{resource_action_marker, ResourceTransportGate};
pub(crate) use transport::{ResourceTransportAction, ResourceTransportKind};
pub(super) use types::AuthorizationCredential;
#[cfg(feature = "runtime-internals")]
pub use types::AuthorizationRegistryBinding;
pub use types::{
    AuthorizationApiBinding, AuthorizationContextBundle, AuthorizationContextPolicy,
    AuthorizationInstallation, AuthorizationNativeTransport, AuthorizationRoutingMaterial,
    AuthorizationRuntimeBinding, AuthorizationRuntimeTransports,
};
