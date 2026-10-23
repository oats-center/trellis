//! Browser-only upstream redirects, bound to the isolated portal's guest cookie.

use super::with_store;
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
    let result = with_store(&engine, |store| {
        store.authority.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT provider_id,display_name \
                FROM auth_oidc_providers \
                WHERE state='active' \
                ORDER BY display_name,provider_id",
            )?;
            let providers = statement
                .query_map([], |row| {
                    Ok(json!({
                        "providerId": row.get::<_, String>(0)?,
                        "displayName": row.get::<_, String>(1)?
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(providers)
        })
    })
    .await;
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
    let authority = engine.server.store().authority.clone();
    let (session_cookie, session_csrf) = (cookie.clone(), csrf.clone());
    let session = match with_store(&engine, move |store| {
        store
            .authority
            .portal_session_identity(&session_cookie, &session_csrf, now)
    })
    .await
    {
        Ok(session) => session,
        Err(_) => return error(ErrorCode::AccessDenied),
    };
    let review = form.get("review").cloned();
    let (transaction, deadline, client_id, account_token) = if let Some(review) = &review {
        if ulid::Ulid::from_string(review).is_err() {
            return error(ErrorCode::InvalidRequest);
        }
        if authority.portal_login(&cookie, &csrf, now).is_ok() {
            return Redirect::to(&format!("/device-enrollment?reviewId={review}")).into_response();
        }
        (review.clone(), now + 300, "device-review".to_owned(), None)
    } else if let Some(token) = form.get("link") {
        if headers.contains_key(axum::http::header::ORIGIN)
            && !super::http::portal_origin(&engine, &headers)
        {
            return error(ErrorCode::InvalidRequest);
        }
        let (link_token, cookie, csrf, provider) = (
            token.clone(),
            cookie.clone(),
            csrf.clone(),
            provider.clone(),
        );
        let action = match with_store(&engine, move |store| {
            super::account::link_session(store, &link_token, &cookie, &csrf, &provider, now)
        })
        .await
        {
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
        let (flow_transaction, flow_session) = (transaction.clone(), session.clone());
        let (login_cookie, login_csrf) = (cookie.clone(), csrf.clone());
        let found = with_store(&engine, move |store| {
            let authorization = store
                .authorization(&flow_transaction, &flow_session, now)
                .map_err(|_| ErrorCode::InvalidRequest)?;
            let signed_in = store
                .authority
                .portal_login(&login_cookie, &login_csrf, now)
                .is_ok();
            Ok((authorization, signed_in))
        })
        .await;
        let authorization = match found {
            Ok((_, true)) => {
                return Redirect::to(&format!("/consent?transaction={transaction}"))
                    .into_response();
            }
            Ok((authorization, false)) => authorization,
            Err(code) => return error(code),
        };
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
    let selected = provider.clone();
    let configured = match with_store(&engine, move |store| {
        store.authority.upstream_provider(&selected)
    })
    .await
    {
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
        device_review: review,
        session_id: session,
        authorization_transaction: transaction.clone(),
        provider: snapshot.identity,
        nonce: nonce.secret().clone(),
        pkce_verifier: verifier.secret().clone(),
        expires_at: deadline.min(now + 300),
        request_id: super::super::sqlite::id(),
    };
    let state = state.secret().clone();
    if with_store(&engine, move |store| {
        store.store_upstream(&state, &flow, &client_id)
    })
    .await
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
    let authority = engine.server.store().authority.clone();
    let (session_cookie, session_csrf, state) = (cookie.clone(), csrf.clone(), state.clone());
    let flow = match with_store(&engine, move |store| {
        let session = store
            .authority
            .portal_session_identity(&session_cookie, &session_csrf, now)
            .map_err(|_| ErrorCode::AccessDenied)?;
        let flow = store
            .take_upstream(&state, &session, now)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        // Account links and device approvals have no authorization to resume.
        if flow.account_token.is_none()
            && flow.device_review.is_none()
            && store
                .authorization(&flow.authorization_transaction, &session, now)
                .is_err()
        {
            return Err(ErrorCode::InvalidRequest);
        }
        Ok(flow)
    })
    .await
    {
        Ok(flow) => flow,
        Err(code) => return error(code),
    };
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
        if let Some(review) = &flow.device_review {
            return Redirect::to(&format!(
                "/device-enrollment?reviewId={review}&error=upstream_login_failed"
            ))
            .into_response();
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
    let selected = flow.provider.provider_id.clone();
    let configured = match with_store(&engine, move |store| {
        store.authority.upstream_provider(&selected)
    })
    .await
    {
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
        return match with_store(&engine, move |store| {
            super::account::complete_link(store, &flow, &identity, &cookie, &csrf, now)
        })
        .await
        {
            Ok(()) => super::account::html(
                &engine,
                "Company sign-in linked",
                "<p class=\"muted\">You can now sign in with your company account. You can \
                close this window.</p>",
            ),
            Err(_) => error(ErrorCode::AccessDenied),
        };
    }
    let mut replacement = [0; 32];
    if getrandom::fill(&mut replacement).is_err() {
        return error(ErrorCode::ServerError);
    }
    let replacement = URL_SAFE_NO_PAD.encode(replacement);
    let transaction = flow.authorization_transaction.clone();
    let device_review = flow.device_review.clone();
    let provider_id = flow.provider.provider_id.clone();
    let (login_csrf, login_cookie) = (csrf.clone(), replacement.clone());
    match with_store(&engine, move |store| {
        store.authority.portal_upstream_login(
            &flow,
            &identity,
            &cookie,
            &login_csrf,
            &login_cookie,
            now,
        )
    })
    .await
    {
        Ok(login) => cookie_response(
            &replacement,
            Some(&csrf),
            (login.expires_at - now) as u32,
            Redirect::to(&match &device_review {
                Some(review) => format!("/device-enrollment?reviewId={review}"),
                None => format!("/consent?transaction={transaction}"),
            })
            .into_response(),
        ),
        Err(_) => {
            tracing::warn!(%provider_id, "upstream OIDC linked-account login rejected");
            error(ErrorCode::AccessDenied)
        }
    }
}
