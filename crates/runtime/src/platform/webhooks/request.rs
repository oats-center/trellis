//! A received request as Trellis keeps it: in the intake stream, for the
//! decide handler, and in the `Received` event.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Headers that describe one hop, that carry a person's session or a
/// proxy's credential, or that `clientAddress` replaces. None of them is
/// stored or delivered.
const DROPPED_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "cookie",
    "proxy-authorization",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-real-ip",
];

/// Whether a header is never stored.
pub(crate) fn dropped(name: &str) -> bool {
    DROPPED_HEADERS.contains(&name)
}

/// The checks an intake verifier passed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Check {
    pub(crate) kind: String,
    pub(crate) key: Option<String>,
}

/// A request as received, without credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredRequest {
    pub(crate) attempt_id: String,
    pub(crate) deployment_id: String,
    pub(crate) endpoint: String,
    pub(crate) received_at_ms: i64,
    pub(crate) client_address: String,
    pub(crate) method: String,
    /// The configured origin plus the path and query as received.
    pub(crate) url: String,
    pub(crate) path: String,
    pub(crate) query: String,
    /// Lowercase names, in order.
    pub(crate) headers: Vec<(String, String)>,
    #[serde(with = "body")]
    pub(crate) body: Vec<u8>,
    /// Checks that passed at intake.
    pub(crate) checks: Vec<Check>,
}

mod body {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(serde::de::Error::custom)
    }
}

impl StoredRequest {
    /// Lowercase hex SHA-256 of the body.
    pub(crate) fn body_sha256(&self) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(&self.body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub(crate) fn content_type(&self) -> Option<&str> {
        super::verify::header(self, "content-type")
    }

    /// Remove a query parameter that carried a credential from `query` and `url`.
    pub(crate) fn remove_query_parameter(&mut self, parameter: &str) {
        let kept: Vec<&str> = self
            .query
            .split('&')
            .filter(|pair| {
                let name = pair.split_once('=').map_or(*pair, |(name, _)| name);
                url::form_urlencoded::parse(name.as_bytes())
                    .next()
                    .map_or(name != parameter, |(decoded, _)| decoded != parameter)
            })
            .collect();
        let query = kept.join("&");
        let base = self
            .url
            .split_once('?')
            .map_or(self.url.as_str(), |(base, _)| base);
        self.url = if query.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{query}")
        };
        self.query = query;
    }

    /// The `WebhookDelivery` JSON the decide handler and the event carry.
    pub(crate) fn delivery(
        &self,
        delivery_id: &str,
        checks: &[Check],
        dedupe_key: Option<&str>,
    ) -> serde_json::Value {
        let headers: Vec<_> = self
            .headers
            .iter()
            .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
            .collect();
        serde_json::json!({
            "deliveryId": delivery_id,
            "service": self.deployment_id,
            "endpoint": self.endpoint,
            "receivedAt": super::rfc3339(self.received_at_ms),
            "clientAddress": self.client_address,
            "method": self.method,
            "url": self.url,
            "headers": headers,
            "contentType": self.content_type(),
            "body": base64::engine::general_purpose::STANDARD.encode(&self.body),
            "bodySha256": self.body_sha256(),
            "checks": checks,
            "dedupeKey": dedupe_key,
        })
    }
}
