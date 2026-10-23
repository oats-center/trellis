//! Browser-only upstream redirects, bound to the isolated portal's guest cookie.

use super::{
    http::{cookie_response, error, fields, portal_cookie},
    oidc::{ProviderSnapshot, UpstreamFlow},
    OAuthEngine,
};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use oauth_as::error::ErrorCode;
use openidconnect::{
    core::CoreAuthenticationFlow, AuthorizationCode, CsrfToken, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, Scope,
};
use serde_json::json;
use std::sync::Arc;

pub(super) async fn providers(State(engine): State<Arc<OAuthEngine>>) -> Response {
    let result = (|| {
        let connection = engine
            .server
            .store()
            .authority
            .connection
            .lock()
            .map_err(|_| super::super::AuthError::Unavailable)?;
        let mut statement = connection.prepare(
            "SELECT provider_id,display_name \
            FROM auth_oidc_providers \
            WHERE state='active' \
            ORDER BY display_name,provider_id",
        )?;
        let providers = statement
            .query_map([], |row| {
                Ok(json!({
                    "providerId":row.get::<_,
                    String>(0)?,
                    "displayName":row.get::<_,
                    String>(1)?
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, super::super::AuthError>(providers)
    })();
    match result {
        Ok(providers) => Json(providers).into_response(),
        Err(_) => error(ErrorCode::ServerError),
    }
}

pub(super) async fn start(
    State(engine): State<Arc<OAuthEngine>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    if ulid::Ulid::from_string(&provider).is_err() {
        return error(ErrorCode::InvalidRequest);
    }
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let (Some(cookie), Some(csrf)) = (
        portal_cookie(&headers, "__Host-trellis-portal"),
        portal_cookie(&headers, "__Host-trellis-portal-csrf"),
    ) else {
        return error(ErrorCode::AccessDenied);
    };
    let now = crate::clock::unix_now();
    let authority = &engine.server.store().authority;
    let session = match authority.portal_session_identity(&cookie, &csrf, now) {
        Ok(session) => session,
        Err(_) => return error(ErrorCode::AccessDenied),
    };
    let (transaction, deadline, client_id, account_token) = if let Some(token) = form.get("link") {
        if headers.contains_key(axum::http::header::ORIGIN)
            && !super::http::portal_origin(&engine, &headers)
        {
            return error(ErrorCode::InvalidRequest);
        }
        let action = match super::account::link_session(
            engine.server.store(),
            token,
            &cookie,
            &csrf,
            &provider,
            now,
        ) {
            Ok(action) => action,
            Err(_) => return error(ErrorCode::AccessDenied),
        };
        (
            action.id,
            action.expires,
            "account-link".to_owned(),
            Some(token.clone()),
        )
    } else {
        let Some(transaction) = form
            .get("transaction")
            .filter(|transaction| ulid::Ulid::from_string(transaction).is_ok())
        else {
            return error(ErrorCode::InvalidRequest);
        };
        let authorization = match engine
            .server
            .store()
            .authorization(transaction, &session, now)
        {
            Ok(flow) => flow,
            Err(_) => return error(ErrorCode::InvalidRequest),
        };
        if authority.portal_login(&cookie, &csrf, now).is_ok() {
            return Redirect::to(&format!("/consent?transaction={transaction}")).into_response();
        }
        let intent = match engine.consent_intent(&authorization.request).await {
            Ok(intent) => intent,
            Err(response) => return error(response.error),
        };
        (
            transaction.clone(),
            authorization.expires_at,
            intent.client_id.as_str().to_owned(),
            None,
        )
    };
    let configured = match authority.upstream_provider(&provider) {
        Ok(provider) => provider,
        Err(_) => return error(ErrorCode::AccessDenied),
    };
    let snapshot = match ProviderSnapshot::discover(
        provider,
        configured.revision,
        configured.issuer,
        configured.client_id,
        &engine.upstream_http,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let secret = match engine
        .server
        .store()
        .upstream_client_secret(&snapshot.identity.provider_id, configured.secret.as_deref())
    {
        Ok(secret) => secret,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let client = match snapshot.client(
        secret,
        format!("{}/portal/oidc/callback", authority.settings.oauth_origin),
    ) {
        Ok(client) => client,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .set_pkce_challenge(challenge);
    if account_token.is_some() {
        request = request.add_extra_param("prompt", "login");
    }
    for scope in configured
        .scopes
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    {
        if scope != "openid" {
            request = request.add_scope(Scope::new(scope));
        }
    }
    let (url, state, nonce) = request.url();
    let flow = UpstreamFlow {
        account_token,
        session_id: session,
        authorization_transaction: transaction.clone(),
        provider: snapshot.identity,
        nonce: nonce.secret().clone(),
        pkce_verifier: verifier.secret().clone(),
        expires_at: deadline.min(now + 300),
        request_id: super::super::sqlite::id(),
    };
    if engine
        .server
        .store()
        .store_upstream(state.secret(), &flow, &client_id)
        .is_err()
    {
        return error(ErrorCode::ServerError);
    }
    Redirect::to(url.as_str()).into_response()
}

pub(super) async fn callback(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let Some(state) = form
        .get("state")
        .filter(|state| !state.is_empty() && state.len() <= 128)
    else {
        return error(ErrorCode::InvalidRequest);
    };
    let (Some(cookie), Some(csrf)) = (
        portal_cookie(&headers, "__Host-trellis-portal"),
        portal_cookie(&headers, "__Host-trellis-portal-csrf"),
    ) else {
        return error(ErrorCode::AccessDenied);
    };
    let now = crate::clock::unix_now();
    let authority = &engine.server.store().authority;
    let session = match authority.portal_session_identity(&cookie, &csrf, now) {
        Ok(session) => session,
        Err(_) => return error(ErrorCode::AccessDenied),
    };
    let flow = match engine.server.store().take_upstream(state, &session, now) {
        Ok(flow) => flow,
        Err(_) => return error(ErrorCode::InvalidRequest),
    };
    if flow.account_token.is_none()
        && engine
            .server
            .store()
            .authorization(&flow.authorization_transaction, &session, now)
            .is_err()
    {
        return error(ErrorCode::InvalidRequest);
    }
    if form
        .get("iss")
        .is_some_and(|issuer| issuer != &flow.provider.issuer)
    {
        return error(ErrorCode::InvalidRequest);
    }
    if form.contains_key("error") {
        if flow.account_token.is_some() {
            return error(ErrorCode::AccessDenied);
        }
        return Redirect::to(&format!(
            "/login?transaction={}&error=upstream_login_failed",
            flow.authorization_transaction
        ))
        .into_response();
    }
    let Some(code) = form
        .get("code")
        .filter(|code| !code.is_empty() && code.len() <= 8192)
    else {
        return error(ErrorCode::InvalidRequest);
    };
    let configured = match authority.upstream_provider(&flow.provider.provider_id) {
        Ok(configured) => configured,
        Err(_) => return error(ErrorCode::AccessDenied),
    };
    if configured.revision != flow.provider.revision
        || configured.issuer != flow.provider.issuer
        || configured.client_id != flow.provider.client_id
    {
        return error(ErrorCode::AccessDenied);
    }
    let secret = match engine
        .server
        .store()
        .upstream_client_secret(&flow.provider.provider_id, configured.secret.as_deref())
    {
        Ok(secret) => secret,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let snapshot = match ProviderSnapshot::discover(
        flow.provider.provider_id.clone(),
        configured.revision,
        configured.issuer,
        configured.client_id,
        &engine.upstream_http,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let client = match snapshot.client(
        secret,
        format!("{}/portal/oidc/callback", authority.settings.oauth_origin),
    ) {
        Ok(client) => client,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let exchange = match client.exchange_code(AuthorizationCode::new(code.clone())) {
        Ok(exchange) => exchange,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let tokens = match exchange
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier.clone()))
        .request_async(&engine.upstream_http)
        .await
    {
        Ok(tokens) => tokens,
        Err(_) => {
            tracing::warn!(provider_id=%flow.provider.provider_id,"upstream OIDC token exchange rejected");
            return error(ErrorCode::AccessDenied);
        }
    };
    let mut identity = match snapshot.verify(&client, &tokens, &flow.nonce) {
        Ok(identity) => identity,
        Err(_) => {
            tracing::warn!(provider_id=%flow.provider.provider_id,"upstream OIDC ID token verification rejected");
            return error(ErrorCode::AccessDenied);
        }
    };
    snapshot
        .enrich(
            &mut identity,
            &tokens,
            &authority.settings.upstream_tls_roots_pem,
        )
        .await;
    let now = crate::clock::unix_now();
    if flow.account_token.is_some() {
        return match super::account::complete_link(
            engine.server.store(),
            &flow,
            &identity,
            &cookie,
            &csrf,
            now,
        ) {
            Ok(()) => super::account::html(
                &engine,
                "Identity linked",
                "<p>You can now sign in using this identity provider.</p>",
            ),
            Err(_) => error(ErrorCode::AccessDenied),
        };
    }
    let mut replacement = [0; 32];
    if getrandom::fill(&mut replacement).is_err() {
        return error(ErrorCode::ServerError);
    }
    let replacement = URL_SAFE_NO_PAD.encode(replacement);
    match authority.portal_upstream_login(&flow, &identity, &cookie, &csrf, &replacement, now) {
        Ok(login) => cookie_response(
            &replacement,
            Some(&csrf),
            (login.expires_at - now) as u32,
            Redirect::to(&format!(
                "/consent?transaction={}",
                flow.authorization_transaction
            ))
            .into_response(),
        ),
        Err(_) => {
            tracing::warn!(provider_id=%flow.provider.provider_id,"upstream OIDC linked-account login rejected");
            error(ErrorCode::AccessDenied)
        }
    }
}
