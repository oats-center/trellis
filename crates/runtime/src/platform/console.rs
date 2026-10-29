//! Bundled Console serving on the application origin, never the OAuth listener.

use crate::{supervisor::RuntimeError, RuntimeConfig};
use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

static ASSETS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub(super) fn router(config: &RuntimeConfig) -> Result<Router, RuntimeError> {
    let registration = super::auth::console_registration(&config.public_origin())
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let oauth = config
        .resolve_oauth_listener()
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    let base = serde_json::json!({"authUrl":oauth.origin,"clientId":super::auth::CONSOLE_CLIENT_ID,
        "callbackUri":registration.redirect_uris[0],"loopbackDevelopment":matches!(
            oauth.transport,
            crate::config::ListenerTransport::LoopbackDevelopment
        ),
        // Every Console user sees a banner while the installation serves its
        // own generated certificates.
        "generatedTls":crate::tls::serves_generated_certificate(config,config.http_transport())});
    // RFC 9728: the Console's address names its sign-in, so native programs
    // accept the address people use in a browser.
    let resource = serde_json::json!({
        "resource": config.public_origin(),
        "authorization_servers": [oauth.origin],
    });
    Ok(Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || std::future::ready(axum::Json(resource.clone()))),
        )
        .route(
            "/assets/web/runtime-config.js",
            get(move || {
                let script = format!("globalThis.__TRELLIS_RUNTIME_CONFIG__ = {base};\n");
                async move {
                    (
                        [
                            (
                                header::CONTENT_TYPE,
                                "application/javascript; charset=utf-8",
                            ),
                            (header::CACHE_CONTROL, "no-store"),
                            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                        ],
                        script,
                    )
                }
            }),
        )
        .fallback(asset))
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.split('/').any(|part| part == "." || part == "..") || path.contains('\\') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let selected = ASSETS.iter().find(|(name, _)| *name == path).or_else(|| {
        if !path.rsplit('/').next().unwrap_or("").contains('.') {
            ASSETS.iter().find(|(name, _)| *name == "200.html")
        } else {
            None
        }
    });
    let Some((name, bytes)) = selected else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        *bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn console_http_bootstrap_uses_application_callback_and_isolated_oauth_origin() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config:RuntimeConfig=serde_json::from_value(serde_json::json!({
            "http":{"public_origin":origin},
            "auth":{"instance_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","oauth_storage_key_file":"./auth/oauth.key",
                "oauth":{"origin":"http://localhost:3001","bind_address":"127.0.0.1","port":3001,
                    "transport":{"mode":"loopback_development"}}}
        })).unwrap();
        let app = router(&config).unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let shutdown = stop.clone();
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        }));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .get(format!("{origin}/assets/web/runtime-config.js"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = response.text().await.unwrap();
        let value: serde_json::Value = serde_json::from_str(
            body.strip_prefix("globalThis.__TRELLIS_RUNTIME_CONFIG__ = ")
                .unwrap()
                .trim_end()
                .trim_end_matches(';'),
        )
        .unwrap();
        assert_eq!(value["callbackUri"], format!("{origin}/login"));
        assert_eq!(value["authUrl"], "http://localhost:3001");
        assert_eq!(value["clientId"], super::super::auth::CONSOLE_CLIENT_ID);
        assert!(client
            .get(format!("{origin}/login"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        assert_eq!(
            client
                .get(format!("{origin}/missing.js"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        stop.cancel();
        server.await.unwrap();
    }
}
