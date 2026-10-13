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
mod transport;
mod upstream_http;

#[cfg(test)]
#[path = "../oauth_engine_tests.rs"]
pub(super) mod oauth_engine_tests;

pub(super) use crate::config::OAuthTransport;
pub(super) use binding::TokenBinding;
pub(super) use cimd::{ClientDocument, ClientDocuments};
pub(super) use details::RequestedAuthority;
pub(crate) use engine::{OAuthEngine, TokenError, TrellisTokenResponse};
pub(crate) use http::router;
pub(super) use portal::ConsentPresentation;
pub(crate) use storage::OAuthStorage;
pub(crate) use transport::{OAuthListener, OAuthTransportError};
