//! Broker admission reads and subject-coverage checks for transport generations.

use std::time::Duration;

use async_nats::Client;
use trellis_protocol::{
    TransportAuthorizationV1, TransportPolicyClass, TRANSPORT_AUTHORIZATION_FORMAT_V1,
};

use super::super::TrellisClientError;

/// Prefix the runtime sets on every callout-owned attachment's authenticated
/// name: `trellis.auth.v1:<contextDigest>:<serverId>:<clientId>`.
const ADMISSION_NAME_PREFIX: &str = "trellis.auth.v1:";

/// Broker subject that returns the caller's own authenticated connection info.
const OWN_USER_INFO_SUBJECT: &str = "$SYS.REQ.USER.INFO";

/// Result of one bounded own-user-information read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnAdmission {
    /// The authenticated connection name the broker installed.
    pub authenticated_user: String,
    /// Signed-context digest parsed from a well-formed Trellis admission name.
    pub context_digest: Option<String>,
}

/// Parse the signed-context digest out of an authenticated admission name.
///
/// Returns [`None`] for any name that is not a well-formed Trellis admission
/// marker, so a foreign or malformed name is never mistaken for admitted
/// authority.
/// The complete marker requires nonempty opaque digest and server fields,
/// no whitespace, and a canonical positive decimal `u64` client ID.
#[must_use]
pub fn admission_context_digest(authenticated_user: &str) -> Option<&str> {
    let remainder = authenticated_user.strip_prefix(ADMISSION_NAME_PREFIX)?;
    let mut fields = remainder.split(':');
    let digest = fields.next()?;
    let server_id = fields.next()?;
    let client_id = fields.next()?;
    if fields.next().is_some()
        || digest.is_empty()
        || server_id.is_empty()
        || remainder
            .chars()
            .any(|character| character.is_whitespace() || character == '\u{feff}')
        || client_id.starts_with('0')
        || !client_id.bytes().all(|byte| byte.is_ascii_digit())
        || client_id.parse::<u64>().ok()? == 0
    {
        return None;
    }
    Some(digest)
}

/// Read this attachment's authenticated identity from the broker.
///
/// A missing or malformed reply must never install authority, so any failure
/// yields [`None`] instead of an error.
pub async fn read_own_admission(nats: &Client, timeout: Duration) -> Option<OwnAdmission> {
    let reply = tokio::time::timeout(
        timeout,
        nats.request(OWN_USER_INFO_SUBJECT, bytes::Bytes::new()),
    )
    .await
    .ok()?
    .ok()?;
    let info = serde_json::from_slice::<serde_json::Value>(&reply.payload).ok()?;
    let user = info.get("data")?.get("user")?.as_str()?;
    if user.is_empty() {
        return None;
    }
    Some(OwnAdmission {
        authenticated_user: user.to_owned(),
        context_digest: admission_context_digest(user).map(str::to_owned),
    })
}

/// Canonicalize a subject-pattern list to the sorted, unique policy form.
fn canonical_patterns(patterns: &[String]) -> Vec<String> {
    let mut canonical = patterns.to_vec();
    canonical.sort();
    canonical.dedup();
    canonical
}

/// Resource family a client gates one operation on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceTransportKind {
    Kv,
    Store,
}

/// Resource operation a client distinguishes for transport admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceTransportAction {
    /// An operation that only reads from the resource.
    Read,
    /// An operation that writes to the resource.
    Write,
}

/// The single subject the runtime grants to distinguish one resource action.
///
/// These are the minimal distinguishing markers from the runtime compiler's
/// `compile_resource`; a shared fixture pins them so they cannot drift from the
/// server silently. The response allowance is never part of the question.
pub fn resource_action_marker(
    kind: ResourceTransportKind,
    bucket: &str,
    action: ResourceTransportAction,
) -> String {
    match (kind, action) {
        (ResourceTransportKind::Kv, ResourceTransportAction::Read) => {
            format!("$JS.API.DIRECT.GET.KV_{bucket}")
        }
        (ResourceTransportKind::Kv, ResourceTransportAction::Write) => {
            format!("$KV.{bucket}.>")
        }
        (ResourceTransportKind::Store, ResourceTransportAction::Read) => {
            format!("$JS.API.STREAM.INFO.OBJ_{bucket}")
        }
        (ResourceTransportKind::Store, ResourceTransportAction::Write) => {
            format!("$O.{bucket}.C.>")
        }
    }
}

