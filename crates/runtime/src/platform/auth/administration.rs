//! The six administrative capabilities: installation-wide powers in the
//! reserved `trellis.*` namespace, declared by the builtin Auth contract.

use super::sqlite::{AuthError, Connection, SqliteAuthorizationStore};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Administration {
    ManagePrincipals,
    ManageRoles,
    AcceptApis,
    ForceReplaceApis,
    ManageClients,
    ManagePlatform,
}

impl Administration {
    pub(crate) const ALL: [Self; 6] = [
        Self::ManagePrincipals,
        Self::ManageRoles,
        Self::AcceptApis,
        Self::ForceReplaceApis,
        Self::ManageClients,
        Self::ManagePlatform,
    ];

    pub(crate) fn capability_id(self) -> &'static str {
        match self {
            Self::ManagePrincipals => "trellis.auth.v1:users.manage",
            Self::ManageRoles => "trellis.auth.v1:roles.manage",
            Self::AcceptApis => "trellis.auth.v1:apis.accept",
            Self::ForceReplaceApis => "trellis.auth.v1:apis.forceReplace",
            Self::ManageClients => "trellis.auth.v1:clients.manage",
            Self::ManagePlatform => "trellis.auth.v1:platform.manage",
        }
    }
}

/// Whether a capability ID is one of the administrative capabilities.
pub(crate) fn is_administrative(capability: &str) -> bool {
    Administration::ALL
        .iter()
        .any(|administration| administration.capability_id() == capability)
}

/// Whether a capability is administrative, or a group contains one.
pub(super) fn administrative(connection: &Connection, id: &str) -> Result<bool, AuthError> {
    Ok(is_administrative(id)
        || super::policy::current_members(connection, id)?
            .is_some_and(|members| members.iter().any(|member| is_administrative(member))))
}

impl SqliteAuthorizationStore {
    /// Recheck inside the mutation's transaction that `actor` still holds
    /// `administration` through a live entitlement path: a direct grant, a role,
    /// or an identity-provider mapping under any of their active logins.
    pub(super) fn require_administration(
        connection: &Connection,
        actor: &str,
        administration: Administration,
    ) -> Result<(), AuthError> {
        if holds(connection, actor, administration.capability_id())? {
            Ok(())
        } else {
            Err(AuthError::Denied)
        }
    }

    /// Changing who holds an administrative capability is itself the power to
    /// manage the platform.
    pub(super) fn require_platform_for<'a>(
        connection: &Connection,
        actor: &str,
        capabilities: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), AuthError> {
        for capability in capabilities {
            if administrative(connection, capability)? {
                return Self::require_administration(
                    connection,
                    actor,
                    Administration::ManagePlatform,
                );
            }
        }
        Ok(())
    }
}

fn holds(connection: &Connection, actor: &str, capability: &str) -> Result<bool, AuthError> {
    use rusqlite::OptionalExtension as _;
    let now = crate::clock::unix_now();
    let active: Option<bool> = connection
        .query_row(
            "SELECT state='active' FROM auth_principals WHERE principal_id=?1",
            [actor],
            |row| row.get(0),
        )
        .optional()?;
    if active != Some(true) {
        return Ok(false);
    }
    let mut logins: Vec<Option<String>> = vec![None];
    logins.extend(
        connection
            .prepare(
                "SELECT login_session_id FROM auth_login_sessions \
                WHERE principal_id=?1 AND revoked_at IS NULL AND expires_at>?2",
            )?
            .query_map(rusqlite::params![actor, now], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?
            .into_iter()
            .map(Some),
    );
    for login in logins {
        if super::policy::entitlements(connection, actor, login.as_deref(), now)?
            .contains_key(capability)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
