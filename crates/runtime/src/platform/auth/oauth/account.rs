//! Bounded account actions over the existing portal and account-flow repository.

use super::super::{
    rpc_users::hash_password,
    sqlite::{digest, id},
    AuthError, Mutation, SqliteAuthorizationStore,
};
use super::with_store;
use super::{
    http::{cookie_response, error, fields, portal_cookie, portal_origin},
    oidc::{UpstreamFlow, VerifiedIdentity},
    OAuthEngine, OAuthStorage,
};
use crate::platform::auth::blocking;
use crate::platform::auth::sqlite::Connection;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Redirect, Response},
    Form,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::error::ErrorCode;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;
use trellis_runtime_apis::__types::trellis::{
    AuthIdentityLinkCreateRequest, AuthIdentityLinkCreateResponse, AuthPasswordResetCreateRequest,
    AuthPasswordResetCreateResponse,
};

#[derive(Serialize, Deserialize)]
struct Payload {
    actor: String,
    reset_authority: Option<PasswordResetAuthority>,
    credential_version: Option<u64>,
    username: Option<String>,
    provider_revision: Option<u64>,
    protected: bool,
    #[serde(default)]
    handoff: Option<LinkHandoff>,
}

#[derive(Serialize, Deserialize)]
enum PasswordResetAuthority {
    AuthorizationSession(String),
    LocalOperator,
}

#[derive(Serialize, Deserialize)]
struct LinkHandoff {
    session: String,
    expires: i64,
    used: bool,
}

pub(super) struct Action {
    pub(super) id: String,
    pub(super) target: String,
    pub(super) provider: Option<String>,
    pub(super) expires: i64,
    payload: Payload,
}

fn hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}
fn token() -> Result<String, AuthError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn reset_requester(
    connection: &Connection,
    actor: &str,
    session: &str,
    now: i64,
) -> Result<(), AuthError> {
    use super::super::{authorization_sessions::credential_snapshot, Credential};
    use trellis_protocol::PlatformPrivilege;
    SqliteAuthorizationStore::require_privilege(connection, actor, "principals.manage")?;
    let (grant, identity, accepted): (Option<String>, Option<String>, String) = connection
        .query_row(
            "SELECT oauth_grant_id,identity_key_id,approved_privileges_json \
            FROM auth_authorization_sessions \
            WHERE authorization_session_id=?1 \
            AND principal_id=?2 \
            AND state='active' AND hard_deadline>?3",
            params![session, actor, now],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(AuthError::Denied)?;
    let accepted: Vec<PlatformPrivilege> = serde_json::from_str(&accepted)?;
    if !accepted.contains(&PlatformPrivilege::PrincipalsManage) {
        return Err(AuthError::Denied);
    }
    let credential = match (grant, identity) {
        (Some(grant), None) => Credential::OAuthGrant(grant),
        (None, Some(identity)) => Credential::ProvisionedIdentity(identity),
        _ => return Err(AuthError::Denied),
    };
    let current = credential_snapshot(connection, &credential, now)?;
    if current.principal_id != actor
        || !current
            .platform_privileges
            .contains(&PlatformPrivilege::PrincipalsManage)
    {
        return Err(AuthError::Denied);
    }
    Ok(())
}

fn action(connection: &Connection, token: &str, kind: &str, now: i64) -> Result<Action, AuthError> {
    if token.len() != 43
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(AuthError::Denied);
    }
    let (id, target, provider, expires, payload): (String, String, Option<String>, i64, String) =
        connection
            .query_row(
                "SELECT flow_id,target_principal_id,target_provider_id,expires_at,\
                payload_json \
                FROM auth_account_flows \
                WHERE token_hash=?1 \
                AND kind=?2 AND state='pending' \
                AND expires_at>?3",
                params![hash(token), kind, now],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AuthError::Denied)?;
    Ok(Action {
        id,
        target,
        provider,
        expires,
        payload: serde_json::from_str(&payload)?,
    })
}

