use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, COOKIE, ORIGIN, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

use super::super::ephemeral::AuthBrowserFlow;
use super::super::ephemeral::AuthOAuthState;
use super::super::{LoginPortalRecord, LoginSettingsRecord};
use super::{digest_parts, HttpError};

const CONTENT_SECURITY_POLICY_PREFIX: &str = "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'";

pub(super) fn content_security_policy(
    websocket_nats_servers: &[String],
) -> Result<HeaderValue, url::ParseError> {
    let mut origins = websocket_nats_servers
        .iter()
        .map(|server| Url::parse(server).map(|url| url.origin().ascii_serialization()))
        .collect::<Result<Vec<_>, _>>()?;
    origins.sort();
    origins.dedup();
    Ok(HeaderValue::from_str(&format!(
        "{CONTENT_SECURITY_POLICY_PREFIX}{}",
        origins
            .iter()
            .map(|origin| format!(" {origin}"))
            .collect::<String>()
    ))
    .expect("URL origins produce a valid CSP header"))
}

pub(super) const PORTAL_BINDING_HEADER: HeaderName =
    HeaderName::from_static("trellis-portal-binding");

pub(super) fn validate_portal_binding_digest(value: &str) -> Result<(), HttpError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HttpError::bad_request("portal_binding_digest_invalid"))?;
    if decoded.len() != 32 {
        return Err(HttpError::bad_request("portal_binding_digest_invalid"));
    }
    Ok(())
}

pub(super) fn require_portal_binding(
    flow: &AuthBrowserFlow,
    headers: &HeaderMap,
) -> Result<(), HttpError> {
    let binding = headers
        .get(&PORTAL_BINDING_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| HttpError::forbidden("portal_binding_invalid"))?;
    let binding = URL_SAFE_NO_PAD
        .decode(binding)
        .map_err(|_| HttpError::forbidden("portal_binding_invalid"))?;
    if binding.len() != 32 {
        return Err(HttpError::forbidden("portal_binding_invalid"));
    }
    let actual = URL_SAFE_NO_PAD.encode(Sha256::digest(binding));
    let expected = flow
        .portal_binding_digest
        .as_deref()
        .ok_or_else(|| HttpError::forbidden("portal_binding_invalid"))?;
    if bool::from(actual.as_bytes().ct_eq(expected.as_bytes())) {
        Ok(())
    } else {
        Err(HttpError::forbidden("portal_binding_invalid"))
    }
}

pub(super) fn validate_redirect(
    redirect: &str,
    allowed_origins: &[String],
) -> Result<(), HttpError> {
    let redirect = Url::parse(redirect).map_err(|_| HttpError::bad_request("invalid_redirect"))?;
    if redirect.fragment().is_some() || redirect.username() != "" || redirect.password().is_some() {
        return Err(HttpError::bad_request("invalid_redirect"));
    }
    if !allowed_origins.contains(&canonical_origin(redirect.as_str())?) {
        return Err(HttpError::bad_request("redirect_origin_not_allowed"));
    }
    Ok(())
}

pub(super) fn canonical_origin(value: &str) -> Result<String, HttpError> {
    let url = Url::parse(value).map_err(|_| HttpError::bad_request("invalid_origin"))?;
    let host = url
        .host_str()
        .ok_or_else(|| HttpError::bad_request("invalid_origin"))?;
    let mut origin = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        origin.push(':');
        origin.push_str(&port.to_string());
    }
    Ok(origin)
}

pub(super) fn oidc_portal_policy_digest(
    portal: &LoginPortalRecord,
    settings: &LoginSettingsRecord,
) -> Result<String, HttpError> {
    trellis_protocol::digest_json(&json!({
        "portalId": portal.portal_id,
        "portalVersion": portal.version,
        "disabled": portal.disabled,
        "removed": portal.removed,
        "providerIds": portal.provider_ids,
        "settingsVersion": settings.version,
        "federatedRegistrationEnabled": settings.federated_registration_enabled,
    }))
    .map_err(|_| HttpError::internal("oauth_policy_digest"))
}

pub(super) fn oauth_cookie_name(state_id: &str) -> String {
    format!("trellis_oauth_{}", digest_parts(&[state_id]))
}

pub(super) fn oauth_cookie_header(
    name: &str,
    value: &str,
    provider_id: &str,
    secure: bool,
    max_age_seconds: u64,
) -> Result<HeaderValue, HttpError> {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/auth/callback/{provider_id}; Max-Age={max_age_seconds}; HttpOnly; SameSite=Lax{secure}"
    ))
    .map_err(|_| HttpError::internal("oauth_cookie"))
}

fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| {
            let (cookie_name, value) = cookie.split_once('=')?;
            (cookie_name == name).then(|| value.to_owned())
        })
}

pub(super) fn require_oauth_browser_binding(
    state_id: &str,
    state: &AuthOAuthState,
    headers: &HeaderMap,
) -> Result<(), HttpError> {
    let browser_binding = request_cookie(headers, &oauth_cookie_name(state_id))
        .ok_or_else(|| HttpError::bad_request("oauth_browser_binding_invalid"))?;
    let actual = URL_SAFE_NO_PAD.encode(Sha256::digest(browser_binding.as_bytes()));
    if bool::from(
        actual
            .as_bytes()
            .ct_eq(state.browser_binding_digest.as_bytes()),
    ) {
        Ok(())
    } else {
        Err(HttpError::bad_request("oauth_browser_binding_invalid"))
    }
}

pub(super) fn require_portal_origin(
    headers: &HeaderMap,
    public_origin: &str,
) -> Result<(), HttpError> {
    let Some(origin) = headers.get(ORIGIN) else {
        return Err(HttpError::forbidden("origin_required"));
    };
    if origin
        .to_str()
        .ok()
        .and_then(|origin| canonical_origin(origin).ok())
        .as_deref()
        != Some(canonical_origin(public_origin)?.as_str())
    {
        return Err(HttpError::forbidden("origin_mismatch"));
    }
    Ok(())
}

pub(super) fn require_selected_portal_origin(
    headers: &HeaderMap,
    portal: &LoginPortalRecord,
    public_origin: &str,
) -> Result<(), HttpError> {
    require_portal_origin(
        headers,
        portal.entry_url.as_deref().unwrap_or(public_origin),
    )
}

pub(super) async fn security_headers(
    State(content_security_policy): State<HeaderValue>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers
        .entry(CONTENT_SECURITY_POLICY)
        .or_insert(content_security_policy);
    response
}

#[cfg(test)]
mod tests {
    use super::content_security_policy;

    #[test]
    fn content_security_policy_allows_wasm_without_javascript_eval() {
        let policy = content_security_policy(&[]).expect("content security policy");
        let policy = policy.to_str().expect("ASCII content security policy");
        assert!(policy.contains("'wasm-unsafe-eval'"));
        assert!(!policy.contains(" 'unsafe-eval'"));
    }

    #[test]
    fn content_security_policy_allows_only_configured_websocket_origins() {
        let policy = content_security_policy(&[
            "wss://nats.example.com/client".to_owned(),
            "ws://localhost:8080".to_owned(),
            "ws://localhost:8080/duplicate".to_owned(),
        ])
        .expect("content security policy");
        assert!(policy
            .to_str()
            .expect("ASCII content security policy")
            .ends_with(" ws://localhost:8080 wss://nats.example.com"));
    }
}
