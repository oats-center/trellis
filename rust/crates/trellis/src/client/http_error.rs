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
