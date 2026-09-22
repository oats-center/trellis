use std::collections::BTreeMap;

use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    identifiers::{validate_logical_name, validate_version},
    ProtocolError,
};

/// Exact generated event identity carried by a signed publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventDescriptorIdentity {
    api_id: String,
    event_name: String,
    parameter_count: usize,
}

impl EventDescriptorIdentity {
    /// Return the qualified API identity.
    #[must_use]
    pub fn api_id(&self) -> &str {
        &self.api_id
    }

    /// Return the exact API-local event name.
    #[must_use]
    pub fn event_name(&self) -> &str {
        &self.event_name
    }

    /// Return the exact number of subject parameters declared by the descriptor.
    #[must_use]
    pub const fn parameter_count(&self) -> usize {
        self.parameter_count
    }
}

/// Subjects derived for a set of communication surfaces.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedApiSubjects {
    /// RPC subjects keyed by logical name.
    pub rpc: BTreeMap<String, String>,
    /// Operation subjects keyed by logical name.
    pub operations: BTreeMap<String, String>,
    /// Event base and wildcard subjects keyed by logical name.
    pub events: BTreeMap<String, DerivedEventSubjects>,
    /// Feed subjects keyed by logical name.
    pub feeds: BTreeMap<String, String>,
}

/// Base, publish template, and wildcard subscription subjects for one event.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedEventSubjects {
    /// Event subject before parameter tokens are appended.
    pub base: String,
    /// Publish subject template with JSON-pointer placeholders for parameters.
    pub template: String,
    /// Subscription subject with one wildcard per event parameter.
    pub wildcard: String,
}

fn subject_token(value: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes())
}

/// Encode one event descriptor identity into its canonical opaque wire form.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] for an invalid API ID, event
/// name, or parameter count that cannot be represented on the wire.
pub fn encode_event_descriptor_identity(
    api_id: &str,
    event_name: &str,
    parameter_count: usize,
) -> Result<String, ProtocolError> {
    crate::validate_api_id(api_id)?;
    validate_logical_name(event_name)?;
    let parameter_count =
        u32::try_from(parameter_count).map_err(|_| ProtocolError::InvalidIdentifier {
            field: "event parameter count",
            reason: "must fit an unsigned 32-bit integer",
        })?;
    Ok(format!(
        "v1.{}.{}.{parameter_count}",
        subject_token(api_id),
        subject_token(event_name)
    ))
}

/// Parse and validate a canonical opaque event descriptor identity.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] when the value is malformed or
/// non-canonical.
pub fn decode_event_descriptor_identity(
    value: &str,
) -> Result<EventDescriptorIdentity, ProtocolError> {
    let invalid = || ProtocolError::InvalidIdentifier {
        field: "event descriptor identity",
        reason: "must be canonical v1 descriptor identity",
    };
    let mut parts = value.split('.');
    if parts.next() != Some("v1") {
        return Err(invalid());
    }
    let api_token = parts.next().ok_or_else(invalid)?;
    let event_token = parts.next().ok_or_else(invalid)?;
    let parameter_token = parts.next().ok_or_else(invalid)?;
    if parts.next().is_some() || parameter_token.is_empty() {
        return Err(invalid());
    }
    let decode = |token: &str| {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| invalid())?;
        String::from_utf8(bytes).map_err(|_| invalid())
    };
    let api_id = decode(api_token)?;
    let event_name = decode(event_token)?;
    let parameter_count = parameter_token.parse::<u32>().map_err(|_| invalid())? as usize;
    if parameter_count.to_string() != parameter_token
        || encode_event_descriptor_identity(&api_id, &event_name, parameter_count)? != value
    {
        return Err(invalid());
    }
    Ok(EventDescriptorIdentity {
        api_id,
        event_name,
        parameter_count,
    })
}