/// Issue a replayable reset response without persisting its bearer token in plaintext.
pub(crate) fn create_reset(
    storage: &OAuthStorage,
    identity: &Mutation,
    input: &AuthPasswordResetCreateRequest,
    requester_session: &str,
    now: i64,
) -> Result<AuthPasswordResetCreateResponse, AuthError> {
    let lookup = digest(&(
        identity.actor.as_str(),
        identity.request_id.as_str(),
        "password_reset",
    ))?;
    let sealed: Vec<u8> = storage.authority.mutate(
        identity,
        "rpc.users.passwordReset.create",
        input,
        now,
        |connection| {
            reset_requester(connection, &identity.actor, requester_session, now)?;
            let target = input.user_id.to_string();
            let protected: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_bootstrap_administrator \
                WHERE principal_id=?1)",
                [&target],
                |row| row.get(0),
            )?;
            if protected {
                return Err(AuthError::Denied);
            }
            let (username, version): (Option<String>, Option<u64>) = connection
                .query_row(
                    "SELECT coalesce(c.normalized_username,(SELECT provider_subject \
                    FROM auth_provider_identities \
                    WHERE principal_id=p.principal_id \
                    AND provider='local')),c.version \
                    FROM auth_principals p \
                    LEFT JOIN auth_local_credentials c USING(principal_id) \
                    WHERE p.principal_id=?1 \
                    AND p.kind='user' AND p.state='active'",
                    [&target],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            let username = username.ok_or(AuthError::Denied)?;
            let token = token()?;
            let payload = Payload {
                actor: identity.actor.clone(),
                reset_authority: Some(PasswordResetAuthority::AuthorizationSession(
                    requester_session.to_owned(),
                )),
                username: Some(username),
                credential_version: version,
                provider_revision: None,
                protected,
                handoff: None,
            };
            insert(
                connection,
                &token,
                "password_reset",
                &target,
                None,
                &payload,
                now,
            )?;
            let response = AuthPasswordResetCreateResponse {
                token,
                expires_at: super::super::sqlite::timestamp(now + 300)?,
                extra: Default::default(),
            };
            storage
                .encode("account-response", &lookup, &response)
                .map_err(|_| AuthError::Unavailable)
        },
    )?;
    storage
        .decode("account-response", &lookup, &sealed)
        .map_err(|_| AuthError::Unavailable)
}

/// Issue an isolated portal link action; identity verification happens only upstream.
pub(crate) fn create_link(
    storage: &OAuthStorage,
    identity: &Mutation,
    input: &AuthIdentityLinkCreateRequest,
    now: i64,
) -> Result<AuthIdentityLinkCreateResponse, AuthError> {
    let lookup = digest(&(
        identity.actor.as_str(),
        identity.request_id.as_str(),
        "identity_link",
    ))?;
    let sealed: Vec<u8> = storage.authority.mutate(
        identity,
        "rpc.users.identityLink.create",
        input,
        now,
        |connection| {
            let user: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_principals \
                WHERE principal_id=?1 \
                AND kind='user' AND state='active')",
                [&identity.actor],
                |row| row.get(0),
            )?;
            if !user {
                return Err(AuthError::Denied);
            }
            let provider = input.provider_id.to_string();
            let revision: u64 = connection
                .query_row(
                    "SELECT revision FROM auth_oidc_providers \
                    WHERE provider_id=?1 \
                    AND state='active'",
                    [&provider],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(AuthError::NotFound)?;
            let protected: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 \
                FROM auth_bootstrap_administrator \
                WHERE principal_id=?1)",
                [&identity.actor],
                |row| row.get(0),
            )?;
            let token = token()?;
            let payload = Payload {
                actor: identity.actor.clone(),
                reset_authority: None,
                username: None,
                credential_version: None,
                provider_revision: Some(revision),
                protected,
                handoff: None,
            };
            insert(
                connection,
                &token,
                "identity_link",
                &identity.actor,
                Some(&provider),
                &payload,
                now,
            )?;
            let response = AuthIdentityLinkCreateResponse {
                authorization_url: format!(
                    "{}/account/link?token={token}",
                    storage.authority.settings.oauth_origin
                ),
                expires_at: super::super::sqlite::timestamp(now + 300)?,
                extra: Default::default(),
            };
            storage
                .encode("account-response", &lookup, &response)
                .map_err(|_| AuthError::Unavailable)
        },
    )?;
    storage
        .decode("account-response", &lookup, &sealed)
        .map_err(|_| AuthError::Unavailable)
}

/// Issue an account reset under local filesystem/operator authority, never RPC
/// authority. Initialization and explicit local recovery share this issuer.
pub(crate) fn create_local_reset_in_transaction(
    connection: &Connection,
    principal: &str,
    now: i64,
) -> Result<AuthPasswordResetCreateResponse, AuthError> {
    let (username, version): (String, u64) = connection
        .query_row(
            "SELECT c.normalized_username,c.version \
            FROM auth_local_credentials c \
            JOIN auth_principals p USING(principal_id) \
            WHERE c.principal_id=?1 \
            AND p.state='active' \
            AND p.kind='user'",
            [principal],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(AuthError::NotFound)?;
    let protected = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM auth_bootstrap_administrator WHERE principal_id=?1)",
        [principal],
        |row| row.get(0),
    )?;
    let token = token()?;
    let payload = Payload {
        actor: principal.into(),
        reset_authority: Some(PasswordResetAuthority::LocalOperator),
        username: Some(username),
        credential_version: Some(version),
        provider_revision: None,
        protected,
        handoff: None,
    };
    connection.execute(
        "UPDATE auth_account_flows \
        SET state='revoked',version=version+1 \
        WHERE target_principal_id=?1 \
        AND kind='password_reset' \
        AND state='pending'",
        [principal],
    )?;
    insert(
        connection,
        &token,
        "password_reset",
        principal,
        None,
        &payload,
        now,
    )?;
    Ok(AuthPasswordResetCreateResponse {
        token,
        expires_at: super::super::sqlite::timestamp(now + 300)?,
        extra: Default::default(),
    })
}

