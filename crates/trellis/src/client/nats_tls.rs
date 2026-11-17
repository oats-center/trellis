//! TLS for Trellis's NATS connections: the options every connection shares,
//! a broker dialled at an address its certificate doesn't name, and which
//! failed connections retrying can't fix.

use std::path::PathBuf;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::TrellisClientError;

/// `options` with TLS left to each server URL, and `root_certificates`
/// trusted, or the broker's certificate checked against `server_name`
/// whatever address is dialled.
///
/// A `tls://` or `wss://` URL, a server whose INFO requires TLS, and a server
/// discovered on a connection with TLS are dialled with TLS; a `nats://` or
/// `ws://` URL to a server that doesn't require it is not. The CA files and
/// name only say what to trust, as Trellis's configuration and discovery
/// choose TLS by the URL scheme. Without this, async-nats would ask every
/// server for TLS once any is set and refuse a `ws://` URL.
///
/// # Errors
///
/// Returns [`TrellisClientError::Bootstrap`] as [`named_server_tls`] does.
pub fn nats_tls_options(
    options: async_nats::ConnectOptions,
    root_certificates: &[PathBuf],
    server_name: Option<&str>,
) -> Result<async_nats::ConnectOptions, TrellisClientError> {
    let options = options.tls_optional();
    Ok(match server_name {
        Some(name) => options.tls_client_config(named_server_tls(root_certificates, name)?),
        None => root_certificates
            .iter()
            .cloned()
            .fold(options, async_nats::ConnectOptions::add_root_certificates),
    })
}

/// Whether a failed NATS connect fails the same way on every attempt with the
/// same settings, so retrying it can't help.
///
/// That is a broker that doesn't offer the TLS asked for, a certificate not
/// issued for the expected name or not chaining to a trusted CA, TLS settings
/// that can't be loaded, and server URLs or options that can't be used. A
/// certificate that has expired or isn't valid yet is not counted: renewing it
/// fixes the broker without changing the client. Timeouts, DNS, I/O and
/// authorization failures can pass.
#[must_use]
pub fn connect_error_is_permanent(error: &async_nats::ConnectError) -> bool {
    use async_nats::ConnectErrorKind;
    use rustls::CertificateError;
    match error.kind() {
        ConnectErrorKind::ServerParse | ConnectErrorKind::Tls | ConnectErrorKind::Configuration => {
            true
        }
        // async-nats reports a certificate that fails to verify for any reason
        // but its name as I/O, carrying the rustls error.
        ConnectErrorKind::Io => {
            let mut source = std::error::Error::source(error);
            while let Some(current) = source {
                let rustls_error = current
                    .downcast_ref::<std::io::Error>()
                    .and_then(|io| io.get_ref())
                    .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                    .or_else(|| current.downcast_ref::<rustls::Error>());
                if let Some(rustls::Error::InvalidCertificate(certificate)) = rustls_error {
                    return !matches!(
                        certificate,
                        CertificateError::Expired
                            | CertificateError::ExpiredContext { .. }
                            | CertificateError::NotValidYet
                            | CertificateError::NotValidYetContext { .. }
                    );
                }
                source = current.source();
            }
            false
        }
        ConnectErrorKind::Dns
        | ConnectErrorKind::Authentication
        | ConnectErrorKind::AuthorizationViolation
        | ConnectErrorKind::TimedOut
        | ConnectErrorKind::MaxReconnects => false,
    }
}

/// A client TLS configuration that checks the broker's certificate against
/// `server_name` whatever address is dialled, such as a runtime reaching its
/// own broker at loopback.
///
/// The certificate is verified in full. It must chain to `root_certificates`
/// when any are given, as with `ConnectOptions::add_root_certificates`, and to
/// the platform's trust store otherwise.
///
/// # Errors
///
/// Returns [`TrellisClientError::Bootstrap`] when `server_name` isn't a valid
/// TLS name or a root certificate can't be read.
fn named_server_tls(
    root_certificates: &[PathBuf],
    server_name: &str,
) -> Result<rustls::ClientConfig, TrellisClientError> {
    let bootstrap = |error: String| TrellisClientError::Bootstrap(error);
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|error| bootstrap(format!("broker name '{server_name}': {error}")))?;
    let inner: Arc<dyn ServerCertVerifier> = if root_certificates.is_empty() {
        Arc::new(
            rustls_platform_verifier::Verifier::new(provider.clone())
                .map_err(|error| bootstrap(error.to_string()))?,
        )
    } else {
        let mut roots = rustls::RootCertStore::empty();
        for path in root_certificates {
            let read = |error: rustls::pki_types::pem::Error| {
                bootstrap(format!("read CA '{}': {error}", path.display()))
            };
            for certificate in CertificateDer::pem_file_iter(path).map_err(read)? {
                roots
                    .add(certificate.map_err(read)?)
                    .map_err(|error| bootstrap(format!("CA '{}': {error}", path.display())))?;
            }
        }
        rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .map_err(|error| bootstrap(error.to_string()))?
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| bootstrap(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NamedServer { inner, name }))
        .with_no_client_auth())
}

/// Verifies every certificate as if `name` had been dialled.
#[derive(Debug)]
struct NamedServer {
    inner: Arc<dyn ServerCertVerifier>,
    name: ServerName<'static>,
}

impl ServerCertVerifier for NamedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.inner
            .verify_server_cert(end_entity, intermediates, &self.name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner
            .verify_tls12_signature(message, certificate, signature)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner
            .verify_tls13_signature(message, certificate, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
