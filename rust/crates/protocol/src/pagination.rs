use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::sha256_base64url;

/// An invalid or query-incompatible pagination cursor.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid pagination cursor")]
pub struct InvalidPagination;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cursor<T> {
    v: u8,
    query_digest: String,
    after: T,
}

/// Computes the cursor binding for an endpoint and its normalized query.
pub fn pagination_query_digest(endpoint: &str, query: &Value) -> Result<String, InvalidPagination> {
    let value = serde_json::json!({ "endpoint": endpoint, "query": query });
    let canonical = crate::canonicalize_json(&value).map_err(|_| InvalidPagination)?;
    Ok(sha256_base64url(&canonical))
}

/// Encodes a versioned opaque keyset cursor.
pub fn encode_pagination_cursor<T: Serialize>(
    query_digest: &str,
    after: &T,
) -> Result<String, InvalidPagination> {
    serde_json::to_vec(&Cursor {
        v: 1,
        query_digest: query_digest.to_owned(),
        after,
    })
    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    .map_err(|_| InvalidPagination)
}

/// Decodes and validates a versioned opaque keyset cursor for one query.
pub fn decode_pagination_cursor<T: DeserializeOwned>(
    encoded: &str,
    query_digest: &str,
) -> Result<T, InvalidPagination> {
    if encoded.is_empty() {
        return Err(InvalidPagination);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| InvalidPagination)?;
    let cursor: Cursor<T> = serde_json::from_slice(&bytes).map_err(|_| InvalidPagination)?;
    if cursor.v != 1 || cursor.query_digest != query_digest {
        return Err(InvalidPagination);
    }
    Ok(cursor.after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_bound_to_version_query_and_after_type() {
        let digest =
            pagination_query_digest("Events.Query", &serde_json::json!({"sort":"asc"})).unwrap();
        let cursor = encode_pagination_cursor(&digest, &("time", 7_u64)).unwrap();
        assert_eq!(
            decode_pagination_cursor::<(String, u64)>(&cursor, &digest).unwrap(),
            ("time".to_owned(), 7)
        );
        assert!(decode_pagination_cursor::<String>(&cursor, &digest).is_err());
        assert!(decode_pagination_cursor::<(String, u64)>(&cursor, "other").is_err());
        let wrong_version = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "v": 2,
                "queryDigest": digest,
                "after": ["time", 7]
            }))
            .unwrap(),
        );
        assert!(decode_pagination_cursor::<(String, u64)>(&wrong_version, &digest).is_err());
    }
}
