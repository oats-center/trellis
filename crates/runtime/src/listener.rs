//! Serving shared by the application and OAuth listeners.
//!
//! Every listener selects one [`ListenerTransport`]. TLS material is loaded
//! before the listener serves and replaced files are picked up without a
//! restart; a failed load or reload never falls back to plaintext.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;

use crate::config::ListenerTransport;

/// How often TLS certificate files are checked for replacement.
const TLS_RELOAD_INTERVAL: Duration = Duration::from_secs(5);

/// A bound listener with its transport loaded and ready to serve.
pub(crate) struct LoadedListener {
    listener: std::net::TcpListener,
    mode: Mode,
    forwarded_by: Arc<[ipnet::IpNet]>,
}

enum Mode {
    Tls {
        config: RustlsConfig,
        certificate_chain: PathBuf,
        private_key: PathBuf,
    },
    Proxy(Arc<[ipnet::IpNet]>),
    Plaintext,
}

impl LoadedListener {
    /// Loads `transport` for `listener` so certificate failures surface before readiness.
    ///
    /// A TLS leaf must be currently valid and name `origin`'s host; serving a
    /// certificate every client would reject is a startup error.
    pub(crate) async fn load(
        listener: tokio::net::TcpListener,
        origin: &url::Url,
        transport: &ListenerTransport,
    ) -> std::io::Result<Self> {
        let mode = match transport {
            ListenerTransport::Tls {
                certificate_chain,
                private_key,
            } => {
                check_leaf(certificate_chain, origin)?;
                if rustls::crypto::CryptoProvider::get_default().is_none() {
                    let _ = rustls::crypto::ring::default_provider().install_default();
                }
                let config = RustlsConfig::from_pem_file(certificate_chain, private_key)
                    .await
                    .map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!(
                                "load TLS certificate '{}' and key '{}': {error}",
                                certificate_chain.display(),
                                private_key.display()
                            ),
                        )
                    })?;
                Mode::Tls {
                    config,
                    certificate_chain: certificate_chain.clone(),
                    private_key: private_key.clone(),
                }
            }
            ListenerTransport::Proxy { trusted_proxies } => {
                Mode::Proxy(trusted_proxies.as_slice().into())
            }
            ListenerTransport::LoopbackDevelopment => Mode::Plaintext,
        };
        Ok(Self {
            listener: listener.into_std()?,
            mode,
            forwarded_by: Arc::new([]),
        })
    }

    /// Accepts `X-Forwarded-For` from `trusted` peers of a listener that also
    /// faces clients directly, such as TLS behind a re-encrypting proxy. Other
    /// peers are served as themselves. A `proxy` transport already does this
    /// for its own proxies and refuses every other peer.
    pub(crate) fn forwarded_by(mut self, trusted: &[ipnet::IpNet]) -> Self {
        self.forwarded_by = trusted.into();
        self
    }

    /// Serves `router` until `shutdown` resolves, then drains for up to ten seconds.
    pub(crate) async fn serve(
        self,
        router: Router,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let handle = axum_server::Handle::new();
        let drain = handle.clone();
        let drain = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            shutdown.await;
            drain.graceful_shutdown(Some(Duration::from_secs(10)));
        }));
        let router = if self.forwarded_by.is_empty() {
            router
        } else {
            let trusted = self.forwarded_by;
            router.layer(axum::middleware::from_fn(move |request, next| {
                forward(Arc::clone(&trusted), false, request, next)
            }))
        };
        let result = match self.mode {
            Mode::Tls {
                config,
                certificate_chain,
                private_key,
            } => {
                let _reload = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                    reload_certificates(config.clone(), certificate_chain, private_key),
                ));
                axum_server::from_tcp_rustls(self.listener, config)?
                    .handle(handle)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
            Mode::Proxy(trusted) => {
                let router = router.layer(axum::middleware::from_fn(move |request, next| {
                    forward(Arc::clone(&trusted), true, request, next)
                }));
                axum_server::from_tcp(self.listener)?
                    .handle(handle)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
            Mode::Plaintext => {
                axum_server::from_tcp(self.listener)?
                    .handle(handle)
                    .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
        };
        drop(drain);
        result
    }
}

