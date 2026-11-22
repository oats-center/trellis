//! The standard verifiers. Each compares secrets and signatures in constant
//! time, and a failure names only the check that failed, never a value.

use base64::Engine as _;
use subtle::ConstantTimeEq as _;
use trellis_idl::webhook::{parse_template, TemplatePart};
use trellis_idl::{WebhookHeaderItem, WebhookHmac};

use super::request::StoredRequest;

/// Most signatures Trellis tries from one request; some senders send several
/// while they change secrets.
const MAX_SIGNATURES: usize = 8;

/// Why a verifier did not pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Failure {
    /// The request is not genuine, for the named reason.
    Rejected(String),
    /// A secret the verifier needs has not been set.
    Unset(String),
}

/// The key that matched: the current secret or the one before a change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum KeyMatch {
    Current,
    Previous,
}

impl KeyMatch {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Previous => "previous",
        }
    }
}

/// The secrets a verifier may use: the current one, and the previous one
/// while the change overlap lasts.
#[derive(Clone, Debug, Default)]
pub(crate) struct Secrets {
    pub(crate) current: Option<String>,
    pub(crate) previous: Option<String>,
}

impl Secrets {
    /// Which secret a presented credential matches, where `expected` gives
    /// the credential each secret makes.
    pub(crate) fn matching(
        &self,
        presented: &[u8],
        expected: impl Fn(&str) -> Vec<u8>,
    ) -> Option<KeyMatch> {
        [
            (&self.current, KeyMatch::Current),
            (&self.previous, KeyMatch::Previous),
        ]
        .into_iter()
        .filter_map(|(secret, key)| Some((secret.as_deref()?, key)))
        .find(|(secret, _)| equal(presented, &expected(secret)))
        .map(|(_, key)| key)
    }
}

/// Whether two byte strings are equal, in time that depends only on their lengths.
pub(crate) fn equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn decode(encoding: &str, text: &str) -> Option<Vec<u8>> {
    match encoding {
        "text" => Some(text.as_bytes().to_vec()),
        "hex" => hex(text),
        "base64" => base64::engine::general_purpose::STANDARD.decode(text).ok(),
        "base64url" => base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text.trim_end_matches('='))
            .ok(),
        _ => None,
    }
}

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

/// The items of every value of `item.header` that start with its prefix,
/// with the prefix removed.
fn header_items(request: &StoredRequest, item: &WebhookHeaderItem) -> Vec<String> {
    let mut items = Vec::new();
    for (_, value) in request
        .headers
        .iter()
        .filter(|(name, _)| *name == item.header)
    {
        let parts: Vec<&str> = match &item.separator {
            Some(separator) => value.split(separator.as_str()).collect(),
            None => vec![value.as_str()],
        };
        for part in parts {
            let part = part.trim();
            match &item.prefix {
                Some(prefix) => {
                    if let Some(rest) = part.strip_prefix(prefix.as_str()) {
                        items.push(rest.to_owned());
                    }
                }
                None => items.push(part.to_owned()),
            }
        }
    }
    items
}

/// The first value of a lowercase header name.
pub(crate) fn header<'a>(request: &'a StoredRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

