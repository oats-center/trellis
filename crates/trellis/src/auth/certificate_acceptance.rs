//! Accepting a Trellis certificate nothing vouches for, the way a browser does.
//!
//! A development installation serves certificates from a CA it generated
//! itself. Rather than installing that CA, a program fetches the certificate
//! the server presents, shows its issuer's SHA-256 fingerprint, and remembers
//! the answer per program and server, like SSH host keys. A later different
//! certificate for that server is refused.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest as _, Sha256};

use super::{discover_native_oauth, NativeOAuthError, NativeOAuthServer};

/// Accepts only a certificate whose issuer has this SHA-256 fingerprint.
pub const CERTIFICATE_FINGERPRINT_ENV: &str = "TRELLIS_CERTIFICATE_FINGERPRINT";
/// When `1`, accepts whatever certificate the server presents on first contact.
pub const ACCEPT_SELF_SIGNED_ENV: &str = "TRELLIS_ACCEPT_SELF_SIGNED";

/// How [`discover_accepting_certificate`] may accept an unvouched certificate.
#[derive(Clone, Debug)]
pub struct CertificateAcceptance {
    /// This program's folder for accepted certificates, one file per server.
    pub directory: PathBuf,
    /// Accept only this issuer fingerprint. When `None`,
    /// [`CERTIFICATE_FINGERPRINT_ENV`] supplies one.
    pub fingerprint: Option<String>,
    /// Ask on the terminal when nothing else decides.
    pub interactive: bool,
}

impl CertificateAcceptance {
    /// Remembers accepted certificates for `program` under the user's state
    /// folder (`$XDG_STATE_HOME/trellis/<program>/servers`), asking on a
    /// terminal when one is attached.
    #[must_use]
    pub fn for_program(program: &str) -> Self {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .unwrap_or_else(std::env::temp_dir);
        Self {
            directory: state.join("trellis").join(program).join("servers"),
            fingerprint: None,
            interactive: true,
        }
    }
}

/// Why a server's certificate wasn't accepted.
#[derive(Debug, thiserror::Error)]
pub enum CertificateAcceptanceError {
    /// Discovery failed for a reason other than the certificate.
    #[error(transparent)]
    Discovery(#[from] NativeOAuthError),
    /// Nothing accepted the certificate, and there was no terminal to ask on.
    #[error(
        "{server} presents a certificate nothing on this machine vouches for; its issuer's \
        SHA-256 fingerprint is {fingerprint}. After confirming it with the operator, set \
        {CERTIFICATE_FINGERPRINT_ENV} to it, or set {ACCEPT_SELF_SIGNED_ENV}=1 for development"
    )]
    Untrusted {
        /// `host:port` of the server.
        server: String,
        /// The presented issuer's fingerprint.
        fingerprint: String,
    },
    /// The presented certificate doesn't match the expected fingerprint.
    #[error("{server} presents issuer fingerprint {presented}, not the expected {expected}")]
    Mismatch {
        /// `host:port` of the server.
        server: String,
        /// The fingerprint that was expected.
        expected: String,
        /// The fingerprint the server presented.
        presented: String,
    },
    /// The operator answered no.
    #[error("the certificate for {server} was not accepted")]
    Declined {
        /// `host:port` of the server.
        server: String,
    },
    /// The server now presents a different certificate than the accepted one.
    #[error(
        "{server} presents a different certificate than the one accepted for it \
        (accepted {accepted}, presented {presented}); someone may be intercepting the \
        connection. If the operator replaced it, delete {} and connect again",
        file.display()
    )]
    Changed {
        /// `host:port` of the server.
        server: String,
        /// The accepted issuer's fingerprint.
        accepted: String,
        /// The fingerprint the server presented.
        presented: String,
        /// The file holding the accepted certificate.
        file: PathBuf,
    },
    /// Reading or saving the accepted certificate failed.
    #[error("remember the accepted certificate: {0}")]
    Io(#[from] std::io::Error),
}

