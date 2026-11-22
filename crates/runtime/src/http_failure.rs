//! One reference per HTTP request, and one log line when the server fails it.
//!
//! [`assign`] wraps every route the Trellis listener serves. It gives the
//! request an ID and runs the handler inside a scope that [`record`] and
//! [`reference`] reach, including from store work on the blocking pool (see
//! [`crate::blocking::spawn`]). A handler that answers a server-side failure
//! takes the ID with [`reference`] and shows it to the caller; once the
//! response is ready, [`assign`] writes the request's one failure line with the
//! fields RPC failures use (see `design/core/observability-patterns.md`) and
//! sends the ID back in the `Trellis-Reference` header.

use std::{
    fmt::Display,
    sync::{Arc, Mutex},
};

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};

/// The response header that carries a failed request's reference.
pub(crate) const REFERENCE_HEADER: HeaderName = HeaderName::from_static("trellis-reference");

tokio::task_local! {
    static REQUEST: Arc<HttpRequest>;
}

/// The request being answered, as its failure line describes it.
#[derive(Debug)]
pub(crate) struct HttpRequest {
    id: String,
    route: String,
    trace_id: Option<String>,
    failure: Mutex<Failure>,
}

#[derive(Debug, Default)]
struct Failure {
    /// What the server could not do, the first recorded, and each recorded
    /// `what: why`, in order.
    error: Option<String>,
    causes: Vec<String>,
    /// The error code the caller was given with the reference.
    code: Option<String>,
}

/// Middleware: answers `request` inside its own failure scope, then logs the
/// failure the handler reported, if any, under the request's reference.
pub(crate) async fn assign(request: Request, next: Next) -> Response {
    let trace_id = request
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            // version-traceid-parentid-flags; an all-zero trace is invalid.
            let trace = value.split('-').nth(1)?;
            (trace.len() == 32
                && trace.bytes().all(|byte| byte.is_ascii_hexdigit())
                && trace.bytes().any(|byte| byte != b'0'))
            .then(|| trace.to_ascii_lowercase())
        });
    // The path only: a query can carry codes and tokens.
    let current = Arc::new(HttpRequest {
        id: ulid::Ulid::generate().to_string(),
        route: format!("{} {}", request.method(), request.uri().path()),
        trace_id,
        failure: Mutex::new(Failure::default()),
    });
    let mut response = REQUEST.scope(Arc::clone(&current), next.run(request)).await;
    let failure = std::mem::take(&mut *current.failure.lock().unwrap_or_else(|e| e.into_inner()));
    let Some(code) = failure.code else {
        return response;
    };
    let cause = (!failure.causes.is_empty()).then(|| failure.causes.join("; "));
    tracing::error!(
        error_id = %current.id,
        request_id = %current.id,
        trace_id = current.trace_id.as_deref(),
        route = %current.route,
        service = "trellis",
        error_type = "UnexpectedError",
        code = %code,
        status = response.status().as_u16(),
        error = failure
            .error
            .as_deref()
            .unwrap_or("the server could not answer the request"),
        cause = cause.as_deref(),
        "request failed",
    );
    if let Ok(value) = HeaderValue::from_str(&current.id) {
        response.headers_mut().insert(REFERENCE_HEADER, value);
    }
    response
}

/// Records what the server could not do while answering the current request,
/// and why. Only the request's log line repeats it, and only when the handler
/// answers with [`reference`]: its `error` is the first thing recorded, and
/// its `cause` lists everything recorded, innermost first, up to eight.
///
/// Outside a request, such as in a background task, nothing is recorded.
pub(crate) fn record(error: impl Display, cause: impl Display) {
    let _ = REQUEST.try_with(|current| {
        let mut failure = current.failure.lock().unwrap_or_else(|e| e.into_inner());
        let error = error.to_string();
        if failure.causes.len() < 8 {
            failure.causes.push(format!("{error}: {cause}"));
        }
        failure.error.get_or_insert(error);
    });
}

/// Whether anything was [`record`]ed for the current request, for a handler
/// whose library call can succeed after part of its work failed.
pub(crate) fn recorded() -> bool {
    REQUEST
        .try_with(|current| {
            !current
                .failure
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .causes
                .is_empty()
        })
        .unwrap_or(false)
}

/// Marks the current request as failed by the server with `code` and returns
/// its reference, for the caller to be shown. The request is logged once,
/// however often this is called. Outside a request there is no reference.
pub(crate) fn reference(code: &str) -> Option<String> {
    REQUEST
        .try_with(|current| {
            let mut failure = current.failure.lock().unwrap_or_else(|e| e.into_inner());
            failure.code.get_or_insert_with(|| code.to_owned());
            current.id.clone()
        })
        .ok()
}

/// The current request's scope, to carry onto another thread.
pub(crate) fn current() -> Option<Arc<HttpRequest>> {
    REQUEST.try_with(Arc::clone).ok()
}

/// `future`, run inside the current request's scope, for a task it spawns.
pub(crate) fn carry<F: std::future::Future>(
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    match current() {
        Some(request) => futures_util::future::Either::Left(REQUEST.scope(request, future)),
        None => futures_util::future::Either::Right(future),
    }
}

/// Runs `work` inside `request`'s scope, when there is one.
pub(crate) fn within<T>(request: Option<Arc<HttpRequest>>, work: impl FnOnce() -> T) -> T {
    match request {
        Some(request) => REQUEST.sync_scope(request, work),
        None => work(),
    }
}
