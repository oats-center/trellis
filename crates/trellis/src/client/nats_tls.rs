//! TLS for a broker dialled at an address its certificate doesn't name.

use std::path::PathBuf;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::TrellisClientError;

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
pub fn named_server_tls(
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
