//! Admitted-versus-renewed transport authorization for one logical connection.
//!
//! `A` is the exact signed policy the broker admitted on the current physical
//! attachment; `D` is the newest valid signed application policy. Renewal
//! promotes `D` in place without touching `A`, so the application can observe a
//! retained upgrade notice and adopt it only through an explicit transport
//! refresh. This module installs no authority: a change here only records a
//! passive notice or marks transport unavailable.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::Client;
use tokio::sync::watch;
use trellis_protocol::{
    TransportAuthorizationV1, TransportPolicyClass, TRANSPORT_AUTHORIZATION_FORMAT_V1,
};

use super::super::TrellisClientError;

/// Prefix the runtime sets on every callout-owned attachment's authenticated
/// name: `trellis.auth.v1:<contextDigest>:<serverId>:<clientId>`.
const ADMISSION_NAME_PREFIX: &str = "trellis.auth.v1:";

/// Broker subject that returns the caller's own authenticated connection info.
const OWN_USER_INFO_SUBJECT: &str = "$SYS.REQ.USER.INFO";

/// Retained transport-authorization status for one logical connection.
///
/// [`TransportAuthorizationStatus::Unavailable`] covers a real disconnect and a
/// socket whose own authority no longer covers its admitted policy; it never
/// means "application authorization is revoked", which is a separate
/// observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportAuthorizationStatus {
    /// No admitted policy is retained, or the retained policy is no longer
    /// authorized or still valid.
    Unavailable,
    /// The admitted policy still covers renewed authorization.
    Current,
    /// Renewed authorization is valid but offers transport capability the
    /// admitted policy does not cover.
    UpgradeAvailable,
}

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
#[must_use]
pub fn admission_context_digest(authenticated_user: &str) -> Option<&str> {
    let remainder = authenticated_user.strip_prefix(ADMISSION_NAME_PREFIX)?;
    let separator = remainder.find(':')?;
    if separator == 0 {
        return None;
    }
    Some(&remainder[..separator])
}

/// Read this attachment's authenticated identity from the broker.
///
/// Best effort by design: notification is triggered by a hint or a refresh, and
/// a missing or malformed reply must never install authority or lose
/// enforcement, so any failure yields [`None`] instead of an error.
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

/// Retained admitted-transport state for one logical connection.
///
/// Holds the exact signed policy actually admitted on the current physical
/// attachment (`A`) and recomputes the retained upgrade status against the
/// newest valid application policy (`D`).
#[derive(Clone, Debug)]
pub struct TransportAuthorizationState {
    retained: Arc<Mutex<Retained>>,
    status: Arc<watch::Sender<TransportAuthorizationStatus>>,
}

#[derive(Clone, Debug, Default)]
struct Retained {
    admitted_digest: Option<String>,
    admitted: Option<TransportAuthorizationV1>,
    allowed: Option<TransportAuthorizationV1>,
}

impl Default for TransportAuthorizationState {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportAuthorizationState {
    /// Create state with no retained admission.
    #[must_use]
    pub fn new() -> Self {
        let (status, _) = watch::channel(TransportAuthorizationStatus::Unavailable);
        Self {
            retained: Arc::new(Mutex::new(Retained::default())),
            status: Arc::new(status),
        }
    }

    /// Return the retained status.
    #[must_use]
    pub fn status(&self) -> TransportAuthorizationStatus {
        *self.status.borrow()
    }

    /// Subscribe to retained status changes; the current value is available
    /// immediately, so a late listener sees an outstanding upgrade notice.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<TransportAuthorizationStatus> {
        self.status.subscribe()
    }

    /// Return the admitted policy `A`.
    #[must_use]
    pub fn admitted_policy(&self) -> Option<TransportAuthorizationV1> {
        self.retained.lock().ok()?.admitted.clone()
    }

    /// Return the newest valid application policy `D` used for the last comparison.
    #[must_use]
    pub fn allowed_policy(&self) -> Option<TransportAuthorizationV1> {
        self.retained.lock().ok()?.allowed.clone()
    }

