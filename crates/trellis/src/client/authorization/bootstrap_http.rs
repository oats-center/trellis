use serde_json::Value;
use trellis_protocol::{parse_authorization_context, SignedAuthorizationContext};

use super::super::http_error::read_bounded_http_body;
use super::super::{decode_trellis_http_error, TrellisClientError};

/// Return the configured HTTPS origin, allowing explicitly selected loopback HTTP.
///
/// # Errors
///
/// Returns [`TrellisClientError::Bootstrap`] when the URL is not an accepted
/// origin or carries credentials, a query, or a fragment.
pub fn canonical_trellis_origin(trellis_url: &str) -> Result<String, TrellisClientError> {
    canonical_trellis_origin_with_insecure(trellis_url, false)
}

/// Return the configured origin, additionally accepting non-loopback HTTP when
/// the caller explicitly allow-listed it as an insecure origin.
///
/// Callers select this only from an explicit operator opt-in (for example the
/// CLI `--allow-insecure-origin` flag or a service's connect options); the
/// default path stays [`canonical_trellis_origin`].
pub fn canonical_trellis_origin_with_insecure(
    trellis_url: &str,
    allow_insecure: bool,
) -> Result<String, TrellisClientError> {
    let url = reqwest::Url::parse(trellis_url)
        .map_err(|error| TrellisClientError::Bootstrap(format!("invalid Trellis URL: {error}")))?;
    let origin = url.origin().ascii_serialization();
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain("localhost")) => true,
        _ => false,
    };
    let scheme_allowed =
        url.scheme() == "https" || (url.scheme() == "http" && (loopback || allow_insecure));
    if !scheme_allowed
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TrellisClientError::Bootstrap(
            "Trellis URL must be HTTPS, or explicitly configured loopback HTTP, without embedded credentials, query, or fragment".into(),
        ));
    }
    Ok(origin)
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
    pub(crate) fn new(trellis_url: &str, allow_insecure: bool) -> Result<Self, TrellisClientError> {
        let base = reqwest::Url::parse(&canonical_trellis_origin_with_insecure(
            trellis_url,
            allow_insecure,
        )?)
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
    use super::{canonical_trellis_origin, canonical_trellis_origin_with_insecure};

    #[test]
    fn strict_origin_rejects_non_loopback_http() {
        assert!(canonical_trellis_origin("http://phi.oats:3000").is_err());
        assert!(canonical_trellis_origin("http://localhost:3000").is_ok());
        assert!(canonical_trellis_origin("http://127.0.0.1:3000").is_ok());
        assert!(canonical_trellis_origin("https://phi.oats").is_ok());
    }

    #[test]
    fn explicit_insecure_origin_accepts_non_loopback_http() {
        assert_eq!(
            canonical_trellis_origin_with_insecure("http://phi.oats:3000", true).unwrap(),
            "http://phi.oats:3000"
        );
        assert!(canonical_trellis_origin_with_insecure("http://phi.oats:3000", false).is_err());
        assert!(canonical_trellis_origin_with_insecure("http://phi.oats:3000?x=1", true).is_err());
    }
}
