//! Credential and runtime-authority adapters over the existing Auth owners.

use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use trellis_protocol::{AuthenticatedCaller, PlatformPrivilege};
use trellis_rs::{
    generated::{ParticipantDescriptor, TrellisError},
    service::{DeclaredRpcError, ServerError},
};
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo, CursorQuery},
    apis::trellis_auth_v1::{errors, Provider},
};

use super::{
    authorization_sessions::{session_credential, AdmissionIdentity},
    lifecycle::ClientRegistration,
    revocation::{enqueue_effect, enqueue_scope, EnforcementScope},
    rpc::{caller, mutation, rpc_error},
    sqlite::{decode, digest, json as canonical_json},
    AuthError, Mutation, SqliteAuthorizationStore,
};

/// Register the credential routes actually declared by the native Auth contract.
pub(crate) fn register_credential_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    macro_rules! query {
        ($route:ident, $privilege:expr, $operation:expr) => {{
            let store = store.clone();
            provider.$route(move |context, input| {
                let store = store.clone();
                async move {
                    let actor = caller(&context, $privilege).map_err(rpc_error)?;
                    let verified = context
                        .request()
                        .verified_request()
                        .ok_or_else(|| rpc_error(AuthError::Denied))?
                        .caller()
                        .clone();
                    tokio::task::spawn_blocking(move || {
                        ($operation)(&store, &actor, &verified, &input)
                    })
                    .await
                    .map_err(|_| rpc_error(AuthError::Unavailable))?
                    .map_err(rpc_error)
                }
            });
        }};
    }
    macro_rules! mutate {
        ($route:ident, $privilege:expr, $operation:expr) => {{
            let store = store.clone();
            provider.$route(move |context, input| {
                let store = store.clone();
                async move {
                    let identity = mutation(&context, &input.idempotency_key, $privilege, &input)
                        .map_err(rpc_error)?;
                    let verified = context
                        .request()
                        .verified_request()
                        .ok_or_else(|| rpc_error(AuthError::Denied))?
                        .caller()
                        .clone();
                    tokio::task::spawn_blocking(move || {
                        ($operation)(
                            &store,
                            &identity,
                            &verified,
                            &input,
                            time::OffsetDateTime::now_utc().unix_timestamp(),
                        )
                    })
                    .await
                    .map_err(|_| rpc_error(AuthError::Unavailable))?
                    .map_err(rpc_error)
                }
            });
        }};
    }
    query!(
        register_o_auth_clients_list,
        Some(PlatformPrivilege::ClientsManage),
        |store: &SqliteAuthorizationStore,
         actor: &str,
         _: &AuthenticatedCaller,
         input: &AuthPageRequest| {
            store.transaction(|connection| {
        SqliteAuthorizationStore::require_privilege(connection, actor, "clients.manage")?;
        let (cursor, limit) = page_bounds(input.page.as_ref())?;
        let mut statement = connection.prepare("SELECT client_id FROM auth_oauth_clients WHERE state!='deleted' AND client_id>?1 AND (?2 IS NULL OR instr(lower(client_id),lower(?2))>0 OR instr(lower(display_name),lower(?2))>0) ORDER BY client_id LIMIT ?3")?;
        let ids = statement.query_map(params![cursor, input.search.as_ref().map(|value|value.as_ref()), limit+1], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        page(ids, limit, |id| read_client(connection, id))
    })
        }
    );
    query!(
        register_o_auth_clients_get,
        Some(PlatformPrivilege::ClientsManage),
        |store: &SqliteAuthorizationStore,
         actor: &str,
         _: &AuthenticatedCaller,
         input: &AuthOAuthClientGetRequest| store.transaction(|connection| {
            SqliteAuthorizationStore::require_privilege(connection, actor, "clients.manage")?;
            Ok(AuthOAuthClientGetResponse {
                client: read_client(connection, &input.client_id)?,
                extra: Default::default(),
            })
        })
    );
    mutate!(
        register_o_auth_clients_put,
        Some(PlatformPrivilege::ClientsManage),
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         _: &AuthenticatedCaller,
         input: &AuthOAuthClientPutRequest,
         now| put_client(store, identity, input, now)
    );
    mutate!(
        register_o_auth_clients_disable,
        Some(PlatformPrivilege::ClientsManage),
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         _: &AuthenticatedCaller,
         input: &AuthOAuthClientMutationRequest,
         now| change_client_state(store, identity, input, "disabled", now)
    );
    mutate!(
        register_o_auth_clients_delete,
        Some(PlatformPrivilege::ClientsManage),
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         _: &AuthenticatedCaller,
         input: &AuthOAuthClientMutationRequest,
         now| change_client_state(store, identity, input, "deleted", now)
    );
    query!(
        register_sessions_list,
        None,
        |store: &SqliteAuthorizationStore,
         actor: &str,
         _: &AuthenticatedCaller,
         input: &AuthPageRequest| {
            store.transaction(|connection| {
        let (cursor, limit) = page_bounds(input.page.as_ref())?;
        let mut statement = connection.prepare("SELECT login_session_id FROM auth_login_sessions WHERE principal_id=?1 AND login_session_id>?2 ORDER BY login_session_id LIMIT ?3")?;
        let ids = statement.query_map(params![actor,cursor,limit+1], |row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        page(ids, limit, |id| read_login(connection, id))
    })
        }
    );
    mutate!(
        register_sessions_revoke,
        None,
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         verified: &AuthenticatedCaller,
         input: &AuthLoginRevokeRequest,
         now| revoke_login(store, identity, verified, input, now)
    );
    query!(
        register_delegations_list,
        None,
        |store: &SqliteAuthorizationStore,
         actor: &str,
         verified: &AuthenticatedCaller,
         input: &AuthDelegationsListRequest| {
            store.transaction(|connection| {
        let owner=input.principal_id.as_ref().map(ToString::to_string).unwrap_or_else(||actor.into());
        if owner!=actor {
            if !verified.platform_privileges.contains(&PlatformPrivilege::PrincipalsManage) {return Err(AuthError::Denied);}
            SqliteAuthorizationStore::require_privilege(connection,actor,"principals.manage")?;
        }
        let (cursor,limit)=page_bounds(input.page.as_ref())?;
        let mut statement=connection.prepare("SELECT oauth_grant_id FROM auth_oauth_grants WHERE principal_id=?1 AND oauth_grant_id>?2 AND (?3 IS NULL OR client_id=?3) AND revoked_at IS NULL ORDER BY oauth_grant_id LIMIT ?4")?;
        let ids=statement.query_map(params![owner,cursor,input.client_id.as_ref().map(|id|id.as_ref()),limit+1],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        page(ids,limit,|id|read_delegation(connection,id))
    })
        }
    );
    query!(
        register_delegations_review,
        None,
        |store: &SqliteAuthorizationStore,
         actor: &str,
         verified: &AuthenticatedCaller,
         input: &AuthDelegationReviewRequest| store.transaction(|connection| {
            let delegation = read_delegation(connection, &input.delegation_id.to_string())?;
            if delegation.principal_id.to_string() != actor {
                if !verified
                    .platform_privileges
                    .contains(&PlatformPrivilege::PrincipalsManage)
                {
                    return Err(AuthError::Denied);
                }
                SqliteAuthorizationStore::require_privilege(
                    connection,
                    actor,
                    "principals.manage",
                )?;
            }
            let mut current_capabilities = Vec::new();
            for choice in &delegation.choices {
                let requested = AuthCapabilitiesListRequest {
                    api_id: None,
                    page: Some(CursorQuery {
                        cursor: None,
                        limit: Some(200),
                    }),
                    search: Some(choice.capability.capability_id.to_string().into()),
                    extra: Default::default(),
                };
                let current = super::rpc_policy::read_capabilities(connection, &requested)?;
                current_capabilities.extend(current.items.into_iter().filter(|current| {
                    current.identity.capability_id == choice.capability.capability_id
                }));
            }
            Ok(AuthDelegationReviewResponse {
                current_capabilities,
                delegation,
                extra: Default::default(),
            })
        })
    );
    mutate!(
        register_delegations_revoke,
        None,
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         verified: &AuthenticatedCaller,
         input: &AuthDelegationRevokeRequest,
         now| revoke_delegation(store, identity, verified, input, now)
    );
    let logout_store = store.clone();
    provider.register_sessions_logout(move |context, _input| {
        let store = logout_store.clone();
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            let verified = context
                .request()
                .verified_request()
                .ok_or_else(|| rpc_error(AuthError::Denied))?
                .caller()
                .clone();
            tokio::task::spawn_blocking(move || {
                let login = verified
                    .login_session_id
                    .as_ref()
                    .ok_or(AuthError::Denied)?;
                // Logout has no caller-supplied replay key; the login itself is
                // the immutable lifecycle identity of this one-way operation.
                let identity = Mutation {
                    actor,
                    request_id: login.clone(),
                    request_digest: digest(login)?,
                };
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                store.mutate(&identity, "rpc.login.logout", login, now, |connection| {
                    let scope = EnforcementScope::Login(login.clone());
                    let root = Mutation {
                        request_digest: digest(&scope)?,
                        ..identity.clone()
                    };
                    let work = store.revoke_root_in_transaction(connection, &root, &scope, now)?;
                    Ok(AuthSessionsLogoutResponse {
                        login_session_id: login.parse().map_err(|_| AuthError::Unavailable)?,
                        enforcement: read_enforcement(connection, &work)?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)
        }
    });
    query!(
        register_connections_list,
        Some(PlatformPrivilege::PrincipalsManage),
        |store: &SqliteAuthorizationStore,
         actor: &str,
         _: &AuthenticatedCaller,
         input: &AuthConnectionsListRequest| {
            store.transaction(|connection| {
        SqliteAuthorizationStore::require_privilege(connection, actor, "principals.manage")?;
        let (cursor,limit)=page_bounds(input.page.as_ref())?;
        let state = input.state.as_ref().map(|state|serde_json::to_value(state)).transpose()?;
        let mut statement=connection.prepare("SELECT a.attachment_id FROM auth_attachments a JOIN auth_authorization_sessions s USING(authorization_session_id) WHERE a.attachment_id>?1 AND (?2 IS NULL OR s.principal_id=?2) AND (?3 IS NULL OR s.authorization_session_id=?3) AND (?4 IS NULL OR a.state=?4) ORDER BY a.attachment_id LIMIT ?5")?;
        let ids=statement.query_map(params![cursor,input.principal_id.as_ref().map(ToString::to_string),input.authorization_session_id.as_ref().map(ToString::to_string),state.as_ref().and_then(|state|state.as_str()),limit+1],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        page(ids,limit,|id|read_attachment(connection,id))
    })
        }
    );
    query!(
        register_authorization_sessions_list,
        Some(PlatformPrivilege::PrincipalsManage),
        |store: &SqliteAuthorizationStore,
         actor: &str,
         _: &AuthenticatedCaller,
         input: &AuthAuthorizationSessionsListRequest| {
            store.transaction(|connection| {
                SqliteAuthorizationStore::require_privilege(connection, actor, "principals.manage")?;
                let (cursor, limit) = page_bounds(input.page.as_ref())?;
                let state = input.state.as_ref().map(serde_json::to_value).transpose()?;
                let mut statement = connection.prepare("SELECT s.authorization_session_id FROM auth_authorization_sessions s LEFT JOIN auth_oauth_grants g ON g.oauth_grant_id=s.oauth_grant_id WHERE s.authorization_session_id>?1 AND (?2 IS NULL OR s.principal_id=?2) AND (?3 IS NULL OR json_extract(s.binding_json,'$.clientId')=?3) AND (?4 IS NULL OR g.login_session_id=?4) AND (?5 IS NULL OR s.oauth_grant_id=?5) AND (?6 IS NULL OR json_extract(s.binding_json,'$.deploymentId')=?6) AND (?7 IS NULL OR s.state=?7) ORDER BY s.authorization_session_id LIMIT ?8")?;
                let ids = statement.query_map(params![cursor,input.principal_id.as_ref().map(ToString::to_string),input.client_id.as_ref().map(|id|id.as_ref()),input.login_session_id.as_ref().map(ToString::to_string),input.oauth_grant_id.as_ref().map(ToString::to_string),input.deployment_id.as_ref().map(ToString::to_string),state.as_ref().and_then(|state|state.as_str()),limit+1], |row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                page(ids, limit, |id| read_authorization_session(connection, id))
            })
        }
    );
    mutate!(
        register_authorization_sessions_revoke,
        Some(PlatformPrivilege::PrincipalsManage),
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         _: &AuthenticatedCaller,
         input: &AuthAuthorizationSessionRevokeRequest,
         now| {
            store.mutate(
                identity,
                "rpc.authorizationSession.revoke",
                input,
                now,
                |connection| {
                    SqliteAuthorizationStore::require_privilege(
                        connection,
                        &identity.actor,
                        "principals.manage",
                    )?;
                    let scope =
                        EnforcementScope::Session(input.authorization_session_id.to_string());
                    let root = Mutation {
                        request_digest: digest(&scope)?,
                        ..identity.clone()
                    };
                    let work = store.revoke_root_in_transaction(connection, &root, &scope, now)?;
                    Ok(AuthAuthorizationSessionRevokeResponse {
                        authorization_session_id: input.authorization_session_id,
                        retired_at: timestamp(now)?,
                        enforcement: read_enforcement(connection, &work)?,
                        extra: Default::default(),
                    })
                },
            )
        }
    );
    mutate!(
        register_connections_kick,
        Some(PlatformPrivilege::PrincipalsManage),
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         _: &AuthenticatedCaller,
         input: &AuthConnectionsKickRequest,
         now| kick_connection(store, identity, input, now)
    );
    query!(
        register_authority_resolve,
        None,
        |store: &SqliteAuthorizationStore,
         _: &str,
         _: &AuthenticatedCaller,
         input: &AuthAuthorityResolveRequest| store
            .transaction(|connection| read_authority(connection, &input.context_digest))
    );
    query!(
        register_authority_renew,
        None,
        |store: &SqliteAuthorizationStore,
         _: &str,
         verified: &AuthenticatedCaller,
         input: &AuthAuthorityRenewRequest| renew_authority(
            store,
            verified,
            input,
            time::OffsetDateTime::now_utc().unix_timestamp()
        )
    );
    query!(
        register_issuers_resolve_chain,
        None,
        |store: &SqliteAuthorizationStore,
         _: &str,
         _: &AuthenticatedCaller,
         input: &AuthIssuerChainRequest| store
            .transaction(|connection| read_chain(connection, input))
    );
    mutate!(
        register_issuers_revoke,
        Some(PlatformPrivilege::PrivilegesManage),
        |store: &SqliteAuthorizationStore,
         identity: &Mutation,
         _: &AuthenticatedCaller,
         input: &AuthIssuerRevokeRequest,
         now| store.mutate(identity, "rpc.issuer.revoke", input, now, |connection| {
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "privileges.manage",
            )?;
            let scope = EnforcementScope::Issuer(input.key_id.to_string());
            let root = Mutation {
                request_digest: digest(&scope)?,
                ..identity.clone()
            };
            let work = store.revoke_root_in_transaction(connection, &root, &scope, now)?;
            Ok(AuthIssuerRevokeResponse {
                key_id: input.key_id.clone(),
                enforcement: read_enforcement(connection, &work)?,
                extra: Default::default(),
            })
        })
    );
    let rotation_store = store.clone();
    provider.register_issuers_rotate(move |context, input| {
        let store = rotation_store.clone();
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
                    let current: String = connection.query_row(
                        "SELECT key_id FROM auth_authorization_issuers WHERE is_current=1",
                        [],
                        |row| row.get(0),
                    )?;
                    if current != input.expected_current_key_id.as_ref() {
                        return Err(AuthError::Conflict);
                    }
                    Ok(())
                })
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)?;
            // The domain rotation owner requires a provisioned next public key
            // and installation of its private custody. This wire request has
            // neither; generating a replacement root here would strand Auth.
            Err(domain_failure(
                "issuer_custody_required",
                "Install next issuer signing custody before rotation",
            ))
        }
    });
    let admission_store = store.clone();
    provider.register_admission_renew(move |context, input| {
        let store = admission_store.clone();
        async move {
            caller(&context, None).map_err(rpc_error)?;
            let verified = context
                .request()
                .verified_request()
                .ok_or_else(|| rpc_error(AuthError::Denied))?
                .caller()
                .clone();
            let renewed = tokio::task::spawn_blocking(move || {
                renew_admission(
                    &store,
                    &verified,
                    &input,
                    time::OffsetDateTime::now_utc().unix_timestamp(),
                )
            })
            .await
            .map_err(|_| rpc_error(AuthError::Unavailable))?
            .map_err(rpc_error)?;
            renewed.ok_or_else(|| {
                domain_failure(
                    "provisioned_signature_required",
                    "Provisioned admission requires the client's original identity signature",
                )
            })
        }
    });
}

fn domain_failure(code: &str, message: &str) -> ServerError {
    ServerError::DeclaredRpc(DeclaredRpcError::new(
        errors::AuthError::TYPE,
        message,
        [("code", serde_json::Value::String(code.into()))],
    ))
}

fn page_bounds(page: Option<&CursorQuery>) -> Result<(String, usize), AuthError> {
    let limit = page.and_then(|page| page.limit).unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(AuthError::Invalid("invalid page limit".into()));
    }
    Ok((
        page.and_then(|page| page.cursor.clone())
            .unwrap_or_default(),
        limit as usize,
    ))
}

fn page<T>(
    mut ids: Vec<String>,
    limit: usize,
    read: impl Fn(&str) -> Result<T, AuthError>,
) -> Result<CursorPage<T>, AuthError> {
    let next_cursor = (ids.len() > limit).then(|| ids[limit - 1].clone());
    ids.truncate(limit);
    Ok(CursorPage {
        items: ids.iter().map(|id| read(id)).collect::<Result<_, _>>()?,
        page: CursorPageInfo { next_cursor },
    })
}

fn timestamp(value: i64) -> Result<trellis_rs::generated::Timestamp, AuthError> {
    let value = time::OffsetDateTime::from_unix_timestamp(value)
        .map_err(|_| AuthError::Unavailable)?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| AuthError::Unavailable)?;
    trellis_rs::generated::Timestamp::parse(&value).map_err(|_| AuthError::Unavailable)
}

pub(super) fn read_client(connection: &Connection, id: &str) -> Result<AuthOAuthClient, AuthError> {
    let (kind,title,redirects,development,implied,metadata,state,revision):(String,String,String,bool,String,String,String,u64)=connection.query_row("SELECT kind,display_name,redirect_uris_json,development,implied_capabilities_json,metadata_json,state,revision FROM auth_oauth_clients WHERE client_id=?1 AND state!='deleted'",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))).optional()?.ok_or(AuthError::NotFound)?;
    let metadata: serde_json::Value = decode(&metadata)?;
    Ok(AuthOAuthClient {
        client_id: id.to_owned().into(),
        description: metadata
            .get("description")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .into(),
        development,
        enabled: state == "active",
        implied_consent: decode(&implied)?,
        kind: serde_json::from_value(json!(kind))?,
        redirect_uris: decode(&redirects)?,
        revision: AuthRevision(revision.into()),
        title,
        extra: Default::default(),
    })
}

