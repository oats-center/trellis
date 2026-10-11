//! CIMD transport policy. OAuth metadata validation belongs to `oauth-as`;
//! DNS, the actual socket destination, response bounds, and cache lifetime do not.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use oauth_as::cimd::{CimdPolicy, ClientIdUrl, ValidatedClientIdDocument};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use sha2::{Digest, Sha256};

use super::super::AuthError;

#[derive(Clone)]
pub(crate) struct ClientDocument {
    pub(crate) document: ValidatedClientIdDocument,
    pub(crate) revision: String,
    pub(crate) metadata: serde_json::Value,
    pub(crate) kind: String,
    expires: Instant,
}

pub(crate) struct ClientDocuments {
    client: reqwest::Client,
    policy: CimdPolicy,
    cache: Mutex<BTreeMap<String, ClientDocument>>,
    portal_origin: String,
}

struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            // Refuse a mixed public/private answer altogether. Never let fallback
            // or happy-eyeballs select a destination rejected by policy.
            if addresses.is_empty() || addresses.iter().any(|addr| !public_address(addr.ip())) {
                return Err(std::io::Error::other("CIMD destination is not public").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_broadcast()
                && !address.is_documentation()
                && a != 0
                && a < 240
                && !(a == 100 && (64..128).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(address) => {
            let [a, b, ..] = address.segments();
            // Global unicast only; exclude protocol-assignment, documentation,
            // 6to4, mapped IPv4, local, multicast, and reserved destinations.
            (0x2000..0x4000).contains(&a)
                && !(a == 0x2001 && b < 0x0200)
                && !(a == 0x2001 && b == 0x0db8)
                && a != 0x2002
                && a != 0x3ffe
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

impl ClientDocuments {
    pub(crate) fn new(portal_origin: String) -> Result<Self, AuthError> {
        // The runtime selects ring, but reqwest's no-provider build must have
        // that choice installed before constructing its TLS connector. Preserve
        // an already selected process provider, including concurrent startup.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .dns_resolver(Arc::new(PublicDns))
            .build()
            .map_err(|_| AuthError::Unavailable)?;
        let mut policy = CimdPolicy::new();
        // A public metadata host is not the app's binding origin. Exact listed
        // callbacks may belong to another HTTPS app or a loopback development
        // app; only the metadata GET is constrained to public destinations.
        policy.redirect_uris_same_origin = false;
        Ok(Self {
            client,
            policy,
            cache: Mutex::new(BTreeMap::new()),
            portal_origin,
        })
    }

    pub(crate) async fn get(&self, client_id: &str) -> Result<ClientDocument, AuthError> {
        let identity =
            ClientIdUrl::parse(client_id, &self.policy).map_err(|_| AuthError::Denied)?;
        let url = url::Url::parse(client_id).map_err(|_| AuthError::Denied)?;
        if url.origin().ascii_serialization() == self.portal_origin
            || url.port_or_known_default() != Some(443)
            || url.as_str() != client_id
        {
            return Err(AuthError::Denied);
        }
        if let Some(host) = url.host() {
            let address = match host {
                url::Host::Ipv4(address) => Some(IpAddr::V4(address)),
                url::Host::Ipv6(address) => Some(IpAddr::V6(address)),
                url::Host::Domain(_) => None,
            };
            if address.is_some_and(|address| !public_address(address)) {
                return Err(AuthError::Denied);
            }
        }
        {
            let mut cache = self.cache.lock().map_err(|_| AuthError::Unavailable)?;
            cache.retain(|_, entry| entry.expires > Instant::now());
            if let Some(entry) = cache.get(client_id) {
                return Ok(entry.clone());
            }
        }
        let mut response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        if !response.status().is_success()
            || response.url() != &url
            || response
                .remote_addr()
                .is_none_or(|address| !public_address(address.ip()))
            || response
                .content_length()
                .is_some_and(|length| length > self.policy.max_document_bytes as u64)
            || response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|encoding| encoding != "identity")
            || response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_none_or(|value| {
                    value.split(';').next().map(str::trim) != Some("application/json")
                })
        {
            return Err(AuthError::Denied);
        }
        let mut ttl = 60u64;
        if let Some(control) = response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
        {
            for directive in control.split(',').map(str::trim) {
                if directive.eq_ignore_ascii_case("no-cache")
                    || directive.eq_ignore_ascii_case("no-store")
                {
                    ttl = 0;
                } else if let Some((name, value)) = directive.split_once('=') {
                    if name.eq_ignore_ascii_case("max-age") {
                        ttl = ttl.min(value.trim_matches('"').parse::<u64>().unwrap_or(0));
                    }
                }
            }
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Unavailable)? {
            if body.len().saturating_add(chunk.len()) > self.policy.max_document_bytes {
                return Err(AuthError::Denied);
            }
            body.extend_from_slice(&chunk);
        }
        let document = ValidatedClientIdDocument::validate(&identity, &body, &self.policy)
            .map_err(|_| AuthError::Denied)?;
        let portal = url::Url::parse(&self.portal_origin).map_err(|_| AuthError::Denied)?;
        for callback in &document.to_client().redirect_uris {
            let callback = url::Url::parse(callback).map_err(|_| AuthError::Denied)?;
            let loopback = matches!(
                callback.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            );
            if callback.origin().ascii_serialization() == self.portal_origin
                || callback.host_str().is_none()
                || !callback.username().is_empty()
                || callback.password().is_some()
                || callback.fragment().is_some()
                || (callback.scheme() != "https" && !(callback.scheme() == "http" && loopback))
            {
                return Err(AuthError::Denied);
            }
            super::super::warn_oauth_cookie_isolation(&portal, &callback);
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| AuthError::Denied)?;
        let kind = match metadata
            .get("application_type")
            .and_then(serde_json::Value::as_str)
        {
            None | Some("web") => "browser".into(),
            Some("native") => "native".into(),
            _ => return Err(AuthError::Denied),
        };
        let entry = ClientDocument {
            metadata,
            kind,
            document,
            revision: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(&body)),
            expires: Instant::now() + Duration::from_secs(ttl),
        };
        let mut cache = self.cache.lock().map_err(|_| AuthError::Unavailable)?;
        cache.retain(|_, entry| entry.expires > Instant::now());
        if ttl > 0 && cache.len() < 128 {
            cache.insert(client_id.into(), entry.clone());
        }
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_document_fetch_rejects_nonpublic_destinations_before_http() {
        let documents = ClientDocuments::new("https://auth.example".into()).unwrap();
        for client in [
            "http://public.example/app.json",
            "https://127.0.0.1/app.json",
            "https://[::1]/app.json",
            "https://[::ffff:127.0.0.1]/app.json",
            "https://169.254.169.254/app.json",
            "https://192.168.1.1/app.json",
            "https://auth.example/app.json",
            "https://public.example:0/app.json",
        ] {
            assert!(documents.get(client).await.is_err(), "{client}");
        }
        let resolver = PublicDns;
        assert!(resolver
            .resolve("localhost".parse().unwrap())
            .await
            .is_err());
    }
}
