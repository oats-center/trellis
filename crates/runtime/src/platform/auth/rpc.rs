//! Shared adapters from verified RPC openings to authoritative Auth operations.

use std::sync::Arc;

use serde::Serialize;
use trellis_protocol::PlatformPrivilege;
use trellis_rs::{
    generated::TrellisError,
    service::{DeclaredRpcError, ServerError, ServiceHandlerContext},
};
use trellis_runtime_apis::apis::trellis_auth_v1::errors;

use super::{sqlite::digest, AuthError, Mutation, SqliteAuthorizationStore};

/// Register native Auth handlers on the normally connected platform provider.
pub(crate) fn register_rpc<P: trellis_rs::generated::ParticipantDescriptor>(
    provider: &mut trellis_runtime_apis::apis::trellis_auth_v1::Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
    oauth: Arc<super::OAuthEngine>,
    operational: async_nats::Client,
) {
    super::rpc_policy::register_policy_rpc(provider, store.clone());
    super::rpc_policy_aggregates::register_policy_aggregate_rpc(provider, store.clone());
    super::rpc_resources::register_resource_rpc(provider, store.clone(), operational.clone());
    super::rpc_resource_bindings::register(provider, store.clone());
    super::rpc_users::register_user_rpc(provider, store.clone());
    super::rpc_users::register_user_provider_rpc(provider, oauth.clone());
    super::rpc_credentials::register_credential_rpc(provider, store.clone());
    super::oauth::enrollment::register_rpc(provider, store.clone(), operational.clone());
    super::rpc_deployments::register_deployment_rpc(provider, store.clone(), operational, oauth);
    let users = store.clone();
    provider.register_users_list(move |context, input| {
        let store = users.clone();
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            let delegated = caller(&context, Some(PlatformPrivilege::PrincipalsManage)).is_ok();
            tokio::task::spawn_blocking(move || {
                store.transaction(|connection| {
                    let administrative =
                        super::rpc_users::directory_account_access(connection, &actor, delegated)?;
                    let limit = input
                        .page
                        .as_ref()
                        .and_then(|page| page.limit)
                        .unwrap_or(50);
                    if !(1..=200).contains(&limit) {
                        return Err(AuthError::Invalid("invalid page limit".into()));
                    }
                    let cursor = input
                        .page
                        .as_ref()
                        .and_then(|page| page.cursor.as_deref())
                        .unwrap_or("");
                    let mut statement = connection.prepare(
                        "SELECT p.principal_id \
                        FROM auth_principals p \
                        LEFT JOIN auth_user_profiles u USING(principal_id) \
                        LEFT JOIN auth_local_credentials c USING(principal_id) \
                        WHERE p.kind='user' \
                        AND p.principal_id>?1 \
                        AND (?4 OR p.state='active') \
                        AND (?2 IS NULL OR (?4 \
                        AND instr(lower(coalesce(c.normalized_username,(SELECT \
                        i.provider_subject \
                        FROM auth_provider_identities i \
                        WHERE i.principal_id=p.principal_id \
                        AND i.provider='local'),'')),lower(?2))>0) \
                        OR instr(lower(coalesce(u.display_name,p.principal_id)),lower(?2))>0 \
                        OR instr(lower(coalesce(u.given_name,'')),lower(?2))>0 \
                        OR instr(lower(coalesce(u.family_name,'')),lower(?2))>0 \
                        OR instr(lower(coalesce(u.email,'')),lower(?2))>0) \
                        ORDER BY p.principal_id \
                        LIMIT ?3",
                    )?;
                    let mut rows = statement
                        .query_map(
                            rusqlite::params![
                                cursor,
                                input.search.as_ref().map(|search| search.as_ref()),
                                limit + 1,
                                administrative
                            ],
                            |row| row.get::<_, String>(0),
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let next_cursor =
                        (rows.len() > limit as usize).then(|| rows[limit as usize - 1].clone());
                    rows.truncate(limit as usize);
                    let items = rows
                        .into_iter()
                        .map(|id| {
                            super::rpc_users::read_user(&store, connection, &id, administrative)
                        })
                        .collect::<Result<Vec<_>, AuthError>>()?;
                    Ok(trellis_runtime_apis::__types::CursorPage {
                        items,
                        page: trellis_runtime_apis::__types::CursorPageInfo { next_cursor },
                    })
                })
            })
            .await
            .map_err(|error| {
                tracing::error!(%error, "Auth users query failed");
                rpc_error(AuthError::Unavailable)
            })?
            .map_err(rpc_error)
        }
    });
    let sessions = store.clone();
    provider.register_sessions_me(move |context, _input| {
        let store = sessions.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            let caller = context
                .request()
                .verified_request()
                .ok_or_else(|| rpc_error(AuthError::Denied))?
                .caller()
                .clone();
            tokio::task::spawn_blocking(move || store.transaction(|connection| {
                let now = crate::clock::unix_now();
                let principal = super::rpc_policy::read_effective_access(
                    connection,
                    &caller.principal_id,
                    now,
                )?
                .principal;
                let login = if let Some(login) = &caller.login_session_id {
                    use rusqlite::OptionalExtension;
                    let row = connection.query_row("SELECT method,provider_id,created_at,expires_at,fresh_until,revoked_at FROM auth_login_sessions WHERE login_session_id=?1 AND principal_id=?2", rusqlite::params![login,caller.principal_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,Option<i64>>(4)?,row.get::<_,Option<i64>>(5)?))).optional()?.ok_or(AuthError::NotFound)?;
                    let ulid = |value: &str| value.parse().map_err(|_| AuthError::Unavailable);
                    Some(trellis_runtime_apis::types::AuthLoginSession {
                        login_session_id: ulid(login)?,
                        principal_id: ulid(&caller.principal_id)?,
                        method: row.0.into(),
                        provider_id: row.1.as_deref().map(ulid).transpose()?,
                        created_at: super::sqlite::timestamp(row.2)?,
                        expires_at: super::sqlite::timestamp(row.3)?,
                        claim_fresh_until: row.4.map(super::sqlite::timestamp).transpose()?,
                        state: if row.5.is_some() {
                            trellis_runtime_apis::types::AuthLoginState::Revoked
                        } else {
                            trellis_runtime_apis::types::AuthLoginState::Active
                        },
                        extra: Default::default(),
                    })
                } else {
                    None
                };
                Ok(trellis_runtime_apis::types::AuthSessionsMeResponse {
                    principal,
                    login,
                    authorization_session_id: caller
                        .authorization_session_id
                        .parse()
                        .map_err(|_| AuthError::Unavailable)?,
                    extra: Default::default(),
                })
            })).await.map_err(|error| {
                tracing::error!(%error, "Auth session query failed");
                rpc_error(AuthError::Unavailable)
                    })?.map_err(rpc_error)
        }
    });
    provider.register_authority_status(move |context, input| {
        let store = store.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            tokio::task::spawn_blocking(move || {
                store.authorization_session_status(input.authorization_session_id)
            })
            .await
            .map_err(|error| {
                tracing::error!(%error, "Auth status task failed");
                rpc_error(AuthError::Unavailable)
            })?
            .map_err(rpc_error)
        }
    });
}

