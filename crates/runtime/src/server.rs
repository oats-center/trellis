use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use thiserror::Error;

use crate::{RuntimeConfig, RuntimeMode};

/// Version and process metadata returned by the readiness endpoint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// Crate package version compiled into this binary.
    pub version: &'static str,
    /// Runtime mode selected for this process.
    pub mode: String,
}

/// Error returned by the runtime HTTP server.
#[derive(Debug, Error)]
pub enum ServerError {
    /// The TCP listener could not bind to the configured address.
    #[error("failed to bind runtime HTTP listener at {addr}: {source}")]
    Bind {
        /// Listener address that failed.
        addr: SocketAddr,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The HTTP server exited with an error.
    #[error(transparent)]
    Serve(#[from] std::io::Error),
}

/// Builds the version metadata exposed by the runtime HTTP server.
#[must_use]
pub fn build_version_info(mode: RuntimeMode) -> VersionInfo {
    VersionInfo {
        version: env!("CARGO_PKG_VERSION"),
        mode: mode.to_string(),
    }
}

/// Binds the runtime HTTP listener without serving it yet.
///
/// Binding early reserves the configured port so it is stable for the whole
/// process lifetime, and lets the supervisor start serving the bootstrap
/// routes before built-in live providers attempt native bootstrap.
///
/// # Errors
///
/// Returns [`ServerError::Bind`] when the listener cannot bind.
pub async fn bind_http_listener(
    config: &RuntimeConfig,
) -> Result<tokio::net::TcpListener, ServerError> {
    let addr = SocketAddr::new(config.http_bind_address(), config.http_port());
    tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::Bind { addr, source })
}

/// Shared state for the runtime liveness and readiness endpoints.
#[derive(Clone)]
struct HttpState {
    version: VersionInfo,
    ready: Arc<AtomicBool>,
}

/// Serves `application_router` plus the readiness endpoints on a bound listener.
///
/// `/healthz` always answers process metadata. `/readyz` answers `503` until
/// the supervisor marks startup complete and `200` afterwards, so a caller
/// never begins work against a runtime whose routes and built-in live
/// providers are still bootstrapping.
///
/// # Errors
///
/// Returns [`ServerError::Serve`] when the HTTP server exits with an error.
pub async fn serve_http_listener(
    listener: tokio::net::TcpListener,
    mode: RuntimeMode,
    application_router: Router,
    ready: Arc<AtomicBool>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let state = HttpState {
        version: build_version_info(mode),
        ready,
    };
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
        .merge(application_router);

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(ServerError::Serve)
}

/// Runs the runtime readiness HTTP server until `shutdown` resolves.
pub async fn run_http_server(
    config: &RuntimeConfig,
    mode: RuntimeMode,
    application_router: Router,
    ready: Arc<AtomicBool>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let listener = bind_http_listener(config).await?;
    serve_http_listener(listener, mode, application_router, ready, shutdown).await
}

/// Returns liveness metadata for the runtime process.
async fn healthz(State(state): State<HttpState>) -> Json<VersionInfo> {
    Json(state.version)
}

/// Reports ready only once runtime startup has completed.
async fn readyz(State(state): State<HttpState>) -> impl IntoResponse {
    let status = if state.ready.load(Ordering::SeqCst) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(state.version))
}
