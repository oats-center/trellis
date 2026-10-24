//! Runtime-only support for Trellis's own built-in services and test harnesses.
//!
//! Nothing here is part of the public API or covered by semver. Applications
//! and services should not use it.

use std::future::Future;
use std::sync::Arc;

use crate::client::{AuthorizationOwner, PreparedTrellisEvent, TrellisClient, TrellisClientError};
use crate::jobs::JobsRuntime;
use crate::service::{ConnectedServiceRuntime, EventsRuntime, LocalAuthVerifier};

pub use crate::service::internal::*;
pub use crate::service::live_router::LiveProviderOwner;

/// Runtime access to a connected client.
pub trait RuntimeClient {
    /// The authenticated provider NATS connection.
    fn runtime_nats(&self) -> async_nats::Client;
}

impl RuntimeClient for TrellisClient {
    fn runtime_nats(&self) -> async_nats::Client {
        self.provider_nats()
    }
}

/// Runtime ownership of a built-in provider's logical connection.
pub trait RuntimeOwner: Sized {
    /// Adopt a connected client as a live provider owner.
    fn from_connected_client(client: Arc<TrellisClient>) -> Self;

    /// The authenticated NATS connection built-in routers serve over.
    fn runtime_nats(&self) -> async_nats::Client;

    /// Generated service APIs called through the owner's authenticated client.
    fn runtime_generated_client(&self) -> crate::generated::Client;

    /// A request verifier sharing the connection's authority owner.
    fn runtime_validator(&self, api_id: impl Into<String>) -> LocalAuthVerifier;

    /// The connection's pinned verification and revocation state.
    fn runtime_authorization_owner(&self) -> AuthorizationOwner;

    /// Publish durable subsystem output through the installed provider authority.
    fn publish_prepared<'a>(
        &'a self,
        event: &'a PreparedTrellisEvent,
    ) -> impl Future<Output = Result<(), TrellisClientError>> + Send + 'a;

    /// End the logical connection after its subsystem stops, closing transport
    /// ownership even while child routers retain client references.
    fn shutdown_native_runtime(
        &self,
    ) -> impl Future<Output = Result<(), TrellisClientError>> + Send + '_;

    /// Request terminal transport closure without awaiting generation drainage.
    fn abort_native_runtime(&self);
}

impl RuntimeOwner for LiveProviderOwner {
    fn from_connected_client(client: Arc<TrellisClient>) -> Self {
        Self::new(client)
    }

    fn runtime_nats(&self) -> async_nats::Client {
        self.client().provider_nats()
    }

    fn runtime_generated_client(&self) -> crate::generated::Client {
        crate::generated::Client::from_client(Arc::clone(self.client()))
    }

    fn runtime_validator(&self, api_id: impl Into<String>) -> LocalAuthVerifier {
        LocalAuthVerifier::new(
            self.client().authorization_owner().clone(),
            self.runtime_nats(),
            api_id,
        )
    }

    fn runtime_authorization_owner(&self) -> AuthorizationOwner {
        self.client().authorization_owner().clone()
    }

    fn publish_prepared<'a>(
        &'a self,
        event: &'a PreparedTrellisEvent,
    ) -> impl Future<Output = Result<(), TrellisClientError>> + Send + 'a {
        self.client().publish_prepared(event)
    }

    fn shutdown_native_runtime(
        &self,
    ) -> impl Future<Output = Result<(), TrellisClientError>> + Send + '_ {
        self.client().shutdown_native_runtime()
    }

    fn abort_native_runtime(&self) {
        self.client().abort_native_runtime();
    }
}

/// Runtime ownership of a connected service.
pub trait RuntimeService {
    /// The service's logical owner, retained for host-managed shutdown.
    ///
    /// The host must close and join this owner before releasing its broker,
    /// including when cancellation drops the service's running future while
    /// accepted callbacks still retain client references.
    fn runtime_native_owner(&self) -> LiveProviderOwner;
}

impl<C> RuntimeService for ConnectedServiceRuntime<C> {
    fn runtime_native_owner(&self) -> LiveProviderOwner {
        LiveProviderOwner::new(Arc::clone(self.client()))
    }
}

/// A runtime facade over a Trellis-owned built-in provider's connection.
pub trait FromProviderNats {
    /// Create the facade over the provider's NATS connection.
    fn from_nats(nats: async_nats::Client) -> Self;
}

impl FromProviderNats for JobsRuntime {
    fn from_nats(nats: async_nats::Client) -> Self {
        Self::from_provider_nats(nats)
    }
}

impl FromProviderNats for EventsRuntime {
    fn from_nats(nats: async_nats::Client) -> Self {
        Self::from_provider_nats(nats)
    }
}
