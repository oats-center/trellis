use serde::{Deserialize, Serialize};

/// User projection returned by `Auth.Sessions.Me`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedUser {
    /// Stable user identifier.
    pub user_id: String,
    /// Stable identity identifier.
    pub principal_id: String,
    /// Current lifecycle state.
    pub state: String,
    /// Profile email.
    pub email: Option<String>,
    /// Profile image URL.
    pub image: Option<String>,
    /// Display name.
    pub name: Option<String>,
}
