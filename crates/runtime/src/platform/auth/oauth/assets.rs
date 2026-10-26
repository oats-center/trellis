//! Assets from the isolated OAuth portal build; no Console or application bundle
//! is embedded into this origin.

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "generated/portal"]
struct PortalAssets;

/// The portal stylesheet, so server-rendered Portal pages match the app.
pub(super) fn stylesheet() -> Option<String> {
    let index = PortalAssets::get("index.html")?;
    let index = std::str::from_utf8(&index.data).ok()?;
    let start = index.find("rel=\"stylesheet\"")?;
    let tag = &index[index[..start].rfind('<')?..];
    let href = tag.split("href=\"").nth(1)?.split('"').next()?;
    href.starts_with("/portal-assets/").then(|| href.to_owned())
}

pub(super) fn asset(path: &str) -> Response {
    if path.starts_with(['/', '.'])
        || path.contains('\\')
        || path.split('/').any(|part| part == ".." || part == ".")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(file) = PortalAssets::get(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        file.data.into_owned(),
    )
        .into_response()
}

pub(super) fn page(engine: &super::OAuthEngine, callback: Option<&str>) -> Response {
    let mut form_action = String::from("'self'");
    if let Some(callback) = callback {
        let Ok(callback) = url::Url::parse(callback) else {
            return super::http::error(oauth_as::error::ErrorCode::ServerError);
        };
        if !matches!(callback.scheme(), "https" | "http") {
            return super::http::error(oauth_as::error::ErrorCode::ServerError);
        }
        form_action.push(' ');
        form_action.push_str(&callback.origin().ascii_serialization());
    }
    let csp = format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
        connect-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors \
        'none'; form-action {form_action}"
    );
    let Ok(csp) = HeaderValue::from_str(&csp) else {
        return super::http::error(oauth_as::error::ErrorCode::ServerError);
    };
    let mut response = asset("index.html");
    if let Some(file) = PortalAssets::get("index.html") {
        let mut html = String::from_utf8_lossy(&file.data).into_owned();
        if engine.generated_tls() {
            // The Portal shows its not-for-production banner from this marker.
            html = html.replacen("<html", "<html data-generated-tls", 1);
        }
        // After the Portal's own stylesheet, so the installation's wins.
        html = html.replacen(
            "</head>",
            "<link rel=\"stylesheet\" href=\"/portal-theme/stylesheet.css\"></head>",
            1,
        );
        *response.body_mut() = html.into();
    }
    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, csp);
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        // Suppress cross-origin referrers without turning same-origin form
        // submissions into `Origin: null`, which strict portal CSRF rejects.
        HeaderValue::from_static("same-origin"),
    );
    response
}
