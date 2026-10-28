//! TLS trust for deployments that serve a self-generated certificate.
//!
//! Like SSH host keys, a certificate nothing vouches for is shown by its
//! issuer's fingerprint on first login and remembered per deployment once the
//! user accepts it. A later login that presents a different one is refused.

use std::path::{Path, PathBuf};

use miette::{miette, IntoDiagnostic};
use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};
use trellis_rs::auth as authlib;

use crate::cli::OutputFormat;
use crate::output;

/// Discovers the deployment's OAuth server over TLS the user trusts, returning
/// the CA files that trust rests on (empty when system roots suffice).
pub(super) async fn discover(
    format: OutputFormat,
    trellis_url: &str,
    ca_certificate: Option<&Path>,
    ca_fingerprint: Option<&str>,
) -> miette::Result<(authlib::NativeOAuthServer, Vec<PathBuf>)> {
    if let Some(path) = ca_certificate {
        let path = path.canonicalize().into_diagnostic()?;
        let ca = pem_fingerprint(&path)?;
        if !output::is_json(format) {
            output::print_warning(&format!(
                "trusting only the CA {} for this deployment; confirm its SHA-256 \
                fingerprint matches the one its operator published:\n  {}",
                path.display(),
                ca
            ));
        }
        let roots = vec![path];
        let server = authlib::discover_native_oauth(trellis_url, &roots)
            .await
            .into_diagnostic()?;
        return Ok((server, roots));
    }
    let acceptance = authlib::CertificateAcceptance {
        directory: super::native_auth::directory()?.with_file_name("trusted-ca"),
        fingerprint: ca_fingerprint.map(str::to_owned),
        interactive: !output::is_json(format),
    };
    match authlib::discover_accepting_certificate(trellis_url, &acceptance).await {
        Ok(server) => {
            let roots = server.trust.root_certificates.clone();
            Ok((server, roots))
        }
        Err(error) => {
            // Fingerprints are reported on their own as well, so scripts can
            // compare and copy them whole.
            if output::is_json(format) {
                match &error {
                    authlib::CertificateAcceptanceError::Untrusted {
                        server,
                        fingerprint,
                    } => output::print_json_progress(&serde_json::json!({
                        "untrustedCa": { "origin": server, "presented": fingerprint }
                    }))?,
                    authlib::CertificateAcceptanceError::Changed {
                        server,
                        accepted,
                        presented,
                        file,
                    } => output::print_json_progress(&serde_json::json!({
                        "pinnedCaMismatch": {
                            "origin": server,
                            "pinned": accepted,
                            "presented": presented,
                            "pin": file,
                        }
                    }))?,
                    _ => {}
                }
            }
            match error {
                authlib::CertificateAcceptanceError::Untrusted { .. } => {
                    Err(miette!("{error}; or log in again with --ca-fingerprint"))
                }
                error => Err(miette!("{error}")),
            }
        }
    }
}

/// The SHA-256 fingerprint of the first certificate in a PEM file.
fn pem_fingerprint(path: &Path) -> miette::Result<String> {
    use rustls::pki_types::pem::PemObject as _;
    let certificate = CertificateDer::from_pem_file(path)
        .map_err(|error| miette!("{} is not a PEM certificate: {error}", path.display()))?;
    Ok(Sha256::digest(certificate.as_ref())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}
