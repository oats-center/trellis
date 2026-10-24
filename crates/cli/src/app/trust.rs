//! TLS trust for deployments that serve a self-generated installation CA.
//!
//! Like SSH host keys, a CA no system root vouches for is shown by fingerprint
//! on first login and pinned per deployment origin once the user accepts it.
//! A later login whose deployment presents a different CA is a hard error.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use miette::{miette, IntoDiagnostic};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
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
        let ca = trellis_runtime::tls::inspect_certificate(&path).into_diagnostic()?;
        if !output::is_json(format) {
            output::print_warning(&format!(
                "trusting only the CA {} for this deployment; confirm its SHA-256 \
                fingerprint matches the one its operator published:\n  {}",
                path.display(),
                ca.sha256_fingerprint
            ));
        }
        let roots = vec![path];
        let server = authlib::discover_native_oauth(trellis_url, &roots)
            .await
            .into_diagnostic()?;
        return Ok((server, roots));
    }
    let origin = url::Url::parse(trellis_url).into_diagnostic()?;
    if origin.scheme() != "https" {
        let server = authlib::discover_native_oauth(trellis_url, &[])
            .await
            .into_diagnostic()?;
        return Ok((server, Vec::new()));
    }
    let host = origin
        .host_str()
        .ok_or_else(|| miette!("{trellis_url} has no host"))?
        .to_owned();
    let port = origin.port_or_known_default().unwrap_or(443);
    let pin = super::native_auth::directory()?
        .with_file_name("trusted-ca")
        .join(format!("{host}_{port}.pem"));

    // Fingerprints are reported on their own, never inside a wrapped error
    // message, so they can be compared and copied whole.
    let report = |kind: &str, fields: serde_json::Value, text: String| {
        if output::is_json(format) {
            output::print_json_progress(&serde_json::json!({ kind: fields }))
        } else {
            output::print_warning(&text);
            Ok(())
        }
    };
    if pin.is_file() {
        let roots = vec![pin.clone()];
        return match authlib::discover_native_oauth(trellis_url, &roots).await {
            Ok(server) => Ok((server, roots)),
            Err(error) => {
                let pinned = trellis_runtime::tls::inspect_certificate(&pin)
                    .into_diagnostic()?
                    .sha256_fingerprint;
                match offered_ca(&host, port)
                    .await
                    .map(|offered| fingerprint(&offered))
                {
                    Some(presented) if presented != pinned => {
                        report(
                            "pinnedCaMismatch",
                            serde_json::json!({
                                "origin": format!("{host}:{port}"),
                                "pinned": pinned,
                                "presented": presented,
                                "pin": pin,
                            }),
                            format!(
                                "{host}:{port} presents a different CA than the one pinned \
                                for it.\n  pinned:    {pinned}\n  presented: {presented}\n\
                                If the operator replaced the installation CA, confirm the \
                                new fingerprint with them, delete {}, and log in again.",
                                pin.display()
                            ),
                        )?;
                        Err(miette!(
                            "{host}:{port} no longer presents its pinned CA; someone may \
                            be intercepting the connection"
                        ))
                    }
                    _ => Err(error).into_diagnostic(),
                }
            }
        };
    }

    let error = match authlib::discover_native_oauth(trellis_url, &[]).await {
        Ok(server) => return Ok((server, Vec::new())),
        Err(error) => error,
    };
    let Some(offered) = offered_ca(&host, port).await else {
        return Err(error).into_diagnostic();
    };
    let presented = fingerprint(&offered);
    report(
        "untrustedCa",
        serde_json::json!({ "origin": format!("{host}:{port}"), "presented": presented }),
        format!(
            "{host}:{port} uses a certificate authority this system does not trust.\n\
            Confirm its SHA-256 fingerprint with the deployment's operator:\n  {presented}"
        ),
    )?;
    let accepted = match ca_fingerprint {
        Some(expected) => {
            let normalize = |value: &str| value.replace(':', "").to_ascii_uppercase();
            if normalize(expected) != normalize(&presented) {
                return Err(miette!(
                    "{host}:{port} presents a different CA than --ca-fingerprint; nothing \
                    was trusted"
                ));
            }
            true
        }
        None if !output::is_json(format) && std::io::stdin().is_terminal() => {
            eprint!("Trust this CA for {host}:{port}? [y/N] ");
            std::io::stderr().flush().into_diagnostic()?;
            let mut answer = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut answer)
                .into_diagnostic()?;
            matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes")
        }
        None => {
            return Err(miette!(
                "{host}:{port} presents an untrusted CA; after confirming its fingerprint, \
                log in again with --ca-fingerprint"
            ))
        }
    };
    if !accepted {
        return Err(miette!("the CA for {host}:{port} was not trusted"));
    }

    let directory = pin.parent().expect("pin has a parent");
    std::fs::create_dir_all(directory).into_diagnostic()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .into_diagnostic()?;
    }
    let mut staged = tempfile::NamedTempFile::new_in(directory).into_diagnostic()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(offered.as_ref());
    staged
        .write_all(b"-----BEGIN CERTIFICATE-----\n")
        .into_diagnostic()?;
    for line in encoded.as_bytes().chunks(64) {
        staged.write_all(line).into_diagnostic()?;
        staged.write_all(b"\n").into_diagnostic()?;
    }
    staged
        .write_all(b"-----END CERTIFICATE-----\n")
        .into_diagnostic()?;
    staged.as_file().sync_all().into_diagnostic()?;
    staged
        .persist(&pin)
        .map_err(|error| error.error)
        .into_diagnostic()?;
    let roots = vec![pin];
    let server = authlib::discover_native_oauth(trellis_url, &roots)
        .await
        .into_diagnostic()?;
    Ok((server, roots))
}

/// SHA-256 fingerprint in the colon-separated form operators are shown.
fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    Sha256::digest(certificate.as_ref())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The CA that ends the chain the server presents, without trusting it.
///
/// `None` when the server cannot be reached or presents only its leaf.
async fn offered_ca(host: &str, port: u16) -> Option<CertificateDer<'static>> {
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
        let name = ServerName::try_from(host.trim_matches(['[', ']']).to_owned()).ok()?;
        let mut connection = rustls::ClientConnection::new(Arc::new(config), name).ok()?;
        let mut socket =
            std::net::ToSocketAddrs::to_socket_addrs(&(host.trim_matches(['[', ']']), port))
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
