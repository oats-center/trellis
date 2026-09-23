use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

use crate::{
    canonicalize_json, identifiers::compare_protocol_strings, sha256_base64url, ProtocolError,
};

/// The first canonical grant-set wire format.
pub const GRANT_SET_FORMAT_V1: &str = "trellis.grant-set.v1";

/// An externally visible API surface kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ApiSurfaceKind {
    /// A request/reply RPC.
    Rpc,
    /// A caller-visible asynchronous operation.
    Operation,
    /// A published event.
    Event,
    /// A subscribable live observation.
    Live,
    /// Shared state.
    State,
}

impl ApiSurfaceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::Operation => "operation",
            Self::Event => "event",
            Self::Live => "live",
            Self::State => "state",
        }
    }
}

/// A participant-private resource kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ParticipantResourceKind {
    /// A NATS key-value resource.
    Kv,
    /// A blob store resource.
    Store,
    /// A private Jobs queue.
    JobQueue,
    /// A durable contract-event consumer.
    EventConsumer,
    /// Participant-local private state.
    State,
}

impl ParticipantResourceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Kv => "kv",
            Self::Store => "store",
            Self::JobQueue => "jobQueue",
            Self::EventConsumer => "eventConsumer",
            Self::State => "state",
        }
    }
}

/// A machine-enforceable permission action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionAction {
    /// Call an RPC.
    Call,
    /// Start an operation.
    Invoke,
    /// Observe an operation.
    Observe,
    /// Cancel an operation.
    Cancel,
    /// Send an operation control signal.
    Control,
    /// Publish an event.
    Publish,
    /// Subscribe to an event or live observation.
    Subscribe,
    /// Read state or a resource.
    Read,
    /// Write state or a resource.
    Write,
    /// Delete a resource entry.
    Delete,
    /// Submit a private job.
    Submit,
    /// Process a private job.
    Process,
    /// Consume from a durable event consumer.
    Consume,
}

impl PermissionAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Invoke => "invoke",
            Self::Observe => "observe",
            Self::Cancel => "cancel",
            Self::Control => "control",
            Self::Publish => "publish",
            Self::Subscribe => "subscribe",
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::Submit => "submit",
            Self::Process => "process",
            Self::Consume => "consume",
        }
    }
}

/// The exact API surface, operation signal, or participant resource targeted by a permission.
///
/// Owner and local-name strings must be nonempty, have no surrounding
/// whitespace, and contain no ASCII control characters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum PermissionTarget {
    /// An externally visible surface owned by an API definition.
    #[serde(rename = "apiSurface")]
    ApiSurface {
        /// The owning versioned API identifier.
        api: String,
        /// The surface family.
        surface: ApiSurfaceKind,
        /// The API-local surface name.
        name: String,
    },
    /// A private resource owned by a participant definition.
    #[serde(rename = "participantResource")]
    ParticipantResource {
        /// The owning participant identifier.
        participant: String,
        /// The resource family.
        resource: ParticipantResourceKind,
        /// The participant-local resource name.
        name: String,
    },
    /// One named signal on a caller-visible asynchronous operation.
    #[serde(rename = "operationSignal")]
    OperationSignal {
        /// The owning versioned API identifier.
        api: String,
        /// The API-local operation name.
        operation: String,
        /// The operation-local signal name.
        signal: String,
    },
}

impl<'de> Deserialize<'de> for PermissionTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind")]
        enum WireTarget {
            #[serde(rename = "apiSurface")]
            ApiSurface {
                api: String,
                surface: ApiSurfaceKind,
                name: String,
            },
            #[serde(rename = "participantResource")]
            ParticipantResource {
                participant: String,
                resource: ParticipantResourceKind,
                name: String,
            },
            #[serde(rename = "operationSignal")]
            OperationSignal {
                api: String,
                operation: String,
                signal: String,
            },
        }

        match WireTarget::deserialize(deserializer)? {
            WireTarget::ApiSurface { api, surface, name } => {
                Self::api_surface(api, surface, name).map_err(D::Error::custom)
            }
            WireTarget::ParticipantResource {
                participant,
                resource,
                name,
            } => Self::participant_resource(participant, resource, name).map_err(D::Error::custom),
            WireTarget::OperationSignal {
                api,
                operation,
                signal,
            } => Self::operation_signal(api, operation, signal).map_err(D::Error::custom),
        }
    }
}

