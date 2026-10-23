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
        if capabilities.into_iter().any(is_administrative) {
            Self::require_administration(connection, actor, Administration::ManagePlatform)?;
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

/// Register the peer reviews of administrative delegations.
pub(crate) fn register_rpc<P: trellis_rs::generated::ParticipantDescriptor>(
    provider: &mut trellis_runtime_apis::apis::trellis_auth_v1::Provider<'_, P>,
    store: std::sync::Arc<SqliteAuthorizationStore>,
) {
    use super::blocking;
    use super::rpc::{caller, mutation, rpc_error};
    let list = store.clone();
    provider.register_administration_reviews_list(move |context, input| {
        let store = list.clone();
        async move {
            let actor =
                caller(&context, Some(Administration::ManagePlatform)).map_err(rpc_error)?;
            blocking(move || {
                store.read(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManagePlatform,
                    )?;
                    list_reviews(connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_administration_reviews_decide(move |context, input| {
        let store = store.clone();
        async move {
            let identity = mutation(
                &context,
                &input.idempotency_key,
                Some(Administration::ManagePlatform),
                &input,
            )
            .map_err(rpc_error)?;
            blocking(move || decide(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });
}

fn list_reviews(
    connection: &Connection,
    input: &trellis_runtime_apis::types::AuthPageRequest,
) -> Result<trellis_runtime_apis::types::AuthAdministrationReviewsListResponse, AuthError> {
    let limit = input
        .page
        .as_ref()
        .and_then(|page| page.limit)
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Denied);
    }
    let cursor = input
        .page
        .as_ref()
        .and_then(|page| page.cursor.as_deref())
        .unwrap_or("");
    let mut ids = connection
        .prepare(
            "SELECT review_id FROM auth_administration_reviews \
            WHERE review_id>?1 ORDER BY review_id LIMIT ?2",
        )?
        .query_map(rusqlite::params![cursor, limit + 1], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit as usize).then(|| ids[limit as usize - 1].clone());
    ids.truncate(limit as usize);
    Ok(trellis_runtime_apis::__types::CursorPage {
        items: ids
            .iter()
            .map(|id| read_review(connection, id))
            .collect::<Result<Vec<_>, _>>()?,
        page: trellis_runtime_apis::__types::CursorPageInfo { next_cursor },
    })
}

fn read_review(
    connection: &Connection,
    review: &str,
) -> Result<trellis_runtime_apis::types::AuthAdministrationReview, AuthError> {
    use rusqlite::OptionalExtension as _;
    let row: (String, String, String, String, String, String, i64, u64) = connection
        .query_row(
            "SELECT r.principal_id,coalesce(u.display_name,\
            (SELECT c.normalized_username FROM auth_local_credentials c \
            WHERE c.principal_id=r.principal_id),r.principal_id),\
            r.client_id,coalesce((SELECT o.display_name FROM auth_oauth_clients o \
            WHERE o.client_id=r.client_id),r.client_id),\
            r.capability_id,r.state,r.requested_at,r.revision \
            FROM auth_administration_reviews r \
            LEFT JOIN auth_user_profiles u ON u.principal_id=r.principal_id \
            WHERE r.review_id=?1",
            [review],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let (principal, title, client, client_name, capability, state, requested, revision) = row;
    Ok(serde_json::from_value(serde_json::json!({
        "reviewId": review,
        "principalId": principal,
        "principalTitle": title,
        "clientId": client,
        "clientName": client_name,
        "capabilityId": capability,
        "state": state,
        "requestedAt": super::sqlite::timestamp(requested)?,
        "revision": revision.to_string(),
    }))?)
}

/// Decide a pending review. Another holder of `trellis.auth.v1:platform.manage` decides;
/// nobody approves their own delegation. Approval also reaches the user's
/// live grants for that app binding that asked for the capability.
pub(super) fn decide(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &trellis_runtime_apis::types::AuthAdministrationReviewDecideRequest,
    now: i64,
) -> Result<trellis_runtime_apis::types::AuthAdministrationReviewResponse, AuthError> {
    use trellis_runtime_apis::types::{AuthAdministrationReviewState, AuthDecision};
    store.mutate(
        identity,
        "rpc.administration.reviews.decide",
        input,
        now,
        |connection| {
            SqliteAuthorizationStore::require_administration(
                connection,
                &identity.actor,
                Administration::ManagePlatform,
            )?;
            let id = input.review_id.to_string();
            let review = read_review(connection, &id)?;
            if review.principal_id.to_string() == identity.actor {
                return Err(AuthError::Denied);
            }
            if review.state != AuthAdministrationReviewState::Pending
                || review.revision != input.expected_revision
            {
                return Err(AuthError::Conflict);
            }
            let state = match input.decision {
                AuthDecision::Approved => "approved",
                AuthDecision::Declined => "rejected",
                _ => return Err(AuthError::Denied),
            };
            connection.execute(
                "UPDATE auth_administration_reviews \
                SET state=?2,decided_at=?3,decided_by=?4,revision=revision+1 \
                WHERE review_id=?1 AND state='pending'",
                rusqlite::params![id, state, now, identity.actor],
            )?;
            if state == "approved" {
                extend_grants(connection, &id, now)?;
            }
            Ok(
                trellis_runtime_apis::types::AuthAdministrationReviewResponse {
                    review: read_review(connection, &id)?,
                    extra: Default::default(),
                },
            )
        },
    )
}

fn extend_grants(connection: &Connection, review: &str, now: i64) -> Result<(), AuthError> {
    use super::authorization_sessions::GrantConsentBasis;
    use trellis_protocol::CapabilityAuthority;
    let (principal, capability_id): (String, String) = connection.query_row(
        "SELECT principal_id,capability_id FROM auth_administration_reviews WHERE review_id=?1",
        [review],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let capability = super::policy::capability(connection, &capability_id)?;
    let grants = connection
        .prepare(
            "SELECT g.oauth_grant_id,g.approved_capabilities_json,g.consent_basis_json \
            FROM auth_oauth_grants g \
            JOIN auth_administration_reviews r ON r.principal_id=g.principal_id \
            AND r.client_id=g.client_id AND r.binding_kind=g.binding_kind \
            AND r.binding_value=g.binding_value \
            WHERE r.review_id=?1 AND g.revoked_at IS NULL AND g.expires_at>?2 \
            AND (EXISTS(SELECT 1 FROM json_each(g.required_capabilities_json) \
            WHERE value=r.capability_id) \
            OR EXISTS(SELECT 1 FROM json_each(g.optional_capabilities_json) \
            WHERE value=r.capability_id))",
        )?
        .query_map(rusqlite::params![review, now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (grant, approved, basis) in grants {
        let mut approved: Vec<CapabilityAuthority> = super::sqlite::decode(&approved)?;
        let mut basis: std::collections::BTreeMap<String, GrantConsentBasis> =
            super::sqlite::decode(&basis)?;
        if approved
            .iter()
            .any(|held| held.capability_id == capability_id)
        {
            continue;
        }
        approved.push(capability.clone());
        approved.sort_by(|a, b| a.capability_id.cmp(&b.capability_id));
        basis.insert(
            capability_id.clone(),
            GrantConsentBasis {
                revision: 0,
                expires_at: None,
            },
        );
        connection.execute(
            "UPDATE auth_oauth_grants SET approved_capabilities_json=?2,\
            consent_basis_json=?3,revision=revision+1 WHERE oauth_grant_id=?1",
            rusqlite::params![
                grant,
                super::sqlite::json(&approved)?,
                super::sqlite::json(&basis)?
            ],
        )?;
    }
    super::revocation::enqueue_scope(
        connection,
        &super::revocation::EnforcementScope::Principal(principal),
        now,
    )?;
    Ok(())
}
