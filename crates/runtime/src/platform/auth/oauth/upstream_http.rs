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

/// What an OIDC provider said when it refused a sign-in.
pub(super) struct Refusal {
    provider_id: String,
    error: String,
    description: String,
    /// The provider's own words, rather than Trellis's account of a step that failed.
    from_provider: bool,
    /// The request's reference when Trellis itself failed the sign-in.
    reference: Option<String>,
    expires_at: i64,
}

impl Refusal {
    /// A refusal to show for ten minutes. A `server_error` names the current
    /// request's reference, which finds its log line.
    fn new(
        provider_id: String,
        code: &ErrorCode,
        reason: &str,
        description: &str,
        from_provider: bool,
    ) -> Self {
        // Provider text is shown as plain text; bound it and drop control
        // characters so it stays one readable line.
        let clean = |text: &str, limit: usize| -> String {
            text.chars()
                .filter(|character| !character.is_control())
                .take(limit)
                .collect()
        };
        Self {
            provider_id,
            error: clean(reason, 64),
            description: clean(description, 300),
            from_provider,
            reference: matches!(code, ErrorCode::ServerError)
                .then(|| crate::http_failure::reference("server_error"))
                .flatten(),
            expires_at: crate::clock::unix_now() + 600,
        }
    }
}

/// Keeps `refusal` for the sign-in page of `key` (the browser's Portal session
/// and its transaction or device review), and sends the browser back there.
fn refuse(engine: &OAuthEngine, key: (String, String), refusal: Refusal, page: &str) -> Response {
    if let Ok(mut refusals) = engine.upstream_refusals.lock() {
        let now = crate::clock::unix_now();
        refusals.retain(|_, refusal| refusal.expires_at > now);
        if refusals.len() < 1024 || refusals.contains_key(&key) {
            refusals.insert(key, refusal);
        }
    }
    Redirect::to(page).into_response()
}

/// The sign-in methods for a `transaction` or device-approval `review` (see
/// [`sign_in`]). The sign-in page carries them in the page itself.
pub(super) async fn providers(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let find = |name: &str| {
        pairs
            .iter()
            .find_map(|(key, value)| (key == name).then(|| value.clone()))
    };
    match sign_in(&engine, &headers, find("transaction"), find("review")).await {
        Some(sign_in) => Json(sign_in).into_response(),
        None => error(super::http::failed(
            "could not read the sign-in methods",
            "they could not be read",
        )),
    }
}

