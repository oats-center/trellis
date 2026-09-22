use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};

use axum::routing::get;
use axum::Json;
use axum::Router;
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

/// Runs the runtime readiness HTTP server until `shutdown` resolves.
pub async fn run_http_server(
    config: &RuntimeConfig,
    mode: RuntimeMode,
    application_router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.http_port()));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServerError::Bind { addr, source })?;
    let version = build_version_info(mode);
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(healthz))
        .with_state(version)
        .merge(application_router);

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(ServerError::Serve)
}

/// Returns readiness metadata for runtime liveness probes.
async fn healthz(
    axum::extract::State(version): axum::extract::State<VersionInfo>,
) -> Json<VersionInfo> {
    Json(version)
}
