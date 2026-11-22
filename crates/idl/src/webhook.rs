//! Webhook templates: the `signed` and `dedupe` strings of a webhook endpoint.
//!
//! The compiler checks templates when it compiles an endpoint, and the runtime
//! parses the same text again to build the bytes it signs or the key it keeps.

/// One part of a parsed webhook template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplatePart {
    /// Literal text; `{{` and `}}` stand for braces.
    Literal(String),
    /// The body as received, or after decoding its content encoding.
    Body,
    /// The timestamp text exactly as sent.
    Timestamp,
    /// The first value of a header.
    Header(String),
    /// The request method.
    Method,
    /// The configured Trellis origin plus the path and query as received.
    Url,
    /// The request path.
    Path,
    /// The query without its `?`.
    Query,
    /// The form-encoded body sorted by field name, each name then its value.
    FormSorted,
    /// The string or number a JSON Pointer (RFC 6901) names in the body.
    /// Only dedupe templates may use it.
    Json(String),
    /// The first value of a field of the form-encoded body. Only dedupe
    /// templates may use it.
    Form(String),
}

/// Parse a webhook template into its parts.
///
/// # Errors
///
/// Returns a message naming the problem for an unknown placeholder, an
/// unclosed `{`, a lone `}`, an empty header or form field name, or a JSON
/// Pointer that is not one.
pub fn parse_template(template: &str) -> Result<Vec<TemplatePart>, String> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut characters = template.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
                literal.push('{');
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
                literal.push('}');
            }
            '}' => return Err("a lone '}' must be written '}}'".to_owned()),
            '{' => {
                let mut name = String::new();
                loop {
                    match characters.next() {
                        Some('}') => break,
                        Some(character) => name.push(character),
                        None => return Err(format!("placeholder '{{{name}' is not closed")),
                    }
                }
                if !literal.is_empty() {
                    parts.push(TemplatePart::Literal(std::mem::take(&mut literal)));
                }
                parts.push(match name.as_str() {
                    "body" => TemplatePart::Body,
                    "timestamp" => TemplatePart::Timestamp,
                    "method" => TemplatePart::Method,
                    "url" => TemplatePart::Url,
                    "path" => TemplatePart::Path,
                    "query" => TemplatePart::Query,
                    "form_sorted" => TemplatePart::FormSorted,
                    _ => {
                        if let Some(header) = name.strip_prefix("header:") {
                            if header.is_empty()
                                || !header.bytes().all(|byte| {
                                    byte.is_ascii_alphanumeric() || b"-_".contains(&byte)
                                })
                            {
                                return Err(format!("unknown placeholder '{{{name}}}'"));
                            }
                            TemplatePart::Header(header.to_ascii_lowercase())
                        } else if let Some(pointer) = name.strip_prefix("json:") {
                            // RFC 6901: each reference token follows a '/',
                            // and '~' is only ever '~0' or '~1'.
                            if !pointer.starts_with('/')
                                || pointer
                                    .split('~')
                                    .skip(1)
                                    .any(|rest| !rest.starts_with('0') && !rest.starts_with('1'))
                            {
                                return Err(format!(
                                    "'{{{name}}}' needs a JSON Pointer such as {{json:/id}}"
                                ));
                            }
                            TemplatePart::Json(pointer.to_owned())
                        } else if let Some(field) = name.strip_prefix("form:") {
                            if field.is_empty() {
                                return Err(format!("'{{{name}}}' needs a form field name"));
                            }
                            TemplatePart::Form(field.to_owned())
                        } else {
                            return Err(format!("unknown placeholder '{{{name}}}'"));
                        }
                    }
                });
            }
            character => literal.push(character),
        }
    }
    if !literal.is_empty() {
        parts.push(TemplatePart::Literal(literal));
    }
    Ok(parts)
}

