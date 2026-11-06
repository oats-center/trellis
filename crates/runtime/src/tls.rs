//! Inspection of installation TLS certificates for operator and user warnings.

pub use trellis_bootstrap::{inspect_certificate, CertificateError, CertificateSummary};

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
            "Trellis is serving certificates it generated itself, which suit \
            development only. Browsers and clients ask before accepting them; \
            accept only if the issuing CA has this fingerprint, and renew leaf \
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
