use super::super::*;
use axum::body::Body;
use axum::http::header::CONTENT_SECURITY_POLICY;
use axum::http::{HeaderValue, Request};
use tower::ServiceExt as _;

const PROXY_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::platform::auth::http) struct AuthStartRequest {
    request_id: String,
    #[serde(rename = "issuedAt")]
    _issued_at: i64,
    session_public_key: String,
    participant_id: String,
    redirect_target: String,
    proof: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthStartResponse {
    flow_id: String,
    login_url: String,
}

pub(crate) async fn start_auth<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Json(raw): Json<Value>,
) -> Result<Json<AuthStartResponse>, HttpError>
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + GrantRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let request: AuthStartRequest = serde_json::from_value(raw.clone()).map_err(|error| {
        tracing::warn!(%error, "invalid auth request shape");
        HttpError::bad_request("invalid_auth_request")
    })?;
    if ulid::Ulid::from_string(&request.request_id)
        .map(|parsed| parsed.to_string() != request.request_id)
        .unwrap_or(true)
    {
        return Err(HttpError::bad_request("invalid_auth_request"));
    }
    validate_redirect(&request.redirect_target, &state.allowed_redirect_origins)?;
    let request_digest = proof_request_digest(&raw).map_err(|error| {
        tracing::warn!(%error, "invalid auth request proof envelope");
        HttpError::bad_request("invalid_auth_request")
    })?;
    let mut unsigned_request = raw.clone();
    unsigned_request
        .as_object_mut()
        .ok_or_else(|| HttpError::bad_request("invalid_auth_request"))?
        .remove("proof");
    let input = SessionProofInput::user_auth_request(UserAuthRequestSessionProofInput {
        origin: state.public_origin.clone(),
        unsigned_request,
    })
    .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    let proof = parse_session_proof(&request.proof)
        .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    let now = now_ms()?;
    let (portal, _) = select_login_portal(
        state.service.repository(),
        &request.participant_id,
        &request.redirect_target,
    )
    .await?;
    verify_session_proof(
        &input,
        &proof,
        &request.session_public_key,
        now,
        state.proof_policy,
    )
    .map_err(|_| HttpError::unauthorized("invalid_proof"))?;
    let (installed_revision, binding) = state
        .service
        .repository()
        .get_installed_participant_record(request.participant_id.clone(), None)
        .await?
        .ok_or_else(|| {
            tracing::warn!(participant_id = %request.participant_id, "auth request participant is not installed");
            HttpError::not_found("participant_not_found")
        })?;
    if binding.state != ParticipantBindingState::Resolved {
        tracing::warn!(participant_id = %request.participant_id, "auth request participant binding is unresolved");
        return Err(HttpError::bad_request("participant_binding_mismatch"));
    }
    if state
        .service
        .repository()
        .is_companion_participant(binding.participant_id.clone())
        .await?
    {
        return Err(HttpError::bad_request(
            "companion_requires_parent_activation",
        ));
    }
    let consent = browser_consent(&binding, installed_revision)?;
    let flow_id = request.request_id.clone();
    let flow = AuthBrowserFlow {
        format: BROWSER_FLOW_FORMAT.to_owned(),
        flow_id: flow_id.clone(),
        kind: AuthBrowserFlowKind::UserAuth,
        state: AuthBrowserFlowState::ChooseProvider,
        request_id: request.request_id,
        request_digest,
        participant_id: request.participant_id,
        installed_revision,
        target_grant_revision: 0,
        consent,
        session_public_key: request.session_public_key,
        portal_id: portal.portal_id.clone(),
        redirect_target: Some(request.redirect_target),
        principal_id: None,
        authenticated_provider_id: None,
        authenticated_roles: Vec::new(),
        portal_binding_digest: None,
        claim_owner: None,
        claimed_at: None,
        durable_result_digest: None,
        completed_at: None,
        created_at: now,
        expires_at: checked_add(now, state.browser_flow_ttl_ms)?,
        version: 1,
    };
    match state.ephemeral.create_browser_flow(flow.clone()).await {
        Ok(()) => {}
        Err(AuthorizationStateError::StorageConflict) => {
            let existing = state
                .ephemeral
                .get_browser_flow(&flow_id)
                .await?
                .ok_or_else(|| HttpError::conflict("proof_replay"))?;
            if existing.request_digest != flow.request_digest
                || existing.session_public_key != flow.session_public_key
            {
                return Err(HttpError::conflict("proof_replay"));
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Json(AuthStartResponse {
        flow_id: flow_id.clone(),
        login_url: portal_url(&portal, &state.public_origin, &flow_id)?,
    }))
}

async fn select_login_portal(
    repository: &impl PortalRepository,
    participant_id: &str,
    redirect_target: &str,
) -> Result<(LoginPortalRecord, LoginSettingsRecord), HttpError> {
    let origin = canonical_origin(redirect_target)
        .map_err(|_| HttpError::bad_request("invalid_redirect_target"))?;
    for route in repository.list_portal_routes().await? {
        if route.deployment_id.is_some()
            || route
                .participant_id
                .as_deref()
                .is_some_and(|value| value != participant_id)
            || route.origin.as_deref().is_some_and(|value| value != origin)
        {
            continue;
        }
        if let Some((portal, settings)) = repository.get_login_portal(&route.portal_id).await? {
            if !portal.disabled && !portal.removed {
                return Ok((portal, settings));
            }
        }
    }
    repository
        .get_login_portal("builtin")
        .await?
        .filter(|(portal, _)| !portal.disabled && !portal.removed)
        .ok_or_else(|| HttpError::internal("builtin_portal_unavailable"))
}

pub(crate) async fn select_device_portal(
    repository: &impl PortalRepository,
    participant_id: &str,
    deployment_id: &str,
) -> Result<LoginPortalRecord, HttpError> {
    for route in repository.list_portal_routes().await? {
        if route.origin.is_some()
            || route
                .participant_id
                .as_deref()
                .is_some_and(|value| value != participant_id)
            || route
                .deployment_id
                .as_deref()
                .is_some_and(|value| value != deployment_id)
        {
            continue;
        }
        if let Some((portal, _)) = repository.get_login_portal(&route.portal_id).await? {
            if !portal.disabled && !portal.removed {
                return Ok(portal);
            }
        }
    }
    repository
        .get_login_portal("builtin")
        .await?
        .map(|(portal, _)| portal)
        .filter(|portal| !portal.disabled && !portal.removed)
        .ok_or_else(|| HttpError::internal("builtin_portal_unavailable"))
}

pub(super) fn portal_url(
    portal: &LoginPortalRecord,
    public_origin: &str,
    flow_id: &str,
) -> Result<String, HttpError> {
    let entry = portal.entry_url.as_deref().map_or_else(
        || format!("{}/login", public_origin.trim_end_matches('/')),
        ToOwned::to_owned,
    );
    let mut url = Url::parse(&entry).map_err(|_| HttpError::internal("portal_entry_invalid"))?;
    url.query_pairs_mut().append_pair("flowId", flow_id);
    Ok(url.into())
}

pub(crate) async fn portal_index<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    request: Request<Body>,
) -> Response
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    serve_source(&state.portal_source, "200.html", Some("200.html"), request).await
}

