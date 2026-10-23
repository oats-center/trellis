//! Native policy RPC projections over the authoritative authorization store.

use super::sqlite::timestamp;
use super::Administration;
use crate::platform::auth::blocking;
use std::sync::Arc;

use crate::platform::auth::sqlite::Connection;
use rusqlite::{params, OptionalExtension};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo, CursorQuery},
    apis::trellis_auth_v1::Provider,
};

use super::{
    rpc::{caller, mutation, rpc_error},
    AuthError, PolicyMutation, SqliteAuthorizationStore,
};

/// Register native role, assignment, grant, and effective-access policy RPCs.
pub(crate) fn register_policy_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    let roles = Arc::clone(&store);
    provider.register_roles_list(move |context, input| {
        let store = Arc::clone(&roles);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    read_roles(connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let roles = Arc::clone(&store);
    provider.register_roles_get(move |context, input| {
        let store = Arc::clone(&roles);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    Ok(AuthRoleGetResponse {
                        role: read_role(connection, &input.role_id.to_string())?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    macro_rules! register_mutation {
        ($registration:ident, $aggregate:ident) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(Administration::ManageRoles),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    blocking(move || {
                        $aggregate(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(rpc_error)
                }
            });
        }};
    }
    register_mutation!(register_roles_put, put_role);
    register_mutation!(register_roles_delete, delete_role);
    register_mutation!(register_role_assignments_assign, assign_role);
    register_mutation!(register_role_assignments_revoke, revoke_assignment);
    register_mutation!(register_capability_grants_grant, grant_capability);
    register_mutation!(register_capability_grants_revoke, revoke_grant);
    let assignments = Arc::clone(&store);
    provider.register_role_assignments_list(move |context, input| {
        let store = Arc::clone(&assignments);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    read_assignments(connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let grants = Arc::clone(&store);
    provider.register_capability_grants_list(move |context, input| {
        let store = Arc::clone(&grants);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    read_grants(connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let capabilities = Arc::clone(&store);
    provider.register_capabilities_list(move |context, input| {
        let store = Arc::clone(&capabilities);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    read_capabilities(connection, &input)
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    provider.register_principals_effective_access(move |context, input| {
        let store = Arc::clone(&store);
        async move {
            let actor = caller(&context, Some(Administration::ManageRoles)).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_administration(
                        connection,
                        &actor,
                        Administration::ManageRoles,
                    )?;
                    read_effective_access(
                        connection,
                        &input.principal_id.to_string(),
                        crate::clock::unix_now(),
                    )
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
}

pub(super) fn put_role(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &AuthRolePutRequest,
    now: i64,
) -> Result<AuthRolePutResponse, AuthError> {
    store.mutate(identity, "rpc.roles.put", input, now, |connection| {
        let role_id = input
            .role_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(super::sqlite::id);
        let command = PolicyMutation::RolePut {
            role_id: role_id.clone(),
            expected_revision: input.expected_revision.0,
            title: input.title.to_string(),
            description: input.description.to_string(),
            capabilities: input
                .capability_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
        };
        super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
        Ok(AuthRolePutResponse {
            role: read_role(connection, &role_id)?,
            enforcement: None,
            extra: Default::default(),
        })
    })
}

pub(super) fn delete_role(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &AuthRoleDeleteRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(identity, "rpc.roles.delete", input, now, |connection| {
        let command = PolicyMutation::RoleDelete {
            role_id: input.role_id.to_string(),
            expected_revision: input.expected_revision.0 .0,
        };
        let revision =
            super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
        Ok(AuthMutationResult {
            revision: AuthRevision(revision.into()),
            enforcement: None,
            extra: Default::default(),
        })
    })
}

pub(super) fn assign_role(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &AuthRoleAssignRequest,
    now: i64,
) -> Result<AuthRoleAssignResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.roleAssignments.assign",
        input,
        now,
        |connection| {
            let principal_id = input.principal_id.to_string();
            let role_id = input.role_id.to_string();
            let command = PolicyMutation::RoleAssign {
                principal_id: principal_id.clone(),
                role_id: role_id.clone(),
                expected_revision: input.expected_revision.0,
                expires_at: input
                    .expires_at
                    .as_ref()
                    .map(|value| {
                        time::OffsetDateTime::parse(
                            value.as_str(),
                            &time::format_description::well_known::Rfc3339,
                        )
                        .map(|value| value.unix_timestamp())
                        .map_err(|_| AuthError::Invalid("invalid expiry".into()))
                    })
                    .transpose()?,
            };
            super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
            let (generation, _) = super::policy::role(connection, &role_id)?;
            let assignment_id: String = connection.query_row(
                "SELECT assignment_id \
                FROM auth_role_assignments \
                WHERE principal_id=?1 \
                AND role_id=?2 AND role_generation=?3",
                params![principal_id, role_id, generation],
                |row| row.get(0),
            )?;
            Ok(AuthRoleAssignResponse {
                assignment: read_assignment(connection, &assignment_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn revoke_assignment(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &AuthRoleRevokeRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.roleAssignments.revoke",
        input,
        now,
        |connection| {
            // Resolve only after idempotency replay, inside the mutation transaction.
            let assignment = read_assignment(connection, &input.assignment_id.to_string())?;
            let (generation, _) = super::policy::role(connection, &assignment.role_id.to_string())?;
            if generation != assignment.role_identity_generation.0 .0 {
                return Err(AuthError::Conflict);
            }
            let command = PolicyMutation::RoleRevoke {
                principal_id: assignment.principal_id.to_string(),
                role_id: assignment.role_id.to_string(),
                expected_revision: input.expected_revision.0 .0,
            };
            let revision =
                super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
            Ok(AuthMutationResult {
                revision: AuthRevision(revision.into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn grant_capability(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &AuthCapabilityGrantRequest,
    now: i64,
) -> Result<AuthCapabilityGrantResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.capabilityGrants.grant",
        input,
        now,
        |connection| {
            let principal_id = input.principal_id.to_string();
            let capability_id = input.capability_id.to_string();
            let command = PolicyMutation::CapabilityGrant {
                principal_id: principal_id.clone(),
                capability_id: capability_id.clone(),
                expected_revision: input.expected_revision.0,
                expires_at: input
                    .expires_at
                    .as_ref()
                    .map(|value| {
                        time::OffsetDateTime::parse(
                            value.as_str(),
                            &time::format_description::well_known::Rfc3339,
                        )
                        .map(|value| value.unix_timestamp())
                        .map_err(|_| AuthError::Invalid("invalid expiry".into()))
                    })
                    .transpose()?,
            };
            super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
            let capability = super::policy::capability(connection, &capability_id)?;
            let grant_id: String = connection.query_row(
                "SELECT grant_id FROM auth_direct_capabilities \
                WHERE principal_id=?1 \
                AND capability_id=?2 \
                AND capability_generation=?3",
                params![
                    principal_id,
                    capability_id,
                    capability.identity_generation.get()
                ],
                |row| row.get(0),
            )?;
            Ok(AuthCapabilityGrantResponse {
                grant: read_grant(connection, &grant_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn revoke_grant(
    store: &SqliteAuthorizationStore,
    identity: &super::Mutation,
    input: &AuthCapabilityRevokeRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.capabilityGrants.revoke",
        input,
        now,
        |connection| {
            let grant = read_grant(connection, &input.grant_id.to_string())?;
            let capability =
                super::policy::capability(connection, &grant.capability.capability_id)?;
            if capability.identity_generation.get() != grant.capability.identity_generation.0 .0 {
                return Err(AuthError::Conflict);
            }
            let command = PolicyMutation::CapabilityRevoke {
                principal_id: grant.principal_id.to_string(),
                capability_id: grant.capability.capability_id.to_string(),
                expected_revision: input.expected_revision.0 .0,
            };
            let revision =
                super::policy::apply_policy_in_transaction(connection, identity, &command, now)?;
            Ok(AuthMutationResult {
                revision: AuthRevision(revision.into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

fn read_assignment(
    connection: &Connection,
    assignment_id: &str,
) -> Result<AuthRoleAssignment, AuthError> {
    let (principal, role, generation, revision, expiry) = connection
        .query_row(
            "SELECT principal_id,role_id,role_generation,revision,expires_at \
            FROM auth_role_assignments \
            WHERE assignment_id=?1",
            [assignment_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    Ok(AuthRoleAssignment {
        assignment_id: assignment_id.parse().map_err(|_| AuthError::Unavailable)?,
        principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
        role_id: role.parse().map_err(|_| AuthError::Unavailable)?,
        role_identity_generation: AuthRevision(generation.into()),
        revision: AuthRevision(revision.into()),
        expires_at: expiry.map(timestamp).transpose()?,
        extra: Default::default(),
    })
}

fn read_grant(connection: &Connection, grant_id: &str) -> Result<AuthCapabilityGrant, AuthError> {
    let (principal, capability, generation, revision, expiry) = connection
        .query_row(
            "SELECT principal_id,capability_id,capability_generation,revision,\
            expires_at \
            FROM auth_direct_capabilities \
            WHERE grant_id=?1",
            [grant_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, u64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    Ok(AuthCapabilityGrant {
        grant_id: grant_id.parse().map_err(|_| AuthError::Unavailable)?,
        principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
        capability: AuthCapabilityIdentity {
            capability_id: capability.into(),
            identity_generation: AuthRevision(generation.into()),
            extra: Default::default(),
        },
        revision: AuthRevision(revision.into()),
        expires_at: expiry.map(timestamp).transpose()?,
        extra: Default::default(),
    })
}

pub(super) fn read_assignments(
    connection: &Connection,
    input: &AuthRoleAssignmentsListRequest,
) -> Result<AuthRoleAssignmentsListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT assignment_id \
        FROM auth_role_assignments \
        WHERE assignment_id>?1 \
        AND (?2 IS NULL OR principal_id=?2) \
        AND (?3 IS NULL OR role_id=?3) \
        ORDER BY assignment_id \
        LIMIT ?4",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.principal_id.as_ref().map(ToString::to_string),
                input.role_id.as_ref().map(ToString::to_string),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    Ok(CursorPage {
        items: ids
            .iter()
            .map(|id| read_assignment(connection, id))
            .collect::<Result<Vec<_>, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

pub(super) fn read_grants(
    connection: &Connection,
    input: &AuthCapabilityGrantsListRequest,
) -> Result<AuthCapabilityGrantsListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT grant_id FROM auth_direct_capabilities \
        WHERE grant_id>?1 AND (?2 IS NULL \
        OR principal_id=?2) \
        AND (?3 IS NULL OR capability_id=?3) \
        ORDER BY grant_id LIMIT ?4",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.principal_id.as_ref().map(ToString::to_string),
                input.capability_id.as_ref().map(|value| value.as_ref()),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    Ok(CursorPage {
        items: ids
            .iter()
            .map(|id| read_grant(connection, id))
            .collect::<Result<Vec<_>, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

fn page_bounds(page: Option<&CursorQuery>) -> Result<(&str, usize), AuthError> {
    let limit = page.and_then(|page| page.limit).unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    Ok((
        page.and_then(|page| page.cursor.as_deref()).unwrap_or(""),
        limit as usize,
    ))
}

pub(super) fn read_roles(
    connection: &Connection,
    input: &AuthPageRequest,
) -> Result<AuthRolesListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT role_id FROM auth_roles \
        WHERE state='active' \
        AND role_id>?1 AND (?2 IS NULL \
        OR instr(lower(title),lower(?2))>0 \
        OR instr(lower(description),lower(?2))>0) \
        ORDER BY role_id LIMIT ?3",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.search.as_ref().map(|value| value.as_ref()),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    let items = ids
        .iter()
        .map(|id| read_role(connection, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CursorPage {
        items,
        page: CursorPageInfo { next_cursor },
    })
}

pub(super) fn read_role(connection: &Connection, role_id: &str) -> Result<AuthRole, AuthError> {
    let (generation, revision, title, description) = connection
        .query_row(
            "SELECT identity_generation,revision,title,description \
            FROM auth_roles WHERE role_id=?1 \
            AND state='active'",
            [role_id],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let mut statement = connection.prepare(
        "SELECT capability_id,capability_generation \
        FROM auth_role_capabilities \
        WHERE role_id=?1 AND role_generation=?2 \
        ORDER BY capability_id",
    )?;
    let capabilities = statement
        .query_map(params![role_id, generation], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?
        .map(|row| {
            let (id, generation) = row?;
            Ok(AuthCapabilityIdentity {
                capability_id: id.into(),
                identity_generation: AuthRevision(generation.into()),
                extra: Default::default(),
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    Ok(AuthRole {
        role_id: role_id.parse().map_err(|_| AuthError::Unavailable)?,
        identity_generation: AuthRevision(generation.into()),
        revision: AuthRevision(revision.into()),
        title,
        description,
        capabilities,
        extra: Default::default(),
    })
}

pub(super) fn read_capabilities(
    connection: &Connection,
    input: &AuthCapabilitiesListRequest,
) -> Result<AuthCapabilitiesListResponse, AuthError> {
    let (cursor, limit) = page_bounds(input.page.as_ref())?;
    let mut statement = connection.prepare(
        "SELECT capability_id \
        FROM auth_capability_identities \
        WHERE state='active' \
        AND capability_id>?1 \
        AND (?2 IS NULL OR api_id=?2) \
        AND (?3 IS NULL OR instr(lower(title),lower(?3))>0 \
        OR instr(lower(description),lower(?3))>0 \
        OR instr(lower(capability_id),lower(?3))>0) \
        ORDER BY capability_id \
        LIMIT ?4",
    )?;
    let mut ids = statement
        .query_map(
            params![
                cursor,
                input.api_id.as_ref().map(|value| value.as_ref()),
                input.search.as_ref().map(|value| value.as_ref()),
                limit + 1
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    let items = ids
        .iter()
        .map(|id| read_capability(connection, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CursorPage {
        items,
        page: CursorPageInfo { next_cursor },
    })
}

fn read_capability(
    connection: &Connection,
    capability_id: &str,
) -> Result<AuthCapabilityDefinition, AuthError> {
    let (generation, consent_revision, api_id, title, description, consequence, group) = connection
        .query_row(
            "SELECT c.identity_generation,c.consent_revision,c.api_id,c.title,\
            c.description,c.consequence,c.group_name,g.title,g.description \
            FROM auth_capability_identities c \
            JOIN auth_capability_groups g \
            ON g.api_id=c.api_id AND g.group_name=c.group_name \
            WHERE c.capability_id=?1 \
            AND c.state='active'",
            [capability_id],
            |row| {
                let api_id = row.get::<_, String>(2)?;
                let name = row.get::<_, String>(6)?;
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    api_id.clone(),
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    AuthCapabilityGroup {
                        id: if name.is_empty() {
                            api_id
                        } else {
                            format!("{api_id}::{name}")
                        },
                        title: row.get(7)?,
                        description: row.get(8)?,
                        extra: Default::default(),
                    },
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let mut statement = connection.prepare(
        "SELECT a.descriptor_json \
        FROM auth_capability_actions c \
        JOIN auth_api_actions a ON a.api_id=c.api_id \
        AND a.generation=c.api_generation \
        AND a.action_key=c.action_key \
        WHERE c.capability_id=?1 \
        AND c.identity_generation=?2 \
        ORDER BY a.action_key",
    )?;
    let allows = statement
        .query_map(params![capability_id, generation], |row| {
            row.get::<_, String>(0)
        })?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect::<Result<Vec<AuthActionSummary>, AuthError>>()?;
    Ok(AuthCapabilityDefinition {
        identity: AuthCapabilityIdentity {
            capability_id: capability_id.to_owned().into(),
            identity_generation: AuthRevision(generation.into()),
            extra: Default::default(),
        },
        consent_revision: AuthRevision(consent_revision.into()),
        api_id: api_id.into(),
        title,
        description,
        consequence,
        group,
        allows,
        extra: Default::default(),
    })
}

pub(super) fn read_effective_access(
    connection: &Connection,
    principal_id: &str,
    now: i64,
) -> Result<AuthEffectiveAccessResponse, AuthError> {
    let (kind, state, revision, created_at, title) = connection
        .query_row(
            "SELECT p.kind,p.state,p.version,p.created_at,coalesce(u.display_name,\
            (SELECT c.normalized_username FROM auth_local_credentials c \
            WHERE c.principal_id=p.principal_id),p.principal_id) \
            FROM auth_principals p \
            LEFT JOIN auth_user_profiles u ON u.principal_id=p.principal_id \
            WHERE p.principal_id=?1",
            [principal_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let principal = AuthPrincipal {
        principal_id: principal_id.parse().map_err(|_| AuthError::Unavailable)?,
        kind: kind.into(),
        enabled: state == "active",
        revision: AuthRevision(revision.into()),
        created_at: timestamp(created_at)?,
        title,
        extra: Default::default(),
    };
    let entitlements = super::policy::entitlements(connection, principal_id, None, now)?;
    let capabilities = entitlements
        .into_iter()
        .map(
            |(
                id,
                super::policy::Entitlement {
                    deadline,
                    sources: origins,
                },
            )| {
                let definition = read_capability(connection, &id)?;
                let sources = origins
                    .into_iter()
                    .map(|origin| {
                        let role_id = origin
                            .strip_prefix("role:")
                            .map(|value| value.parse().map_err(|_| AuthError::Unavailable))
                            .transpose()?;
                        Ok(AuthCapabilitySource {
                            kind: if role_id.is_some() {
                                AuthCapabilitySourceKind::Role
                            } else if origin == "administrator" {
                                AuthCapabilitySourceKind::Administrator
                            } else {
                                AuthCapabilitySourceKind::Direct
                            },
                            role_id,
                            grant_id: if origin == "direct" {
                                let grant_id: String = connection.query_row(
                                    "SELECT grant_id FROM auth_direct_capabilities \
                                    WHERE principal_id=?1 \
                                    AND capability_id=?2 \
                                    AND capability_generation=?3",
                                    params![
                                        principal_id,
                                        id,
                                        definition.identity.identity_generation.0 .0
                                    ],
                                    |row| row.get(0),
                                )?;
                                Some(grant_id.parse().map_err(|_| AuthError::Unavailable)?)
                            } else {
                                None
                            },
                            mapping_id: None,
                            provider_id: None,
                            extra: Default::default(),
                        })
                    })
                    .collect::<Result<Vec<_>, AuthError>>()?;
                let expires_at = deadline.map(timestamp).transpose()?;
                Ok(AuthEffectiveCapability {
                    definition,
                    sources,
                    expires_at,
                    extra: Default::default(),
                })
            },
        )
        .collect::<Result<Vec<_>, AuthError>>()?;
    Ok(AuthEffectiveAccessResponse {
        principal,
        capabilities,
        extra: Default::default(),
    })
}
