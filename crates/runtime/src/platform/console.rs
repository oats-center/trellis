//! Bundled Console, served at `/console` on the Trellis origin, or on an
//! origin of its own that the same listener answers.

use crate::{supervisor::RuntimeError, RuntimeConfig};
use axum::{
    extract::Request,
    http::{header, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};

static ASSETS: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub(super) fn router(config: &RuntimeConfig) -> Result<Router, RuntimeError> {
    if !config.console_enabled() {
        return Ok(Router::new());
    }
    let platform = |error: &dyn std::fmt::Display| RuntimeError::Platform(error.to_string());
    let origin = config
        .resolve_console_origin()
        .map_err(|error| platform(&error))?;
    let registration =
        super::auth::console_registration(&origin).map_err(|error| platform(&error))?;
    let base = serde_json::json!({"authUrl":config.public_origin(),"clientId":super::auth::CONSOLE_CLIENT_ID,
        "callbackUri":registration.redirect_uris[0],
        // Every Console user sees a banner while the installation serves its
        // own generated certificates.
        "generatedTls":crate::tls::serves_generated_certificate(config,config.http_transport())});
    let mut router = Router::new()
        .route("/", get(|| std::future::ready(Redirect::to("/console/"))))
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
        .route("/assets/{*path}", get(asset))
        .route("/console", get(asset))
        .route("/console/", get(asset))
        .route("/console/{*path}", get(asset));
    let trellis = url::Url::parse(&config.public_origin()).map_err(|error| platform(&error))?;
    let own = url::Url::parse(&origin).map_err(|error| platform(&error))?;
    if own.origin() == trellis.origin() {
        return Ok(router);
    }
    // RFC 9728: the Console's own address names its sign-in, so native
    // programs accept the address people use in a browser.
    let resource = serde_json::json!({
        "resource": origin,
        "authorization_servers": [config.public_origin()],
    });
    router = router.route(
        "/.well-known/oauth-protected-resource",
        get(move || std::future::ready(axum::Json(resource.clone()))),
    );
    // Console pages run only on the Console's origin, apart from sign-in.
    // The Trellis hostname sends people there instead.
    Ok(router.layer(axum::middleware::from_fn(
        move |request: Request, next: Next| {
            let own = own.clone();
            async move {
                let host = request
                    .uri()
                    .authority()
                    .map(|authority| authority.as_str().to_owned())
                    .or_else(|| {
                        request
                            .headers()
                            .get(header::HOST)
                            .and_then(|host| host.to_str().ok())
                            .map(str::to_owned)
                    });
                let on_console = host.is_some_and(|host| {
                    url::Url::parse(&format!("{}://{host}", own.scheme()))
                        .is_ok_and(|actual| actual.origin() == own.origin())
                });
                if on_console {
                    return next.run(request).await;
                }
                let path = request.uri().path();
                if path == "/" || path == "/console" || path.starts_with("/console/") {
                    let target = match request.uri().path_and_query() {
                        _ if path == "/" => "/console/",
                        Some(target) => target.as_str(),
                        None => path,
                    };
                    return match own.join(target) {
                        Ok(url) => Redirect::to(url.as_str()).into_response(),
                        Err(_) => StatusCode::NOT_FOUND.into_response(),
                    };
                }
                StatusCode::NOT_FOUND.into_response()
            }
        },
    )))
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

    /// Serves the Console router for `config` on `listener` until dropped.
    fn serve(
        config: serde_json::Value,
        listener: tokio::net::TcpListener,
    ) -> tokio_util::task::AbortOnDropHandle<()> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config: RuntimeConfig = serde_json::from_value(config).unwrap();
        let app = router(&config).unwrap();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }))
    }

    fn callback(body: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(
            body.strip_prefix("globalThis.__TRELLIS_RUNTIME_CONFIG__ = ")
                .unwrap()
                .trim_end()
                .trim_end_matches(';'),
        )
        .unwrap();
        assert_eq!(value["clientId"], super::super::auth::CONSOLE_CLIENT_ID);
        value["callbackUri"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn console_is_served_at_console_on_the_trellis_origin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let trellis = format!("http://{}", listener.local_addr().unwrap());
        let _server = serve(
            serde_json::json!({
                "http":{"public_origin":trellis},
                "auth":{"instance_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","oauth_storage_key_file":"./auth/oauth.key"}
            }),
            listener,
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let get = |path: &str| client.get(format!("{trellis}{path}")).send();
        let landing = get("/").await.unwrap();
        assert_eq!(landing.headers()[header::LOCATION], "/console/");
        for page in ["/console/", "/console/admin/users"] {
            assert!(get(page).await.unwrap().status().is_success(), "{page}");
        }
        let script = get("/assets/web/runtime-config.js").await.unwrap();
        assert_eq!(
            callback(&script.text().await.unwrap()),
            format!("{trellis}/console/login")
        );
        assert_eq!(
            get("/assets/web/missing.js").await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_turned_off_console_serves_nothing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let trellis = format!("http://{}", listener.local_addr().unwrap());
        let _server = serve(
            serde_json::json!({
                "http":{"public_origin":trellis},
                "console":{"enabled":false},
                "auth":{"instance_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","oauth_storage_key_file":"./auth/oauth.key"}
            }),
            listener,
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        for path in ["/", "/console/", "/assets/web/runtime-config.js"] {
            let response = client.get(format!("{trellis}{path}")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    /// The Console's own hostname serves its pages; the Trellis hostname,
    /// where sign-in runs, only sends people there.
    #[tokio::test]
    async fn console_on_its_own_origin_runs_only_there() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let trellis = format!("http://localhost:{port}");
        let console = format!("http://127.0.0.1:{port}");
        let _server = serve(
            serde_json::json!({
                "http":{"public_origin":trellis},
                "console":{"origin":console},
                "auth":{"instance_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","oauth_storage_key_file":"./auth/oauth.key"}
            }),
            listener,
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        // Requests reach the same socket; only the Host header differs.
        let get = |host: &str, path: &str| {
            client
                .get(format!("{console}{path}"))
                .header(header::HOST, host)
                .send()
        };
        let trellis_host = format!("localhost:{port}");
        let console_host = format!("127.0.0.1:{port}");
        let moved = get(&trellis_host, "/console/admin?tab=1").await.unwrap();
        assert_eq!(
            moved.headers()[header::LOCATION],
            format!("{console}/console/admin?tab=1").as_str()
        );
        let landing = get(&trellis_host, "/").await.unwrap();
        assert_eq!(
            landing.headers()[header::LOCATION],
            format!("{console}/console/").as_str()
        );
        assert_eq!(
            get(&trellis_host, "/assets/web/runtime-config.js")
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(get(&console_host, "/console/admin")
            .await
            .unwrap()
            .status()
            .is_success());
        let script = get(&console_host, "/assets/web/runtime-config.js")
            .await
            .unwrap();
        assert_eq!(
            callback(&script.text().await.unwrap()),
            format!("{console}/console/login")
        );
        let resource: serde_json::Value =
            get(&console_host, "/.well-known/oauth-protected-resource")
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(resource["authorization_servers"][0], trellis.as_str());
    }
}