/// Validate that a concrete event subject has exactly the descriptor's shape.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] when the subject is not the
/// descriptor-derived base followed by exactly its declared parameter tokens.
pub fn validate_event_descriptor_subject(
    identity: &EventDescriptorIdentity,
    subject: &str,
) -> Result<(), ProtocolError> {
    let base = derive_event_subject(identity.api_id(), identity.event_name())?;
    let suffix = subject
        .strip_prefix(&base)
        .ok_or(ProtocolError::InvalidIdentifier {
            field: "event subject",
            reason: "must match the signed event descriptor identity",
        })?;
    let parameters = if suffix.is_empty() {
        0
    } else {
        let suffix = suffix
            .strip_prefix('.')
            .ok_or(ProtocolError::InvalidIdentifier {
                field: "event subject",
                reason: "must match the signed event descriptor identity",
            })?;
        let tokens = suffix.split('.').collect::<Vec<_>>();
        if tokens.iter().any(|token| token.is_empty()) {
            return Err(ProtocolError::InvalidIdentifier {
                field: "event subject",
                reason: "contains an empty parameter token",
            });
        }
        tokens.len()
    };
    if parameters != identity.parameter_count() {
        return Err(ProtocolError::InvalidIdentifier {
            field: "event subject",
            reason: "parameter suffix does not match the signed descriptor shape",
        });
    }
    Ok(())
}

fn derive_bound_subject(
    family: &str,
    api_id: &str,
    provider_deployment_id: &str,
    action: &str,
) -> Result<String, ProtocolError> {
    crate::validate_api_id(api_id)?;
    validate_logical_name(action)?;
    if provider_deployment_id.is_empty() {
        return Err(ProtocolError::InvalidIdentifier {
            field: "provider deployment id",
            reason: "must not be empty",
        });
    }
    Ok(format!(
        "{family}.v1.{}.{}.{}",
        subject_token(api_id),
        subject_token(provider_deployment_id),
        action
    ))
}

/// Derive a deployment-bound RPC subject.
pub fn derive_bound_rpc_subject(
    api_id: &str,
    provider_deployment_id: &str,
    action: &str,
) -> Result<String, ProtocolError> {
    derive_bound_subject("rpc", api_id, provider_deployment_id, action)
}

/// Derive a deployment-bound Operation start subject.
pub fn derive_bound_operation_subject(
    api_id: &str,
    provider_deployment_id: &str,
    action: &str,
) -> Result<String, ProtocolError> {
    derive_bound_subject("operation", api_id, provider_deployment_id, action)
}

/// Derive a deployment-bound Feed open subject.
pub fn derive_bound_feed_subject(
    api_id: &str,
    provider_deployment_id: &str,
    action: &str,
) -> Result<String, ProtocolError> {
    derive_bound_subject("feed", api_id, provider_deployment_id, action)
}

/// Derive the wildcard control subscription for one Feed owner instance.
#[must_use]
pub fn derive_feed_control_subject(feed_subject: &str, owner_instance_id: &str) -> String {
    format!(
        "{feed_subject}.control.{}.*",
        subject_token(owner_instance_id)
    )
}

/// Derive the exact control subject for one Feed instance.
#[must_use]
pub fn derive_feed_instance_control_subject(
    feed_subject: &str,
    owner_instance_id: &str,
    feed_id: &str,
) -> String {
    format!(
        "{feed_subject}.control.{}.{}",
        subject_token(owner_instance_id),
        subject_token(feed_id)
    )
}