fn insert(
    connection: &Connection,
    token: &str,
    kind: &str,
    target: &str,
    provider: Option<&str>,
    payload: &Payload,
    now: i64,
) -> Result<(), AuthError> {
    connection.execute(
        "INSERT INTO auth_account_flows(flow_id,kind,token_hash,\
        target_principal_id,target_provider_id,payload_json,state,created_at,\
        expires_at,version) \
        VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?8,1)",
        params![
            id(),
            kind,
            hash(token),
            target,
            provider,
            serde_json::to_string(payload)?,
            now,
            now + 300
        ],
    )?;
    Ok(())
}

/// Validate exact current portal-user continuity before leaving for upstream reauthentication.
pub(super) fn link_session(
    storage: &OAuthStorage,
    token: &str,
    cookie: &str,
    csrf: &str,
    provider: &str,
    now: i64,
) -> Result<Action, AuthError> {
    let login = storage.authority.portal_login(cookie, csrf, now)?;
    storage.authority.transaction(|connection| {
        let mut action = action(connection, token, "identity_link", now)?;
        if action.target != login.principal_id
            || action.payload.actor != login.principal_id
            || action.provider.as_deref() != Some(provider)
        {
            return Err(AuthError::Denied);
        }
        let current: Option<u64> = connection
            .query_row(
                "SELECT revision FROM auth_oidc_providers WHERE provider_id=?1 AND state='active'",
                [provider],
                |row| row.get(0),
            )
            .optional()?;
        if current != action.payload.provider_revision {
            return Err(AuthError::Denied);
        }
        let handoff = action.payload.handoff.as_mut().ok_or(AuthError::Denied)?;
        if handoff.session != login.session_id || handoff.expires <= now || handoff.used {
            return Err(AuthError::Denied);
        }
        handoff.used = true;
        connection.execute(
            "UPDATE auth_account_flows SET payload_json=?1,version=version+1 WHERE flow_id=?2",
            params![serde_json::to_string(&action.payload)?, action.id],
        )?;
        Ok(action)
    })
}

/// Redeem a reset in one transaction after the expensive password hash was computed off-lock.
pub(crate) fn complete_reset(
    storage: &OAuthStorage,
    token: &str,
    cookie: &str,
    csrf: &str,
    replacement: &str,
    now: i64,
) -> Result<(), AuthError> {
    storage
        .authority
        .portal_session_identity(cookie, csrf, now)?;
    storage.authority.transaction(|connection| {
        let action = action(connection, token, "password_reset", now)?;
        let session: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_portal_sessions \
            WHERE cookie_hash=?1 \
            AND csrf_hash=?2 AND expires_at>?3)",
            params![hash(cookie), hash(csrf), now],
            |row| row.get(0),
        )?;
        if !session {
            return Err(AuthError::Denied);
        }
        let protected: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_bootstrap_administrator \
            WHERE principal_id=?1)",
            [&action.target],
            |row| row.get(0),
        )?;
        let active: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_principals \
            WHERE principal_id=?1 \
            AND kind='user' AND state='active')",
            [&action.target],
            |row| row.get(0),
        )?;
        if protected != action.payload.protected || !active {
            return Err(AuthError::Denied);
        }
        match &action.payload.reset_authority {
            Some(PasswordResetAuthority::AuthorizationSession(session)) => {
                if protected {
                    return Err(AuthError::Denied);
                }
                reset_requester(connection, &action.payload.actor, session, now)?;
            }
            Some(PasswordResetAuthority::LocalOperator)
                if action.payload.actor == action.target => {}
            _ => return Err(AuthError::Denied),
        }
        let revision = action.payload.credential_version.unwrap_or(0);
        match action.payload.credential_version {
            Some(version) => {
                if connection.execute(
                    "UPDATE auth_local_credentials \
                    SET password_hash=?1,version=version+1,password_changed_at=?2,\
                    updated_at=?2,failed_attempts=0,locked_until=NULL \
                    WHERE principal_id=?3 \
                    AND version=?4",
                    params![replacement, now, action.target, version],
                )? != 1
                {
                    return Err(AuthError::Conflict);
                }
            }
            None => {
                let present: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 \
                    FROM auth_local_credentials \
                    WHERE principal_id=?1)",
                    [&action.target],
                    |row| row.get(0),
                )?;
                if present {
                    return Err(AuthError::Conflict);
                }
                connection.execute(
                    "INSERT INTO auth_local_credentials(principal_id,normalized_username,\
                    password_hash,hash_profile,password_changed_at,failed_attempts,\
                    updated_at,version) \
                    VALUES(?1,?2,?3,1,?4,0,?4,1)",
                    params![action.target, action.payload.username, replacement, now],
                )?;
            }
        }
        let scope = super::super::revocation::EnforcementScope::Principal(action.target.clone());
        let audit = serde_json::json!({"flowId": action.id, "credentialRevision": revision + 1});
        let mut statement = connection.prepare(
            "SELECT login_session_id \
            FROM auth_login_sessions \
            WHERE principal_id=?1 \
            AND revoked_at IS NULL",
        )?;
        let logins = statement
            .query_map([&action.target], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for login in logins {
            let root = super::super::revocation::EnforcementScope::Login(login);
            let identity = Mutation {
                actor: action.payload.actor.clone(),
                request_id: id(),
                request_digest: digest(&root)?,
            };
            storage
                .authority
                .revoke_root_in_transaction(connection, &identity, &root, now)?;
        }
        SqliteAuthorizationStore::policy_changed(connection, &scope, now)?;
        consume(connection, &action.id, now)?;
        let identity = Mutation {
            actor: action.payload.actor,
            request_id: action.id.clone(),
            request_digest: digest(&audit)?,
        };
        connection.execute(
            "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,\
            target_id,data_json,created_at) \
            VALUES(?1,?2,'account.passwordReset.complete',?3,?4,?5)",
            params![
                id(),
                identity.actor,
                action.target,
                serde_json::to_string(&audit)?,
                now
            ],
        )?;
        Ok(())
    })
}