/// Read the caller authorized by the registered route, including only privileges
/// delegated to this session. Store mutations additionally check current policy.
pub(super) fn caller(
    context: &ServiceHandlerContext,
    privilege: Option<PlatformPrivilege>,
) -> Result<String, AuthError> {
    let request = context
        .request()
        .verified_request()
        .filter(|request| request.authorized_action().is_some())
        .ok_or(AuthError::Denied)?;
    let caller = request.caller();
    if privilege.is_some_and(|privilege| !caller.platform_privileges.contains(&privilege)) {
        return Err(AuthError::Denied);
    }
    Ok(caller.principal_id.clone())
}

/// Use the explicit durable idempotency key and canonical domain input. The
/// opening already authenticates the complete raw wire payload; ephemeral proof
/// IDs and timestamps must not change the identity of a replayed mutation.
pub(super) fn mutation(
    context: &ServiceHandlerContext,
    idempotency_key: &str,
    privilege: Option<PlatformPrivilege>,
    input: &impl Serialize,
) -> Result<Mutation, AuthError> {
    Ok(Mutation {
        actor: caller(context, privilege)?,
        request_id: idempotency_key.to_owned(),
        request_digest: digest(input)?,
    })
}

/// Return fixed contract classifications, never internal failure text.
pub(super) fn rpc_error(error: AuthError) -> ServerError {
    let (error_type, code, message) = match &error {
        AuthError::NotFound => (errors::AuthError::TYPE, "not_found", "Not found"),
        AuthError::Conflict => (
            errors::AuthError::TYPE,
            "revision_conflict",
            "Revision conflict",
        ),
        AuthError::Denied => (errors::AuthError::TYPE, "access_denied", "Not authorized"),
        AuthError::RequiredMissing => (
            errors::AuthError::TYPE,
            "required_authority_unavailable",
            "Required authority unavailable",
        ),
        AuthError::ConsentChanged => (
            errors::AuthError::TYPE,
            "consent_changed",
            "Displayed consent changed",
        ),
        AuthError::ResourceLimit => (
            errors::AuthError::TYPE,
            "resource_limit",
            "Resource limit reached",
        ),
        AuthError::Retired => (
            errors::AuthError::TYPE,
            "session_retired",
            "Authorization session retired",
        ),
        AuthError::Invalid(_) => (
            errors::ValidationError::TYPE,
            "invalid_request",
            "Invalid request",
        ),
        AuthError::Unavailable | AuthError::Broker(_) => (
            errors::UnexpectedError::TYPE,
            "temporarily_unavailable",
            "Temporarily unavailable",
        ),
        AuthError::Storage(_) | AuthError::Json(_) | AuthError::Protocol(_) => {
            tracing::error!(error = %error, "Auth RPC failed");
            (
                errors::UnexpectedError::TYPE,
                "server_error",
                "Request failed",
            )
        }
    };
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        error_type,
        message,
        [("code", serde_json::Value::String(code.to_owned()))],
    ))
}