impl PermissionTarget {
    /// Construct an API-surface permission target.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidIdentifier`] when the API identifier or
    /// surface name violates the target string rules.
    pub fn api_surface(
        api: impl Into<String>,
        surface: ApiSurfaceKind,
        name: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let target = Self::ApiSurface {
            api: api.into(),
            surface,
            name: name.into(),
        };
        target.validate()?;
        Ok(target)
    }

    /// Construct a participant-resource permission target.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidIdentifier`] when the participant
    /// identifier or resource name violates the target string rules.
    pub fn participant_resource(
        participant: impl Into<String>,
        resource: ParticipantResourceKind,
        name: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let target = Self::ParticipantResource {
            participant: participant.into(),
            resource,
            name: name.into(),
        };
        target.validate()?;
        Ok(target)
    }

    /// Construct an operation-signal permission target.
    pub fn operation_signal(
        api: impl Into<String>,
        operation: impl Into<String>,
        signal: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let target = Self::OperationSignal {
            api: api.into(),
            operation: operation.into(),
            signal: signal.into(),
        };
        target.validate()?;
        Ok(target)
    }

    /// Return the API identifier, surface kind, and local name for an API target.
    pub fn as_api_surface(&self) -> Option<(&str, ApiSurfaceKind, &str)> {
        match self {
            Self::ApiSurface { api, surface, name } => Some((api, *surface, name)),
            Self::ParticipantResource { .. } => None,
            Self::OperationSignal { .. } => None,
        }
    }

    /// Return the API, operation, and signal names for an operation-signal target.
    pub fn as_operation_signal(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::OperationSignal {
                api,
                operation,
                signal,
            } => Some((api, operation, signal)),
            _ => None,
        }
    }

    /// Return the participant identifier, resource kind, and local resource name.
    #[must_use]
    pub fn as_participant_resource(&self) -> Option<(&str, ParticipantResourceKind, &str)> {
        match self {
            Self::ParticipantResource {
                participant,
                resource,
                name,
            } => Some((participant, *resource, name)),
            Self::ApiSurface { .. } | Self::OperationSignal { .. } => None,
        }
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::ApiSurface { api, name, .. } => {
                validate_identifier("API identifier", api)?;
                validate_identifier("surface name", name)
            }
            Self::ParticipantResource {
                participant, name, ..
            } => {
                validate_identifier("participant identifier", participant)?;
                validate_identifier("resource name", name)
            }
            Self::OperationSignal {
                api,
                operation,
                signal,
            } => {
                validate_identifier("API identifier", api)?;
                validate_identifier("operation name", operation)?;
                validate_identifier("signal name", signal)
            }
        }
    }

    fn ordering_key(&self) -> (&'static str, &str, &'static str, &str, &str) {
        match self {
            Self::ApiSurface { api, surface, name } => {
                ("apiSurface", api, surface.as_str(), name, "")
            }
            Self::ParticipantResource {
                participant,
                resource,
                name,
            } => (
                "participantResource",
                participant,
                resource.as_str(),
                name,
                "",
            ),
            Self::OperationSignal {
                api,
                operation,
                signal,
            } => ("operationSignal", api, "operation", operation, signal),
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::ApiSurface { surface, .. } => surface.as_str(),
            Self::ParticipantResource { resource, .. } => resource.as_str(),
            Self::OperationSignal { .. } => "operationSignal",
        }
    }
}

/// One exact machine-enforceable permission.
///
/// Valid actions depend on the target family. For example, RPCs accept `call`,
/// events accept `publish` or `subscribe`, and private Jobs queues accept
/// `submit` or `process`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PermissionAtom {
    target: PermissionTarget,
    action: PermissionAction,
}

impl PermissionAtom {
    /// Construct and validate a permission atom.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidIdentifier`] for an invalid target or
    /// [`ProtocolError::InvalidPermission`] when the action is not valid for the
    /// target family.
    pub fn new(target: PermissionTarget, action: PermissionAction) -> Result<Self, ProtocolError> {
        target.validate()?;
        let atom = Self { target, action };
        atom.validate_action()?;
        Ok(atom)
    }

    /// Return the exact permission target.
    pub fn target(&self) -> &PermissionTarget {
        &self.target
    }

    /// Return the machine action.
    pub fn action(&self) -> PermissionAction {
        self.action
    }

