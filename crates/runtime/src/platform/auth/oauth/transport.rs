//! A listener serving exactly one origin, such as the Console's. Plain HTTP is
//! either an explicit loopback mode or trusted-proxy service of an HTTPS
//! origin, never a failed-TLS fallback or an automatically inferred deployment
//! mode.

use crate::config::ListenerTransport;
use crate::listener::LoadedListener;
use axum::Router;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OAuthTransportError {
    #[error("invalid listener configuration: {0}")]
    Configuration(&'static str),
    #[error("could not load the listener transport: {0}")]
    Tls(std::io::Error),
    #[error("listener failed: {0}")]
    Listener(#[from] std::io::Error),
}

pub(crate) struct OriginListener {
    listener: LoadedListener,
}

impl OriginListener {
    /// Load transport before publishing readiness. The passed socket is the
    /// ordinary runtime-owned listener and can also be supplied by a supervisor.
    pub(crate) async fn load(
        listener: TcpListener,
        selected_origin: &str,
        transport: &ListenerTransport,
    ) -> Result<Self, OAuthTransportError> {
        let origin = url::Url::parse(selected_origin)
            .map_err(|_| OAuthTransportError::Configuration("origin must be an absolute URL"))?;
        if origin.host().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(OAuthTransportError::Configuration(
                "origin must not contain credentials, a path, a query, or a fragment",
            ));
        }
        crate::config::validate_listener_transport(&origin, listener.local_addr()?.ip(), transport)
            .map_err(OAuthTransportError::Configuration)?;
        Ok(Self {
            listener: LoadedListener::load(listener, &origin, transport)
                .await
                .map_err(OAuthTransportError::Tls)?,
        })
    }

    /// Serve `router` alone on this origin.
    pub(crate) async fn run(
        self,
        router: Router,
        shutdown: CancellationToken,
    ) -> Result<(), OAuthTransportError> {
        self.listener
            .serve(router, shutdown.cancelled_owned())
            .await
            .map_err(OAuthTransportError::Listener)
    }
}
