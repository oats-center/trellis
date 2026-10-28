//! Inspection of installation TLS certificates for operator and user warnings.

use std::net::IpAddr;
use std::path::Path;

use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use x509_parser::extensions::GeneralName;

/// Identity of one PEM certificate, used to verify trust out of band.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateSummary {
    /// Uppercase colon-separated SHA-256 fingerprint of the DER certificate,
    /// matching the format browsers and `openssl x509 -fingerprint` display.
    pub sha256_fingerprint: String,
    /// End of the certificate validity period.
    pub not_after: OffsetDateTime,
    /// DNS subject alternative names.
    pub dns_names: Vec<String>,
    /// IP address subject alternative names.
    pub ip_addresses: Vec<IpAddr>,
}

/// Failure to read or parse a PEM certificate.
#[derive(Debug, thiserror::Error)]
pub enum CertificateError {
    /// The certificate file could not be read.
    #[error("read certificate '{path}': {source}")]
    Read {
        /// Certificate path.
        path: std::path::PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The file does not begin with a parseable PEM X.509 certificate.
    #[error("certificate '{path}' is not a PEM X.509 certificate")]
    Parse {
        /// Certificate path.
        path: std::path::PathBuf,
    },
}

/// Reads the first certificate of a PEM file and summarizes its identity.
///
/// # Errors
///
/// Returns [`CertificateError`] when the file cannot be read or does not begin
/// with a valid PEM X.509 certificate.
pub fn inspect_certificate(path: &Path) -> Result<CertificateSummary, CertificateError> {
    let bytes = std::fs::read(path).map_err(|source| CertificateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let parse = || CertificateError::Parse {
        path: path.to_path_buf(),
    };
    let (_, pem) = x509_parser::pem::parse_x509_pem(&bytes).map_err(|_| parse())?;
    let certificate = pem.parse_x509().map_err(|_| parse())?;
    let mut dns_names = Vec::new();
    let mut ip_addresses = Vec::new();
    if let Ok(Some(names)) = certificate.subject_alternative_name() {
        for name in &names.value.general_names {
            match name {
                GeneralName::DNSName(name) => dns_names.push((*name).to_owned()),
                GeneralName::IPAddress(bytes) => match bytes.len() {
                    4 => ip_addresses.push(IpAddr::from(
                        <[u8; 4]>::try_from(*bytes).map_err(|_| parse())?,
                    )),
                    16 => ip_addresses.push(IpAddr::from(
                        <[u8; 16]>::try_from(*bytes).map_err(|_| parse())?,
                    )),
                    _ => return Err(parse()),
                },
                _ => {}
            }
        }
    }
    let digest = Sha256::digest(&pem.contents);
    Ok(CertificateSummary {
        sha256_fingerprint: digest
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
        not_after: certificate.validity().not_after.to_datetime(),
        dns_names,
        ip_addresses,
    })
}

/// Certificate identities reported while serving Trellis-generated TLS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedTlsNotice {
    /// The installation CA users must trust.
    pub ca: CertificateSummary,
    /// The application listener's current leaf certificate.
    pub leaf: Option<CertificateSummary>,
}

impl GeneratedTlsNotice {
    /// Reads the generated CA named by `config` and the leaf `transport` serves.
    ///
    /// Returns `None` when the installation does not use Trellis-generated TLS.
    ///
    /// # Errors
    ///
    /// Returns [`CertificateError`] when a configured certificate cannot be read.
    pub fn from_config(
        config: &crate::RuntimeConfig,
        transport: &crate::ListenerTransport,
    ) -> Result<Option<Self>, CertificateError> {
        let Some(ca) = config
            .tls
            .as_ref()
            .and_then(|tls| tls.generated_ca_certificate.as_deref())
        else {
            return Ok(None);
        };
        let leaf = match transport {
            crate::ListenerTransport::Tls {
                certificate_chain, ..
            } => Some(inspect_certificate(certificate_chain)?),
            _ => None,
        };
        Ok(Some(Self {
            ca: inspect_certificate(ca)?,
            leaf,
        }))
    }

    /// Logs the always-on operator warning for a private generated CA.
    pub fn warn(&self) {
        let format = time::format_description::well_known::Rfc3339;
        let leaf_expires = self
            .leaf
            .as_ref()
            .and_then(|leaf| leaf.not_after.format(&format).ok());
        tracing::warn!(
            security_event = "generated_tls_in_use",
            ca_sha256_fingerprint = %self.ca.sha256_fingerprint,
            leaf_not_after = leaf_expires.as_deref().unwrap_or("not served by this listener"),
            "Trellis is serving certificates from its generated installation CA. \
            Browsers and native clients must trust that CA explicitly; verify the \
            fingerprint out of band before trusting it, and renew leaf \
            certificates before they expire with `trellis-server tls renew`."
        );
    }
}

/// Whether `transport` serves a certificate issued by the Trellis-generated
/// installation CA, so browser users see the not-for-production banner.
pub(crate) fn serves_generated_certificate(
    config: &crate::RuntimeConfig,
    transport: &crate::ListenerTransport,
) -> bool {
    config
        .tls
        .as_ref()
        .is_some_and(|tls| tls.generated_ca_certificate.is_some())
        && matches!(transport, crate::ListenerTransport::Tls { .. })
}