/// Rejects an expired leaf or one that does not name the origin host.
fn check_leaf(certificate_chain: &std::path::Path, origin: &url::Url) -> std::io::Result<()> {
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let leaf = crate::tls::inspect_certificate(certificate_chain)
        .map_err(|error| invalid(error.to_string()))?;
    if leaf.not_after <= time::OffsetDateTime::now_utc() {
        return Err(invalid(format!(
            "TLS certificate '{}' expired at {}; renew it (generated certificates: \
            `trellis tls renew`) before starting",
            certificate_chain.display(),
            leaf.not_after
        )));
    }
    let covered = match origin.host() {
        Some(url::Host::Ipv4(ip)) => leaf.ip_addresses.contains(&IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => leaf.ip_addresses.contains(&IpAddr::V6(ip)),
        Some(url::Host::Domain(host)) => leaf.dns_names.iter().any(|name| {
            name.eq_ignore_ascii_case(host)
                || name.strip_prefix("*.").is_some_and(|suffix| {
                    host.split_once('.')
                        .is_some_and(|(_, parent)| parent.eq_ignore_ascii_case(suffix))
                })
        }),
        None => false,
    };
    if !covered {
        return Err(invalid(format!(
            "TLS certificate '{}' does not name origin host '{}'; issue a \
            certificate for this hostname",
            certificate_chain.display(),
            origin.host_str().unwrap_or_default()
        )));
    }
    Ok(())
}

/// Replaces the served certificate when renewal rewrites its files.
///
/// A replacement that fails to load is logged and the previous certificate
/// keeps serving until valid files appear.
async fn reload_certificates(
    config: RustlsConfig,
    certificate_chain: PathBuf,
    private_key: PathBuf,
) {
    let modified = |path: &PathBuf| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    };
    let mut loaded: (Option<SystemTime>, Option<SystemTime>) =
        (modified(&certificate_chain), modified(&private_key));
    let mut interval = tokio::time::interval(TLS_RELOAD_INTERVAL);
    interval.tick().await;
    loop {
        interval.tick().await;
        let current = (modified(&certificate_chain), modified(&private_key));
        if current == loaded {
            continue;
        }
        match config
            .reload_from_pem_file(&certificate_chain, &private_key)
            .await
        {
            Ok(()) => {
                loaded = current;
                tracing::info!(
                    certificate = %certificate_chain.display(),
                    "reloaded TLS certificate"
                );
            }
            Err(error) => tracing::warn!(
                certificate = %certificate_chain.display(),
                error = %error,
                "replacement TLS certificate did not load; continuing with the previous certificate"
            ),
        }
    }
}

/// Identifies the client a trusted peer forwarded, replacing [`ConnectInfo`]
/// so per-client limits key on the real client rather than on the proxy.
///
/// An untrusted peer is refused when `refuse_untrusted`, and otherwise served
/// as itself. A malformed `X-Forwarded-For` from a trusted peer is rejected.
async fn forward(
    trusted: Arc<[ipnet::IpNet]>,
    refuse_untrusted: bool,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let Some(peer) = peer.filter(|peer| trusted.iter().any(|network| network.contains(peer)))
    else {
        if !refuse_untrusted {
            return next.run(request).await;
        }
        tracing::warn!(
            peer = ?peer,
            "refused a connection from outside the configured trusted_proxies"
        );
        return StatusCode::FORBIDDEN.into_response();
    };
    let Some(client) = forwarded_client(&trusted, peer, request.headers()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(client, 0)));
    next.run(request).await
}