/// Describe a verifier in plain words for approval screens and listings,
/// such as "HMAC-SHA256 over the timestamp and body, 5 minute tolerance".
#[must_use]
pub fn describe_verifier(verifier: &crate::WebhookVerifier) -> String {
    use crate::WebhookVerifier;
    match verifier {
        WebhookVerifier::Hmac(hmac) => {
            let crate::WebhookHmac {
                algorithm,
                signed,
                timestamp,
                ..
            } = &**hmac;
            let parts = parse_template(signed).unwrap_or_default();
            let mut covered = Vec::new();
            for part in &parts {
                let words = match part {
                    TemplatePart::Literal(_) => continue,
                    TemplatePart::Body => "body".to_owned(),
                    TemplatePart::Timestamp => "timestamp".to_owned(),
                    TemplatePart::Header(name) => format!("{name} header"),
                    TemplatePart::Method => "method".to_owned(),
                    TemplatePart::Url => "URL".to_owned(),
                    TemplatePart::Path => "path".to_owned(),
                    TemplatePart::Query => "query".to_owned(),
                    TemplatePart::FormSorted => "sorted form fields".to_owned(),
                    TemplatePart::Json(pointer) => format!("{pointer} JSON value"),
                    TemplatePart::Form(field) => format!("{field} form field"),
                };
                if !covered.contains(&words) {
                    covered.push(words);
                }
            }
            let covered = match covered.split_last() {
                None => "nothing".to_owned(),
                Some((last, [])) => format!("the {last}"),
                Some((last, rest)) => format!("the {} and {last}", rest.join(", ")),
            };
            let mut text = format!("HMAC-{} over {covered}", algorithm.to_ascii_uppercase());
            if let Some(timestamp) = timestamp {
                text.push_str(&format!(
                    ", {} tolerance",
                    duration_words(timestamp.tolerance_ms)
                ));
            }
            text
        }
        WebhookVerifier::Token {
            header,
            query,
            prefix,
            ..
        } => match (header, query) {
            (Some(header), _) if prefix.is_some() => {
                format!("Token after a prefix in the {header} header")
            }
            (Some(header), _) => format!("Token in the {header} header"),
            (None, Some(query)) => format!("Token in the {query} query parameter"),
            (None, None) => "Token".to_owned(),
        },
        WebhookVerifier::Basic { .. } => "HTTP Basic credentials".to_owned(),
        WebhookVerifier::Address { ranges } => {
            format!("Client address within the ranges in the {ranges} setting")
        }
        WebhookVerifier::None => "Nothing: anyone with the URL can send requests".to_owned(),
    }
}

/// A duration in milliseconds as plain words, such as "5 minute".
fn duration_words(ms: u64) -> String {
    let units = [
        (86_400_000, "day"),
        (3_600_000, "hour"),
        (60_000, "minute"),
        (1_000, "second"),
    ];
    units
        .iter()
        .find(|(size, _)| ms >= *size && ms.is_multiple_of(*size))
        .map_or_else(
            || format!("{ms} ms"),
            |(size, unit)| format!("{} {unit}", ms / size),
        )
}

#[cfg(test)]
mod tests {
    use super::{parse_template, TemplatePart};

    #[test]
    fn templates_parse_placeholders_and_escaped_braces() {
        assert_eq!(
            parse_template("v0:{timestamp}:{body}{{x}}{header:X-Hub}").unwrap(),
            vec![
                TemplatePart::Literal("v0:".into()),
                TemplatePart::Timestamp,
                TemplatePart::Literal(":".into()),
                TemplatePart::Body,
                TemplatePart::Literal("{x}".into()),
                TemplatePart::Header("x-hub".into()),
            ]
        );
        assert_eq!(
            parse_template("{json:/data/object/a~1b~0c}-{form:MessageSid}").unwrap(),
            vec![
                TemplatePart::Json("/data/object/a~1b~0c".into()),
                TemplatePart::Literal("-".into()),
                TemplatePart::Form("MessageSid".into()),
            ]
        );
        for refused in [
            "{json:id}",
            "{json:}",
            "{json:/a~2}",
            "{json:/a~}",
            "{form:}",
        ] {
            assert!(parse_template(refused).is_err(), "{refused} is refused");
        }
        assert!(parse_template("{bogus}").is_err());
        assert!(parse_template("{body").is_err());
        assert!(parse_template("a}b").is_err());
    }
}
