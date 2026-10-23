//! Native aggregate policy mutations and projections.

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use trellis_protocol::PlatformPrivilege;
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo},
    apis::trellis_auth_v1::Provider,
};

use super::{
    policy::apply_policy_in_transaction,
    rpc::{caller, mutation, rpc_error},
    rpc_users::read_mapping,
    sqlite::{decode, id},
    AuthError, Mutation, PolicyMutation, SqliteAuthorizationStore,
};

/// Register finite platform privilege assignments and multi-role OIDC mapping writes.
pub(crate) fn register_policy_aggregate_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    macro_rules! register_mutation {
        ($registration:ident, $aggregate:ident, $privilege:ident) => {{
            let store = Arc::clone(&store);
            provider.$registration(move |context, input| {
                let store = Arc::clone(&store);
                async move {
                    let identity = mutation(
                        &context,
                        &input.idempotency_key,
                        Some(PlatformPrivilege::$privilege),
                        &input,
                    )
                    .map_err(rpc_error)?;
                    tokio::task::spawn_blocking(move || {
                        $aggregate(&store, &identity, &input, crate::clock::unix_now())
                    })
                    .await
                    .map_err(|_| rpc_error(AuthError::Unavailable))?
                    .map_err(rpc_error)
                }
            });
        }};
    }
    register_mutation!(
        register_platform_privileges_assign,
        assign_privileges,
        PrivilegesManage
    );
    register_mutation!(
        register_platform_privileges_revoke,
        revoke_privileges,
        PrivilegesManage
    );
    register_mutation!(register_oidc_role_mappings_put, put_mapping, RolesManage);
    provider.register_platform_privileges_list(move |context, input| {
        let store = Arc::clone(&store);
        async move {
            let actor =
                caller(&context, Some(PlatformPrivilege::PrivilegesManage)).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                store.transaction(|connection| {
                    SqliteAuthorizationStore::require_privilege(
                        connection,
                        &actor,
                        "privileges.manage",
                    )?;
                    read_assignments(connection, &input)
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
}

pub(super) fn assign_privileges(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthPlatformAssignRequest,
    now: i64,
) -> Result<AuthPlatformAssignResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.platformPrivileges.assign",
        input,
        now,
        |connection| {
            let privileges: Vec<PlatformPrivilege> =
                serde_json::from_value(serde_json::to_value(&input.privileges)?)
                    .map_err(|_| AuthError::Invalid("unknown platform privilege".into()))?;
            apply_policy_in_transaction(
                connection,
                identity,
                &PolicyMutation::PrivilegeAssign {
                    principal_id: input.principal_id.to_string(),
                    privileges,
                    expected_revision: input.expected_revision.0,
                },
                now,
            )?;
            let assignment_id: String = connection.query_row(
                "SELECT assignment_id FROM auth_platform_assignments WHERE principal_id=?1",
                [input.principal_id.to_string()],
                |row| row.get(0),
            )?;
            Ok(AuthPlatformAssignResponse {
                assignment: read_assignment(connection, &assignment_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn revoke_privileges(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthPlatformRevokeRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.platformPrivileges.revoke",
        input,
        now,
        |connection| {
            let revision = apply_policy_in_transaction(
                connection,
                identity,
                &PolicyMutation::PrivilegeRevoke {
                    assignment_id: input.assignment_id.to_string(),
                    expected_revision: input.expected_revision.0 .0,
                },
                now,
            )?;
            Ok(AuthMutationResult {
                revision: AuthRevision(revision.into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn put_mapping(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOIDCRoleMappingPutRequest,
    now: i64,
) -> Result<AuthOIDCRoleMappingPutResponse, AuthError> {
    store.mutate(
        identity,
        "rpc.oidcRoleMappings.put",
        input,
        now,
        |connection| {
            let mapping_id = input
                .mapping_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(id);
            let config: String = connection
                .query_row(
                    "SELECT config_json FROM auth_oidc_providers \
                    WHERE provider_id=?1 \
                    AND state='active'",
                    [input.provider_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            let config: serde_json::Value = decode(&config)?;
            let claim_name = config
                .get("roleClaim")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| AuthError::Invalid("provider has no role claim".into()))?;
            apply_policy_in_transaction(
                connection,
                identity,
                &PolicyMutation::OidcMappingPut {
                    mapping_id: mapping_id.clone(),
                    provider_id: input.provider_id.to_string(),
                    claim_name: claim_name.into(),
                    claim_value: input.external_role.to_string(),
                    role_ids: input.role_ids.iter().map(ToString::to_string).collect(),
                    expected_revision: input.expected_revision.0,
                },
                now,
            )?;
            Ok(AuthOIDCRoleMappingPutResponse {
                mapping: read_mapping(connection, &mapping_id)?,
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn read_assignments(
    connection: &Connection,
    input: &AuthPageRequest,
) -> Result<AuthPlatformAssignmentsListResponse, AuthError> {
    let limit = input
        .page
        .as_ref()
        .and_then(|page| page.limit)
        .unwrap_or(50);
    if limit == 0 || limit > 200 {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    let cursor = input
        .page
        .as_ref()
        .and_then(|page| page.cursor.as_deref())
        .unwrap_or("");
    let search = input
        .search
        .as_ref()
        .map(|value| value.as_ref())
        .unwrap_or("");
    let mut statement = connection.prepare(
        "SELECT assignment_id \
        FROM auth_platform_assignments \
        WHERE assignment_id>?1 \
        AND instr(principal_id,?2)>0 \
        ORDER BY assignment_id \
        LIMIT ?3",
    )?;
    let mut ids = statement
        .query_map(params![cursor, search, limit + 1], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_cursor = (ids.len() > limit as usize).then(|| ids[limit as usize - 1].clone());
    ids.truncate(limit as usize);
    Ok(CursorPage {
        items: ids
            .iter()
            .map(|id| read_assignment(connection, id))
            .collect::<Result<Vec<_>, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

fn read_assignment(
    connection: &Connection,
    assignment_id: &str,
) -> Result<AuthPlatformAssignment, AuthError> {
    let (principal, revision): (String, u64) = connection
        .query_row(
            "SELECT principal_id,revision FROM auth_platform_assignments WHERE assignment_id=?1",
            [assignment_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let mut statement = connection.prepare(
        "SELECT privilege FROM auth_platform_privileges WHERE principal_id=?1 ORDER BY privilege",
    )?;
    let privileges = statement
        .query_map([&principal], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|value| {
            serde_json::from_value(serde_json::Value::String(value)).map_err(AuthError::from)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AuthPlatformAssignment {
        assignment_id: assignment_id.parse().map_err(|_| AuthError::Unavailable)?,
        principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
        revision: AuthRevision(revision.into()),
        privileges,
        extra: Default::default(),
    })
}