pub(crate) async fn portal_page<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    if !embedded_path_is_safe(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let fallback = (!path.starts_with("assets/") && (path.contains('/') || !path.contains('.')))
        .then_some("200.html");
    serve_source(
        &state.portal_source,
        &format!("login/{path}"),
        fallback,
        request,
    )
    .await
}

pub(crate) async fn portal_asset<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    serve_source(
        &state.portal_source,
        &format!("assets/login/{path}"),
        None,
        request,
    )
    .await
}

pub(crate) async fn console_index<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    request: Request<Body>,
) -> Response
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    serve_source(
        &state.console_source,
        "index.html",
        Some("200.html"),
        request,
    )
    .await
}

pub(crate) async fn console_page<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    Path(path): Path<String>,
    request: Request<Body>,
) -> Response
where
    R: AccountRepository
        + AuthorityEvidenceRepository
        + ContextRepository
        + DeploymentRepository
        + OutboxRepository
        + PortalRepository
        + ProvisioningRepository
        + SessionRepository
        + Clone
        + Send
        + Sync
        + 'static,
    E: AuthEphemeralRepository + Clone,
{
    let fallback = (!path.starts_with("assets/") && (path.contains('/') || !path.contains('.')))
        .then_some(if state.console_source_is_override {
            "index.html"
        } else {
            "200.html"
        });
    let source_path = if state.console_source_is_override {
        path
    } else {
        format!("console/{path}")
    };
    serve_source(&state.console_source, &source_path, fallback, request).await
}

