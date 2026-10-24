//! Same-origin NATS WebSocket for browsers.
//!
//! When configured, the application listener forwards WebSocket upgrades on
//! [`PATH`] to the broker's plaintext WebSocket listener and then relays bytes
//! both ways. Browsers reach NATS through the Console's own host and
//! certificate, so one trusted certificate covers both.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Path on the application origin that carries the NATS WebSocket.
pub const PATH: &str = "/nats";

/// Bound on the broker's upgrade response head.
const MAX_RESPONSE_HEAD: usize = 16 * 1024;

/// Request headers a WebSocket upgrade needs at the broker.
const FORWARDED_REQUEST: [header::HeaderName; 6] = [
    header::UPGRADE,
    header::SEC_WEBSOCKET_KEY,
    header::SEC_WEBSOCKET_VERSION,
    header::SEC_WEBSOCKET_PROTOCOL,
    header::SEC_WEBSOCKET_EXTENSIONS,
    header::ORIGIN,
];

/// Response headers that complete the upgrade at the browser.
const FORWARDED_RESPONSE: [header::HeaderName; 4] = [
    header::UPGRADE,
    header::SEC_WEBSOCKET_ACCEPT,
    header::SEC_WEBSOCKET_PROTOCOL,
    header::SEC_WEBSOCKET_EXTENSIONS,
];

/// Routes [`PATH`] to the broker WebSocket listener at `upstream` (`host:port`).
pub(crate) fn router(upstream: String) -> Router {
    Router::new().route(PATH, get(proxy)).with_state(upstream)
}

async fn proxy(State(upstream): State<String>, mut request: Request) -> Response {
    let headers = request.headers();
    if !headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
    {
        return (StatusCode::UPGRADE_REQUIRED, "WebSocket upgrade required").into_response();
    }
    let mut head = format!("GET / HTTP/1.1\r\nHost: {upstream}\r\nConnection: Upgrade\r\n");
    for name in FORWARDED_REQUEST {
        for value in headers.get_all(&name) {
            let Ok(value) = value.to_str() else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str("\r\n");

    let mut broker = match tokio::net::TcpStream::connect(&upstream).await {
        Ok(broker) => broker,
        Err(error) => {
            tracing::warn!(upstream, error = %error, "NATS WebSocket upstream unavailable");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    if broker.write_all(head.as_bytes()).await.is_err() {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    // The broker sends INFO right after its 101, so bytes past the response
    // head belong to the relayed stream.
    let mut received = Vec::new();
    let end = loop {
        if let Some(end) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if received.len() > MAX_RESPONSE_HEAD {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        let mut chunk = [0_u8; 4096];
        match broker.read(&mut chunk).await {
            Ok(0) | Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
            Ok(read) => received.extend_from_slice(&chunk[..read]),
        }
    };
    let mut parsed = [httparse::EMPTY_HEADER; 32];
    let mut response_head = httparse::Response::new(&mut parsed);
    if !matches!(
        response_head.parse(&received[..end]),
        Ok(httparse::Status::Complete(_))
    ) || response_head.code != Some(101)
    {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    for parsed in response_head.headers.iter() {
        if let Some(name) = FORWARDED_RESPONSE
            .iter()
            .find(|name| name.as_str().eq_ignore_ascii_case(parsed.name))
        {
            let Ok(value) = HeaderValue::from_bytes(parsed.value) else {
                return StatusCode::BAD_GATEWAY.into_response();
            };
            response_headers.append(name.clone(), value);
        }
    }
    let early = received[end..].to_vec();
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let mut browser = hyper_util::rt::TokioIo::new(upgraded);
        if browser.write_all(&early).await.is_ok() {
            let _ = tokio::io::copy_bidirectional(&mut browser, &mut broker).await;
        }
    });
    (StatusCode::SWITCHING_PROTOCOLS, response_headers).into_response()
}