/// Ways to sign in, and to create an account for the pending authorization's
/// app when `transaction` names one. For this browser's sign-in on that
/// `transaction` or device `review`, it also returns what an OIDC provider
/// reported when it last refused the sign-in.
pub(super) async fn sign_in(
    engine: &OAuthEngine,
    headers: &HeaderMap,
    transaction: Option<String>,
    review: Option<String>,
) -> Option<serde_json::Value> {
    let page = transaction.clone().or(review);
    let cookies = portal_cookie(headers, "__Host-trellis-portal")
        .zip(portal_cookie(headers, "__Host-trellis-portal-csrf"));
    let result = with_store(engine, move |store| {
        let now = crate::clock::unix_now();
        let session = cookies.and_then(|(cookie, csrf)| {
            store
                .authority
                .portal_session_identity(&cookie, &csrf, now)
                .ok()
        });
        let sign_up = match &transaction {
            Some(transaction) => Some(store.authorization_sign_up(transaction, now)?),
            None => None,
        };
        store.authority.read(|connection| {
            let settings = super::super::sign_in::read(connection)?;
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
            let names = connection
                .prepare("SELECT provider_id,display_name FROM auth_oidc_providers")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<std::collections::HashMap<_, _>, _>>()?;
            Ok((
                json!({
                    "localIdentity": settings.local_users,
                    "passwordMinLength": settings.password_min_length.0,
                    "providers": providers,
                    "signUp": {
                        "password": sign_up.as_ref().is_some_and(|sign_up| sign_up.password),
                        "providers": sign_up.as_ref().is_some_and(|sign_up| sign_up.providers)
                            && !providers.is_empty(),
                    },
                }),
                session,
                names,
            ))
        })
    })
    .await;
    let (mut sign_in, session, names) = result
        .map_err(|cause| crate::http_failure::record("could not read the sign-in methods", cause))
        .ok()?;
    let now = crate::clock::unix_now();
    let refusal = session.zip(page).and_then(|key| {
        let refusals = engine.upstream_refusals.lock().ok()?;
        let refusal = refusals
            .get(&key)
            .filter(|refusal| refusal.expires_at > now)?;
        Some(json!({
            "provider": names.get(&refusal.provider_id),
            "error": refusal.error,
            "description": refusal.description,
            "fromProvider": refusal.from_provider,
            "reference": refusal.reference,
        }))
    });
    if let Some(refusal) = refusal {
        sign_in["refusal"] = refusal;
    }
    Some(sign_in)
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
        Err(cause) => {
            return error(super::http::denied_or_failed(
                "could not read the Portal session",
                cause,
            ))
        }
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
            Err(cause) => {
                return error(super::http::denied_or_failed(
                    "could not read the account link",
                    cause,
                ))
            }
        };
        // Identity links always expire; only the setup reset may not.
        let Some(expires) = action.expires else {
            return error(ErrorCode::AccessDenied);
        };
        (
            action.id,
            expires,
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
    // A server failure from here sends the browser back to the page it signed
    // in from, which names the failure's reference. An account link has no
    // such page.
    let page = match &review {
        Some(review) => format!("/device-enrollment?reviewId={review}&error=upstream_login_failed"),
        None => format!("/login?transaction={transaction}&error=upstream_login_failed"),
    };
    let (refusal_key, refusal_provider) =
        ((session.clone(), transaction.clone()), provider.clone());
    let linking = account_token.is_some();
    let failed = |reason: &str, error: String, cause: String| {
        crate::http_failure::record(error, cause);
        if linking {
            return super::http::error(ErrorCode::ServerError);
        }
        let refusal = Refusal::new(
            refusal_provider.clone(),
            &ErrorCode::ServerError,
            reason,
            "Trellis could not start the sign-in with this provider.",
            false,
        );
        refuse(&engine, refusal_key.clone(), refusal, &page)
    };
    let selected = provider.clone();
    let configured = match with_store(&engine, move |store| {
        store.authority.upstream_provider(&selected)
    })
    .await
    {
        Ok(provider) => provider,
        Err(cause) if !matches!(cause, super::super::AuthError::Denied) => {
            return failed(
                "provider_unreadable",
                format!("could not read OIDC provider {provider}"),
                cause.to_string(),
            )
        }
        Err(cause) => {
            tracing::warn!(
                provider_id = %provider,
                error = %cause,
                "OIDC sign-in started with a provider that is not active",
            );
            return error(ErrorCode::AccessDenied);
        }
    };
    let http = match engine.upstream_http.for_provider(&provider, &configured) {
        Ok(http) => http,
        Err(cause) => {
            return failed(
                "discovery_failed",
                format!("could not trust OIDC provider {provider}'s pinned certificate"),
                cause.to_string(),
            )
        }
    };
    let snapshot = match ProviderSnapshot::discover(
        provider.clone(),
        configured.revision,
        configured.issuer.clone(),
        configured.client_id.clone(),
        &http,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(cause) => {
            return failed(
                "discovery_failed",
                format!(
                    "could not read OIDC provider {provider}'s OpenID configuration from {}",
                    configured.issuer
                ),
                cause.to_string(),
            )
        }
    };
    let secret = match engine
        .server
        .store()
        .upstream_client_secret(&snapshot.identity.provider_id, configured.secret.as_deref())
    {
        Ok(secret) => secret,
        Err(cause) => {
            return failed(
                "client_secret_unreadable",
                format!("could not read OIDC provider {provider}'s client secret"),
                cause.to_string(),
            )
        }
    };
    let client = match snapshot.client(
        secret,
        format!("{}/portal/oidc/callback", authority.settings.oauth_issuer),
    ) {
        Ok(client) => client,
        Err(cause) => {
            return failed(
                "callback_invalid",
                format!(
                    "the OIDC sign-in callback URL {}/portal/oidc/callback is not valid",
                    authority.settings.oauth_issuer
                ),
                cause.to_string(),
            )
        }
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .set_pkce_challenge(challenge);
    // Linking must prove the provider account now. A browser that signed out
    // gets the provider's configured prompt, so its session does not sign the
    // person straight back in, unless this provider already refused it.
    let cookies = || {
        headers
            .get_all(axum::http::header::COOKIE)
            .iter()
            .filter_map(|line| line.to_str().ok())
            .flat_map(|line| line.split(';'))
            .map(str::trim)
    };
    let signed_out = cookies().any(|part| part == "__Host-trellis-portal-reauth=1");
    let refused_here = cookies()
        .filter_map(|part| part.strip_prefix("__Host-trellis-portal-reauth-refused="))
        .flat_map(|value| value.split('.'))
        .any(|refused| refused == snapshot.identity.provider_id);
    let sign_out_prompt = configured
        .sign_out_prompt
        .filter(|_| signed_out && !refused_here && account_token.is_none());
    if account_token.is_some() {
        request = request.add_extra_param("prompt", "login");
    } else if let Some(prompt) = sign_out_prompt {
        request = request.add_extra_param("prompt", prompt);
    }
    // Put refuses the names Auth sets itself.
    for parameter in configured.authorization_parameters {
        request = request.add_extra_param(parameter.name, parameter.value);
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
        sign_out_prompt: sign_out_prompt.map(str::to_owned),
    };
    let state = state.secret().clone();
    if let Err(cause) = with_store(&engine, move |store| {
        store.store_upstream(&state, &flow, &client_id)
    })
    .await
    {
        return failed(
            "sign_in_unsaved",
            "could not save an OIDC sign-in before sending the user to the provider".into(),
            cause.to_string(),
        );
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
            .map_err(|cause| {
                super::http::denied_or_failed("could not read the Portal session", cause)
            })?;
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
    if let Some(issuer) = form
        .get("iss")
        .filter(|issuer| *issuer != &flow.provider.issuer)
    {
        tracing::warn!(
            provider_id = %flow.provider.provider_id,
            expected = %flow.provider.issuer,
            returned = %issuer,
            "OIDC provider returned a different issuer",
        );
        return error(ErrorCode::InvalidRequest);
    }
    // From here on, a failure sends the browser back to the page it signed
    // in from, which shows why. An account link has no such page.
    let refusal_key = (
        flow.session_id.clone(),
        flow.device_review
            .clone()
            .unwrap_or_else(|| flow.authorization_transaction.clone()),
    );
    let return_to = match &flow.device_review {
        Some(review) => format!("/device-enrollment?reviewId={review}&error=upstream_login_failed"),
        None => format!(
            "/login?transaction={}&error=upstream_login_failed",
            flow.authorization_transaction
        ),
    };
    let (refusal_provider, linking) = (
        flow.provider.provider_id.clone(),
        flow.account_token.is_some(),
    );
    let refused = |code: ErrorCode, reason: &str, description: &str, from_provider: bool| {
        if linking {
            return error(code);
        }
        let refusal = Refusal::new(
            refusal_provider.clone(),
            &code,
            reason,
            description,
            from_provider,
        );
        refuse(&engine, refusal_key.clone(), refusal, &return_to)
    };
    if let Some(reason) = form.get("error") {
        // The provider's own refusal, such as a client it doesn't recognize,
        // a redirect URL it wasn't given, or the person cancelling.
        let description = form.get("error_description").map_or("", String::as_str);
        tracing::warn!(
            provider_id = %flow.provider.provider_id,
            error = %reason,
            description,
            "OIDC provider refused the sign-in"
        );
        // A provider that cannot honour the prompt sent after a sign-out would
        // refuse every retry the same way, so this browser stops sending it to
        // that provider and the person learns which setting to change. The
        // sign-out marker stays, so other providers still get their prompt.
        let rejected_prompt = flow.sign_out_prompt.as_deref().filter(|_| {
            matches!(
                reason.as_str(),
                "login_required"
                    | "interaction_required"
                    | "consent_required"
                    | "account_selection_required"
            ) || (reason == "invalid_request" && description.to_lowercase().contains("prompt"))
        });
        let Some(prompt) = rejected_prompt else {
            return refused(ErrorCode::AccessDenied, reason, description, true);
        };
        let explained = format!(
            "it would not accept prompt={prompt}, sent on the first sign-in after a sign-out. \
            An administrator can change the provider's \"Prompt after sign-out\" setting; \
            this browser won't send it again. {}",
            description.chars().take(90).collect::<String>()
        );
        let mut response = refused(ErrorCode::AccessDenied, reason, explained.trim_end(), true);
        // Provider ids are ULIDs, so the list needs no escaping. It only has an
        // effect while the sign-out marker is present.
        let mut providers = headers
            .get_all(axum::http::header::COOKIE)
            .iter()
            .filter_map(|line| line.to_str().ok())
            .flat_map(|line| line.split(';'))
            .filter_map(|part| {
                part.trim()
                    .strip_prefix("__Host-trellis-portal-reauth-refused=")
            })
            .flat_map(|value| value.split('.'))
            .filter(|refused| ulid::Ulid::from_string(refused).is_ok())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !providers.contains(&flow.provider.provider_id) {
            providers.push(flow.provider.provider_id.clone());
        }
        let keep = providers.len().saturating_sub(32);
        if let Ok(cookie) = axum::http::HeaderValue::from_str(&format!(
            "__Host-trellis-portal-reauth-refused={}; Path=/; Secure; HttpOnly; \
            SameSite=Lax; Max-Age=2592000",
            providers[keep..].join(".")
        )) {
            response
                .headers_mut()
                .append(axum::http::header::SET_COOKIE, cookie);
        }
        return response;
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
        Err(cause) => {
            tracing::warn!(
                provider_id = %flow.provider.provider_id,
                error = %cause,
                "OIDC provider stopped being active during the sign-in",
            );
            return refused(
                ErrorCode::AccessDenied,
                "provider_unavailable",
                "The provider was turned off during the sign-in.",
                false,
            );
        }
    };
    if configured.revision != flow.provider.revision
        || configured.issuer != flow.provider.issuer
        || configured.client_id != flow.provider.client_id
    {
        tracing::warn!(
            provider_id = %flow.provider.provider_id,
            "OIDC provider was edited during the sign-in",
        );
        return refused(
            ErrorCode::AccessDenied,
            "provider_changed",
            "The provider's settings changed during the sign-in. Try again.",
            false,
        );
    }
    let secret = match engine
        .server
        .store()
        .upstream_client_secret(&flow.provider.provider_id, configured.secret.as_deref())
    {
        Ok(secret) => secret,
        Err(cause) => {
            crate::http_failure::record(
                format!(
                    "could not read OIDC provider {}'s client secret",
                    flow.provider.provider_id
                ),
                cause,
            );
            return refused(
                ErrorCode::ServerError,
                "client_secret_unreadable",
                "Trellis could not read the provider's client secret.",
                false,
            );
        }
    };
    let http = match engine
        .upstream_http
        .for_provider(&flow.provider.provider_id, &configured)
    {
        Ok(http) => http,
        Err(cause) => {
            crate::http_failure::record(
                format!(
                    "could not trust OIDC provider {}'s pinned certificate",
                    flow.provider.provider_id
                ),
                cause,
            );
            return refused(
                ErrorCode::ServerError,
                "discovery_failed",
                "Trellis could not trust the provider's pinned certificate.",
                false,
            );
        }
    };
    let snapshot = match ProviderSnapshot::discover(
        flow.provider.provider_id.clone(),
        configured.revision,
        configured.issuer.clone(),
        configured.client_id,
        &http,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(cause) => {
            crate::http_failure::record(
                format!(
                    "could not read OIDC provider {}'s OpenID configuration from {}",
                    flow.provider.provider_id, configured.issuer
                ),
                cause.to_string(),
            );
            return refused(
                ErrorCode::ServerError,
                "discovery_failed",
                "Trellis could not read the provider's OpenID configuration. Check that its \
                issuer matches the provider's discovery document exactly.",
                false,
            );
        }
    };
    let client = match snapshot.client(
        secret,
        format!("{}/portal/oidc/callback", authority.settings.oauth_issuer),
    ) {
        Ok(client) => client,
        Err(cause) => {
            crate::http_failure::record(
                format!(
                    "the OIDC sign-in callback URL {}/portal/oidc/callback is not valid",
                    authority.settings.oauth_issuer
                ),
                cause,
            );
            return refused(
                ErrorCode::ServerError,
                "callback_invalid",
                "This Trellis's sign-in callback URL is not valid.",
                false,
            );
        }
    };
    let exchange = match client.exchange_code(AuthorizationCode::new(code.clone())) {
        Ok(exchange) => exchange,
        Err(cause) => {
            crate::http_failure::record(
                format!(
                    "OIDC provider {} has no usable token endpoint",
                    flow.provider.provider_id
                ),
                cause,
            );
            return refused(
                ErrorCode::ServerError,
                "token_endpoint_missing",
                "The provider's OpenID configuration has no usable token endpoint.",
                false,
            );
        }
    };
    let tokens = match exchange
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier.clone()))
        .request_async(&http)
        .await
    {
        Ok(tokens) => tokens,
        Err(cause) => {
            return match &cause {
                // The provider's own refusal. A wrong client secret shows here
                // as `invalid_client`.
                openidconnect::RequestTokenError::ServerResponse(response) => {
                    tracing::warn!(
                        provider_id = %flow.provider.provider_id,
                        error = %super::oidc::error_chain(&cause),
                        "upstream OIDC token exchange rejected",
                    );
                    refused(
                        ErrorCode::AccessDenied,
                        response.error().as_ref(),
                        response
                            .error_description()
                            .map_or("The provider would not exchange the sign-in code.", |d| d),
                        true,
                    )
                }
                // No answer, or one that isn't a token response: the exchange
                // failed, and the request's log line says how.
                _ => {
                    crate::http_failure::record(
                        format!(
                            "could not exchange the sign-in code with OIDC provider {}",
                            flow.provider.provider_id
                        ),
                        super::oidc::error_chain(&cause),
                    );
                    refused(
                        ErrorCode::ServerError,
                        "token_exchange_failed",
                        "Trellis could not exchange the sign-in code with the provider.",
                        false,
                    )
                }
            };
        }
    };
    let mut identity = match snapshot.verify(&client, &tokens, &flow.nonce) {
        Ok(identity) => identity,
        Err(_) => {
            tracing::warn!(
                provider_id=%flow.provider.provider_id,
                "upstream OIDC ID token verification rejected",
            );
            return refused(
                ErrorCode::AccessDenied,
                "id_token_rejected",
                "The provider's ID token did not verify: its signature, audience, nonce or \
                expiry was wrong.",
                false,
            );
        }
    };
    snapshot.enrich(&mut identity, &tokens, &http).await;
    let now = crate::clock::unix_now();
    if flow.account_token.is_some() {
        return match with_store(&engine, move |store| {
            super::account::complete_link(store, &flow, &identity, &cookie, &csrf, now)
        })
        .await
        {
            Ok(()) => super::account::html(
                &engine,
                "OIDC sign-in linked",
                "<p class=\"muted\">You can now sign in with your OIDC account. You can \
                close this window.</p>",
            ),
            Err(cause) => error(super::http::denied_or_failed(
                "could not link the OIDC sign-in",
                cause,
            )),
        };
    }
    let mut replacement = [0; 32];
    if let Err(cause) = getrandom::fill(&mut replacement) {
        return error(super::http::failed(
            "could not make a Portal login cookie",
            cause,
        ));
    }
    let replacement = URL_SAFE_NO_PAD.encode(replacement);
    let transaction = flow.authorization_transaction.clone();
    let device_review = flow.device_review.clone();
    let provider_id = flow.provider.provider_id.clone();
    let (login_csrf, login_cookie) = (csrf.clone(), replacement.clone());
    match with_store(&engine, move |store| {
        // Device enrollment has no app whose sign-up settings could apply.
        let sign_up = flow.device_review.is_none()
            && store
                .authorization_sign_up(&flow.authorization_transaction, now)?
                .providers;
        store.authority.portal_upstream_login(
            &flow,
            &identity,
            &cookie,
            &login_csrf,
            &login_cookie,
            sign_up,
            now,
        )
    })
    .await
    {
        Ok(login) => {
            let mut response = cookie_response(
                &replacement,
                Some(&csrf),
                (login.expires_at - now) as u32,
                Redirect::to(&match &device_review {
                    Some(review) => format!("/device-enrollment?reviewId={review}"),
                    None => format!("/consent?transaction={transaction}"),
                })
                .into_response(),
            );
            // The provider has just signed the person in, so later sign-ins
            // may reuse its session again.
            for cleared in [
                "__Host-trellis-portal-reauth=; Path=/; Secure; HttpOnly; SameSite=Lax; \
                Max-Age=0",
                "__Host-trellis-portal-reauth-refused=; Path=/; Secure; HttpOnly; \
                SameSite=Lax; Max-Age=0",
            ] {
                response.headers_mut().append(
                    axum::http::header::SET_COOKIE,
                    axum::http::HeaderValue::from_static(cleared),
                );
            }
            response
        }
        Err(cause) => {
            tracing::warn!(
                %provider_id,
                error = %cause,
                "upstream OIDC linked-account login rejected",
            );
            match cause {
                super::super::AuthError::NotFound(_) => refused(
                    ErrorCode::AccessDenied,
                    "no_linked_account",
                    "No Trellis account is linked to this provider account. Sign in another \
                    way, then link the provider from your account page.",
                    false,
                ),
                super::super::AuthError::Conflict(_) => refused(
                    ErrorCode::AccessDenied,
                    "provider_changed",
                    "The provider or OIDC sign-in was changed or turned off during the \
                    sign-in. Try again.",
                    false,
                ),
                _ => refused(
                    ErrorCode::AccessDenied,
                    "sign_in_expired",
                    "This sign-in expired or was already finished. Start again.",
                    false,
                ),
            }
        }
    }
}
