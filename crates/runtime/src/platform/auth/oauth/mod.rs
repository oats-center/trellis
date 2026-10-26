//! Embedded OAuth integration. The engine owns OAuth and DPoP validation;
//! Trellis adds capability policy and the signed ephemeral NATS-key binding.

pub(super) mod account;
mod assets;
mod binding;
mod cimd;
mod details;
mod device;
mod engine;
pub(super) mod enrollment;
mod http;
mod nonce;
mod oidc;
mod portal;
mod storage;
mod theme;
mod transport;
mod upstream_http;

#[cfg(test)]
pub(super) mod engine_tests;

#[cfg(test)]
pub(super) use crate::config::ListenerTransport as OAuthTransport;
pub(super) use binding::TokenBinding;
pub(super) use cimd::{ClientDocument, ClientDocuments};
pub(super) use details::RequestedAuthority;
pub(crate) use engine::OAuthEngine;
#[cfg(test)]
pub(crate) use engine::{TokenError, TrellisTokenResponse};
pub(crate) use http::router;
pub(super) use portal::ConsentPresentation;
pub(crate) use storage::OAuthStorage;
pub(crate) use theme::PortalThemes;
pub(crate) use transport::OAuthListener;

/// Run a handler's synchronous storage work on the blocking pool.
pub(super) async fn with_store<T, E>(
    engine: &OAuthEngine,
    work: impl FnOnce(&OAuthStorage) -> Result<T, E> + Send + 'static,
) -> Result<T, E>
where
    T: Send + 'static,
    E: From<super::AuthError> + Send + 'static,
{
    let store = engine.server.store().clone();
    super::sqlite::blocking(move || work(&store)).await
}
