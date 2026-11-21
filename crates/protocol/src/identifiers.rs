use crate::ProtocolError;

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

/// The workspace of every package that declares none.
///
/// It is reserved: such a package can be installed from source and used in
/// tests, but never published.
pub const LOCAL_WORKSPACE: &str = "localhost";

/// The longest workspace, in bytes.
pub const MAX_WORKSPACE_BYTES: usize = 128;

/// The longest API ID, in bytes, workspace included.
pub const MAX_API_ID_BYTES: usize = 255;

/// Validate one workspace: a registry host and a repository path.
///
/// A workspace is written in one canonical form, such as
/// `ghcr.io/krishi-diagnostics` or `localhost:5000/dev`: a lower-case
/// registry host, with its port when it has one, followed by at least one
/// repository path segment, and no scheme, trailing slash, tag, or digest.
/// The host must look like one (it has a `.` or a port, or is `localhost`),
/// so Docker Hub is written `docker.io/<org>`. The single word
/// [`LOCAL_WORKSPACE`] is the one workspace without a path segment.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] when `value` is not a
/// canonical workspace.
pub fn validate_workspace(value: &str) -> Result<(), ProtocolError> {
    validate_protocol_identifier("workspace", value)?;
    if value.len() > MAX_WORKSPACE_BYTES {
        return Err(invalid("workspace", "must be at most 128 bytes"));
    }
    if value == LOCAL_WORKSPACE {
        return Ok(());
    }
    let mut segments = value.split('/');
    let host = segments.next().unwrap_or_default();
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    let host_valid = !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && name
            .split('.')
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'))
        && port.is_none_or(|port| {
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
        })
        && (name.contains('.') || port.is_some() || name == LOCAL_WORKSPACE);
    if !host_valid {
        return Err(invalid(
            "workspace",
            "must start with a lower-case registry host, such as ghcr.io or localhost:5000",
        ));
    }
    let mut paths = 0;
    for segment in segments {
        paths += 1;
        if !repository_component(segment) {
            return Err(invalid(
                "workspace",
                "must continue with lower-case repository path segments, with no trailing \
                slash, tag, or digest",
            ));
        }
    }
    if paths == 0 {
        return Err(invalid(
            "workspace",
            "must name a repository path after the registry host, such as ghcr.io/acme",
        ));
    }
    Ok(())
}

/// One OCI repository path component: lower-case alphanumeric runs joined by
/// `.`, `_`, `__`, or runs of `-`.
fn repository_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
    {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len()
            && !(bytes[index].is_ascii_lowercase() || bytes[index].is_ascii_digit())
        {
            index += 1;
        }
        let separator = &value[start..index];
        let dashes = separator.bytes().all(|byte| byte == b'-');
        if !(matches!(separator, "." | "_" | "__") || dashes) {
            return false;
        }
    }
    true
}

/// Split a workspace-qualified ID into its workspace and its short ID.
///
/// Everything before the last `/` is the workspace; IDs of the built-in
/// `trellis` package have none. `ghcr.io/acme/orders.billing@v1` splits into
/// `Some("ghcr.io/acme")` and `orders.billing@v1`.
#[must_use]
pub fn split_workspace(id: &str) -> (Option<&str>, &str) {
    match id.rsplit_once('/') {
        Some((workspace, short)) => (Some(workspace), short),
        None => (None, id),
    }
}

/// The short ID of a workspace-qualified ID: the ID without its workspace.
#[must_use]
pub fn short_id(id: &str) -> &str {
    split_workspace(id).1
}

/// Validate one stable `[<workspace>/]lineage@vN` API identifier.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] when `value` is not a valid API ID.
pub fn validate_api_id(value: &str) -> Result<(), ProtocolError> {
    validate_protocol_identifier("api id", value)?;
    if value.len() > MAX_API_ID_BYTES {
        return Err(invalid("api id", "must be at most 255 bytes"));
    }
    let (workspace, short) = split_workspace(value);
    if let Some(workspace) = workspace {
        validate_workspace(workspace).map_err(|_| {
            invalid(
                "api id",
                "must name a canonical workspace before its last '/'",
            )
        })?;
    }
    let Some((lineage, major)) = short.rsplit_once("@v") else {
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
            "must use lowercase alphanumeric lineage tokens separated by '.', \
            '_', or '-' before '@vN'",
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

#[cfg(test)]
mod tests {
    use super::{split_workspace, validate_api_id, validate_workspace};

    #[test]
    fn workspaces_have_one_canonical_spelling() {
        for valid in [
            "ghcr.io/krishi-diagnostics",
            "localhost:5000/dev",
            "docker.io/acme/team",
            "localhost",
            "registry.example/a__b/c.d",
        ] {
            assert!(validate_workspace(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "GHCR.io/acme",
            "https://ghcr.io/acme",
            "ghcr.io/acme/",
            "ghcr.io",
            "acme/orders",
            "ghcr.io/acme:latest",
            "ghcr.io/acme@sha256:00",
            "ghcr.io/Acme",
            "ghcr.io//acme",
        ] {
            assert!(validate_workspace(invalid).is_err(), "{invalid}");
        }
        assert!(validate_workspace(&format!("ghcr.io/{}", "a".repeat(121))).is_err());
    }

    #[test]
    fn full_api_ids_split_at_the_last_slash() {
        let id = "localhost:5000/dev/krishi.samples@v1";
        validate_api_id(id).unwrap();
        assert_eq!(
            split_workspace(id),
            (Some("localhost:5000/dev"), "krishi.samples@v1")
        );
        assert_eq!(
            split_workspace("trellis.auth@v1"),
            (None, "trellis.auth@v1")
        );
        assert!(validate_api_id("Acme/krishi.samples@v1").is_err());
        let long = format!("ghcr.io/{}/k.samples@v1", "a".repeat(110));
        assert!(long.len() > 128);
        validate_api_id(&long).unwrap();
    }
}