    /// Record the policy admitted on the current physical generation and
    /// recompute the status against the newest valid application policy.
    ///
    /// Called only after an actual admission read, never as a side effect of an
    /// ordinary context promotion, so an in-place renewal does not pretend the
    /// socket adopted the newer policy.
    ///
    /// # Errors
    ///
    /// Returns [`TrellisClientError::Protocol`] when the admitted policy is
    /// malformed.
    pub fn record_admission(
        &self,
        context_digest: String,
        policy: TransportAuthorizationV1,
        allowed: Option<TransportAuthorizationV1>,
        now_unix_seconds: i64,
    ) -> Result<(), TrellisClientError> {
        {
            let mut retained = self.lock()?;
            retained.admitted_digest = Some(context_digest);
            retained.admitted = Some(policy);
        }
        self.recompute(allowed, now_unix_seconds)
    }

    /// Recompute against the newest valid application policy without a new read.
    ///
    /// A duplicate renewal that changes only the context digest does not change
    /// the semantic status, so no repeated transition is published.
    ///
    /// # Errors
    ///
    /// Returns [`TrellisClientError::Protocol`] when a retained policy is
    /// malformed.
    pub fn recompute(
        &self,
        allowed: Option<TransportAuthorizationV1>,
        now_unix_seconds: i64,
    ) -> Result<(), TrellisClientError> {
        let next = {
            let mut retained = self.lock()?;
            retained.allowed = allowed;
            let Some(admitted) = retained.admitted.as_ref() else {
                drop(retained);
                self.publish(TransportAuthorizationStatus::Unavailable);
                return Ok(());
            };
            let Some(allowed) = retained.allowed.as_ref() else {
                drop(retained);
                self.publish(TransportAuthorizationStatus::Current);
                return Ok(());
            };
            match admitted.classify(allowed, now_unix_seconds)? {
                TransportPolicyClass::Current => TransportAuthorizationStatus::Current,
                TransportPolicyClass::UpgradeAvailable => {
                    TransportAuthorizationStatus::UpgradeAvailable
                }
                TransportPolicyClass::ReductionRequired => {
                    TransportAuthorizationStatus::Unavailable
                }
            }
        };
        self.publish(next);
        Ok(())
    }

    /// Record a real physical loss; the next admission recomputes from scratch.
    pub fn mark_disconnected(&self) {
        if let Ok(mut retained) = self.retained.lock() {
            retained.admitted_digest = None;
            retained.admitted = None;
        }
        self.publish(TransportAuthorizationStatus::Unavailable);
    }

    /// Whether the admitted policy `A` covers the exact subjects a need requires.
    ///
    /// The response allowance and hard deadline are copied from `A` so this
    /// answers a pure subject-coverage question about the same account.
    ///
    /// # Errors
    ///
    /// Returns [`TrellisClientError::Protocol`] when a policy is malformed.
    pub fn covers(
        &self,
        admitted: &TransportAuthorizationV1,
        publish: &[String],
        subscribe: &[String],
        now_unix_seconds: i64,
    ) -> Result<bool, TrellisClientError> {
        policy_covers(admitted, publish, subscribe, now_unix_seconds)
    }

