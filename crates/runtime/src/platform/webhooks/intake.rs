//! The HTTP side of intake: `/webhooks/<service name>/<random id>`.

use std::io::Read as _;
use std::net::SocketAddr;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use base64::Engine as _;
use trellis_idl::{WebhookMode, WebhookVerifier};

use super::process::{Answer, Verdict};
use super::records::{NewAttempt, Outcome};
use super::request::{dropped, Check, Disposition, Matched, StoredRequest};
use super::verify::{self, Failure};
use super::{Endpoint, Webhooks};

/// Most request headers, and most header bytes, Trellis accepts.
const MAX_HEADERS: usize = 100;
const MAX_HEADER_BYTES: usize = 32 << 10;

/// The webhook routes. They never send CORS headers.
pub(crate) fn router(webhooks: Webhooks) -> Router {
    Router::new()
        .route("/webhooks/{service}/{id}", any(handle))
        .with_state(webhooks)
}

/// Error responses have an empty body, so they tell a prober nothing.
fn empty(status: StatusCode) -> Response {
    let mut response = status.into_response();
    secure(response.headers_mut());
    response
}

fn secure(headers: &mut HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
}

/// The answer for an accepted, ignored, or duplicate request.
fn answer(status: u16, answer: Option<&Answer>) -> Response {
    let Some(answer) = answer else {
        return empty(StatusCode::from_u16(status).unwrap_or(StatusCode::OK));
    };
    let mut response = Response::new(Body::from(answer.body.clone()));
    *response.status_mut() = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::OK);
    for (name, value) in &answer.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            response.headers_mut().append(name, value);
        }
    }
    // Served from the sign-in origin: run nothing, guess nothing, keep nothing.
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    secure(response.headers_mut());
    response
}

struct Refusal {
    status: StatusCode,
    outcome: &'static str,
    reason: String,
    /// Counts toward the client's failed-request budget.
    failure: bool,
    /// A verifier refused the request, rather than a check before them.
    verified: bool,
}

fn refuse(status: StatusCode, outcome: &'static str, reason: impl Into<String>) -> Refusal {
    Refusal {
        status,
        outcome,
        reason: reason.into(),
        failure: matches!(outcome, "rejected"),
        verified: false,
    }
}

/// A verifier's refusal: a verdict on the request, which endpoint health reads.
fn reject(status: StatusCode, reason: &'static str) -> Refusal {
    Refusal {
        verified: true,
        ..refuse(status, "rejected", reason)
    }
}

async fn handle(
    State(webhooks): State<Webhooks>,
    Path((service, id)): Path<(String, String)>,
    request: Request,
) -> Response {
    let started = Instant::now();
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());
    if let (Some(console), Some(host)) = (&webhooks.inner.console, host) {
        if console.names(host) {
            return empty(StatusCode::NOT_FOUND);
        }
    }
    let directory = webhooks.directory();
    let Some(endpoint) = directory
        .by_url
        .get(&id)
        .filter(|endpoint| endpoint.record.service_name == service)
        .cloned()
    else {
        webhooks.count("_unknown", "unknown");
        return empty(StatusCode::NOT_FOUND);
    };
    // A disabled deployment or endpoint stores nothing from the request.
    if !endpoint.record.deployment_enabled || !endpoint.record.endpoint_enabled {
        return empty(StatusCode::SERVICE_UNAVAILABLE);
    }
    let client = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(std::net::IpAddr::from([0, 0, 0, 0]), |info| info.0.ip());
    let attempt_id = ulid::Ulid::generate().to_string();
    let received_at_ms = super::now_ms();
    let method = request.method().clone();
    let mut reason = None;
    let (outcome, status, response, size) = match intake(
        &webhooks,
        &endpoint,
        request,
        &attempt_id,
        received_at_ms,
        client,
    )
    .await
    {
        Ok(result) => result,
        Err((refusal, size, sha)) => {
            if refusal.failure {
                webhooks
                    .inner
                    .limits
                    .failed(&endpoint.record.deployment_id, client);
            }
            let _ = webhooks
                .inner
                .records
                .insert(
                    NewAttempt {
                        attempt_id: &attempt_id,
                        deployment_id: &endpoint.record.deployment_id,
                        endpoint: &endpoint.record.endpoint,
                        received_at_ms,
                        client_address: &client.to_string(),
                        method: method.as_str(),
                        body_size: size,
                        body_sha256: &sha,
                    },
                    Outcome {
                        outcome: refusal.outcome,
                        reason: Some(refusal.reason.clone()),
                        status: Some(refusal.status.as_u16()),
                        reached_verification: refusal.verified,
                        ..Outcome::default()
                    },
                )
                .await;
            reason = Some(refusal.reason);
            let mut response = empty(refusal.status);
            match refusal.status {
                StatusCode::METHOD_NOT_ALLOWED => {
                    if let Ok(allow) =
                        HeaderValue::from_str(&endpoint.record.definition.methods.join(", "))
                    {
                        response.headers_mut().insert(header::ALLOW, allow);
                    }
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    response
                        .headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                }
                _ => {}
            }
            (refusal.outcome, refusal.status.as_u16(), response, size)
        }
    };
    webhooks.count(&endpoint.record.endpoint, outcome);
    trellis_rs::telemetry::instruments::record_family_duration(
        trellis_rs::telemetry::instruments::DurationFamily::WebhookRequest,
        started.elapsed(),
        &[
            opentelemetry::KeyValue::new(
                "trellis.webhook.endpoint",
                endpoint.record.endpoint.clone(),
            ),
            opentelemetry::KeyValue::new("trellis.outcome", outcome),
        ],
    );
    // The attempt ID is the reference Console's webhook records show.
    macro_rules! log_request {
        ($level:ident) => {
            tracing::$level!(
                attempt_id = %attempt_id,
                service = %endpoint.record.deployment_id,
                endpoint = %endpoint.record.endpoint,
                outcome,
                status,
                reason = reason.as_deref(),
                size,
                duration_ms = started.elapsed().as_millis() as u64,
                client = %client,
                "webhook request"
            )
        };
    }
    if status >= 500 {
        log_request!(warn);
    } else {
        log_request!(info);
    }
    response
}