    fn validate_action(&self) -> Result<(), ProtocolError> {
        let valid = match (&self.target, self.action) {
            (PermissionTarget::ApiSurface { surface, .. }, action) => match surface {
                ApiSurfaceKind::Rpc => matches!(action, PermissionAction::Call),
                ApiSurfaceKind::Operation => matches!(
                    action,
                    PermissionAction::Invoke | PermissionAction::Observe | PermissionAction::Cancel
                ),
                ApiSurfaceKind::Event => matches!(
                    action,
                    PermissionAction::Publish | PermissionAction::Subscribe
                ),
                ApiSurfaceKind::Live => matches!(action, PermissionAction::Subscribe),
                ApiSurfaceKind::State => {
                    matches!(action, PermissionAction::Read | PermissionAction::Write)
                }
            },
            (PermissionTarget::ParticipantResource { resource, .. }, action) => match resource {
                ParticipantResourceKind::Kv | ParticipantResourceKind::Store => matches!(
                    action,
                    PermissionAction::Read | PermissionAction::Write | PermissionAction::Delete
                ),
                ParticipantResourceKind::JobQueue => {
                    matches!(action, PermissionAction::Submit | PermissionAction::Process)
                }
                ParticipantResourceKind::EventConsumer => {
                    matches!(
                        action,
                        PermissionAction::Consume
                            | PermissionAction::Read
                            | PermissionAction::Control
                    )
                }
                ParticipantResourceKind::State => {
                    matches!(
                        action,
                        PermissionAction::Read | PermissionAction::Write | PermissionAction::Delete
                    )
                }
            },
            (PermissionTarget::OperationSignal { .. }, action) => {
                matches!(action, PermissionAction::Control)
            }
        };

        if valid {
            Ok(())
        } else {
            Err(ProtocolError::InvalidPermission {
                action: self.action.as_str().to_string(),
                target: self.target.description(),
            })
        }
    }

    fn ordering_key(&self) -> (&'static str, &str, &'static str, &str, &str, &'static str) {
        let (kind, owner, target_kind, name, detail) = self.target.ordering_key();
        (kind, owner, target_kind, name, detail, self.action.as_str())
    }
}

impl<'de> Deserialize<'de> for PermissionAtom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireAtom {
            target: PermissionTarget,
            action: PermissionAction,
        }

        let wire = WireAtom::deserialize(deserializer)?;
        Self::new(wire.target, wire.action).map_err(D::Error::custom)
    }
}

/// A named capability's normalized machine permissions.
///
/// A capability is an authoring and explanation grouping. It does not itself
/// confer authority; enforcement uses exact atoms in a [`GrantSet`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityDefinition {
    allows: Vec<PermissionAtom>,
}

impl CapabilityDefinition {
    /// Construct a capability and normalize duplicate permissions.
    pub fn new(allows: Vec<PermissionAtom>) -> Self {
        Self {
            allows: normalize_permissions(allows),
        }
    }

    /// Return normalized machine permissions in canonical order.
    pub fn allows(&self) -> &[PermissionAtom] {
        &self.allows
    }
}

impl<'de> Deserialize<'de> for CapabilityDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireCapability {
            allows: Vec<PermissionAtom>,
        }

        Ok(Self::new(WireCapability::deserialize(deserializer)?.allows))
    }
}

/// Human-facing consent copy, separate from enforceable permissions.
///
/// These strings may be shown during owner review, but are non-authoritative.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConsentMetadata {
    /// Short consent heading.
    pub title: String,
    /// Plain-language capability description.
    pub description: String,
    /// Plain-language consequence of granting consent.
    pub consequence: String,
}

/// Server-assigned meta-authority that cannot be expressed by an action permission.
///
/// Participant capabilities and capability groups cannot define these privileges.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum PlatformPrivilege {
    /// Global administration, including access across authorization owners.
    #[serde(rename = "trellis.auth::admin")]
    Admin,
}

/// Server-owned authorization owner shared by grant bindings and signed contexts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GrantOwnerKind {
    /// One deployment and its provisioned service/device instances.
    Deployment,
    /// One user account.
    User,
}

/// A normalized, content-addressed set of machine permissions.
///
/// Construction sorts by UTF-16 code units and removes duplicate atoms. The
/// digest therefore identifies the enforceable set rather than authored order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GrantSet {
    format: String,
    permissions: Vec<PermissionAtom>,
}

impl GrantSet {
    /// Construct a normalized grant set in the current format.
    pub fn new(permissions: Vec<PermissionAtom>) -> Self {
        Self {
            format: GRANT_SET_FORMAT_V1.to_string(),
            permissions: normalize_permissions(permissions),
        }
    }

    /// Return the grant-set wire format.
    pub fn format(&self) -> &str {
        &self.format
    }

