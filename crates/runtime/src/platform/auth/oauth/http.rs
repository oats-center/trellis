//! Bounded HTTP adapter for the embedded engine. OAuth errors expose only fixed
//! machine codes; tokens and admission material are never placed in logs.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::super::sqlite::blocking;
use super::OAuthEngine;
use axum::middleware::{self, Next};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, RawQuery, Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
};
use oauth_as::{
    authorization::{AuthorizationError, AuthorizationRequest},
    client::ClientId,
    error::{ErrorCode, ErrorResponse},
    server::TokenRequest,
    store::Storage,
};
use rusqlite::OptionalExtension;
use serde_json::Value;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub(crate) fn router(engine: Arc<OAuthEngine>, nats_endpoints: Vec<String>) -> Router {
    let origin = engine.server.config().issuer.clone();
    let cors_engine = engine.clone();
    // Single platform owner: bounded fixed-window request admission, not a
    // policy ceiling. Untrusted forwarding headers never select the peer IP.
    let requests = Arc::new(Mutex::new(BTreeMap::<
        (Option<std::net::IpAddr>, String),
        (Instant, u32),
    >::new()));
    let maintenance = Arc::new(Mutex::new(Instant::now()));
    // Image fan-out cannot spend the login, consent or token request budget.
    let image_requests = Arc::new(Mutex::new(BTreeMap::new()));
    let maintenance_engine = engine.clone();
    let metadata_engine = engine.clone();
    // Discovery must work before a previously unknown client is prepared.
    // These read-only documents contain no cookie-authenticated private state.
    let metadata = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(move || {
                let engine = metadata_engine.clone();
                let endpoints = nats_endpoints.clone();
                async move {
                    let metadata = blocking(move || {
                        let trust = engine
                            .server
                            .store()
                            .authority
                            .runtime_authorization_trust()?;
                        let mut metadata = serde_json::to_value(engine.server.metadata())?;
                        let document = metadata
                            .as_object_mut()
                            .ok_or(super::super::AuthError::Unavailable)?;
                        document.remove("introspection_endpoint");
                        // OpenID RP-initiated logout: an app sends the browser here
                        // to end the Portal login, not just its own grant.
                        document.insert(
                            "end_session_endpoint".into(),
                            format!(
                                "{}/logout",
                                engine.server.store().authority.settings.oauth_origin
                            )
                            .into(),
                        );
                        document.insert(
                            "trellis".into(),
                            serde_json::json!({
                                "trellisInstanceId": trust.instance_id,
                                "audienceNatsAccount": trust.account,
                                "natsEndpoints": endpoints,
                                "authorityFormat": trellis_protocol::SESSION_AUTHORITY_FORMAT_V1,
                                "admissionFormat": trellis_protocol::OAUTH_ADMISSION_FORMAT_V1,
                                "issuers": [trust.issuer],
                                "rotationSequence": trellis_protocol::U64s::new(
                                        trust.rotation_sequence,
                                    ),
                                "requiredAuthorizationDetailsTypes": ["trellis_capabilities"],
                            }),
                        );
                        Ok::<_, super::super::AuthError>(metadata)
                    })
                    .await;
                    match metadata {
                        Ok(metadata) => Json(metadata).into_response(),
                        _ => error(ErrorCode::ServerError),
                    }
                }
            }),
        )
        .route("/.well-known/trellis/issuer-chain", get(issuer_chain))
        .layer(
            CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods([axum::http::Method::GET]),
        );
    // Proof-bearing endpoints remain client-origin-bound. Cookie-authenticated
    // portal routes deliberately do not enable CORS.
    let public = Router::new()
        .route("/token", post(token))
        .route("/device_authorization", post(super::device::start))
        .route("/auth/device/enroll", post(super::enrollment::http_enroll))
        .route("/revoke", post(revoke))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(move |origin, _| {
                    origin.to_str().is_ok_and(|origin| {
                        cors_engine
                            .server
                            .store()
                            .authority
                            .allows_client_origin(origin)
                    })
                }))
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers([header::CONTENT_TYPE, HeaderName::from_static("dpop")])
                .expose_headers([HeaderName::from_static("dpop-nonce")]),
        );
    Router::new()
        .merge(metadata)
        .merge(public)
        .route("/profile/avatars/{token}", get(avatar))
        .route("/authorize", get(authorize))
        .route("/device", get(super::device::page))
        .route(
            "/device/complete",
            get(|State(engine): State<Arc<OAuthEngine>>| async move {
                super::assets::page(&engine, None)
            }),
        )
        .route("/portal/device", post(super::device::begin))
        .route("/device-enrollment", get(super::device::page))
        .route("/device-enrollment/complete", get(super::device::page))
        .route(
            "/portal/device-review/inspect",
            post(super::enrollment::portal_inspect),
        )
        .route(
            "/portal/device-review/decide",
            post(super::enrollment::portal_decide),
        )
        .route(
            "/portal/device-review/login",
            post(super::enrollment::portal_login),
        )
        .route("/login", get(login_page))
        .route("/logout", get(logout_page))
        .route("/consent", get(consent_page))
        .route(
            "/portal-assets/{*path}",
            get(|Path(path): Path<String>| async move { super::assets::asset(&path) }),
        )
        .route("/portal/theme", get(super::theme::view))
        .route("/portal-theme/logo", get(super::theme::logo))
        .route(
            "/portal-theme/stylesheet.css",
            get(super::theme::stylesheet),
        )
        .route("/portal/approve", post(approve))
        .route("/portal/authorization/{transaction}", get(consent))
        .route("/portal/session", post(portal_session))
        .route("/portal/login", post(portal_login))
        .route("/portal/sign-up", post(portal_sign_up))
        .route("/account/{kind}", get(super::account::page))
        .route("/portal/account/{kind}", post(super::account::submit))
        .route("/portal/providers", get(super::upstream_http::providers))
        .route(
            "/portal/oidc/{provider}/begin",
            get(super::upstream_http::start),
        )
        .route("/portal/oidc/callback", get(super::upstream_http::callback))
        .route("/portal/logout", post(portal_logout))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let origin = origin.clone();
            let requests = if request.uri().path().starts_with("/profile/avatars/") {
                image_requests.clone()
            } else {
                requests.clone()
            };
            let maintenance = maintenance.clone();
            let maintenance_engine = maintenance_engine.clone();
            async move {
                // HTTP/2 supplies :authority through the URI rather than Host.
                // If both forms are supplied, every one must select this origin.
                let accepted = {
                    let mut authorities = request
                        .headers()
                        .get_all(header::HOST)
                        .iter()
                        .map(|host| host.to_str().ok())
                        .chain(
                            request
                                .uri()
                                .authority()
                                .map(|authority| Some(authority.as_str())),
                        )
                        .peekable();
                    request.headers().get_all(header::HOST).iter().count() <= 1
                        && authorities.peek().is_some()
                        && url::Url::parse(&origin).is_ok_and(|selected| {
                            authorities.all(|host| {
                                host.is_some_and(|host| {
                                    !host.contains('@')
                                        && host.parse::<axum::http::uri::Authority>().is_ok()
                                        && url::Url::parse(&format!(
                                            "{}://{host}",
                                            selected.scheme()
                                        ))
                                        .is_ok_and(|actual| actual.origin() == selected.origin())
                                })
                            })
                        })
                };
                if !accepted {
                    return error(ErrorCode::InvalidRequest);
                }
                let request_origin = match request.headers().get(header::ORIGIN) {
                    Some(value) => match value.to_str() {
                        Ok(value) if value.len() <= 2048 => value.to_owned(),
                        _ => return error(ErrorCode::InvalidRequest),
                    },
                    None => String::new(),
                };
                // The listener already replaced a trusted proxy's peer address
                // with the client it forwarded.
                let ip = request
                    .extensions()
                    .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                    .map(|peer| peer.0.ip());
                let now = Instant::now();
                let allowed = {
                    let Ok(mut requests) = requests.lock() else {
                        return error(ErrorCode::ServerError);
                    };
                    let expired =
                        |started: &Instant| now.duration_since(*started) >= Duration::from_secs(60);
                    // Expired windows restart when next used; the table is only
                    // swept when it is full, keeping requests off an O(n) scan.
                    if requests.len() >= 4094 {
                        requests.retain(|_, (started, _)| !expired(started));
                    }
                    let peer = (ip, String::new());
                    // Navigations and native clients send no Origin. They are
                    // budgeted per peer only, never in one shared origin bucket.
                    let origin = (!request_origin.is_empty()).then_some((None, request_origin));
                    if requests.len() >= 4094
                        && (!requests.contains_key(&peer)
                            || origin
                                .as_ref()
                                .is_some_and(|origin| !requests.contains_key(origin)))
                    {
                        false
                    } else {
                        let mut count = |key| {
                            let window = requests.entry(key).or_insert((now, 0));
                            if expired(&window.0) {
                                *window = (now, 0);
                            }
                            window.1 = window.1.saturating_add(1);
                            window.1
                        };
                        let peer_allowed = count(peer) <= 120;
                        match origin {
                            Some(origin) => {
                                let count = count(origin);
                                peer_allowed && count <= 600
                            }
                            None => peer_allowed,
                        }
                    }
                };
                if !allowed {
                    let mut response = error(ErrorCode::TemporarilyUnavailable);
                    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
                    response
                        .headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
                    return response;
                }
                let cleanup_due = {
                    let Ok(mut last) = maintenance.lock() else {
                        return error(ErrorCode::ServerError);
                    };
                    if now.duration_since(*last) >= Duration::from_secs(60) {
                        *last = now;
                        true
                    } else {
                        false
                    }
                };
                if cleanup_due {
                    let store = maintenance_engine.server.store().clone();
                    let result =
                        blocking(move || store.cleanup_expired(crate::clock::unix_now())).await;
                    if result.is_err() {
                        tracing::warn!(
                            security_event = "oauth_cleanup_unavailable",
                            "OAuth expiry cleanup failed"
                        );
                    }
                }
                next.run(request).await
            }
        }))
        .with_state(engine)
}

