//! Shared public-only HTTPS transport for untrusted metadata and image URLs.

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{net::IpAddr, sync::Arc, time::Duration};

struct PublicDns;
impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|address| !public_address(address.ip()))
            {
                return Err(std::io::Error::other("destination is not public").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

pub(super) fn public_address(address: IpAddr) -> bool {
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
            (0x2000..0x4000).contains(&a)
                && !(a == 0x2001 && b < 0x0200)
                && !(a == 0x2001 && b == 0x0db8)
                && a != 0x2002
                && a != 0x3ffe
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

pub(super) fn public_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() == Some(443)
        && url.host().is_some_and(|host| match host {
            url::Host::Ipv4(ip) => public_address(IpAddr::V4(ip)),
            url::Host::Ipv6(ip) => public_address(IpAddr::V6(ip)),
            url::Host::Domain(_) => true,
        })
}

pub(super) fn client() -> reqwest::ClientBuilder {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .dns_resolver(Arc::new(PublicDns))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn actual_dns_resolution_refuses_private_loopback_answers() {
        assert!(PublicDns
            .resolve("localhost".parse().unwrap())
            .await
            .is_err());
    }
}