pub(super) fn complete_link(
    storage: &OAuthStorage,
    flow: &UpstreamFlow,
    verified: &VerifiedIdentity,
    cookie: &str,
    csrf: &str,
    now: i64,
) -> Result<(), AuthError> {
    let token = flow.account_token.as_deref().ok_or(AuthError::Denied)?;
    let login = storage.authority.portal_login(cookie, csrf, now)?;
    if login.session_id != flow.session_id {
        return Err(AuthError::Denied);
    }
    storage.authority.transaction(|connection| {
        let action = action(connection, token, "identity_link", now)?;
        if action.target != login.principal_id
            || action.payload.actor != login.principal_id
            || action.id != flow.authorization_transaction
            || action.provider.as_deref() != Some(&flow.provider.provider_id)
            || action.payload.provider_revision != Some(flow.provider.revision)
            || verified.expires_at <= now
            || !action
                .payload
                .handoff
                .as_ref()
                .is_some_and(|handoff| handoff.used && handoff.session == flow.session_id)
        {
            return Err(AuthError::Denied);
        }
        let live: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 \
            FROM auth_portal_sessions s \
            JOIN auth_login_sessions l ON l.login_session_id=s.login_session_id \
            JOIN auth_principals p ON p.principal_id=l.principal_id \
            JOIN auth_oidc_providers o ON o.provider_id=?1 \
            WHERE s.portal_session_id=?2 \
            AND l.principal_id=?3 \
            AND l.revoked_at IS NULL \
            AND l.expires_at>?4 \
            AND s.expires_at>?4 \
            AND p.state='active' \
            AND o.state='active' \
            AND o.revision=?5 AND s.cookie_hash=?6 \
            AND s.csrf_hash=?7)",
            params![
                flow.provider.provider_id,
                login.session_id,
                action.target,
                now,
                flow.provider.revision,
                hash(cookie),
                hash(csrf)
            ],
            |row| row.get(0),
        )?;
        if !live {
            return Err(AuthError::Denied);
        }
        let already_linked = SqliteAuthorizationStore::link_oidc_identity_in(
            connection,
            &action.target,
            &flow.provider.provider_id,
            &verified.subject,
            now,
        )?;
        super::super::profiles::ingest(
            connection,
            &action.target,
            &flow.provider.provider_id,
            &verified.subject,
            &verified.profile,
            now,
        )?;
        consume(connection, &action.id, now)?;
        let audit = serde_json::json!({
            "flowId":action.id,
            "providerId":flow.provider.provider_id,
            "identityAlreadyLinked":already_linked
        });
        let identity = Mutation {
            actor: login.principal_id,
            request_id: flow.request_id.clone(),
            request_digest: digest(&audit)?,
        };
        connection.execute(
            "INSERT INTO auth_security_audit(audit_id,actor_principal_id,kind,\
            target_id,data_json,created_at) \
            VALUES(?1,?2,'account.identityLink.complete',?3,?4,?5)",
            params![
                id(),
                identity.actor,
                action.target,
                serde_json::to_string(&audit)?,
                now
            ],
        )?;
        Ok(())
    })
}

fn consume(connection: &Connection, id: &str, now: i64) -> Result<(), AuthError> {
    if connection.execute(
        "UPDATE auth_account_flows \
        SET state='consumed',consumed_at=?1,version=version+1 \
        WHERE flow_id=?2 AND state='pending' \
        AND expires_at>?1",
        params![now, id],
    )? != 1
    {
        return Err(AuthError::Denied);
    }
    Ok(())
}

