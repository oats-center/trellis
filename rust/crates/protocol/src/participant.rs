use serde::{Deserialize, Serialize};

/// Participant kind carried by runtime evidence and signed authorization state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ParticipantKind {
    /// A service participant.
    Service,
    /// An application participant.
    App,
    /// A device participant.
    Device,
    /// An agent participant.
    Agent,
}