    /// Return permissions in canonical semantic order.
    pub fn permissions(&self) -> &[PermissionAtom] {
        &self.permissions
    }

    /// Render the normalized grant set as canonical Trellis JSON.
    ///
    /// # Errors
    ///
    /// Returns a serialization or canonicalization [`ProtocolError`] if the
    /// normalized value cannot be encoded.
    pub fn canonical_json(&self) -> Result<String, ProtocolError> {
        canonicalize_json(&serde_json::to_value(self)?)
    }

    /// Return the grant set's SHA-256/base64url content digest.
    ///
    /// # Errors
    ///
    /// Returns a serialization or canonicalization [`ProtocolError`] if the
    /// normalized value cannot be encoded before hashing.
    pub fn digest(&self) -> Result<String, ProtocolError> {
        Ok(sha256_base64url(&self.canonical_json()?))
    }
}

impl<'de> Deserialize<'de> for GrantSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireGrantSet {
            format: String,
            permissions: Vec<PermissionAtom>,
        }

        let wire = WireGrantSet::deserialize(deserializer)?;
        if wire.format != GRANT_SET_FORMAT_V1 {
            return Err(D::Error::custom(ProtocolError::InvalidGrantSetFormat(
                wire.format,
            )));
        }
        Ok(Self::new(wire.permissions))
    }
}

fn normalize_permissions(mut permissions: Vec<PermissionAtom>) -> Vec<PermissionAtom> {
    permissions.sort_by(compare_permission_atoms);
    permissions.dedup();
    permissions
}