pub(super) async fn page(
    State(engine): State<Arc<OAuthEngine>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(_) => return error(ErrorCode::InvalidRequest),
    };
    let Some(token) = form.get("token") else {
        return error(ErrorCode::InvalidRequest);
    };
    if kind == "continue" {
        if headers.contains_key(axum::http::header::ORIGIN) && !portal_origin(&engine, &headers) {
            return error(ErrorCode::AccessDenied);
        }
        let link = token.clone();
        let action = match with_store(&engine, move |store| {
            store.authority.transaction(|connection| {
                action(connection, &link, "identity_link", crate::clock::unix_now())
            })
        })
        .await
        {
            Ok(action) => action,
            Err(_) => return error(ErrorCode::AccessDenied),
        };
        let Some(provider) = action.provider else {
            return error(ErrorCode::AccessDenied);
        };
        // Ordinary navigation omits Origin. The reviewed, session-bound one-use
        // handoff is authenticated by link_session, not a manufactured header.
        let mut response = super::upstream_http::start(
            State(engine),
            Path(provider),
            headers,
            Query(vec![("link".into(), token.clone())]),
        )
        .await;
        response.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        response.headers_mut().insert(
            axum::http::header::REFERRER_POLICY,
            axum::http::HeaderValue::from_static("no-referrer"),
        );
        return response;
    }
    let flow_kind = match kind.as_str() {
        "reset" => "password_reset",
        "link" => "identity_link",
        _ => return error(ErrorCode::InvalidRequest),
    };
    let current = portal_cookie(&headers, "__Host-trellis-portal")
        .zip(portal_cookie(&headers, "__Host-trellis-portal-csrf"));
    let flow_token = token.clone();
    let prepared = with_store(&engine, move |storage| {
        let now = crate::clock::unix_now();
        let action = storage
            .authority
            .transaction(|connection| action(connection, &flow_token, flow_kind, now))
            .map_err(|_| ErrorCode::AccessDenied)?;
        let (current, guest) = match current.filter(|(cookie, csrf)| {
            storage
                .authority
                .portal_session_identity(cookie, csrf, now)
                .is_ok()
        }) {
            Some(current) => (current, None),
            None => {
                let session = storage
                    .authority
                    .create_portal_session(now)
                    .map_err(|_| ErrorCode::ServerError)?;
                (
                    (session.cookie.clone(), session.csrf.clone()),
                    Some(session),
                )
            }
        };
        let signed_in = storage
            .authority
            .portal_login(&current.0, &current.1, now)
            .ok()
            .map(|login| login.principal_id);
        Ok((action, current, guest, signed_in))
    })
    .await;
    let (action, (_, csrf), guest, signed_in) = match prepared {
        Ok(prepared) => prepared,
        Err(code) => return error(code),
    };
    let (title, body) = if kind == "reset" {
        (
            "Reset password",
            format!(
                r#"<form method="post" action="/portal/account/reset"><input type="hidden" name="token" value="{token}"><input type="hidden" name="csrf" value="{csrf}"><label>New password <input name="password" type="password" autocomplete="new-password" minlength="12" maxlength="4096" required></label><button>Reset password and sign out existing sessions</button></form>"#
            ),
        )
    } else {
        match signed_in {
            Some(principal) if principal == action.target => (
                "Link sign-in identity",
                format!(
                    r#"<form method="post" action="/portal/account/link"><input type="hidden" name="token" value="{token}"><input type="hidden" name="csrf" value="{csrf}"><button>Continue to identity provider</button></form>"#
                ),
            ),
            Some(_) => return error(ErrorCode::AccessDenied),
            None => (
                "Sign in to link an identity",
                format!(
                    r#"<form method="post" action="/portal/account/login"><input type="hidden" name="token" value="{token}"><input type="hidden" name="csrf" value="{csrf}"><label>Username <input name="username" autocomplete="username" required></label><label>Password <input name="password" type="password" autocomplete="current-password" required></label><button>Sign in</button></form>"#
                ),
            ),
        }
    };
    let response = html(&engine, title, &body);
    match guest {
        Some(guest) => cookie_response(&guest.cookie, Some(&guest.csrf), 300, response),
        None => response,
    }
}