async fn avatar(State(engine): State<Arc<OAuthEngine>>, Path(token): Path<String>) -> Response {
    let now = crate::clock::unix_now();
    let mut response = match engine.avatars.get(&token, now).await {
        // A new photo gets a new URL; the hour bounds how long a browser keeps
        // showing a disabled account's photo.
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "private, max-age=3600"),
            ],
            bytes,
        )
            .into_response(),
        _ => ([(header::CACHE_CONTROL, "no-store")], StatusCode::NOT_FOUND).into_response(),
    };
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("cross-origin"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

async fn issuer_chain(
    State(engine): State<Arc<OAuthEngine>>,
    query: Result<
        Query<trellis_protocol::IssuerChainRequest>,
        axum::extract::rejection::QueryRejection,
    >,
) -> Response {
    let Ok(Query(input)) = query else {
        return error(ErrorCode::InvalidRequest);
    };
    if input.pinned_key_id.len() > 128 || input.target_key_id.len() > 128 {
        return error(ErrorCode::InvalidRequest);
    }
    let result = blocking(move || {
        let store = &engine.server.store().authority;
        store.transaction(|connection| {
            // A compromised pin cannot authenticate a new trust root. Recovery
            // requires an explicitly selected new anchor, not an unsigned key.
            for key in [&input.pinned_key_id, &input.target_key_id] {
                if super::super::SqliteAuthorizationStore::resolve_issuer_in(connection, key)?.state
                    == trellis_protocol::AuthorityIssuerState::Revoked
                {
                    return Err(super::super::AuthError::Denied);
                }
            }
            let chain = super::super::rpc_credentials::read_chain(
                connection,
                &input.pinned_key_id,
                &input.target_key_id,
            )?;
            for signed in &chain.rotations {
                let rotation: trellis_protocol::SignedIssuerRotation =
                    serde_json::from_str(signed)?;
                if super::super::SqliteAuthorizationStore::resolve_issuer_in(
                    connection,
                    &rotation.next_key_id,
                )?
                .state
                    == trellis_protocol::AuthorityIssuerState::Revoked
                {
                    return Err(super::super::AuthError::Denied);
                }
            }
            Ok(chain)
        })
    })
    .await;
    match result {
        Ok(chain) => {
            let mut response = Json(chain).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        _ => error(ErrorCode::InvalidRequest),
    }
}

pub(super) fn portal_origin(engine: &OAuthEngine, headers: &HeaderMap) -> bool {
    headers.get_all(header::ORIGIN).iter().count() == 1
        && headers[header::ORIGIN]
            .to_str()
            .is_ok_and(|origin| origin == engine.server.store().authority.settings.oauth_origin)
}

async fn authorize(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
    query: Result<Query<Vec<(String, String)>>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if raw.as_ref().is_none_or(|raw| raw.len() > 32 * 1024) {
        return error(ErrorCode::InvalidRequest);
    }
    let Ok(Query(pairs)) = query else {
        return error(ErrorCode::InvalidRequest);
    };
    let request = AuthorizationRequest::from_pairs(
        pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    let Some(client) = request.client_id.as_deref() else {
        return error(ErrorCode::InvalidClient);
    };
    if pairs.iter().filter(|(key, _)| key == "client_id").count() != 1 {
        return error(ErrorCode::InvalidRequest);
    }
    if let Err(response) = engine.prepare_client(&ClientId::new(client)).await {
        return error(response.error);
    }
    let validated = match engine.server.validate_authorization_request(&request).await {
        Ok(request) => request,
        Err(response) => return authorization_error(response),
    };
    let Some(details) = request.authorization_details.as_deref() else {
        return error(ErrorCode::InvalidAuthorizationDetails);
    };
    if super::RequestedAuthority::parse(details).is_err() {
        return error(ErrorCode::InvalidRequest);
    }
    let mut keys = pairs.iter().filter(|(key, _)| key == "trellis_dpop_jwk");
    let Some((_, key)) = keys.next() else {
        return error(ErrorCode::InvalidRequest);
    };
    if keys.next().is_some() {
        return error(ErrorCode::InvalidRequest);
    }
    let public_jwk: Value = match serde_json::from_str(key) {
        Ok(key) => key,
        Err(_) => return error(ErrorCode::InvalidRequest),
    };
    let key = match oauth_as::jwt::Jwk::from_json(&public_jwk) {
        Ok(key) => key,
        Err(_) => return error(ErrorCode::InvalidRequest),
    };
    if public_jwk.get("d").is_some()
        || public_jwk.get("kty").and_then(Value::as_str) != Some("EC")
        || public_jwk.get("crv").and_then(Value::as_str) != Some("P-256")
        || validated.dpop_jkt.as_deref() != Some(key.thumbprint().as_str())
    {
        return error(ErrorCode::InvalidRequest);
    }
    let now = crate::clock::unix_now();
    let store = engine.server.store().clone();
    let cookies = portal_cookie(&headers, "__Host-trellis-portal")
        .zip(portal_cookie(&headers, "__Host-trellis-portal-csrf"));
    let client_id = validated.client_id.as_str().to_owned();
    let transaction = super::super::sqlite::id();
    let flow_transaction = transaction.clone();
    let started = blocking(move || {
        let authority = &store.authority;
        let authenticated =
            cookies.and_then(|(cookie, csrf)| authority.portal_login(&cookie, &csrf, now).ok());
        let guest = if authenticated.is_none() {
            Some(
                authority
                    .create_portal_session(now)
                    .map_err(|_| ErrorCode::ServerError)?,
            )
        } else {
            None
        };
        let session_id = match (&authenticated, &guest) {
            (Some(login), _) => login.session_id.clone(),
            (_, Some(guest)) => guest.session_id.clone(),
            _ => return Err(ErrorCode::ServerError),
        };
        let flow = super::portal::PortalAuthorization {
            presentation: None,
            session_id,
            request: super::portal::ConsentRequest::Authorization(pairs),
            public_jwk,
            expires_at: now + 600,
        };
        store
            .store_authorization(&flow_transaction, &flow, &client_id)
            .map_err(|_| ErrorCode::ServerError)?;
        Ok((authenticated, guest))
    })
    .await;
    let (authenticated, guest) = match started {
        Ok(started) => started,
        Err(code) => return error(code),
    };
    let page = if authenticated.is_some() {
        "consent"
    } else {
        "login"
    };
    let mut response = Redirect::to(&format!("/{page}?transaction={transaction}")).into_response();
    if let Some(guest) = guest {
        response.headers_mut().extend(
            cookie_response(
                &guest.cookie,
                Some(&guest.csrf),
                600,
                StatusCode::NO_CONTENT.into_response(),
            )
            .headers()
            .clone(),
        );
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

async fn login_page(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let Some(transaction) = form
        .get("transaction")
        .filter(|id| ulid::Ulid::from_string(id).is_ok())
    else {
        return error(ErrorCode::InvalidRequest);
    };
    let Some(cookie) = portal_cookie(&headers, "__Host-trellis-portal") else {
        return error(ErrorCode::AccessDenied);
    };
    let Some(csrf) = portal_cookie(&headers, "__Host-trellis-portal-csrf") else {
        return error(ErrorCode::AccessDenied);
    };
    let now = crate::clock::unix_now();
    let transaction = transaction.clone();
    let pending = super::with_store(&engine, move |store| {
        let session = store
            .authority
            .portal_session_identity(&cookie, &csrf, now)
            .map_err(|_| ErrorCode::AccessDenied)?;
        store
            .authorization(&transaction, &session, now)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        Ok::<_, ErrorCode>(())
    })
    .await;
    match pending {
        Ok(()) => super::assets::page(&engine, None),
        Err(code) => error(code),
    }
}

/// Serves the Portal's sign-out page for an app that wants the Portal login
/// ended too. The page returns only to an origin the app registered a redirect
/// on, so it never becomes an open redirector.
async fn logout_page(
    State(engine): State<Arc<OAuthEngine>>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let (Some(client), Some(target)) =
        (form.get("client_id"), form.get("post_logout_redirect_uri"))
    else {
        return error(ErrorCode::InvalidRequest);
    };
    let Ok(target) = url::Url::parse(target) else {
        return error(ErrorCode::InvalidRequest);
    };
    let client = ClientId::new(client);
    if let Err(response) = engine.prepare_client(&client).await {
        return error(response.error);
    }
    let Ok(Some(client)) = engine.server.store().get_client(&client).await else {
        return error(ErrorCode::InvalidClient);
    };
    let origin = target.origin();
    if !origin.is_tuple()
        || !client.redirect_uris.iter().any(|redirect| {
            url::Url::parse(redirect).is_ok_and(|redirect| redirect.origin() == origin)
        })
    {
        return error(ErrorCode::InvalidRequest);
    }
    super::assets::page(&engine, None)
}

async fn consent_page(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let Some(transaction) = form
        .get("transaction")
        .filter(|id| ulid::Ulid::from_string(id).is_ok())
    else {
        return error(ErrorCode::InvalidRequest);
    };
    let view = match consent_data(&engine, &headers, transaction).await {
        Ok(view) => view,
        Err(code) => return error(code),
    };
    super::assets::page(&engine, view.get("redirectUri").and_then(Value::as_str))
}

async fn consent(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Path(transaction): Path<String>,
) -> Response {
    match consent_data(&engine, &headers, &transaction).await {
        Ok(body) => {
            let mut response = Json(body).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(code) => error(code),
    }
}

/// Authenticate the portal login and load its pending authorization flow.
async fn login_and_flow(
    engine: &OAuthEngine,
    cookie: String,
    csrf: String,
    transaction: &str,
    now: i64,
) -> Result<
    (
        super::portal::PortalLogin,
        super::portal::PortalAuthorization,
    ),
    ErrorCode,
> {
    let store = engine.server.store().clone();
    let transaction = transaction.to_owned();
    blocking(move || {
        let login = store
            .authority
            .portal_login(&cookie, &csrf, now)
            .map_err(|_| ErrorCode::AccessDenied)?;
        let flow = store
            .authorization(&transaction, &login.session_id, now)
            .map_err(|_| ErrorCode::InvalidRequest)?;
        Ok((login, flow))
    })
    .await
}

async fn consent_data(
    engine: &OAuthEngine,
    headers: &HeaderMap,
    transaction: &str,
) -> Result<Value, ErrorCode> {
    let Some(cookie) = portal_cookie(headers, "__Host-trellis-portal") else {
        return Err(ErrorCode::AccessDenied);
    };
    let Some(csrf) = portal_cookie(headers, "__Host-trellis-portal-csrf") else {
        return Err(ErrorCode::AccessDenied);
    };
    let now = crate::clock::unix_now();
    let (login, flow) = login_and_flow(engine, cookie, csrf, transaction, now).await?;
    let store = engine.server.store().clone();
    let transaction = transaction.to_owned();
    let validated = match engine.consent_intent(&flow.request).await {
        Ok(request) => request,
        Err(response) => return Err(response.error),
    };
    let requested = match super::RequestedAuthority::parse(&validated.authorization_details) {
        Ok(details) => details,
        Err(response) => return Err(response.error),
    };
    let client = match engine.server.store().get_client(&validated.client_id).await {
        Ok(Some(client)) => client,
        _ => return Err(ErrorCode::InvalidClient),
    };
    let result = blocking(move || {
        store.authority.transaction(|connection| {
            let entitled = super::super::policy::entitlements(
                connection,
                &login.principal_id,
                Some(&login.login_session_id),
                now,
            )?;
            let registered: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_oauth_clients \
                WHERE client_id=?1 AND state='active')",
                [validated.client_id.as_str()],
                |row| row.get(0),
            )?;
            let administration = store.authority.settings.administration;
            let kind: String = connection.query_row(
                "SELECT kind FROM auth_oauth_clients \
            WHERE client_id=?1 AND state='active' \
            UNION ALL SELECT kind \
            FROM auth_cimd_clients \
            WHERE client_id=?1 AND NOT EXISTS(SELECT 1 \
            FROM auth_oauth_clients \
            WHERE client_id=?1)",
                [validated.client_id.as_str()],
                |row| row.get(0),
            )?;
            let binding = if kind == "native" {
                validated.dpop_jkt.clone()
            } else {
                url::Url::parse(
                    validated
                        .redirect_uri
                        .as_deref()
                        .ok_or(super::super::AuthError::Denied)?,
                )
                .map_err(|_| super::super::AuthError::Denied)?
                .origin()
                .ascii_serialization()
            };
            let username: Option<String> = connection
                .query_row(
                    "SELECT normalized_username \
                FROM auth_local_credentials \
                WHERE principal_id=?1",
                    [&login.principal_id],
                    |row| row.get(0),
                )
                .optional()?;
            let mut capabilities = Vec::new();
            let mut meanings = std::collections::BTreeMap::new();
            let mut api_generations = std::collections::BTreeMap::new();
            for capability in requested
                .capabilities
                .required
                .iter()
                .chain(&requested.capabilities.optional)
            {
                let meaning = match super::super::policy::capability(connection, capability) {
                    Ok(value) => Some(value),
                    Err(super::super::AuthError::NotFound) => None,
                    Err(error) => return Err(error),
                };
                meanings.insert(capability.clone(), meaning);
                let api: Option<(String, u64)> = connection
                    .query_row(
                        "SELECT a.api_id,a.generation \
                    FROM auth_capability_identities c \
                    JOIN auth_accepted_apis a ON a.api_id=c.api_id \
                    WHERE c.capability_id=?1 \
                    AND c.state='active'",
                        [capability],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((api, generation)) = api {
                    api_generations.insert(api, generation);
                }
                let metadata: Option<(String, String, String)> = connection
                    .query_row(
                        "SELECT title,description,grants \
                        FROM auth_capability_identities \
                        WHERE capability_id=?1 AND state='active'",
                        [capability],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                let (title, description, grants) = metadata.unwrap_or_else(|| {
                    (
                        capability.clone(),
                        "This capability is not installed.".into(),
                        String::new(),
                    )
                });
                // A group is shown with the capabilities it stands for.
                let members = super::super::policy::current_members(connection, capability)?
                    .map(|members| {
                        members
                            .iter()
                            .map(|member| {
                                let title: String = connection.query_row(
                                    "SELECT title FROM auth_capability_identities \
                                    WHERE capability_id=?1 AND state='active'",
                                    [member],
                                    |row| row.get(0),
                                )?;
                                Ok(serde_json::json!({"capabilityId": member, "title": title}))
                            })
                            .collect::<Result<Vec<_>, super::super::AuthError>>()
                    })
                    .transpose()?;
                let remembered: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 \
                FROM auth_remembered_consent d \
                JOIN auth_capability_identities c ON c.capability_id=d.capability_id \
                AND c.identity_generation=d.capability_generation \
                AND c.consent_revision=d.consent_revision \
                WHERE d.principal_id=?1 \
                AND d.binding_kind=?2 \
                AND d.binding_value=?3 \
                AND d.client_id=?4 AND d.capability_id=?5 \
                AND d.decision='approved' \
                AND c.state='active' \
                AND (d.expires_at IS NULL \
                OR d.expires_at>?6))",
                    rusqlite::params![
                        login.principal_id,
                        kind,
                        binding,
                        validated.client_id.as_str(),
                        capability,
                        now
                    ],
                    |row| row.get(0),
                )?;
                let administrative =
                    super::super::administration::administrative(connection, capability)?;
                let required = requested.capabilities.required.contains(capability);
                // An unregistered app never receives administration.
                let available =
                    super::super::policy::covers(connection, &entitled, capability, required)?
                        && (!administrative
                            || registered
                            || !administration.registered_clients_only);
                capabilities.push(serde_json::json!({
                    "capabilityId":capability,
                    "title":title,
                    "description":description,
                    "grants":grants,
                    "kind": if members.is_some() { "group" } else { "capability" },
                    "members":members,
                    "administrative":administrative,
                    "required":required,
                    "available":available,
                    "selected": requested.capabilities.required.contains(capability)
                            || (remembered && available)
                }));
            }
            let presentation_id = super::super::sqlite::id();
            let presentation = super::portal::ConsentPresentation {
                id: presentation_id.clone(),
                principal_id: login.principal_id.clone(),
                login_session_id: login.login_session_id.clone(),
                client_id: validated.client_id.as_str().into(),
                binding_kind: kind,
                binding_value: binding,
                selection: requested.capabilities.clone(),
                capabilities: meanings,
                api_generations,
                lookup_hash: String::new(),
                sealed_digest: String::new(),
            };
            store
                .present_authorization(connection, &transaction, &flow, presentation, now)
                .map_err(|_| super::super::AuthError::Unavailable)?;
            Ok(serde_json::json!({
                "transaction":transaction,
                "presentationId":presentation_id,
                "clientId":validated.client_id.as_str(),
                "clientName":client.name,
                "registered":registered,
                "username":username,
                "principalId":login.principal_id,
                "redirectUri":validated.redirect_uri,
                "deviceUserCode":validated.device_user_code,
                "capabilities":capabilities,
                "confirmAdministration":administration.confirm_on_consent,
                "peerApproval":administration.require_peer_approval
            }))
        })
    })
    .await;
    result.map_err(|_| ErrorCode::ServerError)
}

async fn approve(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if !portal_origin(&engine, &headers) {
        return error(ErrorCode::InvalidRequest);
    }
    let Some(cookie) = portal_cookie(&headers, "__Host-trellis-portal") else {
        return error(ErrorCode::AccessDenied);
    };
    let mut approved = Vec::new();
    let mut controls = Vec::new();
    for (name, value) in pairs {
        match name.as_str() {
            "approved" => {
                if approved.contains(&value) {
                    return error(ErrorCode::InvalidRequest);
                }
                approved.push(value);
            }
            _ => controls.push((name, value)),
        }
    }
    let form = match fields(controls) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let (Some(csrf), Some(transaction), Some(decision)) = (
        form.get("csrf"),
        form.get("transaction"),
        form.get("decision"),
    ) else {
        return error(ErrorCode::InvalidRequest);
    };
    if !["approve", "deny"].contains(&decision.as_str())
        || ulid::Ulid::from_string(transaction).is_err()
    {
        return error(ErrorCode::InvalidRequest);
    }
    let now = crate::clock::unix_now();
    let (login, flow) =
        match login_and_flow(&engine, cookie.clone(), csrf.clone(), transaction, now).await {
            Ok(found) => found,
            Err(code) => return error(code),
        };
    let validated = match engine.consent_intent(&flow.request).await {
        Ok(request) => request,
        Err(response) => return error(response.error),
    };
    if decision == "deny" {
        let store = engine.server.store().clone();
        let (transaction, session) = (transaction.clone(), login.session_id.clone());
        if blocking(move || store.take_authorization(&transaction, &session, now))
            .await
            .is_err()
        {
            return error(ErrorCode::InvalidRequest);
        }
        return match &flow.request {
            super::portal::ConsentRequest::Authorization(pairs) => {
                let request = AuthorizationRequest::from_pairs(
                    pairs
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                );
                match engine.server.validate_authorization_request(&request).await {
                    Ok(validated) => Redirect::to(&validated.denied().location()).into_response(),
                    Err(response) => authorization_error(response),
                }
            }
            super::portal::ConsentRequest::Device { device_code } => {
                let device = match engine.server.store().get_device_grant(device_code).await {
                    Ok(Some(device)) => device,
                    _ => return error(ErrorCode::InvalidRequest),
                };
                match engine.server.deny_device(&device.user_code).await {
                    Ok(()) => Redirect::to("/device/complete?decision=denied").into_response(),
                    Err(_) => error(ErrorCode::InvalidRequest),
                }
            }
        };
    }
    let Some(presentation) = flow
        .presentation
        .as_ref()
        .filter(|presentation| form.get("presentation") == Some(&presentation.id))
    else {
        return Redirect::to(&format!(
            "/consent?transaction={transaction}&error=consent_changed"
        ))
        .into_response();
    };
    let requested = match super::RequestedAuthority::parse(&validated.authorization_details) {
        Ok(request) => request,
        Err(_) => return error(ErrorCode::InvalidRequest),
    };
    let client_kind = {
        let store = engine.server.store().clone();
        let client = validated.client_id.as_str().to_owned();
        let kind = blocking(move || {
            store.authority.read(|connection| {
                Ok(connection.query_row(
                    "SELECT kind FROM auth_oauth_clients \
                    WHERE client_id=?1 AND state='active' \
                    UNION ALL SELECT kind \
                    FROM auth_cimd_clients \
                    WHERE client_id=?1 AND NOT EXISTS(SELECT 1 \
                    FROM auth_oauth_clients \
                    WHERE client_id=?1)",
                    [&client],
                    |row| row.get::<_, String>(0),
                )?)
            })
        })
        .await;
        match kind {
            Ok(kind) => kind,
            Err(_) => return error(ErrorCode::UnauthorizedClient),
        }
    };
    let jkt = validated.dpop_jkt;
    let binding = if client_kind == "browser" {
        match validated
            .redirect_uri
            .as_deref()
            .and_then(|uri| url::Url::parse(uri).ok())
        {
            Some(uri) => uri.origin().ascii_serialization(),
            None => return error(ErrorCode::InvalidRequest),
        }
    } else {
        jkt.clone()
    };
    let remember = match form.get("remember").map(String::as_str) {
        Some("true") => true,
        None | Some("false") => false,
        _ => return error(ErrorCode::InvalidRequest),
    };
    let administration_confirmed = match form.get("confirm_administration").map(String::as_str) {
        Some("true") => true,
        None => false,
        _ => return error(ErrorCode::InvalidRequest),
    };
    let declined = requested
        .capabilities
        .optional
        .iter()
        .filter(|capability| !approved.contains(capability))
        .cloned()
        .collect();
    let approval = super::super::GrantApproval {
        presentation: Some(presentation.clone()),
        principal_id: login.principal_id.clone(),
        login_session_id: login.login_session_id,
        client_id: validated.client_id.as_str().into(),
        binding_kind: client_kind,
        binding_value: binding,
        durable_dpop_jkt: jkt,
        public_jwk: flow.public_jwk,
        selection: requested.capabilities,
        approved,
        reused_remembered: Vec::new(),
        declined,
        administration_confirmed,
        remember,
        remember_until: None,
        expires_at: login.expires_at,
    };
    let digest = match super::super::sqlite::digest(&approval) {
        Ok(digest) => digest,
        Err(_) => return error(ErrorCode::ServerError),
    };
    let identity = super::super::Mutation {
        actor: login.principal_id,
        request_id: presentation.id.clone(),
        request_digest: digest,
        session: None,
    };
    match flow.request {
        super::portal::ConsentRequest::Authorization(pairs) => {
            let request = AuthorizationRequest::from_pairs(
                pairs
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
            match engine.approve(&request, &identity, &approval).await {
                Ok(response) => Redirect::to(
                    &response.location(validated.redirect_uri.as_deref().unwrap_or_default()),
                )
                .into_response(),
                Err(super::portal::ApprovalError::ConsentChanged) => Redirect::to(&format!(
                    "/consent?transaction={transaction}&error=consent_changed"
                ))
                .into_response(),
                Err(super::portal::ApprovalError::Protocol(response)) => error(response.error),
            }
        }
        super::portal::ConsentRequest::Device { device_code } => {
            let device = match engine.server.store().get_device_grant(&device_code).await {
                Ok(Some(device)) => device,
                _ => return error(ErrorCode::InvalidRequest),
            };
            match engine
                .approve_device(&device.user_code, &identity, &approval)
                .await
            {
                Ok(()) => Redirect::to("/device/complete?decision=approved").into_response(),
                Err(super::portal::ApprovalError::ConsentChanged) => Redirect::to(&format!(
                    "/consent?transaction={transaction}&error=consent_changed"
                ))
                .into_response(),
                Err(super::portal::ApprovalError::Protocol(response)) => error(response.error),
            }
        }
    }
}

pub(super) fn portal_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let mut found = None;
    for line in headers.get_all(header::COOKIE) {
        for part in line.to_str().ok()?.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            if name == cookie_name {
                if found.is_some() || value.len() != 43 {
                    return None;
                }
                found = Some(value.to_owned());
            }
        }
    }
    found
}

pub(super) fn cookie_response(
    cookie: &str,
    csrf: Option<&str>,
    max_age: u32,
    mut response: Response,
) -> Response {
    let bearer = format!(
        "__Host-trellis-portal={cookie}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}"
    );
    let Ok(bearer) = HeaderValue::from_str(&bearer) else {
        return error(ErrorCode::ServerError);
    };
    response.headers_mut().append(header::SET_COOKIE, bearer);
    if let Some(csrf) = csrf {
        let csrf = format!(
            "__Host-trellis-portal-csrf={csrf}; Path=/; Secure; SameSite=Lax; Max-Age={max_age}"
        );
        let Ok(csrf) = HeaderValue::from_str(&csrf) else {
            return error(ErrorCode::ServerError);
        };
        response.headers_mut().append(header::SET_COOKIE, csrf);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

async fn portal_session(State(engine): State<Arc<OAuthEngine>>, headers: HeaderMap) -> Response {
    if !portal_origin(&engine, &headers) {
        return error(ErrorCode::InvalidRequest);
    }
    let now = crate::clock::unix_now();
    let authority = engine.server.store().authority.clone();
    match blocking(move || authority.create_portal_session(now)).await {
        Ok(session) => cookie_response(
            &session.cookie,
            Some(&session.csrf),
            600,
            Json(serde_json::json!({"sessionId":session.session_id})).into_response(),
        ),
        Err(_) => error(ErrorCode::ServerError),
    }
}

async fn portal_login(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if !portal_origin(&engine, &headers) {
        return error(ErrorCode::InvalidRequest);
    }
    let Some(cookie) = portal_cookie(&headers, "__Host-trellis-portal") else {
        return error(ErrorCode::AccessDenied);
    };
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let (Some(csrf), Some(username), Some(password), Some(request_id)) = (
        form.get("csrf"),
        form.get("username"),
        form.get("password"),
        form.get("request_id"),
    ) else {
        return error(ErrorCode::InvalidRequest);
    };
    if ulid::Ulid::from_string(request_id).is_err() {
        return error(ErrorCode::InvalidRequest);
    }
    let mut replacement = [0u8; 32];
    if getrandom::fill(&mut replacement).is_err() {
        return error(ErrorCode::ServerError);
    }
    let replacement = URL_SAFE_NO_PAD.encode(replacement);
    let (csrf, username, password, request_id) = (
        csrf.clone(),
        username.clone(),
        password.clone(),
        request_id.clone(),
    );
    let csrf_cookie = csrf.clone();
    let transaction = request_id.clone();
    let html = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.split(';').next().map(str::trim) == Some("text/html"))
        });
    let lifetime = engine
        .server
        .store()
        .authority
        .settings
        .login_lifetime_seconds;
    let authority = engine.server.store().authority.clone();
    let new_cookie = replacement.clone();
    let result = blocking(move || {
        let now = crate::clock::unix_now();
        authority.portal_local_login(
            &cookie,
            &csrf,
            &new_cookie,
            (&username, &password),
            &request_id,
            now,
        )
    })
    .await;
    match result {
        Ok(login) => {
            let response = if html {
                Redirect::to(&format!("/consent?transaction={transaction}")).into_response()
            } else {
                Json(serde_json::json!({"sessionId":login.session_id})).into_response()
            };
            cookie_response(&replacement, Some(&csrf_cookie), lifetime, response)
        }
        Err(super::super::AuthError::Denied) if html => Redirect::to(&format!(
            "/login?transaction={transaction}&error=login_failed"
        ))
        .into_response(),
        Err(super::super::AuthError::Denied) => error(ErrorCode::AccessDenied),
        _ => error(ErrorCode::ServerError),
    }
}

/// Creates someone's own local account and signs them in, when the app they are
/// signing in to and the installation both allow it. The app comes from the
/// server's record of the transaction, never from the form.
async fn portal_sign_up(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if !portal_origin(&engine, &headers) {
        return error(ErrorCode::InvalidRequest);
    }
    let Some(cookie) = portal_cookie(&headers, "__Host-trellis-portal") else {
        return error(ErrorCode::AccessDenied);
    };
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let (Some(csrf), Some(transaction), Some(username), Some(password), Some(name)) = (
        form.get("csrf").cloned(),
        form.get("request_id").cloned(),
        form.get("username").cloned(),
        form.get("password").cloned(),
        form.get("name").cloned(),
    ) else {
        return error(ErrorCode::InvalidRequest);
    };
    if ulid::Ulid::from_string(&transaction).is_err() {
        return error(ErrorCode::InvalidRequest);
    }
    let account = super::portal::LocalSignUp {
        username,
        password,
        name,
        email: form.get("email").cloned().unwrap_or_default(),
    };
    let mut replacement = [0u8; 32];
    if getrandom::fill(&mut replacement).is_err() {
        return error(ErrorCode::ServerError);
    }
    let replacement = URL_SAFE_NO_PAD.encode(replacement);
    let new_cookie = replacement.clone();
    let lifetime = engine
        .server
        .store()
        .authority
        .settings
        .login_lifetime_seconds;
    let (login_csrf, login_transaction) = (csrf.clone(), transaction.clone());
    let result = super::with_store(&engine, move |store| {
        let now = crate::clock::unix_now();
        if !store
            .authorization_sign_up(&login_transaction, now)?
            .password
        {
            return Err(super::super::AuthError::Denied);
        }
        store
            .authority
            .portal_local_sign_up(&cookie, &login_csrf, &account, now)?;
        store.authority.portal_local_login(
            &cookie,
            &login_csrf,
            &new_cookie,
            (&account.username, &account.password),
            &login_transaction,
            now,
        )
    })
    .await;
    let problem = match result {
        Ok(_) => {
            let response = Redirect::to(&format!("/consent?transaction={transaction}"));
            return cookie_response(
                &replacement,
                Some(&csrf),
                lifetime,
                response.into_response(),
            );
        }
        Err(super::super::AuthError::Conflict) => "sign_up_taken",
        Err(super::super::AuthError::Invalid(_)) => "sign_up_invalid",
        Err(super::super::AuthError::Denied) => "sign_up_failed",
        Err(_) => return error(ErrorCode::ServerError),
    };
    Redirect::to(&format!(
        "/login?transaction={transaction}&sign_up=1&error={problem}"
    ))
    .into_response()
}

async fn portal_logout(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if !portal_origin(&engine, &headers) {
        return error(ErrorCode::InvalidRequest);
    }
    let Some(cookie) = portal_cookie(&headers, "__Host-trellis-portal") else {
        return error(ErrorCode::AccessDenied);
    };
    let form = match fields(pairs) {
        Ok(form) => form,
        Err(response) => return error(response.error),
    };
    let (Some(csrf), Some(request_id)) = (form.get("csrf"), form.get("request_id")) else {
        return error(ErrorCode::InvalidRequest);
    };
    let now = crate::clock::unix_now();
    let authority = engine.server.store().authority.clone();
    let (csrf, request_id) = (csrf.clone(), request_id.clone());
    let logged_out = blocking(move || {
        let login = authority
            .portal_login(&cookie, &csrf, now)
            .map_err(|_| ErrorCode::AccessDenied)?;
        let identity = super::super::Mutation {
            actor: login.principal_id.clone(),
            request_id,
            request_digest: super::super::sqlite::digest(&login.login_session_id)
                .map_err(|_| ErrorCode::ServerError)?,
            session: None,
        };
        authority
            .portal_logout(&identity, &login, now)
            .map_err(|_| ErrorCode::AccessDenied)
    })
    .await;
    match logged_out {
        Ok(()) => cookie_response("", Some(""), 0, Json(serde_json::json!({})).into_response()),
        Err(code) => error(code),
    }
}

pub(super) fn error(code: ErrorCode) -> Response {
    let status = StatusCode::from_u16(code.http_status()).unwrap_or(StatusCode::BAD_REQUEST);
    let mut response = (status, Json(serde_json::json!({"error":code.as_str()}))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn authorization_error(response: AuthorizationError) -> Response {
    match response {
        AuthorizationError::Direct(response) => {
            let mut response = error(response.error);
            if response.status() != StatusCode::INTERNAL_SERVER_ERROR {
                *response.status_mut() = StatusCode::BAD_REQUEST;
            }
            response
        }
        AuthorizationError::Redirect(mut response) => {
            response.error.error_description = None;
            response.error.error_uri = None;
            Redirect::to(&response.location()).into_response()
        }
    }
}

pub(super) fn fields(
    pairs: Vec<(String, String)>,
) -> Result<BTreeMap<String, String>, ErrorResponse> {
    let mut fields = BTreeMap::new();
    for (name, value) in pairs {
        if fields.insert(name, value).is_some() {
            return Err(ErrorResponse::new(ErrorCode::InvalidRequest));
        }
    }
    if fields.contains_key("client_secret")
        || fields.contains_key("client_assertion")
        || fields.contains_key("client_assertion_type")
    {
        return Err(ErrorResponse::new(ErrorCode::InvalidClient));
    }
    Ok(fields)
}

async fn token(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if headers.contains_key(header::AUTHORIZATION)
        || headers.get_all("dpop").iter().count() != 1
        || headers.get_all(header::ORIGIN).iter().count() > 1
    {
        return error(ErrorCode::InvalidRequest);
    }
    let Ok(proof) = headers["dpop"].to_str().map(str::to_owned) else {
        return error(ErrorCode::InvalidDpopProof);
    };
    let fields = match fields(pairs) {
        Ok(fields) => fields,
        Err(response) => return error(response.error),
    };
    let Some(client) = fields.get("client_id") else {
        return error(ErrorCode::InvalidClient);
    };
    let client_id = ClientId::new(client);
    let (Some(runtime), Some(nkey)) = (
        fields.get("trellis_runtime_id"),
        fields.get("trellis_session_nkey"),
    ) else {
        return error(ErrorCode::InvalidRequest);
    };
    let request = match fields.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some(code) = fields.get("code") else {
                return error(ErrorCode::InvalidRequest);
            };
            TokenRequest::AuthorizationCode {
                client_id,
                client_secret: None,
                code: code.clone(),
                redirect_uri: fields.get("redirect_uri").cloned(),
                code_verifier: fields.get("code_verifier").cloned(),
            }
        }
        Some("refresh_token") => {
            let Some(token) = fields.get("refresh_token") else {
                return error(ErrorCode::InvalidRequest);
            };
            TokenRequest::RefreshToken {
                client_id,
                client_secret: None,
                refresh_token: token.clone(),
                scope: None,
            }
        }
        Some("urn:ietf:params:oauth:grant-type:device_code") => {
            let Some(code) = fields.get("device_code") else {
                return error(ErrorCode::InvalidRequest);
            };
            TokenRequest::DeviceCode {
                client_id,
                client_secret: None,
                device_code: code.clone(),
            }
        }
        _ => return error(ErrorCode::UnsupportedGrantType),
    };
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if let Some(origin) = &origin {
        let client = match engine
            .server
            .store()
            .get_client(&ClientId::new(client))
            .await
        {
            Ok(Some(client)) => client,
            _ => return error(ErrorCode::InvalidClient),
        };
        if !client.redirect_uris.iter().any(|redirect| {
            url::Url::parse(redirect)
                .is_ok_and(|redirect| redirect.origin().ascii_serialization() == *origin)
        }) {
            return error(ErrorCode::InvalidRequest);
        }
    }
    let (runtime, nkey) = (runtime.clone(), nkey.clone());
    // Complete engine transactions even if the HTTP caller drops its request.
    let result =
        tokio::spawn(async move { engine.token(request, &proof, &runtime, &nkey).await }).await;
    let mut response = match result {
        Ok(Ok(tokens)) => Json(tokens).into_response(),
        Ok(Err(super::engine::TokenError::OAuth(response))) => error(response.error),
        Ok(Err(super::engine::TokenError::UseDpopNonce(nonce))) => {
            let mut response = (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"use_dpop_nonce"})),
            )
                .into_response();
            let Ok(nonce) = HeaderValue::from_str(&nonce) else {
                return error(ErrorCode::ServerError);
            };
            response
                .headers_mut()
                .insert(HeaderName::from_static("dpop-nonce"), nonce);
            response
        }
        Err(_) => error(ErrorCode::ServerError),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    if let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(&origin).ok()) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

async fn revoke(
    State(engine): State<Arc<OAuthEngine>>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if headers.contains_key(header::AUTHORIZATION) {
        return error(ErrorCode::InvalidClient);
    }
    let fields = match fields(pairs) {
        Ok(fields) => fields,
        Err(response) => return error(response.error),
    };
    let (Some(client), Some(token)) = (fields.get("client_id"), fields.get("token")) else {
        return error(ErrorCode::InvalidRequest);
    };
    let (client, token) = (ClientId::new(client), token.clone());
    match tokio::spawn(async move { engine.server.revoke(&client, None, &token, None).await }).await
    {
        Ok(Ok(())) => StatusCode::OK.into_response(),
        Ok(Err(response)) => error(response.error),
        Err(_) => error(ErrorCode::ServerError),
    }
}