fn compare_permission_atoms(left: &PermissionAtom, right: &PermissionAtom) -> std::cmp::Ordering {
    let left = left.ordering_key();
    let right = right.ordering_key();
    compare_protocol_strings(left.0, right.0)
        .then_with(|| compare_protocol_strings(left.1, right.1))
        .then_with(|| compare_protocol_strings(left.2, right.2))
        .then_with(|| compare_protocol_strings(left.3, right.3))
        .then_with(|| compare_protocol_strings(left.4, right.4))
        .then_with(|| compare_protocol_strings(left.5, right.5))
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    let reason = if value.is_empty() {
        Some("must not be empty")
    } else if value.trim() != value {
        Some("must not have leading or trailing whitespace")
    } else if value.chars().any(|character| character.is_ascii_control()) {
        Some("must not contain ASCII control characters")
    } else {
        None
    };

    reason.map_or(Ok(()), |reason| {
        Err(ProtocolError::InvalidIdentifier { field, reason })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_admin_is_a_platform_privilege() {
        assert_eq!(
            serde_json::from_str::<PlatformPrivilege>(r#""trellis.auth::admin""#).unwrap(),
            PlatformPrivilege::Admin
        );
        for value in [
            "trellis.auth::capabilities.delegate",
            "trellis.auth::users.manage",
            "admin",
        ] {
            assert!(serde_json::from_value::<PlatformPrivilege>(serde_json::json!(value)).is_err());
        }
    }

    #[test]
    fn permission_atom_round_trips_and_accepts_current_names() {
        let atom = PermissionAtom::new(
            PermissionTarget::api_surface("trellis.core@v1", ApiSurfaceKind::Rpc, "Documents.Get")
                .unwrap(),
            PermissionAction::Call,
        )
        .unwrap();
        let json = serde_json::to_string(&atom).unwrap();
        assert_eq!(serde_json::from_str::<PermissionAtom>(&json).unwrap(), atom);

        PermissionTarget::participant_resource(
            "billing-refunds",
            ParticipantResourceKind::JobQueue,
            "reindex",
        )
        .unwrap();

        assert!(PermissionAtom::new(
            PermissionTarget::api_surface(
                "trellis.core@v1",
                ApiSurfaceKind::Operation,
                "Documents.Build",
            )
            .unwrap(),
            PermissionAction::Control,
        )
        .is_err());
        PermissionAtom::new(
            PermissionTarget::operation_signal("trellis.core@v1", "Documents.Build", "approve")
                .unwrap(),
            PermissionAction::Control,
        )
        .unwrap();

        for action in [
            PermissionAction::Consume,
            PermissionAction::Read,
            PermissionAction::Control,
        ] {
            PermissionAtom::new(
                PermissionTarget::participant_resource(
                    "documents-worker",
                    ParticipantResourceKind::EventConsumer,
                    "updates",
                )
                .unwrap(),
                action,
            )
            .unwrap();
        }
    }

    #[test]
    fn permission_target_deserialization_validates_direct_values() {
        for invalid in [
            serde_json::json!({
                "kind": "apiSurface",
                "api": "",
                "surface": "rpc",
                "name": "Documents.Get"
            }),
            serde_json::json!({
                "kind": "participantResource",
                "participant": " documents-worker",
                "resource": "jobQueue",
                "name": "reindex"
            }),
            serde_json::json!({
                "kind": "apiSurface",
                "api": "documents@v1",
                "surface": "rpc",
                "name": "Documents.\nGet"
            }),
        ] {
            assert!(serde_json::from_value::<PermissionTarget>(invalid).is_err());
        }

        let value = serde_json::json!({
            "kind": "apiSurface",
            "api": "trellis.core@v1",
            "surface": "rpc",
            "name": "Documents.Get"
        });
        let target: PermissionTarget = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(target).unwrap(), value);

        let extended = serde_json::json!({
            "kind": "operationSignal",
            "api": "trellis.core@v1",
            "operation": "Documents.Build",
            "signal": "approve",
            "extra": true
        });
        let target: PermissionTarget = serde_json::from_value(extended).unwrap();
        assert_eq!(
            serde_json::to_value(target).unwrap(),
            serde_json::json!({
                "kind": "operationSignal",
                "api": "trellis.core@v1",
                "operation": "Documents.Build",
                "signal": "approve"
            })
        );
    }

    #[test]
    fn consent_metadata_is_not_part_of_grant_identity() {
        let grant_set = GrantSet::new(Vec::new());
        let before = grant_set.digest().unwrap();
        let _: ConsentMetadata = serde_json::from_value(serde_json::json!({
            "title": "View documents",
            "description": "Read documents available to your account.",
            "consequence": "This application can view your documents."
        }))
        .unwrap();
        assert_eq!(grant_set.digest().unwrap(), before);
    }

    #[test]
    fn grant_identity_is_independent_of_input_order() {
        let first = PermissionAtom::new(
            PermissionTarget::api_surface("documents@v1", ApiSurfaceKind::Rpc, "Documents.Get")
                .unwrap(),
            PermissionAction::Call,
        )
        .unwrap();
        let second = PermissionAtom::new(
            PermissionTarget::participant_resource(
                "documents-worker",
                ParticipantResourceKind::JobQueue,
                "reindex",
            )
            .unwrap(),
            PermissionAction::Process,
        )
        .unwrap();

        let forward = GrantSet::new(vec![first.clone(), second.clone()]);
        let reverse = GrantSet::new(vec![second, first.clone()]);
        assert_eq!(
            forward.canonical_json().unwrap(),
            reverse.canonical_json().unwrap()
        );
        assert_eq!(forward.digest().unwrap(), reverse.digest().unwrap());

        let capability = CapabilityDefinition::new(vec![first.clone(), first]);
        assert_eq!(capability.allows().len(), 1);
    }

    #[test]
    fn capability_and_consent_objects_ignore_extensions() {
        let capability: CapabilityDefinition = serde_json::from_value(serde_json::json!({
            "allows": [],
            "title": "not machine policy"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(capability).unwrap(),
            serde_json::json!({ "allows": [] })
        );
        let consent: ConsentMetadata = serde_json::from_value(serde_json::json!({
            "title": "View documents",
            "description": "Read documents.",
            "consequence": "Documents are visible.",
            "allows": []
        }))
        .unwrap();
        assert!(serde_json::to_value(consent)
            .unwrap()
            .get("allows")
            .is_none());
    }

    #[test]
    fn permission_and_grant_objects_ignore_extensions() {
        for atom in [
            serde_json::json!({
                "target": { "kind": "apiSurface", "api": "documents@v1", "surface": "rpc", "name": "Documents.Get" },
                "action": "call",
                "extra": true
            }),
            serde_json::json!({
                "target": { "kind": "apiSurface", "api": "documents@v1", "surface": "rpc", "name": "Documents.Get", "extra": true },
                "action": "call"
            }),
        ] {
            let atom: PermissionAtom = serde_json::from_value(atom).unwrap();
            assert!(serde_json::to_value(atom).unwrap().get("extra").is_none());
        }

        let grant: GrantSet = serde_json::from_value(serde_json::json!({
            "format": GRANT_SET_FORMAT_V1,
            "permissions": [],
            "extra": true
        }))
        .unwrap();
        assert_eq!(grant, GrantSet::new(Vec::new()));
    }
}