pub(super) async fn submit(
    State(engine): State<Arc<OAuthEngine>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if !portal_origin(&engine, &headers) {
        return error(ErrorCode::InvalidRequest);
    }
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(_) => return error(ErrorCode::InvalidRequest),
    };
    let (Some(token), Some(csrf), Some(cookie)) = (
        form.get("token"),
        form.get("csrf"),
        portal_cookie(&headers, "__Host-trellis-portal"),
    ) else {
        return error(ErrorCode::AccessDenied);
    };
    if kind == "link" {
        let now = crate::clock::unix_now();
        let (token, csrf) = (token.clone(), csrf.clone());
        let reviewed = with_store(&engine, move |storage| {
            let login = storage.authority.portal_login(&cookie, &csrf, now)?;
            let replacement = self::token()?;
            storage.authority.transaction(|connection| {
                let mut action = action(connection, &token, "identity_link", now)?;
                if action.target != login.principal_id
                    || action.payload.actor != login.principal_id
                    || action.payload.handoff.is_some()
                {
                    return Err(AuthError::Denied);
                }
                let revision: Option<u64> = connection
                    .query_row(
                        "SELECT revision FROM auth_oidc_providers \
                        WHERE provider_id=?1 \
                        AND state='active'",
                        [&action.provider],
                        |row| row.get(0),
                    )
                    .optional()?;
                if revision.is_none() || revision != action.payload.provider_revision {
                    return Err(AuthError::Denied);
                }
                action.payload.handoff = Some(LinkHandoff {
                    session: login.session_id,
                    expires: action.expires.min(now + 60),
                    used: false,
                });
                connection.execute(
                    "UPDATE auth_account_flows \
                    SET token_hash=?1,payload_json=?2,version=version+1 \
                    WHERE flow_id=?3",
                    params![
                        hash(&replacement),
                        serde_json::to_string(&action.payload)?,
                        action.id
                    ],
                )?;
                Ok(replacement)
            })
        })
        .await;
        return match reviewed {
            Ok(token) => {
                let mut response = html(
                    &engine,
                    "Continue to identity provider",
                    &format!(
                        r#"<p>Your account-link request has been reviewed. Continue to sign in with the identity provider.</p><a href="/account/continue?token={token}" rel="noreferrer">Continue to identity provider</a>"#
                    ),
                );
                response.headers_mut().insert(
                    axum::http::header::REFERRER_POLICY,
                    axum::http::HeaderValue::from_static("no-referrer"),
                );
                response
            }
            Err(_) => error(ErrorCode::AccessDenied),
        };
    }
    let Some(password) = form.get("password") else {
        return error(ErrorCode::InvalidRequest);
    };
    let token = token.clone();
    let csrf = csrf.clone();
    let password = password.clone();
    if kind == "reset" {
        let reset_engine = engine.clone();
        let result = blocking(move || {
            let engine = reset_engine;
            engine.server.store().authority.portal_session_identity(
                &cookie,
                &csrf,
                crate::clock::unix_now(),
            )?;
            engine.server.store().authority.transaction(|connection| {
                action(
                    connection,
                    &token,
                    "password_reset",
                    crate::clock::unix_now(),
                )
            })?;
            let hash = hash_password(&password)?;
            complete_reset(
                engine.server.store(),
                &token,
                &cookie,
                &csrf,
                &hash,
                crate::clock::unix_now(),
            )
        })
        .await;
        return match result {
            Ok(()) => html(
                &engine,
                "Password reset",
                "<p>Your password was changed. Existing sessions have been signed out.</p>",
            ),
            _ => error(ErrorCode::AccessDenied),
        };
    }
    if kind == "login" {
        let Some(username) = form.get("username").cloned() else {
            return error(ErrorCode::InvalidRequest);
        };
        let replacement = match self::token() {
            Ok(token) => token,
            Err(_) => return error(ErrorCode::ServerError),
        };
        let new_cookie = replacement.clone();
        let csrf_cookie = csrf.clone();
        let destination = format!("/account/link?token={token}");
        let result = blocking(move || {
            let action = engine.server.store().authority.transaction(|connection| {
                action(
                    connection,
                    &token,
                    "identity_link",
                    crate::clock::unix_now(),
                )
            })?;
            let login = engine.server.store().authority.portal_local_login(
                &cookie,
                &csrf,
                &new_cookie,
                (&username, &password),
                &id(),
                crate::clock::unix_now(),
            )?;
            if login.principal_id != action.target {
                return Err(AuthError::Denied);
            }
            Ok(())
        })
        .await;
        return match result {
            Ok(()) => cookie_response(
                &replacement,
                Some(&csrf_cookie),
                300,
                Redirect::to(&destination).into_response(),
            ),
            _ => error(ErrorCode::AccessDenied),
        };
    }
    error(ErrorCode::InvalidRequest)
}