pub(crate) async fn web_fallback<R, E>(
    State(state): State<AuthHttpState<R, E>>,
    request: Request<Body>,
) -> Response {
    let uri = request.uri().clone();
    if matches!(uri.path(), "/auth" | "/bootstrap")
        || uri.path().starts_with("/auth/")
        || uri.path().starts_with("/bootstrap/")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let fallback = (!path.starts_with("assets/") && !path.contains('.')).then_some("200.html");
    serve_source(&state.web_source, path, fallback, request).await
}

async fn serve_source(
    source: &WebSource,
    path: &str,
    fallback: Option<&str>,
    request: Request<Body>,
) -> Response {
    if let WebSource::Proxy(proxy) = source {
        return proxy_request(proxy, request).await;
    }
    if !embedded_path_is_safe(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let response = match source {
        WebSource::Embedded => embedded_file(EMBEDDED_WEB_ASSETS, path),
        WebSource::Directory(directory) => directory_file(directory, path).await,
        WebSource::Proxy(_) => unreachable!(),
    };
    if response.status() == StatusCode::NOT_FOUND {
        if let Some(fallback) = fallback {
            return match source {
                WebSource::Embedded => embedded_file(EMBEDDED_WEB_ASSETS, fallback),
                WebSource::Directory(directory) => directory_file(directory, fallback).await,
                WebSource::Proxy(_) => unreachable!(),
            };
        }
    }
    response
}

async fn proxy_request(proxy: &axum::Router, request: Request<Body>) -> Response {
    let Ok(mut response) = proxy.clone().oneshot(request).await;
    response
        .headers_mut()
        .entry(CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(PROXY_CONTENT_SECURITY_POLICY));
    response
}

async fn directory_file(directory: &std::path::Path, path: &str) -> Response {
    match tokio::fs::read(directory.join(path)).await {
        Ok(bytes) => embedded_response(path, bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn embedded_file(assets: &[(&str, &[u8])], path: &str) -> Response {
    if !embedded_path_is_safe(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(bytes) = assets
        .iter()
        .find_map(|(asset_path, bytes)| (*asset_path == path).then(|| bytes.to_vec()))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    embedded_response(path, bytes)
}

fn embedded_path_is_safe(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn embedded_response(path: &str, bytes: Vec<u8>) -> Response {
    let content_type = match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("webp") => "image/webp",
        Some("wasm") => "application/wasm",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    ([(CONTENT_TYPE, content_type)], bytes).into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn start_response_has_one_final_shape() {
        assert_eq!(
            serde_json::to_value(super::AuthStartResponse {
                flow_id: "flow_01".to_owned(),
                login_url: "https://auth.example/login?flowId=flow_01".to_owned(),
            })
            .unwrap(),
            serde_json::json!({
                "flowId": "flow_01",
                "loginUrl": "https://auth.example/login?flowId=flow_01",
            })
        );
    }
}