    /// Whether `required` is a granted capability the current attachment has not
    /// adopted yet.
    ///
    /// Returns `false` when transport authority is unavailable or the
    /// requirement is not granted at all, so the caller falls through to the
    /// ordinary server decision rather than masking a real denial.
    ///
    /// # Errors
    ///
    /// Returns [`TrellisClientError::Protocol`] when a policy is malformed.
    pub fn requires_upgrade(
        &self,
        publish: &[String],
        subscribe: &[String],
        now_unix_seconds: i64,
    ) -> Result<bool, TrellisClientError> {
        if self.status() != TransportAuthorizationStatus::UpgradeAvailable {
            return Ok(false);
        }
        let (Some(admitted), Some(allowed)) = (self.admitted_policy(), self.allowed_policy())
        else {
            return Ok(false);
        };
        if self.covers(&admitted, publish, subscribe, now_unix_seconds)? {
            return Ok(false);
        }
        self.covers(&allowed, publish, subscribe, now_unix_seconds)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Retained>, TrellisClientError> {
        self.retained
            .lock()
            .map_err(|_| TrellisClientError::Bootstrap("transport state lock poisoned".into()))
    }

    fn publish(&self, next: TransportAuthorizationStatus) {
        if *self.status.borrow() != next {
            // `send` fails when no receiver is alive and would leave the
            // retained value stale, which would lose a notice for a late
            // listener; `send_replace` always updates it.
            self.status.send_replace(next);
        }
    }
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

    fn policy(publish: &[&str], subscribe: &[&str]) -> TransportAuthorizationV1 {
        TransportAuthorizationV1 {
            format: TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
            account: "ACC".to_owned(),
            publish_allow: publish.iter().map(|s| (*s).to_owned()).collect(),
            subscribe_allow: subscribe.iter().map(|s| (*s).to_owned()).collect(),
            response: None,
            hard_expires_at: None,
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
    }

    #[test]
    fn renewal_reports_upgrade_then_keeps_a_late_listener_informed() {
        let state = TransportAuthorizationState::new();
        assert_eq!(state.status(), TransportAuthorizationStatus::Unavailable);
        state
            .record_admission(
                "d1".to_owned(),
                policy(&["a"], &[]),
                Some(policy(&["a"], &[])),
                100,
            )
            .expect("record");
        assert_eq!(state.status(), TransportAuthorizationStatus::Current);

        // A renewal that widens D without touching A is a retained notice, and
        // the widening is not adopted by recording it.
        state
            .recompute(Some(policy(&["a", "b"], &[])), 100)
            .expect("recompute");
        assert_eq!(
            state.status(),
            TransportAuthorizationStatus::UpgradeAvailable
        );
        assert_eq!(state.admitted_policy().expect("A").publish_allow, vec!["a"]);
        let late = state.watch();
        assert_eq!(
            *late.borrow(),
            TransportAuthorizationStatus::UpgradeAvailable
        );

        // Withdrawing a never-adopted capability clears the notice.
        state
            .recompute(Some(policy(&["a"], &[])), 100)
            .expect("narrow");
        assert_eq!(state.status(), TransportAuthorizationStatus::Current);
    }

    #[test]
    fn a_removed_admitted_capability_is_unavailable_not_an_upgrade() {
        let state = TransportAuthorizationState::new();
        state
            .record_admission(
                "d1".to_owned(),
                policy(&["a", "b"], &[]),
                Some(policy(&["a", "b"], &[])),
                100,
            )
            .expect("record");
        state
            .recompute(Some(policy(&["a"], &[])), 100)
            .expect("narrow");
        assert_eq!(state.status(), TransportAuthorizationStatus::Unavailable);
    }

    #[test]
    fn upgrade_required_distinguishes_unadopted_grants_from_denial() {
        let state = TransportAuthorizationState::new();
        state
            .record_admission(
                "d1".to_owned(),
                policy(&["a"], &[]),
                Some(policy(&["a"], &[])),
                100,
            )
            .expect("record");
        state
            .recompute(Some(policy(&["a", "b.>"], &[])), 100)
            .expect("grow");

        // Granted by D but not admitted on A: the application may adopt it.
        assert!(state
            .requires_upgrade(&["b.>".to_owned()], &[], 100)
            .expect("required"));
        // Not granted at all: fall through to the ordinary server decision.
        assert!(!state
            .requires_upgrade(&["c.>".to_owned()], &[], 100)
            .expect("denied"));
        // Already admitted: no upgrade needed.
        assert!(!state
            .requires_upgrade(&["a".to_owned()], &[], 100)
            .expect("current"));

        // An elapsed hard deadline is a reduction, not an upgrade.
        let mut expired = policy(&["a"], &[]);
        expired.hard_expires_at = Some(50);
        state
            .record_admission("d2".to_owned(), expired.clone(), Some(expired), 100)
            .expect("record expired");
        assert_eq!(state.status(), TransportAuthorizationStatus::Unavailable);
        assert!(!state
            .requires_upgrade(&["a".to_owned()], &[], 100)
            .expect("elapsed"));
    }
}
