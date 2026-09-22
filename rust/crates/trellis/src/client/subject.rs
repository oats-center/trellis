use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::Value;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Errors returned while resolving a contract subject template.
#[derive(Debug, thiserror::Error)]
pub enum SubjectError {
    /// The event could not be serialized before resolving its subject.
    #[error("event subject payload is not serializable: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The event payload violated its contract schema.
    #[error("event payload violated its contract schema: {0}")]
    InvalidPayload(String),
    /// The subject template contains an unterminated placeholder.
    #[error("invalid subject template `{0}`")]
    InvalidTemplate(String),
    /// The payload does not contain the required JSON pointer.
    #[error("subject field `{0}` is missing")]
    Missing(String),
    /// The required JSON pointer resolved to null.
    #[error("subject field `{0}` is null")]
    Null(String),
    /// The required JSON pointer did not resolve to a string or number.
    #[error("subject field `{pointer}` must be a string or number, got {actual}")]
    InvalidType {
        /// JSON pointer from the contract subject template.
        pointer: String,
        /// Resolved JSON value kind.
        actual: &'static str,
    },
    /// An integer subject value cannot round-trip through JavaScript safely.
    #[error("subject field `{pointer}` is outside the JavaScript safe-integer range")]
    UnsafeInteger {
        /// JSON pointer from the contract subject template.
        pointer: String,
    },
}

/// Resolve canonical subject parameter tokens in a generated subject template.
pub fn resolve_subject(template: &str, payload: &Value) -> Result<String, SubjectError> {
    let mut subject = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        subject.push_str(&rest[..open]);
        let placeholder = &rest[open + 1..];
        let close = placeholder
            .find('}')
            .ok_or_else(|| SubjectError::InvalidTemplate(template.to_string()))?;
        let pointer = &placeholder[..close];
        let value = payload
            .pointer(pointer)
            .ok_or_else(|| SubjectError::Missing(pointer.to_string()))?;
        subject.push_str(&encode_token(&subject_value(pointer, value)?));
        rest = &placeholder[close + 1..];
    }

    if rest.contains('}') {
        return Err(SubjectError::InvalidTemplate(template.to_string()));
    }
    subject.push_str(rest);
    Ok(subject)
}

fn subject_value(pointer: &str, value: &Value) -> Result<String, SubjectError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) if value.is_i64() => {
            let value = value.as_i64().expect("checked integer representation");
            if value.unsigned_abs() > MAX_SAFE_INTEGER {
                return Err(SubjectError::UnsafeInteger {
                    pointer: pointer.to_string(),
                });
            }
            Ok(value.to_string())
        }
        Value::Number(value) if value.is_u64() => {
            let value = value.as_u64().expect("checked integer representation");
            if value > MAX_SAFE_INTEGER {
                return Err(SubjectError::UnsafeInteger {
                    pointer: pointer.to_string(),
                });
            }
            Ok(value.to_string())
        }
        Value::Number(value) => {
            let value = value.as_f64().expect("JSON numbers are finite");
            Ok(ryu_js::Buffer::new().format(value).to_string())
        }
        Value::Null => Err(SubjectError::Null(pointer.to_string())),
        value => Err(SubjectError::InvalidType {
            pointer: pointer.to_string(),
            actual: value_kind(value),
        }),
    }
}

fn encode_token(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(token.as_bytes())
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{encode_token, resolve_subject, SubjectError};

    #[test]
    fn encodes_cross_language_subject_vectors() {
        for (input, expected) in [
            ("", ""),
            ("$SYS", "JFNZUw"),
            ("a.b", "YS5i"),
            ("a b", "YSBi"),
            ("*", "Kg"),
            (">", "Pg"),
            ("~", "fg"),
            ("😀", "8J-YgA"),
        ] {
            assert_eq!(encode_token(input), expected);
        }
    }

    #[test]
    fn resolves_string_and_number_subject_fields() {
        let payload = json!({ "deploymentId": "a.b", "attempt": 1.5 });
        assert_eq!(
            resolve_subject(
                "events.v1.Auth.DeviceApproved.{/deploymentId}.{/attempt}",
                &payload,
            )
            .unwrap(),
            "events.v1.Auth.DeviceApproved.YS5i.MS41",
        );
    }

    #[test]
    fn rejects_missing_null_boolean_and_unsafe_integer_fields() {
        assert!(matches!(
            resolve_subject("events.v1.Test.{/missing}", &json!({})),
            Err(SubjectError::Missing(_))
        ));
        assert!(matches!(
            resolve_subject("events.v1.Test.{/value}", &json!({ "value": null })),
            Err(SubjectError::Null(_))
        ));
        assert!(matches!(
            resolve_subject("events.v1.Test.{/value}", &json!({ "value": true })),
            Err(SubjectError::InvalidType { .. })
        ));
        assert!(matches!(
            resolve_subject(
                "events.v1.Test.{/value}",
                &json!({ "value": 9_007_199_254_740_992_u64 }),
            ),
            Err(SubjectError::UnsafeInteger { .. })
        ));
    }
}
