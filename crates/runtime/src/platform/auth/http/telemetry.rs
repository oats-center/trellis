//! HTTP server observation for the Auth/bootstrap listener.
//!
//! One middleware records the catalog duration, method, actual response
//! status, and a bounded route label. Static browser assets and unmatched
//! routes use fixed labels; telemetry-ingest paths are suppressed because they
//! are a proxy concern, not Auth handlers. Streaming responses record
//! response-header latency only, so a long asset body never inflates the
//! request histogram.

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use trellis_rs::telemetry::instruments::{self, DurationFamily, RouteFamily};
use trellis_rs::telemetry::lifecycle::Observation;
use trellis_rs::telemetry::KeyValue;

/// Fixed label for built-in browser asset and page routes.
const STATIC_ROUTE: &str = "_static";
/// Fixed label for requests that matched no registered route.
const UNMATCHED_ROUTE: &str = "_unmatched";

/// Whether this request is browser OTLP ingest rather than an Auth handler.
fn is_telemetry_ingest(path: &str) -> bool {
    path.starts_with("/otel") || path.ends_with("/v1/traces") || path.ends_with("/v1/metrics")
}

/// Whether one request path belongs to the built-in static browser routes.
fn is_static_browser_path(path: &str) -> bool {
    path == "/login"
        || path.starts_with("/login/")
        || path.starts_with("/assets/login/")
        || path == "/console"
        || path.starts_with("/console/")
}

/// Bounded route label for one request.
fn route_label(path: &str, template: Option<&str>) -> &'static str {
    let Some(template) = template else {
        return UNMATCHED_ROUTE;
    };
    if is_static_browser_path(path) {
        return STATIC_ROUTE;
    }
    instruments::route_token(RouteFamily::Http, template)
}

/// Method dimension limited to the catalog values.
fn method_label(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "GET",
        axum::http::Method::POST => "POST",
        axum::http::Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// Outcome classification from the actual response status.
fn status_outcome(status: u16) -> &'static str {
    match status {
        100..=399 => "ok",
        401 | 403 => "denied",
        408 | 504 => "timeout",
        429 => "rate_limited",
        400..=499 => "invalid",
        503 => "unavailable",
        _ => "error",
    }
}

/// Observes one HTTP response without changing it.
pub(crate) async fn observe_http(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    if is_telemetry_ingest(&path) {
        return next.run(request).await;
    }
    let route = route_label(
        &path,
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
    );
    let method = method_label(request.method());
    let attributes = vec![
        KeyValue::new("http.route", route),
        KeyValue::new("http.request.method", method),
    ];
    let span = tracing::info_span!(
        "trellis.http.server",
        "http.route" = route,
        "http.request.method" = method,
    );
    let observation = Observation::start(DurationFamily::HttpServer, attributes, "cancelled");
    // Response-header latency: the body may stream long after this returns.
    let response = next.run(request).instrument(span.clone()).await;
    let status = response.status().as_u16();
    let outcome = status_outcome(status);
    span.record("http.response.status_code", i64::from(status));
    span.record("trellis.outcome", outcome);
    observation.finish_with(
        outcome,
        &[KeyValue::new(
            "http.response.status_code",
            i64::from(status),
        )],
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmatched_and_static_labels_are_fixed() {
        assert_eq!(route_label("/nope", None), UNMATCHED_ROUTE);
        assert_eq!(
            route_label("/console/app", Some("/console/*rest")),
            STATIC_ROUTE
        );
        assert_eq!(route_label("/console", Some("/console")), STATIC_ROUTE);
    }

    #[test]
    fn telemetry_ingest_paths_are_suppressed() {
        assert!(is_telemetry_ingest("/otel/v1/traces"));
        assert!(is_telemetry_ingest("/v1/metrics"));
        assert!(!is_telemetry_ingest("/login"));
    }

    #[test]
    fn outcomes_follow_the_catalog_categories() {
        assert_eq!(status_outcome(200), "ok");
        assert_eq!(status_outcome(403), "denied");
        assert_eq!(status_outcome(429), "rate_limited");
        assert_eq!(status_outcome(404), "invalid");
        assert_eq!(status_outcome(503), "unavailable");
        assert_eq!(status_outcome(500), "error");
    }

    #[test]
    fn methods_are_bounded() {
        assert_eq!(method_label(&axum::http::Method::GET), "GET");
        assert_eq!(method_label(&axum::http::Method::PATCH), "OTHER");
    }
}