/// Render a template into the exact bytes it names.
///
/// # Errors
///
/// Returns the failed check when the template names a missing header, a
/// timestamp the request did not send, a JSON value a body that is not JSON
/// or lacks the pointer cannot give, or a form field the body lacks.
pub(crate) fn render(
    template: &str,
    request: &StoredRequest,
    timestamp: Option<&str>,
) -> Result<Vec<u8>, Failure> {
    let parts = parse_template(template).map_err(Failure::Rejected)?;
    let mut bytes = Vec::new();
    // The body is parsed once, and only when a placeholder reads it.
    let mut json: Option<Option<serde_json::Value>> = None;
    for part in parts {
        match part {
            TemplatePart::Literal(text) => bytes.extend_from_slice(text.as_bytes()),
            TemplatePart::Body => bytes.extend_from_slice(&request.body),
            TemplatePart::Timestamp => bytes.extend_from_slice(
                timestamp
                    .ok_or_else(|| Failure::Rejected("timestamp missing".into()))?
                    .as_bytes(),
            ),
            TemplatePart::Header(name) => bytes.extend_from_slice(
                header(request, &name)
                    .ok_or_else(|| Failure::Rejected(format!("header {name} missing")))?
                    .as_bytes(),
            ),
            TemplatePart::Method => bytes.extend_from_slice(request.method.as_bytes()),
            TemplatePart::Url => bytes.extend_from_slice(request.url.as_bytes()),
            TemplatePart::Path => bytes.extend_from_slice(request.path.as_bytes()),
            TemplatePart::Query => bytes.extend_from_slice(request.query.as_bytes()),
            TemplatePart::FormSorted => {
                let mut fields: Vec<(String, String)> = url::form_urlencoded::parse(&request.body)
                    .into_owned()
                    .collect();
                fields.sort_by(|left, right| left.0.cmp(&right.0));
                for (name, value) in fields {
                    bytes.extend_from_slice(name.as_bytes());
                    bytes.extend_from_slice(value.as_bytes());
                }
            }
            // A string gives its text and a number its JSON text; any other
            // value, like a missing one, gives no key.
            TemplatePart::Json(pointer) => {
                let value = json
                    .get_or_insert_with(|| serde_json::from_slice(&request.body).ok())
                    .as_ref()
                    .and_then(|body| body.pointer(&pointer))
                    .and_then(|value| match value {
                        serde_json::Value::String(text) => Some(text.clone()),
                        serde_json::Value::Number(number) => Some(number.to_string()),
                        _ => None,
                    })
                    .ok_or_else(|| Failure::Rejected(format!("JSON value {pointer} missing")))?;
                bytes.extend_from_slice(value.as_bytes());
            }
            TemplatePart::Form(field) => {
                let (_, value) = url::form_urlencoded::parse(&request.body)
                    .find(|(name, _)| *name == field)
                    .ok_or_else(|| Failure::Rejected(format!("form field {field} missing")))?;
                bytes.extend_from_slice(value.as_bytes());
            }
        }
    }
    Ok(bytes)
}

/// Check one `hmac` verifier against a stored request and its receive time.
pub(crate) fn hmac(
    request: &StoredRequest,
    verifier: &WebhookHmac,
    secrets: &Secrets,
) -> Result<KeyMatch, Failure> {
    if secrets.current.is_none() && secrets.previous.is_none() {
        return Err(Failure::Unset(verifier.key.clone()));
    }
    let timestamp = match &verifier.timestamp {
        None => None,
        Some(timestamp) => {
            let text = header_items(request, &timestamp.location)
                .into_iter()
                .next()
                .ok_or_else(|| Failure::Rejected("timestamp missing".into()))?;
            let sent_ms = match timestamp.format.as_str() {
                "unix_millis" => text.parse::<i64>().ok(),
                "rfc3339" => time::OffsetDateTime::parse(
                    &text,
                    &time::format_description::well_known::Rfc3339,
                )
                .ok()
                .and_then(|time| i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()),
                _ => text
                    .parse::<i64>()
                    .ok()
                    .and_then(|seconds| seconds.checked_mul(1000)),
            }
            .ok_or_else(|| Failure::Rejected("timestamp unreadable".into()))?;
            let distance = (request.received_at_ms - sent_ms).unsigned_abs();
            if distance > timestamp.tolerance_ms {
                return Err(Failure::Rejected(format!(
                    "timestamp {} s outside tolerance",
                    distance / 1000
                )));
            }
            Some(text)
        }
    };
    let signed = render(&verifier.signed, request, timestamp.as_deref())?;
    let signatures: Vec<Vec<u8>> = header_items(request, &verifier.signature)
        .iter()
        .take(MAX_SIGNATURES)
        .filter_map(|item| decode(&verifier.signature_encoding, item))
        .collect();
    if signatures.is_empty() {
        return Err(Failure::Rejected("signature missing".into()));
    }
    let algorithm = match verifier.algorithm.as_str() {
        "sha1" => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        "sha512" => ring::hmac::HMAC_SHA512,
        _ => ring::hmac::HMAC_SHA256,
    };
    for (secret, matched) in [
        (&secrets.current, KeyMatch::Current),
        (&secrets.previous, KeyMatch::Previous),
    ] {
        let Some(secret) = secret else { continue };
        let secret = verifier
            .key_prefix
            .as_deref()
            .and_then(|prefix| secret.strip_prefix(prefix))
            .unwrap_or(secret);
        let Some(key) = decode(&verifier.key_encoding, secret) else {
            continue;
        };
        let key = ring::hmac::Key::new(algorithm, &key);
        if signatures
            .iter()
            .any(|signature| ring::hmac::verify(&key, &signed, signature).is_ok())
        {
            return Ok(matched);
        }
    }
    Err(Failure::Rejected("signature does not match".into()))
}
