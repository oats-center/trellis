use serde_json::Value;
use trellis_protocol::{parse_authorization_context, SignedAuthorizationContext};

use super::super::http_error::read_bounded_http_body;
use super::super::{decode_trellis_http_error, TrellisClientError};

/// Return the canonical HTTP(S) origin of the configured Trellis URL.
///
/// Plaintext HTTP is accepted here: whether a runtime serves it is the
/// runtime's own decision, taken from its public origin and
/// `[http] allow_insecure_origins`. Clients no longer carry a local insecure
/// mode.
///
/// # Errors
///
/// Returns [`TrellisClientError::Bootstrap`] when the URL is not an absolute
/// HTTP(S) URL or carries credentials, a query, or a fragment.
pub fn canonical_trellis_origin(trellis_url: &str) -> Result<String, TrellisClientError> {
    let url = reqwest::Url::parse(trellis_url)
        .map_err(|error| TrellisClientError::Bootstrap(format!("invalid Trellis URL: {error}")))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TrellisClientError::Bootstrap(
            "Trellis URL must be an absolute HTTP(S) URL without embedded credentials, query, or fragment".into(),
        ));
    }
    Ok(url.origin().ascii_serialization())
}

/// Pre-NATS HTTP credential and context recovery client.
///
/// Credential recovery and unknown issuer keys use only the configured origin.
#[derive(Clone, Debug)]
pub(crate) struct BootstrapHttp {
    base: reqwest::Url,
    client: reqwest::Client,
}

impl BootstrapHttp {
    pub(crate) fn new(trellis_url: &str) -> Result<Self, TrellisClientError> {
        let base = reqwest::Url::parse(&canonical_trellis_origin(trellis_url)?)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        Ok(Self { base, client })
    }

    pub(crate) fn origin(&self) -> String {
        self.base.origin().ascii_serialization()
    }

    /// Fetch an unknown public key only from this runtime's authenticated origin.
    pub(crate) async fn issuer_key(
        &self,
        key_id: &str,
    ) -> Result<trellis_protocol::AuthorizationIssuerKey, TrellisClientError> {
        if key_id.len() != 43
            || !key_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(TrellisClientError::Bootstrap(
                "invalid issuer key id".into(),
            ));
        }
        let url = self
            .base
            .join(&format!("/auth/keys/{key_id}"))
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
        if !response.status().is_success() {
            let error = decode_trellis_http_error(response).await;
            return Err(TrellisClientError::BootstrapHttp {
                status: error.status,
                code: error.code,
            });
        }
        let body = read_bounded_http_body(response, 4096)
            .await
            .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
        let issuer: trellis_protocol::AuthorizationIssuerKey = serde_json::from_slice(&body)
            .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
        issuer
            .verifying_key()
            .map_err(|error| TrellisClientError::AuthorizationUnavailable(error.to_string()))?;
        if issuer.key_id != key_id {
            return Err(TrellisClientError::AuthorizationUnavailable(
                "issuer response key id does not match the requested key".into(),
            ));
        }
        Ok(issuer)
    }

    /// POST a JSON body to a same-origin path and return the JSON response.
    pub(crate) async fn post_json(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<Value, TrellisClientError> {
        let url = self
            .base
            .join(path)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        if url.origin() != self.base.origin() {
            return Err(TrellisClientError::Bootstrap(
                "cross-origin bootstrap is forbidden".into(),
            ));
        }
        let response = self
            .client
            .post(url.clone())
            .json(body)
            .send()
            .await
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))?;
        if !response.status().is_success() {
            let error = decode_trellis_http_error(response).await;
            return Err(TrellisClientError::BootstrapHttp {
                status: error.status,
                code: error.code,
            });
        }
        let body = read_bounded_http_body(response, 1024 * 1024).await?;
        serde_json::from_slice(&body)
            .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
    }
}

/// Parse the signed context from an installed bundle.
pub(crate) fn persisted_signed_context(
    bundle: &super::types::AuthorizationContextBundle,
) -> Result<SignedAuthorizationContext, TrellisClientError> {
    parse_authorization_context(&bundle.context)
        .map_err(|error| TrellisClientError::Bootstrap(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::canonical_trellis_origin;

    #[test]
    fn accepts_absolute_http_and_https_origins() {
        assert_eq!(
            canonical_trellis_origin("http://phi.oats:3000").unwrap(),
            "http://phi.oats:3000"
        );
        assert_eq!(
            canonical_trellis_origin("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert_eq!(
            canonical_trellis_origin("https://phi.oats").unwrap(),
            "https://phi.oats"
        );
    }

    #[test]
    fn rejects_relative_credentials_query_and_fragment() {
        assert!(canonical_trellis_origin("/bootstrap").is_err());
        assert!(canonical_trellis_origin("http://phi.oats:3000?x=1").is_err());
        assert!(canonical_trellis_origin("http://user@phi.oats:3000").is_err());
        assert!(canonical_trellis_origin("ftp://phi.oats").is_err());
    }
}
