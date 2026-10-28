use super::super::TrellisClientError;

/// Return the canonical HTTP(S) origin of a configured authorization URL.
///
/// Rejects credentials, queries and fragments rather than forwarding secrets or
/// interpreting ambiguous endpoint configuration. HTTP policy belongs to the
/// selected deployment; this helper does not discover application authority.
pub fn canonical_trellis_origin(trellis_url: &str) -> Result<String, TrellisClientError> {
    let url = url::Url::parse(trellis_url)
        .map_err(|error| TrellisClientError::Bootstrap(format!("invalid Trellis URL: {error}")))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TrellisClientError::Bootstrap(
            "Trellis URL must be an absolute HTTP(S) URL without embedded credentials, \
            query, or fragment"
                .into(),
        ));
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_origin_drops_endpoint_path_and_normalizes_default_port() {
        assert_eq!(
            canonical_trellis_origin("https://Example.COM:443/auth/authorize").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            canonical_trellis_origin("http://[::1]:4321/auth/token").unwrap(),
            "http://[::1]:4321"
        );
    }

    #[test]
    fn canonical_origin_refuses_ambiguous_or_credential_bearing_urls() {
        for url in [
            "https://user:secret@example.com",
            "https://example.com?secret=1",
            "https://example.com#fragment",
            "nats://example.com",
            "/auth/token",
        ] {
            assert!(canonical_trellis_origin(url).is_err(), "{url}");
        }
    }
}