/// Discovers a Trellis like [`discover_native_oauth`], accepting a certificate
/// nothing vouches for the way a browser would.
///
/// A server the platform trust store vouches for needs nothing. Otherwise the
/// certificate accepted earlier for this server is used; a different one is
/// refused. On first contact the issuer's fingerprint must match
/// `acceptance.fingerprint` (or [`CERTIFICATE_FINGERPRINT_ENV`]), or
/// [`ACCEPT_SELF_SIGNED_ENV`] must be `1`, or the operator accepts it on the
/// terminal. The accepted certificate is saved in `acceptance.directory` and
/// travels in the returned trust, so the broker connection uses it too.
///
/// # Errors
///
/// Returns [`CertificateAcceptanceError`] when the certificate isn't accepted
/// or discovery fails.
pub async fn discover_accepting_certificate(
    issuer: &str,
    acceptance: &CertificateAcceptance,
) -> Result<NativeOAuthServer, CertificateAcceptanceError> {
    let url = url::Url::parse(issuer).map_err(|_| NativeOAuthError::InvalidResponse)?;
    let Some(host) = url
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_owned())
    else {
        return Err(NativeOAuthError::InvalidResponse.into());
    };
    if url.scheme() != "https" {
        return Ok(discover_native_oauth(issuer, &[]).await?);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let server = format!("{host}:{port}");
    let file = acceptance
        .directory
        .join(format!("{}_{port}.pem", host.replace(':', "_")));

    if file.is_file() {
        return match discover_native_oauth(issuer, std::slice::from_ref(&file)).await {
            Ok(discovered) => Ok(discovered),
            Err(error) => {
                let accepted = pem_fingerprint(&file)?;
                match presented_issuer(&host, port)
                    .await
                    .map(|ca| fingerprint(&ca))
                {
                    Some(presented) if presented != accepted => {
                        Err(CertificateAcceptanceError::Changed {
                            server,
                            accepted,
                            presented,
                            file,
                        })
                    }
                    _ => Err(error.into()),
                }
            }
        };
    }
    let error = match discover_native_oauth(issuer, &[]).await {
        Ok(discovered) => return Ok(discovered),
        Err(error) => error,
    };
    let Some(presented) = presented_issuer(&host, port).await else {
        return Err(error.into());
    };
    let presented_fingerprint = fingerprint(&presented);
    let expected = acceptance
        .fingerprint
        .clone()
        .or_else(|| std::env::var(CERTIFICATE_FINGERPRINT_ENV).ok())
        .filter(|value| !value.trim().is_empty());
    if let Some(expected) = expected {
        let normalize = |value: &str| value.replace(':', "").trim().to_ascii_uppercase();
        if normalize(&expected) != normalize(&presented_fingerprint) {
            return Err(CertificateAcceptanceError::Mismatch {
                server,
                expected,
                presented: presented_fingerprint,
            });
        }
    } else if std::env::var(ACCEPT_SELF_SIGNED_ENV).is_ok_and(|value| value.trim() == "1") {
        eprintln!(
            "Accepting {server}'s certificate because {ACCEPT_SELF_SIGNED_ENV}=1; its issuer's \
            SHA-256 fingerprint is\n  {presented_fingerprint}"
        );
    } else if acceptance.interactive
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
    {
        eprint!(
            "{server} presents a certificate nothing on this machine vouches for.\n\
            Confirm its issuer's SHA-256 fingerprint with the operator:\n  \
            {presented_fingerprint}\nAccept it? [y/N] "
        );
        std::io::stderr().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err(CertificateAcceptanceError::Declined { server });
        }
    } else {
        return Err(CertificateAcceptanceError::Untrusted {
            server,
            fingerprint: presented_fingerprint,
        });
    }

    remember(&file, &presented)?;
    Ok(discover_native_oauth(issuer, std::slice::from_ref(&file)).await?)
}

/// The colon-separated SHA-256 fingerprint operators are shown.
fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    Sha256::digest(certificate.as_ref())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The fingerprint of the first certificate in a PEM file.
fn pem_fingerprint(path: &Path) -> Result<String, CertificateAcceptanceError> {
    use rustls::pki_types::pem::PemObject as _;
    let certificate = CertificateDer::from_pem_file(path).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a PEM certificate: {error}", path.display()),
        )
    })?;
    Ok(fingerprint(&certificate))
}

/// Saves `certificate` as PEM, atomically, in a folder only this user reads.
fn remember(file: &Path, certificate: &CertificateDer<'_>) -> std::io::Result<()> {
    let directory = file
        .parent()
        .ok_or_else(|| std::io::Error::other("the accepted certificate needs a folder"))?;
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).map_err(std::io::Error::other)?);
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    let staged = file.with_extension(format!("pem.{}", std::process::id()));
    {
        let mut out = std::fs::File::create(&staged)?;
        out.write_all(pem.as_bytes())?;
        out.sync_all()?;
    }
    std::fs::rename(&staged, file)
}

/// The issuer that ends the chain the server presents, without trusting it.
///
/// `None` when the server can't be reached or presents only its leaf.
async fn presented_issuer(host: &str, port: u16) -> Option<CertificateDer<'static>> {
    let host = host.to_owned();
    tokio::task::spawn_blocking(move || {
        let verifier = Arc::new(RecordChain(Mutex::new(Vec::new())));
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
        let name = ServerName::try_from(host.clone()).ok()?;
        let mut connection = rustls::ClientConnection::new(Arc::new(config), name).ok()?;
        let mut socket = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))
            .ok()?
            .find_map(|address| {
                std::net::TcpStream::connect_timeout(&address, Duration::from_secs(10)).ok()
            })?;
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        while connection.is_handshaking() {
            connection.complete_io(&mut socket).ok()?;
        }
        let chain = verifier.0.lock().ok()?;
        (chain.len() > 1).then(|| chain.last().cloned()).flatten()
    })
    .await
    .ok()
    .flatten()
}

/// Records the presented chain; the connection carries no data.
#[derive(Debug)]
struct RecordChain(Mutex<Vec<CertificateDer<'static>>>);

impl ServerCertVerifier for RecordChain {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut chain = self
            .0
            .lock()
            .map_err(|_| rustls::Error::General("certificate chain lock poisoned".into()))?;
        chain.clear();
        chain.push(end_entity.clone().into_owned());
        chain.extend(intermediates.iter().map(|value| value.clone().into_owned()));
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
