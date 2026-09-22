use std::cmp::Ordering;

use crate::ProtocolError;

pub(crate) fn compare_protocol_strings(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn validate_protocol_identifier(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value.trim() != value {
        return Err(invalid(
            field,
            "must not have leading or trailing whitespace",
        ));
    }
    if value.chars().any(|character| character.is_ascii_control()) {
        return Err(invalid(field, "must not contain ASCII control characters"));
    }
    Ok(())
}

/// Validate one stable `lineage@vN` API identifier.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] when `value` is not a valid API ID.
pub fn validate_api_id(value: &str) -> Result<(), ProtocolError> {
    validate_protocol_identifier("api id", value)?;
    if value.len() > 128 {
        return Err(invalid("api id", "must be at most 128 bytes"));
    }
    let Some((lineage, major)) = value.rsplit_once("@v") else {
        return Err(invalid("api id", "must end in one '@vN' major suffix"));
    };
    if lineage.is_empty()
        || !lineage.as_bytes().iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        || !lineage.as_bytes()[0].is_ascii_lowercase() && !lineage.as_bytes()[0].is_ascii_digit()
        || !lineage.as_bytes()[lineage.len() - 1].is_ascii_lowercase()
            && !lineage.as_bytes()[lineage.len() - 1].is_ascii_digit()
        || lineage.as_bytes().windows(2).any(|pair| {
            matches!(pair[0], b'.' | b'_' | b'-') && matches!(pair[1], b'.' | b'_' | b'-')
        })
    {
        return Err(invalid(
            "api id",
            "must use lowercase alphanumeric lineage tokens separated by '.', '_', or '-' before '@vN'",
        ));
    }
    validate_positive_decimal("api id", major)
}

pub(crate) fn validate_version(value: &str) -> Result<(), ProtocolError> {
    let Some(major) = value.strip_prefix('v') else {
        return Err(invalid(
            "surface version",
            "must be a positive 'vN' version",
        ));
    };
    validate_positive_decimal("surface version", major)
}

pub(crate) fn validate_logical_name(value: &str) -> Result<(), ProtocolError> {
    validate_protocol_identifier("logical name", value)?;
    if value.starts_with('.')
        || value.ends_with('.')
        || value.contains("..")
        || value.split('.').any(|token| {
            token.is_empty() || token.contains(['*', '>']) || token.chars().any(char::is_whitespace)
        })
    {
        return Err(invalid(
            "logical name",
            "must be dot-separated non-empty NATS-safe tokens",
        ));
    }
    Ok(())
}

fn validate_positive_decimal(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || !matches!(bytes[0], b'1'..=b'9')
        || !bytes[1..].iter().all(u8::is_ascii_digit)
    {
        return Err(invalid(field, "must use a positive decimal major version"));
    }
    Ok(())
}

fn invalid(field: &'static str, reason: &'static str) -> ProtocolError {
    ProtocolError::InvalidIdentifier { field, reason }
}