pub(super) fn put_client(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOAuthClientPutRequest,
    now: i64,
) -> Result<AuthOAuthClientGetResponse, AuthError> {
    store.mutate(identity,"rpc.client.put",input,now,|connection| {
        SqliteAuthorizationStore::require_privilege(connection,&identity.actor,"clients.manage")?;
        let deleted:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_oauth_clients WHERE client_id=?1 AND state='deleted')",[input.client_id.as_ref()],|row|row.get(0))?;
        if deleted {return Err(AuthError::Conflict);}
        let existing:Option<(String,String)>=connection.query_row("SELECT metadata_json,requested_privileges_json FROM auth_oauth_clients WHERE client_id=?1",[input.client_id.as_ref()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        let (mut metadata,eligible_privileges)=match existing {Some((metadata,privileges))=>(decode::<serde_json::Value>(&metadata)?,decode(&privileges)?),None=>(json!({}),vec![])};
        metadata["description"]=json!(input.description.as_ref());
        let authorization_code = input.kind == AuthPublicClientKind::Browser || !input.redirect_uris.is_empty();
        let mut grant_types = vec!["refresh_token"];
        if authorization_code { grant_types.push("authorization_code"); }
        if input.kind == AuthPublicClientKind::Native { grant_types.push("urn:ietf:params:oauth:grant-type:device_code"); }
        metadata["grant_types"] = json!(grant_types);
        metadata["response_types"] = if authorization_code { json!(["code"]) } else { json!([]) };
        let registration=ClientRegistration {client_id:input.client_id.to_string(),kind:serde_json::to_value(&input.kind)?.as_str().ok_or(AuthError::Denied)?.into(),display_name:input.title.to_string(),redirect_uris:input.redirect_uris.clone(),development:input.development,implied_capabilities:input.implied_consent.iter().map(ToString::to_string).collect(),eligible_privileges,metadata,expected_revision:input.expected_revision.0};
        let domain=Mutation {request_digest:digest(&registration)?,..identity.clone()};
        store.register_client_in_transaction(connection,&domain,&registration,now)?;
        // Put is the supported create/update/enable operation.
        connection.execute("UPDATE auth_oauth_clients SET state='active' WHERE client_id=?1",[input.client_id.as_ref()])?;
        Ok(AuthOAuthClientGetResponse {client:read_client(connection,&input.client_id)?,extra:Default::default()})
    })
}

pub(super) fn change_client_state(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthOAuthClientMutationRequest,
    state: &str,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(identity,if state=="deleted" {"rpc.client.delete"} else {"rpc.client.disable"},input,now,|connection| {
        let operation=super::PolicyMutation::ClientState {client_id:input.client_id.to_string(),expected_revision:input.expected_revision.0.0,state:state.into()};
        let revision=super::policy::apply_policy_in_transaction(connection,identity,&operation,now)?;
        let scope=EnforcementScope::Client(input.client_id.to_string());
        let work:String=connection.query_row("SELECT work_id FROM auth_enforcement_work WHERE scope_key=?1 ORDER BY rowid DESC LIMIT 1",[digest(&scope)?],|row|row.get(0))?;
        Ok(AuthMutationResult {revision:AuthRevision(revision.into()),enforcement:Some(read_enforcement(connection,&work)?),extra:Default::default()})
    })
}

fn read_login(connection: &Connection, id: &str) -> Result<AuthLoginSession, AuthError> {
    let (principal,method,provider,created,expires,fresh,revoked):(String,String,Option<String>,i64,i64,Option<i64>,Option<i64>)=connection.query_row("SELECT principal_id,method,provider_id,created_at,expires_at,fresh_until,revoked_at FROM auth_login_sessions WHERE login_session_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).optional()?.ok_or(AuthError::NotFound)?;
    Ok(AuthLoginSession {
        claim_fresh_until: fresh.map(timestamp).transpose()?,
        created_at: timestamp(created)?,
        expires_at: timestamp(expires)?,
        login_session_id: id.parse().map_err(|_| AuthError::Unavailable)?,
        method: serde_json::from_value(json!(method))?,
        principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
        provider_id: provider
            .map(|id| id.parse().map_err(|_| AuthError::Unavailable))
            .transpose()?,
        state: if revoked.is_some() {
            AuthLoginState::Revoked
        } else {
            AuthLoginState::Active
        },
        extra: Default::default(),
    })
}

pub(super) fn revoke_login(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    verified: &AuthenticatedCaller,
    input: &AuthLoginRevokeRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(identity, "rpc.login.revoke", input, now, |connection| {
        let login = input.login_session_id.to_string();
        let (owner, revision): (String, u64) = connection
            .query_row(
                "SELECT principal_id,revision FROM auth_login_sessions WHERE login_session_id=?1",
                [&login],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(AuthError::NotFound)?;
        if owner != identity.actor {
            if !verified
                .platform_privileges
                .contains(&PlatformPrivilege::PrincipalsManage)
            {
                return Err(AuthError::Denied);
            }
            SqliteAuthorizationStore::require_privilege(
                connection,
                &identity.actor,
                "principals.manage",
            )?;
        }
        let scope = EnforcementScope::Login(login);
        let root = Mutation {
            request_digest: digest(&scope)?,
            ..identity.clone()
        };
        let work = store.revoke_root_in_transaction(connection, &root, &scope, now)?;
        Ok(AuthMutationResult {
            revision: AuthRevision((revision + 1).into()),
            enforcement: Some(read_enforcement(connection, &work)?),
            extra: Default::default(),
        })
    })
}

fn read_attachment(connection: &Connection, id: &str) -> Result<AuthAttachment, AuthError> {
    let (session,server,client,key,policy,state,admitted,closed):(String,String,u64,String,String,String,Option<i64>,Option<i64>)=connection.query_row("SELECT a.authorization_session_id,a.server_id,a.broker_client_id,s.session_public_key,a.transport_policy_digest,a.state,a.admitted_at,a.closed_at FROM auth_attachments a JOIN auth_authorization_sessions s USING(authorization_session_id) WHERE a.attachment_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))).optional()?.ok_or(AuthError::NotFound)?;
    Ok(AuthAttachment {
        admitted_at: admitted.map(timestamp).transpose()?,
        admitted_transport_policy_digest: policy.into(),
        attachment_id: id.parse().map_err(|_| AuthError::Unavailable)?,
        authorization_session_id: session.parse().map_err(|_| AuthError::Unavailable)?,
        broker_client_id: client.into(),
        closed_at: closed.map(timestamp).transpose()?,
        server_id: server,
        session_public_key: key.into(),
        state: serde_json::from_value(json!(state))?,
        extra: Default::default(),
    })
}

fn read_authorization_session(
    connection: &Connection,
    id: &str,
) -> Result<AuthAuthorizationSession, AuthError> {
    let (principal,kind,binding,runtime,key,credential,grant,identity,login,created,deadline):(String,String,String,String,String,String,Option<String>,Option<String>,Option<String>,i64,i64)=connection.query_row("SELECT s.principal_id,p.kind,s.binding_json,s.runtime_id,s.session_public_key,s.credential_kind,s.oauth_grant_id,s.identity_key_id,g.login_session_id,s.issued_at,s.hard_deadline FROM auth_authorization_sessions s JOIN auth_principals p USING(principal_id) LEFT JOIN auth_oauth_grants g ON g.oauth_grant_id=s.oauth_grant_id WHERE s.authorization_session_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?))).optional()?.ok_or(AuthError::NotFound)?;
    let session = id.parse().map_err(|_| AuthError::Unavailable)?;
    let status = SqliteAuthorizationStore::authorization_session_status_in(connection, session)?;
    Ok(serde_json::from_value(json!({
        "authorizationSessionId": session, "principalId": principal, "principalKind": kind,
        "binding": serde_json::from_str::<serde_json::Value>(&binding)?, "runtimeId": runtime,
        "sessionPublicKey": key, "credentialKind": credential, "loginSessionId": login,
        "oauthGrantId": grant, "identityKeyId": identity, "state": status.state,
        "createdAt": timestamp(created)?,
        // Unbounded roots use the protocol-safe integer sentinel in storage,
        // not a fictitious expiry in the public management projection.
        "notAfter": (deadline != 9_007_199_254_740_991_i64)
            .then(|| timestamp(deadline)).transpose()?,
        "maximumContextExpiresAt": status.maximum_context_expires_at,
        "retiredAt": status.cutoff, "reason": status.reason,
    }))?)
}

fn read_enforcement(
    connection: &Connection,
    id: &str,
) -> Result<AuthEnforcementProgress, AuthError> {
    let (state,scanned,retired,pending,failed):(String,u64,u64,u64,u64)=connection.query_row("SELECT state,scanned_sessions,retired_sessions,pending_kicks,failed_kicks FROM auth_enforcement_work WHERE work_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
    Ok(AuthEnforcementProgress {
        failed_kicks: failed.into(),
        pending_kicks: pending.into(),
        retired_sessions: retired.into(),
        scanned_sessions: scanned.into(),
        state: serde_json::from_value(json!(if state == "completed" && pending > 0 {
            "running"
        } else {
            &state
        }))?,
        work_id: id.parse().map_err(|_| AuthError::Unavailable)?,
        extra: Default::default(),
    })
}

pub(super) fn kick_connection(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthConnectionsKickRequest,
    now: i64,
) -> Result<AuthConnectionsKickResponse, AuthError> {
    store.mutate(identity,"rpc.connection.kick",input,now,|connection| {
        SqliteAuthorizationStore::require_privilege(connection,&identity.actor,"principals.manage")?;
        let (server,client,session,state):(String,u64,String,String)=connection.query_row("SELECT server_id,broker_client_id,authorization_session_id,state FROM auth_attachments WHERE attachment_id=?1",[input.attachment_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?.ok_or(AuthError::NotFound)?;
        if state=="closed" {return Err(AuthError::Conflict);}
        let work=enqueue_scope(connection,&EnforcementScope::Session(session.clone()),now)?;
        // A physical kick is not logical-session retirement or policy reduction.
        connection.execute("UPDATE auth_enforcement_work SET state='completed',completed_at=?2,pending_kicks=1 WHERE work_id=?1",params![work,now])?;
        let action=enqueue_effect(connection,"kick",&json!({"attachmentId":input.attachment_id,"serverId":server,"brokerClientId":client,"authorizationSessionId":session,"workId":work}),None,now)?;
        connection.execute("UPDATE auth_post_commit_actions SET enforcement_work_id=?1 WHERE action_id=?2",params![work,action])?;
        Ok(AuthConnectionsKickResponse {attachment_id:input.attachment_id,enforcement:read_enforcement(connection,&work)?,extra:Default::default()})
    })
}

pub(super) fn read_authority(
    connection: &Connection,
    id: &str,
) -> Result<AuthAuthorityResolveResponse, AuthError> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT signed_bytes FROM auth_session_authorities WHERE context_digest=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let authority = trellis_protocol::parse_session_authority(
        &bytes,
        trellis_protocol::MAX_SESSION_AUTHORITY_BYTES,
    )?;
    let mut providers = Vec::new();
    for provider in &authority.unsigned.provider_bindings {
        let bytes: Vec<u8> = connection.query_row(
            "SELECT signed_bytes FROM auth_provider_certificates WHERE certificate_digest=?1",
            [&provider.provider_certificate_digest],
            |row| row.get(0),
        )?;
        providers.push(AuthSignedBlob(bytes.into()));
    }
    // SessionAuthority has one snapshot reference per API. The contract's blob
    // carries their existing signed snapshot collection, not unsigned catalogs.
    let mut catalogs = Vec::new();
    for api in &authority.unsigned.apis {
        let bytes: Vec<u8> = connection.query_row(
            "SELECT signed_bytes FROM auth_api_verification_snapshots WHERE snapshot_digest=?1",
            [&api.catalog_snapshot_digest],
            |row| row.get(0),
        )?;
        catalogs.push(serde_json::from_slice::<serde_json::Value>(&bytes)?);
    }
    Ok(AuthAuthorityResolveResponse {
        authority: AuthSignedBlob(bytes.into()),
        catalog: AuthSignedBlob(canonical_json(&catalogs)?.into_bytes().into()),
        providers,
        extra: Default::default(),
    })
}

fn require_context_owner(
    connection: &Connection,
    id: &str,
    verified: &AuthenticatedCaller,
) -> Result<(), AuthError> {
    let owned:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM auth_session_authorities a JOIN auth_authorization_sessions s USING(authorization_session_id) WHERE a.context_digest=?1 AND s.authorization_session_id=?2 AND s.principal_id=?3 AND s.runtime_id=?4 AND s.session_public_key=?5 AND s.state='active')",params![id,verified.authorization_session_id,verified.principal_id,verified.runtime_id,verified.session_public_key],|row|row.get(0))?;
    if owned {
        Ok(())
    } else {
        Err(AuthError::Denied)
    }
}

pub(super) fn renew_admission(
    store: &SqliteAuthorizationStore,
    verified: &AuthenticatedCaller,
    input: &AuthAdmissionRenewRequest,
    now: i64,
) -> Result<Option<AuthAdmissionRenewResponse>, AuthError> {
    store.transaction(|connection| {
        require_context_owner(connection,&input.context_digest,verified)?;
        let identity=AdmissionIdentity {
            credential:session_credential(connection,&verified.authorization_session_id)?,
            refresh_family_id:connection.query_row("SELECT original_refresh_family_id FROM auth_authorization_sessions WHERE authorization_session_id=?1",[&verified.authorization_session_id],|row|row.get(0))?,
            runtime_id:verified.runtime_id.clone(),session_public_key:verified.session_public_key.clone(),inbox_prefix:verified.inbox_prefix.clone(),
        };
        if !matches!(identity.credential,super::Credential::OAuthGrant(_)) {return Ok(Ok(None));}
        let issued=match store.issue_authority_in_transaction(connection,&identity,Some(&verified.authorization_session_id),now)? {Ok(issued)=>issued,Err(error)=>return Ok(Err(error))};
        let nkey:String=connection.query_row("SELECT ephemeral_nkey FROM auth_attachments WHERE authorization_session_id=?1 AND state IN ('pending','admitted') ORDER BY rowid DESC LIMIT 1",[&verified.authorization_session_id],|row|row.get(0)).optional()?.ok_or(AuthError::Denied)?;
        let mut current=super::policy::evaluate(connection,&identity.credential,now)?;
        current.not_after=current.not_after.min(issued.authority.unsigned.expires_at);
        let signed=store.sign_oauth_admission_in(connection,&identity,&nkey,&current,now)?;
        Ok(Ok(Some(AuthAdmissionRenewResponse {admission_credential:serde_json::to_string(&signed)?,expires_at:timestamp(signed.credential.expires_at)?,extra:Default::default()})))
    })?
}

pub(super) fn renew_authority(
    store: &SqliteAuthorizationStore,
    verified: &AuthenticatedCaller,
    input: &AuthAuthorityRenewRequest,
    now: i64,
) -> Result<AuthAuthorityRenewResponse, AuthError> {
    store.transaction(|connection| {
        require_context_owner(connection, &input.context_digest, verified)?;
        let admission = AdmissionIdentity {
            credential: session_credential(connection, &verified.authorization_session_id)?,
            refresh_family_id: connection.query_row("SELECT original_refresh_family_id FROM auth_authorization_sessions WHERE authorization_session_id=?1",[&verified.authorization_session_id],|row|row.get(0))?,
            runtime_id: verified.runtime_id.clone(),
            session_public_key: verified.session_public_key.clone(),
            inbox_prefix: verified.inbox_prefix.clone(),
        };
        let issued = match store.issue_authority_in_transaction(
            connection,
            &admission,
            Some(&verified.authorization_session_id),
            now,
        )? {
            Ok(issued) => issued,
            Err(error) => return Ok(Err(error)),
        };
        let response = read_authority(connection, &issued.digest)?;
        Ok(Ok(AuthAuthorityRenewResponse {
            authority: response.authority,
            catalog: response.catalog,
            context_digest: issued.digest.into(),
            providers: response.providers,
            extra: Default::default(),
        }))
    })?
}

/// OAuth grant IDs are the persisted identities of actual delegated approvals;
/// remembered consent is a separate binding-wide policy source, not a new grant.
pub(super) fn read_delegation(
    connection: &Connection,
    id: &str,
) -> Result<AuthDelegation, AuthError> {
    let (principal,client,kind,value,jkt,approved,revision):(String,String,String,String,String,String,u64)=connection.query_row("SELECT principal_id,client_id,binding_kind,binding_value,durable_dpop_jkt,approved_capabilities_json,revision FROM auth_oauth_grants WHERE oauth_grant_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).optional()?.ok_or(AuthError::NotFound)?;
    let approved: Vec<trellis_protocol::CapabilityAuthority> = decode(&approved)?;
    let choices = approved
        .into_iter()
        .map(|capability| AuthConsentChoice {
            capability: AuthCapabilityIdentity {
                capability_id: capability.capability_id.into(),
                identity_generation: AuthRevision(capability.identity_generation.get().into()),
                extra: Default::default(),
            },
            consent_revision: AuthRevision(capability.consent_revision.get().into()),
            decision: AuthDecision::Approved,
            expires_at: None,
            extra: Default::default(),
        })
        .collect();
    Ok(AuthDelegation {
        binding: AuthDelegationBinding {
            client_id: client.into(),
            durable_dpop_jkt: Some(jkt),
            kind: serde_json::from_value(json!(kind))?,
            origin: (kind == "browser").then_some(value),
            extra: Default::default(),
        },
        choices,
        delegation_id: id.parse().map_err(|_| AuthError::Unavailable)?,
        principal_id: principal.parse().map_err(|_| AuthError::Unavailable)?,
        revision: AuthRevision(revision.into()),
        extra: Default::default(),
    })
}

pub(super) fn revoke_delegation(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    verified: &AuthenticatedCaller,
    input: &AuthDelegationRevokeRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    store.mutate(
        identity,
        "rpc.delegation.revoke",
        input,
        now,
        |connection| {
            let delegation = read_delegation(connection, &input.delegation_id.to_string())?;
            if delegation.principal_id.to_string() != identity.actor {
                if !verified
                    .platform_privileges
                    .contains(&PlatformPrivilege::PrincipalsManage)
                {
                    return Err(AuthError::Denied);
                }
                SqliteAuthorizationStore::require_privilege(
                    connection,
                    &identity.actor,
                    "principals.manage",
                )?;
            }
            if delegation.revision != input.expected_revision {
                return Err(AuthError::Conflict);
            }
            let binding:(String,String,String)=connection.query_row("SELECT client_id,binding_kind,binding_value FROM auth_oauth_grants WHERE oauth_grant_id=?1",[input.delegation_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            // Clear remembered approval without revoking sibling grants for the
            // same app. The selected grant and its families retire below, once.
            connection.execute("UPDATE auth_remembered_consent SET decision='declined',revision=revision+1,updated_at=?1 WHERE principal_id=?2 AND client_id=?3 AND binding_kind=?4 AND binding_value=?5",params![now,delegation.principal_id.to_string(),binding.0,binding.1,binding.2])?;
            let scope = EnforcementScope::Grant(input.delegation_id.to_string());
            let root = Mutation {
                request_digest: digest(&scope)?,
                ..identity.clone()
            };
            let work = store.revoke_root_in_transaction(connection, &root, &scope, now)?;
            let revision:u64=connection.query_row("SELECT revision FROM auth_oauth_grants WHERE oauth_grant_id=?1",[input.delegation_id.to_string()],|row|row.get(0))?;
            Ok(AuthMutationResult {
                revision: AuthRevision(revision.into()),
                enforcement: Some(read_enforcement(connection, &work)?),
                extra: Default::default(),
            })
        },
    )
}

pub(super) fn read_chain(
    connection: &Connection,
    input: &AuthIssuerChainRequest,
) -> Result<AuthIssuerChainResponse, AuthError> {
    SqliteAuthorizationStore::resolve_issuer_in(connection, &input.pinned_key_id)?;
    SqliteAuthorizationStore::resolve_issuer_in(connection, &input.target_key_id)?;
    let mut key = input.pinned_key_id.to_string();
    let mut rotations = Vec::new();
    while key != input.target_key_id.as_ref() {
        if rotations.len() >= 128 {
            return Err(AuthError::ResourceLimit);
        }
        let (next,bytes):(String,Vec<u8>)=connection.query_row("SELECT next_key_id,signed_bytes FROM auth_issuer_rotations WHERE previous_key_id=?1 ORDER BY sequence LIMIT 1",[&key],|row|Ok((row.get(0)?,row.get(1)?))).optional()?.ok_or(AuthError::NotFound)?;
        rotations.push(AuthSignedBlob(bytes.into()));
        key = next;
    }
    Ok(AuthIssuerChainResponse {
        rotations,
        extra: Default::default(),
    })
}
