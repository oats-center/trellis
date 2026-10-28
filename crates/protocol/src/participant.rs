use serde::{Deserialize, Serialize};

/// Deployable participant kind carried by runtime evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ParticipantKind {
    /// A service participant.
    Service,
    /// A device participant.
    Device,
}