/// Reads `X-Forwarded-For` right to left from a trusted `peer`: entries
/// appended by trusted proxies are skipped and the first untrusted address is
/// the client. `None` means the header is malformed.
fn forwarded_client(trusted: &[ipnet::IpNet], peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
    let is_trusted = |ip: &IpAddr| trusted.iter().any(|network| network.contains(ip));
    let mut forwarded = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        forwarded.extend(value.to_str().ok()?.split(',').map(str::trim));
    }
    let mut client = peer;
    for entry in forwarded.into_iter().rev() {
        if !is_trusted(&client) {
            break;
        }
        client = entry.parse().ok()?;
    }
    Some(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    fn origin(value: &str) -> url::Url {
        url::Url::parse(value).unwrap()
    }

    async fn serve(
        transport: ListenerTransport,
        origin: &url::Url,
    ) -> (SocketAddr, tokio_util::sync::CancellationToken) {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let listener = LoadedListener::load(socket, origin, &transport)
            .await
            .unwrap();
        let router = Router::new().route(
            "/client",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        let stop = tokio_util::sync::CancellationToken::new();
        tokio::spawn(listener.serve(router, stop.clone().cancelled_owned()));
        (address, stop)
    }

    #[tokio::test]
    async fn proxy_transport_serves_only_trusted_proxies_and_keys_on_forwarded_client() {
        let (address, stop) = serve(
            ListenerTransport::Proxy {
                trusted_proxies: vec![
                    "127.0.0.2/32".parse().unwrap(),
                    "10.0.0.0/8".parse().unwrap(),
                ],
            },
            &origin("https://trellis.example"),
        )
        .await;
        let url = format!("http://{address}/client");
        let _ = rustls::crypto::ring::default_provider().install_default();
        let direct = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            direct.get(&url).send().await.unwrap().status(),
            StatusCode::FORBIDDEN,
            "a peer outside the trusted proxies must not reach the plaintext listener"
        );
        let proxy = reqwest::Client::builder()
            .no_proxy()
            .local_address(IpAddr::from([127, 0, 0, 2]))
            .build()
            .unwrap();
        let client =
            |forwarded: &'static str| proxy.get(&url).header("x-forwarded-for", forwarded).send();
        // The rightmost entries were appended by trusted proxies; a spoofed
        // leftmost entry supplied by the client is never selected.
        let response = client("203.0.113.9, 198.51.100.7, 10.1.2.3").await.unwrap();
        assert_eq!(response.text().await.unwrap(), "198.51.100.7");
        let response = proxy.get(&url).send().await.unwrap();
        assert_eq!(response.text().await.unwrap(), "127.0.0.2");
        assert_eq!(
            client("not-an-address").await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        stop.cancel();
    }

    struct Authority {
        issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
        certificate: String,
    }

    fn authority() -> Authority {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = params.self_signed(&key).unwrap().pem();
        Authority {
            issuer: rcgen::Issuer::new(params, key),
            certificate,
        }
    }

    fn write_leaf(
        authority: &Authority,
        directory: &std::path::Path,
        host: &str,
        not_after: time::OffsetDateTime,
    ) -> (PathBuf, PathBuf) {
        let mut params = rcgen::CertificateParams::new(vec![host.to_owned()]).unwrap();
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(2);
        params.not_after = not_after;
        let key = rcgen::KeyPair::generate().unwrap();
        let certificate = params.signed_by(&key, &authority.issuer).unwrap();
        let chain = directory.join("leaf.crt");
        let private_key = directory.join("leaf.key");
        std::fs::write(&private_key, key.serialize_pem()).unwrap();
        std::fs::write(&chain, certificate.pem()).unwrap();
        (chain, private_key)
    }

    fn trusting(authority: &Authority) -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .no_proxy()
            .tls_info(true)
            // Each request performs a fresh handshake so a reload is observable.
            .pool_max_idle_per_host(0)
            .tls_certs_only([
                reqwest::Certificate::from_pem(authority.certificate.as_bytes()).unwrap(),
            ])
            .resolve("trellis.test", SocketAddr::from(([127, 0, 0, 1], 0)))
            .build()
            .unwrap()
    }

    async fn served_leaf(client: &reqwest::Client, address: SocketAddr) -> Vec<u8> {
        let response = client
            .get(format!("https://trellis.test:{}/client", address.port()))
            .send()
            .await
            .unwrap();
        response
            .extensions()
            .get::<reqwest::tls::TlsInfo>()
            .and_then(|info| info.peer_certificate().map(<[u8]>::to_vec))
            .unwrap()
    }

    #[tokio::test]
    async fn tls_transport_reloads_renewed_certificates_without_restart() {
        let directory = tempfile::tempdir().unwrap();
        let authority = authority();
        let later = time::OffsetDateTime::now_utc() + time::Duration::days(30);
        let (certificate_chain, private_key) =
            write_leaf(&authority, directory.path(), "trellis.test", later);
        let (address, stop) = serve(
            ListenerTransport::Tls {
                certificate_chain,
                private_key,
            },
            &origin("https://trellis.test"),
        )
        .await;
        let client = trusting(&authority);
        let first = served_leaf(&client, address).await;
        write_leaf(&authority, directory.path(), "trellis.test", later);
        let renewed = tokio::time::timeout(TLS_RELOAD_INTERVAL * 4, async {
            loop {
                let current = served_leaf(&client, address).await;
                if current != first {
                    return current;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("the listener must serve the renewed certificate without restarting");
        assert_ne!(renewed, first);
        stop.cancel();
    }

    #[tokio::test]
    async fn tls_transport_refuses_expired_or_misnamed_certificates_before_serving() {
        let directory = tempfile::tempdir().unwrap();
        let authority = authority();
        for (host, not_after, expected) in [
            (
                "trellis.test",
                time::OffsetDateTime::now_utc() - time::Duration::hours(1),
                "expired",
            ),
            (
                "elsewhere.test",
                time::OffsetDateTime::now_utc() + time::Duration::days(30),
                "does not name",
            ),
        ] {
            let (certificate_chain, private_key) =
                write_leaf(&authority, directory.path(), host, not_after);
            let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let error = LoadedListener::load(
                socket,
                &origin("https://trellis.test"),
                &ListenerTransport::Tls {
                    certificate_chain,
                    private_key,
                },
            )
            .await
            .err()
            .expect("an unusable certificate must fail before readiness");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