/// Whether admitted policy `A` covers the exact subjects a requirement needs.
///
/// The response allowance and hard deadline are copied from `A` so this answers
/// a pure subject-coverage question about the same account; it never claims
/// authority the attachment did not admit.
///
/// # Errors
///
/// Returns [`TrellisClientError::Protocol`] when a policy is malformed.
pub(crate) fn policy_covers(
    admitted: &TransportAuthorizationV1,
    publish: &[String],
    subscribe: &[String],
    now_unix_seconds: i64,
) -> Result<bool, TrellisClientError> {
    let need = TransportAuthorizationV1 {
        format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
        account: admitted.account.clone(),
        publish_allow: canonical_patterns(publish),
        subscribe_allow: canonical_patterns(subscribe),
        response: admitted.response.clone(),
        hard_expires_at: admitted.hard_expires_at,
    };
    Ok(need.classify(admitted, now_unix_seconds)? != TransportPolicyClass::ReductionRequired)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_action_markers_match_the_shared_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../integration/fixtures/protocol/resource-grants/vectors.json");
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("resource-grant fixture"))
                .expect("resource-grant fixture json");
        for (kind, marker_kind) in [
            (ResourceTransportKind::Kv, "kv"),
            (ResourceTransportKind::Store, "store"),
        ] {
            let bucket = fixture[marker_kind]["bucket"].as_str().expect("bucket");
            for (action, field) in [
                (ResourceTransportAction::Read, "readMarker"),
                (ResourceTransportAction::Write, "writeMarker"),
            ] {
                let expected = fixture[marker_kind][field]
                    .as_str()
                    .expect("fixture marker");
                assert_eq!(
                    resource_action_marker(kind, bucket, action),
                    expected,
                    "{marker_kind} {field} must match the runtime compiler's grant"
                );
            }
        }
    }

    #[test]
    fn admission_name_parsing_rejects_foreign_names() {
        assert_eq!(
            admission_context_digest("trellis.auth.v1:digest:SERVER:7"),
            Some("digest")
        );
        assert_eq!(admission_context_digest("other-service"), None);
        assert_eq!(admission_context_digest("trellis.auth.v1:"), None);
        assert_eq!(admission_context_digest("trellis.auth.v1::S:7"), None);
        assert_eq!(
            admission_context_digest("trellis.auth.v1:opaque-digest:srv-A:18446744073709551615"),
            Some("opaque-digest")
        );
        for malformed in [
            "trellis.auth.v1:digest:",
            "trellis.auth.v1:digest:SERVER",
            "trellis.auth.v1:digest::7",
            "trellis.auth.v1:digest:SERVER:",
            "trellis.auth.v1:digest:SERVER:abc",
            "trellis.auth.v1:digest:SERVER:0",
            "trellis.auth.v1:digest:SERVER:07",
            "trellis.auth.v1:digest:SERVER:+7",
            "trellis.auth.v1:digest:SERVER:-7",
            "trellis.auth.v1:digest:SERVER:7.0",
            "trellis.auth.v1:digest:SERVER:18446744073709551616",
            "trellis.auth.v1:digest:SERVER:7:",
            "trellis.auth.v1:digest:SERVER:7:extra",
            "trellis.auth.v1:di gest:SERVER:7",
            "trellis.auth.v1:digest:SER VER:7",
            "trellis.auth.v1:digest:SER\u{85}VER:7",
            "trellis.auth.v1:digest:SER\u{feff}VER:7",
            "trellis.auth.v1:digest:SERVER:7 ",
            "trellis.auth.v1:digest:SERVER:7\n",
        ] {
            assert_eq!(admission_context_digest(malformed), None, "{malformed:?}");
        }
    }
}
