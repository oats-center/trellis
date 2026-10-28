//! Portal theme endpoints, served from the themes Auth stores (see
//! `portal_themes`). Theming changes how the Portal looks, never what it
//! decides, and theme assets are served sandboxed from the OAuth origin.

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use super::super::portal_themes;

#[derive(Deserialize)]
pub(super) struct ThemeQuery {
    transaction: Option<String>,
    client: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeView {
    name: Option<String>,
    logo: Option<String>,
    colors: BTreeMap<String, String>,
    dark_colors: BTreeMap<String, String>,
    copy: BTreeMap<String, String>,
}

/// The theme for a pending authorization's client, or the installation's.
/// The client comes from the server's own record of the transaction, never
/// from the request, so a crafted link cannot dress one app as another.
pub(super) async fn view(
    State(engine): State<std::sync::Arc<super::OAuthEngine>>,
    Query(query): Query<ThemeQuery>,
) -> Response {
    let store = engine.server.store().clone();
    let view = crate::blocking::spawn(move || {
        let now = crate::clock::unix_now();
        let client = query
            .transaction
            .and_then(|transaction| store.authorization_client(&transaction, now).ok())
            .flatten();
        portal_themes::portal_view(&*store.authority.reader()?, client.as_deref())
    })
    .await;
    let Ok(Ok(theme)) = view else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let base = engine
        .server
        .store()
        .authority
        .settings
        .oauth_path()
        .to_owned();
    let logo = theme.logo_owner.map(|owner| match owner {
        Some(client) => format!(
            "{base}/portal-theme/logo?client={}",
            url::form_urlencoded::byte_serialize(client.as_bytes()).collect::<String>()
        ),
        None => format!("{base}/portal-theme/logo"),
    });
    let mut response = Json(ThemeView {
        name: theme.name,
        logo,
        colors: theme.colors,
        dark_colors: theme.dark_colors,
        copy: theme.copy,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A theme logo. Logos are public branding, so the client may be named.
pub(super) async fn logo(
    State(engine): State<std::sync::Arc<super::OAuthEngine>>,
    Query(query): Query<ThemeQuery>,
) -> Response {
    let store = engine.server.store().clone();
    let logo = crate::blocking::spawn(move || {
        portal_themes::portal_logo(&*store.authority.reader()?, query.client.as_deref())
    })
    .await;
    match logo {
        Ok(Ok(Some((content_type, bytes)))) => sandboxed(content_type, bytes),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// The installation stylesheet, empty when none is set. Every Portal page
/// links it after the Portal's own, so the installation's rules win.
pub(super) async fn stylesheet(
    State(engine): State<std::sync::Arc<super::OAuthEngine>>,
) -> Response {
    let store = engine.server.store().clone();
    let stylesheet = crate::blocking::spawn(move || {
        portal_themes::portal_stylesheet(&*store.authority.reader()?)
    })
    .await;
    match stylesheet {
        Ok(Ok(stylesheet)) => sandboxed(
            "text/css; charset=utf-8",
            stylesheet.unwrap_or_default().into_bytes(),
        ),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Theme files come from administrators, not the Portal; opened directly on
/// the OAuth origin (an SVG logo, say) they must not run script.
fn sandboxed(content_type: &'static str, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
        ],
        bytes,
    )
        .into_response()
}
