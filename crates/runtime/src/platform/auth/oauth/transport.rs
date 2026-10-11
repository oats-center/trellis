//! Dedicated authorization listener. Plain HTTP is an explicit loopback mode,
//! never a failed-TLS fallback or an automatically inferred deployment mode.

use crate::config::OAuthTransport;
use axum::Router;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub(crate) enum OAuthTransportError {
    #[error("invalid OAuth listener configuration: {0}")]
    Configuration(&'static str),
    #[error("could not load the OAuth listener certificate and private key: {0}")]
    Tls(std::io::Error),
    #[error("OAuth listener failed: {0}")]
    Listener(#[from] std::io::Error),
}

pub(crate) struct OAuthListener {
    listener: std::net::TcpListener,
    tls: Option<axum_server::tls_rustls::RustlsConfig>,
}

impl OAuthListener {
    /// Load transport before publishing readiness. The passed socket is the
    /// ordinary runtime-owned listener and can also be supplied by a supervisor.
    pub(crate) async fn load(
        listener: TcpListener,
        selected_origin: &str,
        transport: &OAuthTransport,
    ) -> Result<Self, OAuthTransportError> {
        let origin = url::Url::parse(selected_origin).map_err(|_| {
            OAuthTransportError::Configuration("OAuth origin must be an absolute URL")
        })?;
        if origin.host().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(OAuthTransportError::Configuration(
                "OAuth origin must not contain credentials, a path, a query, or a fragment",
            ));
        }
        let tls = match transport {
            OAuthTransport::Tls {
                certificate_chain,
                private_key,
            } => {
                if origin.scheme() != "https" {
                    return Err(OAuthTransportError::Configuration(
                        "TLS requires an HTTPS OAuth origin",
                    ));
                }
                if rustls::crypto::CryptoProvider::get_default().is_none() {
                    let _ = rustls::crypto::ring::default_provider().install_default();
                }
                Some(
                    axum_server::tls_rustls::RustlsConfig::from_pem_file(
                        certificate_chain,
                        private_key,
                    )
                    .await
                    .map_err(OAuthTransportError::Tls)?,
                )
            }
            OAuthTransport::LoopbackDevelopment => {
                let local_host = match origin.host() {
                    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                    Some(url::Host::Domain(host)) => host == "localhost",
                    None => false,
                };
                if origin.scheme() != "http"
                    || !local_host
                    || !listener.local_addr()?.ip().is_loopback()
                {
                    return Err(OAuthTransportError::Configuration("plaintext development OAuth must use an HTTP loopback origin and a loopback listener"));
                }
                None
            }
        };
        Ok(Self {
            listener: listener.into_std()?,
            tls,
        })
    }

    /// Serve only the OAuth router. No Console router, application assets, or
    /// general-purpose reverse proxy is mounted on this listener.
    pub(crate) async fn run(
        self,
        router: Router,
        shutdown: CancellationToken,
    ) -> Result<(), OAuthTransportError> {
        let handle = axum_server::Handle::new();
        let serve_handle = handle.clone();
        let serve = async move {
            match self.tls {
                Some(tls) => {
                    axum_server::from_tcp_rustls(self.listener, tls)?
                        .handle(serve_handle)
                        .serve(router.into_make_service_with_connect_info::<std::net::SocketAddr>())
                        .await
                }
                None => {
                    axum_server::from_tcp(self.listener)?
                        .handle(serve_handle)
                        .serve(router.into_make_service_with_connect_info::<std::net::SocketAddr>())
                        .await
                }
            }
        };
        tokio::pin!(serve);
        tokio::select! {
            result=&mut serve=>result.map_err(OAuthTransportError::Listener),
            ()=shutdown.cancelled()=> {
                handle.graceful_shutdown(Some(Duration::from_secs(10)));
                serve.await.map_err(OAuthTransportError::Listener)
            },
        }
    }
}