/// Derive the one queue shared by replicas subscribed to an exact route.
#[must_use]
pub fn route_queue_group(subscription_subject: &str) -> String {
    let digest = Sha256::digest(subscription_subject.as_bytes());
    format!(
        "trellis.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    )
}

/// Derive an RPC subject from its version and logical name.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] for an invalid `vN` version or
/// logical surface name.
pub fn derive_rpc_subject(version: &str, logical_name: &str) -> Result<String, ProtocolError> {
    derive_subject("rpc", version, logical_name)
}

/// Derive an operation subject from its version and logical name.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] for an invalid `vN` version or
/// logical surface name.
pub fn derive_operation_subject(
    version: &str,
    logical_name: &str,
) -> Result<String, ProtocolError> {
    derive_subject("operations", version, logical_name)
}

/// Derive an API-qualified event base subject.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] for an invalid qualified API ID
/// or event action path.
pub fn derive_event_subject(api_id: &str, action: &str) -> Result<String, ProtocolError> {
    crate::validate_api_id(api_id)?;
    validate_logical_name(action)?;
    Ok(format!("events.v1.{}.{}", subject_token(api_id), action))
}

/// Derive an event subscription subject with one wildcard per parameter.
///
/// Parameter order is defined by the semantic API; this function appends one
/// wildcard token for each parameter without reordering it.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] for an invalid qualified API ID
/// or event action path.
pub fn derive_event_wildcard_subject(
    api_id: &str,
    action: &str,
    parameter_count: usize,
) -> Result<String, ProtocolError> {
    let mut subject = derive_event_subject(api_id, action)?;
    for _ in 0..parameter_count {
        subject.push_str(".*");
    }
    Ok(subject)
}

/// Derive a feed subject from its version and logical name.
///
/// # Errors
///
/// Returns [`ProtocolError::InvalidIdentifier`] for an invalid `vN` version or
/// logical surface name.
pub fn derive_feed_subject(version: &str, logical_name: &str) -> Result<String, ProtocolError> {
    derive_subject("feed", version, logical_name)
}

/// Return whether two tokenized event subject patterns can match the same subject.
///
/// `*` matches one token. Patterns with different token counts cannot overlap.
#[must_use]
pub fn event_patterns_overlap(left: &str, right: &str) -> bool {
    let left = left.split('.').collect::<Vec<_>>();
    let right = right.split('.').collect::<Vec<_>>();
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left == &"*" || right == "*" || left == &right)
}

fn derive_subject(
    family: &str,
    version: &str,
    logical_name: &str,
) -> Result<String, ProtocolError> {
    validate_version(version)?;
    validate_logical_name(logical_name)?;
    Ok(format!("{family}.{version}.{logical_name}"))
}

#[cfg(test)]
mod tests {
    use super::{
        derive_bound_operation_subject, derive_event_subject, derive_event_wildcard_subject,
        derive_rpc_subject, event_patterns_overlap, route_queue_group,
    };

    #[test]
    fn event_subjects_are_qualified_by_api_identity() {
        assert_eq!(
            derive_event_subject("acme-orders.runtime@v1", "Changed").unwrap(),
            "events.v1.YWNtZS1vcmRlcnMucnVudGltZUB2MQ.Changed"
        );
        assert_eq!(
            derive_event_wildcard_subject("other.runtime@v1", "Changed", 2).unwrap(),
            "events.v1.b3RoZXIucnVudGltZUB2MQ.Changed.*.*"
        );
    }

    #[test]
    fn subject_versions_use_canonical_positive_decimals() {
        assert_eq!(
            derive_rpc_subject("v1", "Documents.Get").unwrap(),
            "rpc.v1.Documents.Get"
        );
        assert_eq!(
            derive_rpc_subject("v10", "Documents.Get").unwrap(),
            "rpc.v10.Documents.Get"
        );
        assert!(derive_rpc_subject("v01", "Documents.Get").is_err());
        assert!(derive_rpc_subject("v00", "Documents.Get").is_err());
    }

    #[test]
    fn event_patterns_overlap_by_token() {
        assert!(event_patterns_overlap(
            "events.v1.Sites.Changed.*",
            "events.v1.Sites.Changed.eu"
        ));
        assert!(!event_patterns_overlap(
            "events.v1.Sites.Changed.*",
            "events.v1.Sites.Changed.eu.extra"
        ));
        assert!(!event_patterns_overlap(
            "events.v1.Sites.Changed.us",
            "events.v1.Sites.Changed.eu"
        ));
    }

    #[test]
    fn deployment_bound_subject_and_queue_are_stable() {
        let subject = derive_bound_operation_subject(
            "acme-orders.orders@v1",
            "deployment-01",
            "Refund.Start",
        )
        .unwrap();
        assert_eq!(
            subject,
            "operation.v1.YWNtZS1vcmRlcnMub3JkZXJzQHYx.ZGVwbG95bWVudC0wMQ.Refund.Start"
        );
        assert_eq!(
            route_queue_group(&subject),
            "trellis.XzNEqEQ-aqJzGNmO3z42LIzwJkDJnEgfRQIylDrYUUQ"
        );
    }
}
