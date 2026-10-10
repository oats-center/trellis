use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

use crate::{canonicalize_json, ProtocolError};

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

    /// Encode this target as its canonical wire bytes.
    ///
    /// The opaque `target` field of `Auth.Grants.Set` and received grant sets
    /// carries a permission target as canonical (RFC 8785) UTF-8 JSON. This
    /// method is the protocol owner for that encoding: it validates the target,
    /// then serializes the protocol representation canonically. Consumers that
    /// author grants must use it rather than reconstruct the bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the target is invalid or cannot be
    /// canonicalized.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let value = serde_json::to_value(self)?;
        Ok(canonicalize_json(&value)?.into_bytes())
    }

    /// Decode a permission target from its canonical wire bytes.
    ///
    /// Forgiving of member order and insignificant whitespace, then validated
    /// identically to a constructed target.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the bytes are not a valid encoded target.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let target: Self = serde_json::from_slice(bytes)?;
        target.validate()?;
        Ok(target)
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

/// Server-assigned meta-authority that cannot be expressed by an action permission.
///
/// Participant capabilities and capability groups cannot define these privileges.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum PlatformPrivilege {
    /// Manage principals, deployments, devices, resources, and OIDC connections.
    #[serde(rename = "principals.manage")]
    PrincipalsManage,
    /// Manage roles, assignments, and OIDC role mappings.
    #[serde(rename = "roles.manage")]
    RolesManage,
    /// Accept normally compatible API definitions.
    #[serde(rename = "apis.accept")]
    ApisAccept,
    /// Explicitly replace an incompatible API generation.
    #[serde(rename = "apis.forceReplace")]
    ApisForceReplace,
    /// Manage registered public OAuth clients.
    #[serde(rename = "clients.manage")]
    ClientsManage,
    /// Manage finite privileges, administrative delegations, and issuer keys.
    #[serde(rename = "privileges.manage")]
    PrivilegesManage,
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
}
