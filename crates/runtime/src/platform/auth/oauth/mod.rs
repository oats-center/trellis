//! Embedded OAuth integration. The engine owns OAuth and DPoP validation;
//! Trellis adds capability policy and the signed ephemeral NATS-key binding.

mod assets;
mod binding;
mod cimd;
mod details;
mod device;
mod engine;
mod http;
mod oidc;
mod portal;
mod storage;
mod transport;
mod upstream_http;

pub(super) use crate::config::OAuthTransport;
pub(super) use binding::TokenBinding;
pub(super) use cimd::{ClientDocument, ClientDocuments};
pub(super) use details::RequestedAuthority;
pub(super) use engine::{OAuthEngine, TrellisTokenResponse};
pub(super) use http::router;
pub(super) use storage::OAuthStorage;
pub(super) use transport::{OAuthListener, OAuthTransportError};
