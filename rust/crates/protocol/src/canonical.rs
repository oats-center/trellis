use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ProtocolError;

/// Render JSON using RFC 8785 canonicalization.
///
/// Unlike ordinary `serde_json::to_string`, this fixes object-member ordering
/// and number rendering so independent implementations produce identical bytes.
///
/// # Errors
///
/// Returns [`ProtocolError::Json`] or [`ProtocolError::NonCanonicalNumber`] when
/// the value cannot be represented by the canonicalizer.
pub fn canonicalize_json(value: &Value) -> Result<String, ProtocolError> {
    Ok(serde_json_canonicalizer::to_string(value)?)
}

/// Compute the unpadded base64url representation of a SHA-256 text digest.
pub fn sha256_base64url(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Canonicalize JSON and return its SHA-256 digest as unpadded base64url.
///
/// # Errors
///
/// Returns [`ProtocolError::Json`] or [`ProtocolError::NonCanonicalNumber`] when
/// `value` cannot be represented as RFC 8785 canonical JSON.
pub fn digest_json(value: &Value) -> Result<String, ProtocolError> {
    Ok(sha256_base64url(&canonicalize_json(value)?))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde::Deserialize;
    use serde_json::Value;

    use super::*;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct Vector {
        name: String,
        input: Option<Value>,
        input_json: Option<String>,
        canonical: Option<String>,
        digest: Option<String>,
        #[serde(default)]
        error: bool,
    }

    #[test]
    fn matches_shared_canonical_json_vectors() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../conformance/canonical-json/vectors.json");
        let fixtures: Vec<Vector> =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();

        for fixture in fixtures {
            let input = fixture.input.map_or_else(
                || serde_json::from_str(fixture.input_json.as_deref().unwrap()),
                Ok,
            );
            if fixture.error {
                match input {
                    Ok(value) => assert!(
                        canonicalize_json(&value).is_err(),
                        "{} unexpectedly canonicalized",
                        fixture.name
                    ),
                    Err(_) => continue,
                }
                continue;
            }

            let input = input.unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
            assert_eq!(
                canonicalize_json(&input).unwrap(),
                fixture.canonical.unwrap(),
                "{}",
                fixture.name
            );
            assert_eq!(
                digest_json(&input).unwrap(),
                fixture.digest.unwrap(),
                "{}",
                fixture.name
            );
        }
    }
}
