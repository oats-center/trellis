use serde::Deserialize;

/// Exact machine-readable failure returned by a Trellis HTTP endpoint.
#[derive(Debug, thiserror::Error)]
#[error("Trellis HTTP request failed with status {status}: {code}")]
pub struct TrellisHttpError {
    /// HTTP response status.
    pub status: u16,
    /// Trellis machine error code.
    pub code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    error: ErrorCode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorCode {
    code: String,
}

/// Decode one failed Trellis HTTP response without retaining its body text.
pub async fn decode_trellis_http_error(response: reqwest::Response) -> TrellisHttpError {
    let status = response.status().as_u16();
    let code = read_bounded_http_body(response, 4096)
        .await
        .ok()
        .and_then(|body| serde_json::from_slice::<ErrorEnvelope>(&body).ok())
        .map(|envelope| envelope.error.code)
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| "invalid_http_error_envelope".to_owned());
    TrellisHttpError { status, code }
}

pub(crate) async fn read_bounded_http_body(
    mut response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>, super::TrellisClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        super::TrellisClientError::Bootstrap(format!("cannot read Trellis response: {error}"))
    })? {
        if chunk.len() > maximum_bytes.saturating_sub(body.len()) {
            return Err(super::TrellisClientError::Bootstrap(
                "Trellis response exceeds its size limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Use the application's provider when installed, otherwise choose ring locally.
/// With no `root_certificates`, certificates verify against platform trust as
/// with Reqwest's default; otherwise only against those PEM files.
#[cfg(feature = "http")]
pub(crate) fn http_client_builder(
    root_certificates: &[std::path::PathBuf],
) -> Result<reqwest::ClientBuilder, super::TrellisClientError> {
    use rustls::pki_types::{pem::PemObject as _, CertificateDer};
    use rustls_platform_verifier::BuilderVerifierExt as _;

    let bootstrap = |error: String| super::TrellisClientError::Bootstrap(error);
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| std::sync::Arc::new(rustls::crypto::ring::default_provider()));
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| bootstrap(error.to_string()))?;
    let builder = if root_certificates.is_empty() {
        builder
            .with_platform_verifier()
            .map_err(|error| bootstrap(error.to_string()))?
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
        builder.with_root_certificates(roots)
    };
    Ok(reqwest::Client::builder().tls_backend_preconfigured(builder.with_no_client_auth()))
}

#[cfg(test)]
mod tests {
    use super::ErrorEnvelope;

    #[test]
    fn exact_error_envelope_preserves_machine_code() {
        let envelope: ErrorEnvelope =
            serde_json::from_str(r#"{"error":{"code":"flow_expired"}}"#).unwrap();
        assert_eq!(envelope.error.code, "flow_expired");
    }

    #[test]
    fn loose_error_envelope_is_rejected() {
        assert!(serde_json::from_str::<ErrorEnvelope>(
            r#"{"error":{"code":"session_revoked"},"message":"ignore me"}"#
        )
        .is_err());
    }
}