pub(super) fn html(engine: &OAuthEngine, title: &str, body: &str) -> Response {
    let notice = if engine.generated_tls() {
        "<p role=\"status\">This Trellis is using self-generated certificates \
and may not be suitable for production.</p>"
    } else {
        ""
    };
    let mut response = Html(format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta \
name=\"viewport\" content=\"width=device-width,\
initial-scale=1\"><title>{title}</title></head><body>{notice}<main><h1>{title}</h1>{body}</main></body></html>")).into_response();
    use axum::http::{header, HeaderValue};
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::auth::{
        lifecycle::OidcProviderRegistration,
        tests::{fixture, mutation},
    };

    #[test]
    fn passwordless_administrator_setup_survives_reopen_and_does_not_replace_completed_credentials()
    {
        use crate::platform::auth::tests::{open, NOW};
        use ed25519_dalek::SigningKey;
        let directory = tempfile::tempdir().unwrap();
        let authority = Arc::new(open(&directory, SigningKey::from_bytes(&[42; 32])));
        authority.initialize_issuer(NOW).unwrap();
        let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
        let (_, url) = storage
            .initialize_local_administrator("https://console.example", "operator", None, NOW)
            .unwrap();
        let url = url.unwrap();
        let token = url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(name, _)| name == "token")
            .unwrap()
            .1
            .into_owned();
        drop(storage);
        drop(authority);
        let authority = Arc::new(open(&directory, SigningKey::from_bytes(&[42; 32])));
        let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
        let renewed = storage
            .reset_local_administrator(NOW + 1, true)
            .unwrap()
            .unwrap();
        let renewed_token = url::Url::parse(&renewed)
            .unwrap()
            .query_pairs()
            .find(|(name, _)| name == "token")
            .unwrap()
            .1
            .into_owned();
        let guest = authority.create_portal_session(NOW + 1).unwrap();
        let replacement = hash_password("administrator setup password").unwrap();
        assert!(matches!(
            complete_reset(
                &storage,
                &token,
                &guest.cookie,
                &guest.csrf,
                &replacement,
                NOW + 1
            ),
            Err(AuthError::Denied)
        ));
        // A later startup must replace an expired setup link as well.
        drop(storage);
        drop(authority);
        let authority = Arc::new(open(&directory, SigningKey::from_bytes(&[42; 32])));
        let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
        let restarted_at = NOW + 601;
        let renewed = storage
            .reset_local_administrator(restarted_at, true)
            .unwrap()
            .unwrap();
        let current_token = url::Url::parse(&renewed)
            .unwrap()
            .query_pairs()
            .find(|(name, _)| name == "token")
            .unwrap()
            .1
            .into_owned();
        let guest = authority.create_portal_session(restarted_at).unwrap();
        assert!(matches!(
            complete_reset(
                &storage,
                &renewed_token,
                &guest.cookie,
                &guest.csrf,
                &replacement,
                restarted_at
            ),
            Err(AuthError::Denied)
        ));
        complete_reset(
            &storage,
            &current_token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            restarted_at,
        )
        .unwrap();
        drop(storage);
        drop(authority);
        let authority = Arc::new(open(&directory, SigningKey::from_bytes(&[42; 32])));
        let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
        assert!(storage
            .reset_local_administrator(restarted_at + 1, true)
            .unwrap()
            .is_none());
        assert!(matches!(
            complete_reset(
                &storage,
                &current_token,
                &guest.cookie,
                &guest.csrf,
                &replacement,
                restarted_at + 1
            ),
            Err(AuthError::Denied)
        ));
        authority
            .portal_local_login(
                &guest.cookie,
                &guest.csrf,
                &self::token().unwrap(),
                ("operator", "administrator setup password"),
                &id(),
                restarted_at + 1,
            )
            .unwrap();
    }

    #[test]
    fn local_operator_admin_reset_replaces_the_password_and_rejects_old_links_and_replay() {
        let f = fixture();
        let authority = f.store;
        let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
        let now = crate::clock::unix_now();
        let first = authority
            .transaction(|connection| create_local_reset_in_transaction(connection, &f.admin, now))
            .unwrap();
        assert!(storage
            .reset_local_administrator(now, true)
            .unwrap()
            .is_none());
        let current = storage
            .reset_local_administrator(now, false)
            .unwrap()
            .unwrap();
        let current_token = url::Url::parse(&current)
            .unwrap()
            .query_pairs()
            .find(|(name, _)| name == "token")
            .unwrap()
            .1
            .into_owned();
        let guest = authority.create_portal_session(now).unwrap();
        let replacement = hash_password("replacement administrator password").unwrap();
        assert!(matches!(
            complete_reset(
                &storage,
                &first.token,
                &guest.cookie,
                &guest.csrf,
                &replacement,
                now
            ),
            Err(AuthError::Denied)
        ));
        complete_reset(
            &storage,
            &current_token,
            &guest.cookie,
            &guest.csrf,
            &replacement,
            now,
        )
        .unwrap();
        assert!(matches!(
            complete_reset(
                &storage,
                &current_token,
                &guest.cookie,
                &guest.csrf,
                &replacement,
                now
            ),
            Err(AuthError::Denied)
        ));
        assert!(matches!(
            authority.portal_local_login(
                &guest.cookie,
                &guest.csrf,
                &token().unwrap(),
                ("admin", "a strong bootstrap password"),
                &id(),
                now
            ),
            Err(AuthError::Denied)
        ));
        authority
            .portal_local_login(
                &guest.cookie,
                &guest.csrf,
                &token().unwrap(),
                ("admin", "replacement administrator password"),
                &id(),
                now,
            )
            .unwrap();
    }

    #[tokio::test]
    async fn reviewed_link_handoff_rejects_other_sessions_stale_csrf_expiry_and_replay() {
        let f = fixture();
        let authority = f.store;
        let storage = OAuthStorage::new(authority.clone(), &[19; 32]).unwrap();
        let provider = id();
        let registration = OidcProviderRegistration {
            provider_id: provider.clone(),
            issuer: "https://identity.example".into(),
            display_name: "Identity".into(),
            client_id: "account".into(),
            sealed_client_secret: None,
            config: serde_json::json!({}),
            claim_freshness_seconds: 300,
            expected_revision: 0,
        };
        let now = crate::clock::unix_now();
        authority
            .transaction(|connection| {
                authority.register_oidc_provider_in_transaction(
                    connection,
                    &mutation(&f.admin, &registration),
                    &registration,
                    now,
                )
            })
            .unwrap();
        let guest = authority.create_portal_session(now).unwrap();
        let cookie = token().unwrap();
        authority
            .portal_local_login(
                &guest.cookie,
                &guest.csrf,
                &cookie,
                ("admin", "a strong bootstrap password"),
                &id(),
                now,
            )
            .unwrap();
        let other_guest = authority.create_portal_session(now).unwrap();
        let other_cookie = token().unwrap();
        authority
            .portal_local_login(
                &other_guest.cookie,
                &other_guest.csrf,
                &other_cookie,
                ("admin", "a strong bootstrap password"),
                &id(),
                now,
            )
            .unwrap();
        let engine = Arc::new(OAuthEngine::new(storage).unwrap());
        for expires in [true, false] {
            let request = AuthIdentityLinkCreateRequest {
                provider_id: provider.parse().unwrap(),
                idempotency_key: id().into(),
                extra: Default::default(),
            };
            let created = create_link(
                engine.server.store(),
                &mutation(&f.admin, &request),
                &request,
                now,
            )
            .unwrap();
            let original = url::Url::parse(&created.authorization_url)
                .unwrap()
                .query_pairs()
                .find(|(name, _)| name == "token")
                .unwrap()
                .1
                .into_owned();
            // A freshly issued account URL cannot skip the protected POST.
            assert!(link_session(
                engine.server.store(),
                &original,
                &cookie,
                &guest.csrf,
                &provider,
                now
            )
            .is_err());
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::ORIGIN,
                authority.settings.oauth_origin.parse().unwrap(),
            );
            headers.insert(
                axum::http::header::COOKIE,
                format!(
                    "__Host-trellis-portal={cookie}; __Host-trellis-portal-csrf={}",
                    guest.csrf
                )
                .parse()
                .unwrap(),
            );
            let response = submit(
                State(engine.clone()),
                Path("link".into()),
                headers,
                Form(vec![
                    ("token".into(), original.clone()),
                    ("csrf".into(), guest.csrf.clone()),
                ]),
            )
            .await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 16384)
                .await
                .unwrap();
            let body = std::str::from_utf8(&bytes).unwrap();
            let document = scraper::Html::parse_document(body);
            let selector = scraper::Selector::parse("a[href]").unwrap();
            let destination = document
                .select(&selector)
                .next()
                .unwrap()
                .value()
                .attr("href")
                .unwrap();
            let handoff = url::Url::parse(&created.authorization_url)
                .unwrap()
                .join(destination)
                .unwrap()
                .query_pairs()
                .find(|(name, _)| name == "token")
                .unwrap()
                .1
                .into_owned();
            assert!(link_session(
                engine.server.store(),
                &original,
                &cookie,
                &guest.csrf,
                &provider,
                now
            )
            .is_err());
            assert!(link_session(
                engine.server.store(),
                &handoff,
                &other_cookie,
                &other_guest.csrf,
                &provider,
                now
            )
            .is_err());
            assert!(link_session(
                engine.server.store(),
                &handoff,
                &cookie,
                &other_guest.csrf,
                &provider,
                now
            )
            .is_err());
            assert!(link_session(
                engine.server.store(),
                &handoff,
                &cookie,
                &guest.csrf,
                &id(),
                now
            )
            .is_err());
            if expires {
                let after_handoff_deadline = crate::clock::unix_now().saturating_add(61);
                assert!(link_session(
                    engine.server.store(),
                    &handoff,
                    &cookie,
                    &guest.csrf,
                    &provider,
                    after_handoff_deadline
                )
                .is_err());
            } else {
                let action = link_session(
                    engine.server.store(),
                    &handoff,
                    &cookie,
                    &guest.csrf,
                    &provider,
                    now,
                )
                .unwrap();
                assert_eq!(action.target, f.admin);
                assert!(link_session(
                    engine.server.store(),
                    &handoff,
                    &cookie,
                    &guest.csrf,
                    &provider,
                    now
                )
                .is_err());
            }
        }
    }
}