type Refused = (Refusal, usize, String);

/// Everything from the cheap checks until Trellis answers.
async fn intake(
    webhooks: &Webhooks,
    endpoint: &Endpoint,
    request: Request,
    attempt_id: &str,
    received_at_ms: i64,
    client: std::net::IpAddr,
) -> Result<(&'static str, u16, Response, usize), Refused> {
    let definition = &endpoint.record.definition;
    let settings = &endpoint.settings;
    let none = || (0usize, String::new());
    let refused = |refusal: Refusal| {
        let (size, sha) = none();
        (refusal, size, sha)
    };
    if !definition
        .methods
        .iter()
        .any(|method| method == request.method().as_str())
    {
        return Err(refused(refuse(
            StatusCode::METHOD_NOT_ALLOWED,
            "rejected",
            "method not declared",
        )));
    }
    let header_bytes: usize = request
        .headers()
        .iter()
        .map(|(name, value)| name.as_str().len() + value.len())
        .sum();
    if request.headers().len() > MAX_HEADERS || header_bytes > MAX_HEADER_BYTES {
        return Err(refused(refuse(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "rejected",
            "too many request headers",
        )));
    }
    if !webhooks
        .inner
        .limits
        .take(endpoint.key(), settings.requests_per_second, settings.burst)
    {
        return Err(refused(refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "limited",
            "endpoint rate limit",
        )));
    }
    if webhooks.inner.limits.failing(
        &endpoint.record.deployment_id,
        client,
        settings.failures_per_minute,
    ) {
        return Err(refused(refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "limited",
            "too many failed requests from this address",
        )));
    }
    let limit =
        usize::try_from(settings.max_body.min(webhooks.inner.ceiling)).unwrap_or(usize::MAX);
    let too_large = || {
        refused(refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "rejected",
            "body over the size limit",
        ))
    };
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > limit)
    {
        return Err(too_large());
    }
    let encoding = request
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "identity");
    if encoding
        .as_ref()
        .is_some_and(|encoding| !definition.content_encodings.contains(encoding))
    {
        return Err(refused(refuse(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "rejected",
            "content encoding not declared",
        )));
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return Err(too_large());
    };
    let body = match encoding.as_deref() {
        None => bytes.to_vec(),
        Some(encoding) => {
            let mut decoded = Vec::new();
            let reader: Box<dyn std::io::Read> = if encoding == "gzip" {
                Box::new(flate2::read::GzDecoder::new(&bytes[..]))
            } else {
                Box::new(flate2::read::ZlibDecoder::new(&bytes[..]))
            };
            match reader.take(limit as u64 + 1).read_to_end(&mut decoded) {
                Ok(read) if read <= limit => decoded,
                Ok(_) => return Err(too_large()),
                Err(_) => {
                    return Err(refused(refuse(
                        StatusCode::BAD_REQUEST,
                        "rejected",
                        "body does not decode",
                    )))
                }
            }
        }
    };
    let query = parts.uri.query().unwrap_or_default().to_owned();
    let path = parts.uri.path().to_owned();
    let mut stored = StoredRequest {
        attempt_id: attempt_id.to_owned(),
        // A refused request is stored only to keep it.
        disposition: match definition.mode {
            WebhookMode::Async => Disposition::Queued,
            WebhookMode::Sync => Disposition::Archived,
        },
        deployment_id: endpoint.record.deployment_id.clone(),
        endpoint: endpoint.record.endpoint.clone(),
        received_at_ms,
        client_address: client.to_string(),
        method: parts.method.to_string(),
        url: if query.is_empty() {
            format!("{}{path}", webhooks.inner.origin)
        } else {
            format!("{}{path}?{query}", webhooks.inner.origin)
        },
        path,
        query,
        headers: parts
            .headers
            .iter()
            .filter(|(name, _)| !dropped(name.as_str()))
            .filter(|(name, _)| encoding.is_none() || *name != header::CONTENT_ENCODING)
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
        body,
        checks: Vec::new(),
        policy: endpoint.policy.clone(),
        generations: endpoint.generations(),
        matched: Vec::new(),
    };
    let size = stored.body.len();
    let sha = stored.body_sha256();
    let fail = |refusal: Refusal| (refusal, size, sha.clone());
    let records = &webhooks.inner.records;
    let (deployment, endpoint_name) = (&endpoint.record.deployment_id, &endpoint.record.endpoint);
    // Credentials are checked as the request arrives, then removed, so they
    // never reach the intake stream, the record, or the event. The secret
    // before a change is accepted while the change overlap lasts.
    for (check, verifier) in definition.verifiers.iter().enumerate() {
        match verifier {
            WebhookVerifier::Address { ranges } => {
                if !endpoint.allows(ranges, client) {
                    return Err(fail(reject(
                        StatusCode::FORBIDDEN,
                        "client address outside the allowed ranges",
                    )));
                }
                stored.checks.push(Check {
                    kind: "address".into(),
                    key: None,
                });
            }
            WebhookVerifier::Token {
                secret,
                header: name,
                query,
                prefix,
            } => {
                let secrets = endpoint.secrets(secret, received_at_ms);
                if secrets.current.is_none() {
                    return Err(fail(refuse(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        format!("secret {secret} not set"),
                    )));
                }
                let presented = match (name, query) {
                    (Some(name), _) => parts
                        .headers
                        .get(name.as_str())
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| match prefix {
                            Some(prefix) => value.strip_prefix(prefix.as_str()),
                            None => Some(value),
                        })
                        .map(str::to_owned),
                    (None, Some(query)) => url::form_urlencoded::parse(stored.query.as_bytes())
                        .find(|(name, _)| name == query.as_str())
                        .map(|(_, value)| value.into_owned()),
                    (None, None) => None,
                };
                let matched = presented.and_then(|presented| {
                    secrets.matching(presented.as_bytes(), |secret| secret.as_bytes().to_vec())
                });
                strip(&mut stored, name.as_deref(), query.as_deref());
                let Some(matched) = matched else {
                    return Err(fail(reject(
                        StatusCode::UNAUTHORIZED,
                        "token does not match",
                    )));
                };
                stored.matched.push(Matched {
                    check,
                    field: secret.clone(),
                    generation: endpoint.matched_generation(secret, matched),
                    key: matched,
                });
                let captured = stored.generations.get(secret).copied();
                if let Some(generation) = endpoint.match_generation(secret, captured) {
                    let _ = records
                        .note_match(
                            deployment,
                            endpoint_name,
                            secret,
                            generation,
                            matched,
                            received_at_ms,
                        )
                        .await;
                }
                stored.checks.push(Check {
                    kind: "token".into(),
                    key: Some(matched.as_str().into()),
                });
            }
            WebhookVerifier::Basic { username, password } => {
                let secrets = endpoint.secrets(password, received_at_ms);
                let (Some(user), Some(_)) = (endpoint.text(username), &secrets.current) else {
                    return Err(fail(refuse(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        format!("setting {username} or secret {password} not set"),
                    )));
                };
                let presented = parts
                    .headers
                    .get(header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Basic "))
                    .and_then(|value| {
                        base64::engine::general_purpose::STANDARD
                            .decode(value.trim())
                            .ok()
                    })
                    .unwrap_or_default();
                strip(&mut stored, Some("authorization"), None);
                let Some(matched) =
                    secrets.matching(&presented, |secret| format!("{user}:{secret}").into_bytes())
                else {
                    return Err(fail(reject(
                        StatusCode::UNAUTHORIZED,
                        "basic credentials do not match",
                    )));
                };
                stored.matched.push(Matched {
                    check,
                    field: password.clone(),
                    generation: endpoint.matched_generation(password, matched),
                    key: matched,
                });
                let captured = stored.generations.get(password).copied();
                if let Some(generation) = endpoint.match_generation(password, captured) {
                    let _ = records
                        .note_match(
                            deployment,
                            endpoint_name,
                            password,
                            generation,
                            matched,
                            received_at_ms,
                        )
                        .await;
                }
                stored.checks.push(Check {
                    kind: "basic".into(),
                    key: Some(matched.as_str().into()),
                });
            }
            WebhookVerifier::None => stored.checks.push(Check {
                kind: "none".into(),
                key: None,
            }),
            WebhookVerifier::Hmac(_) => {}
        }
    }
    let client_address = stored.client_address.clone();
    let new = || NewAttempt {
        attempt_id,
        deployment_id: &endpoint.record.deployment_id,
        endpoint: &endpoint.record.endpoint,
        received_at_ms,
        client_address: &client_address,
        method: parts.method.as_str(),
        body_size: size,
        body_sha256: &sha,
    };
    match definition.mode {
        WebhookMode::Async => {
            // `hmac` needs only the bytes in hand, so it runs before the
            // answer too: a wrong secret gets 401 rather than a 2xx that
            // hides it. Only delivery and the decide handler wait.
            for verifier in &definition.verifiers {
                let WebhookVerifier::Hmac(hmac) = verifier else {
                    continue;
                };
                match verify::hmac(&stored, hmac, &endpoint.secrets(&hmac.key, received_at_ms)) {
                    Ok(_) => {}
                    Err(Failure::Unset(name)) => {
                        return Err(fail(refuse(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "unavailable",
                            format!("secret {name} not set"),
                        )));
                    }
                    Err(Failure::Rejected(reason)) => {
                        let _ = records
                            .insert(
                                new(),
                                Outcome {
                                    outcome: "rejected",
                                    reason: Some(reason),
                                    status: Some(401),
                                    reached_verification: true,
                                    ..Outcome::default()
                                },
                            )
                            .await;
                        webhooks
                            .inner
                            .limits
                            .failed(&endpoint.record.deployment_id, client);
                        // Kept, as a refused `sync` request is, so an
                        // administrator can see what failed and verify it
                        // again once the secret is fixed.
                        stored.disposition = Disposition::Archived;
                        if let Ok(sequence) = webhooks.store(&stored).await {
                            let _ = records.set_stream_sequence(attempt_id, sequence).await;
                        }
                        return Ok(("rejected", 401, empty(StatusCode::UNAUTHORIZED), size));
                    }
                }
            }
            let _ = records
                .insert(
                    new(),
                    Outcome {
                        outcome: "pending",
                        status: Some(definition.status),
                        ..Outcome::default()
                    },
                )
                .await;
            match webhooks.store(&stored).await {
                Ok(sequence) => {
                    let _ = records.set_stream_sequence(attempt_id, sequence).await;
                    Ok((
                        "pending",
                        definition.status,
                        empty(StatusCode::from_u16(definition.status).unwrap_or(StatusCode::OK)),
                        size,
                    ))
                }
                Err(error) => {
                    tracing::warn!(%attempt_id, %error, "webhook intake stream refused a request");
                    let _ = records
                        .set_outcome(
                            attempt_id,
                            Outcome {
                                outcome: "unavailable",
                                reason: Some("intake storage unavailable".into()),
                                status: Some(503),
                                ..Outcome::default()
                            },
                        )
                        .await;
                    Ok((
                        "unavailable",
                        503,
                        empty(StatusCode::SERVICE_UNAVAILABLE),
                        size,
                    ))
                }
            }
        }
        WebhookMode::Sync => {
            let verdict = webhooks
                .verify(endpoint, &stored, WebhookMode::Sync, 1)
                .await;
            let outcome = verdict.outcome(false);
            let label = outcome.outcome;
            let status = outcome.status.unwrap_or(definition.status);
            let _ = records.insert(new(), outcome).await;
            if matches!(verdict, Verdict::Rejected { .. }) {
                webhooks
                    .inner
                    .limits
                    .failed(&endpoint.record.deployment_id, client);
                // Kept so an administrator can see what failed and verify it again.
                if let Ok(sequence) = webhooks.store(&stored).await {
                    let _ = records.set_stream_sequence(attempt_id, sequence).await;
                }
            }
            let response = match &verdict {
                Verdict::Accepted {
                    answer: given,
                    status,
                    ..
                }
                | Verdict::Ignored {
                    answer: given,
                    status,
                    ..
                }
                | Verdict::Duplicate {
                    answer: given,
                    status,
                    ..
                } => answer(*status, given.as_ref()),
                _ => empty(StatusCode::from_u16(status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE)),
            };
            Ok((label, status, response, size))
        }
    }
}

/// Remove the header or query parameter that carried a credential.
fn strip(stored: &mut StoredRequest, header: Option<&str>, query: Option<&str>) {
    if let Some(header) = header {
        let header = header.to_ascii_lowercase();
        stored.headers.retain(|(name, _)| *name != header);
    }
    if let Some(query) = query {
        stored.remove_query_parameter(query);
    }
}
