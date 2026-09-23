use async_nats::header::HeaderMap;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, Stream, StreamExt};

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use super::error::merge_context;
use super::{AuthenticatedRouter, RequestContext, RequestValidator, Router, ServerError};
use crate::telemetry::instruments::{self, DurationFamily, UpDownFamily};
use crate::telemetry::lifecycle::Observation;
use crate::telemetry::propagation;
use crate::telemetry::KeyValue;

static ERROR_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Decoded request message consumed by the host dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(InboundRequest), "`.")]
pub struct InboundRequest {
    #[doc = concat!("The `", stringify!(subject), "` value.")]
    pub subject: String,
    #[doc = concat!("The `", stringify!(payload), "` value.")]
    pub payload: Bytes,
    #[doc = concat!("The `", stringify!(reply_to), "` value.")]
    pub reply_to: Option<String>,
    #[doc = concat!("The `", stringify!(context), "` value.")]
    pub context: RequestContext,
}

/// Outbound response message emitted by the host dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(OutboundReply), "`.")]
pub struct OutboundReply {
    #[doc = concat!("The `", stringify!(reply_to), "` value.")]
    pub reply_to: String,
    #[doc = concat!("The `", stringify!(payload), "` value.")]
    pub payload: Bytes,
    #[doc = concat!("The `", stringify!(is_error), "` value.")]
    pub is_error: bool,
}

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<Bytes, ServerError>> + Send>>;

#[doc = concat!("Public Trellis value set `", stringify!(HandlerResponse), "`.")]
pub enum HandlerResponse {
    Frames(Vec<Bytes>),
    Error(Bytes),
    /// One verified live reservation ready to hand to the connection manager.
    ///
    /// The request loop publishes exactly one signed offer for this response;
    /// it never enters an infinite reply loop. A finite-dispatch handler that
    /// cannot hand ownership to a live manager fails closed instead of
    /// collecting an endless stream into a vector.
    LivePrepared(Box<LivePreparedResponse>),
}

/// One prepared live reservation produced by a live-capable route.
pub struct LivePreparedResponse {
    /// Signed offer body published as the one finite opening reply.
    pub offer: Bytes,
    /// Provider proof headers that authenticate the offer's exact bytes.
    pub headers: async_nats::HeaderMap,
    /// The connection's live manager that now owns the reservation.
    pub manager: std::sync::Arc<crate::live::manager::LiveSessionManager>,
    /// The exact accepted opening request this offer answers.
    pub request_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[doc = concat!("Public Trellis data type `", stringify!(ErrorAnnotationContext), "`.")]
pub struct ErrorAnnotationContext {
    request_id: Option<String>,
    trace_id: Option<String>,
    service: Option<String>,
    contract_id: Option<String>,
    contract_digest: Option<String>,
    method: Option<String>,
    live: Option<String>,
    operation: Option<String>,
}

impl ErrorAnnotationContext {
    fn from_request<H>(subject: &str, context: &RequestContext, handler: &H) -> Self
    where
        H: RequestHandler + ?Sized,
    {
        let mut annotations = Self {
            request_id: context.request_id.clone(),
            trace_id: context
                .traceparent
                .as_deref()
                .and_then(trace_id_from_traceparent)
                .map(ToString::to_string),
            service: handler.handler_service_name().map(ToString::to_string),
            contract_id: handler.handler_contract_id().map(ToString::to_string),
            contract_digest: handler.handler_contract_digest().map(ToString::to_string),
            ..Self::default()
        };

        annotations.set_surface(subject);
        annotations
    }

    fn context_map(&self) -> Map<String, Value> {
        let mut context = Map::new();
        insert_string(&mut context, "requestId", self.request_id.as_deref());
        insert_string(&mut context, "service", self.service.as_deref());
        insert_string(&mut context, "contractId", self.contract_id.as_deref());
        insert_string(
            &mut context,
            "contractDigest",
            self.contract_digest.as_deref(),
        );
        insert_string(&mut context, "method", self.method.as_deref());
        insert_string(&mut context, "live", self.live.as_deref());
        insert_string(&mut context, "operation", self.operation.as_deref());
        context
    }

    fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    fn set_surface(&mut self, subject: &str) {
        if let Some(method) = surface_key(subject, "rpc") {
            self.method = Some(method.to_string());
        } else if let Some(live) = surface_key(subject, "live") {
            self.live = Some(live.to_string());
        } else if let Some(live) = surface_key(subject, "lives") {
            self.live = Some(live.to_string());
        } else if let Some(operation) = surface_key(subject, "operations") {
            self.operation = Some(trim_operation_control(operation).to_string());
        } else if let Some(operation) = surface_key(subject, "op") {
            self.operation = Some(trim_operation_control(operation).to_string());
        }
    }
}

/// Async request handler trait used by host request loops.
pub trait RequestHandler: Send + Sync {
    fn handler_service_name(&self) -> Option<&str> {
        None
    }

    fn handler_contract_id(&self) -> Option<&str> {
        None
    }

    fn handler_contract_digest(&self) -> Option<&str> {
        None
    }

    /// Bounded registered route token for one inbound subject, when routed.
    ///
    /// The token is interned at registration; it is never parsed from the
    /// request subject, and unrecognized input reports `_unknown`.
    fn route_token(&self, _subject: &str) -> Option<&'static str> {
        None
    }

    /// Whether one inbound subject is a registered unary request/reply RPC.
    ///
    /// Handlers backed by a router answer from the registered surface kind so a
    /// Live or Operation route is never a unary request. Other handlers default
    /// to their declared route token, which is request/reply by construction.
    fn is_unary_rpc_route(&self, subject: &str) -> bool {
        self.route_token(subject).is_some()
    }

    /// Whether one inbound subject is a registered live observation route.
    ///
    /// The no-reflection admission rule is selected from the registered route
    /// surface, never from attacker-controlled payload bytes.
    fn is_live_route(&self, _subject: &str) -> bool {
        false
    }

    fn handle<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Bytes, ServerError>>;

    fn handle_frames<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Vec<Bytes>, ServerError>> {
        Box::pin(async move {
            match self.handle_response(subject, payload, context).await? {
                HandlerResponse::Frames(frames) => Ok(frames),
                HandlerResponse::Error(payload) => Ok(vec![payload]),
                HandlerResponse::LivePrepared(_) => Err(ServerError::Nats(
                    "a live response requires a live owner and cannot be collected".to_owned(),
                )),
            }
        })
    }

    fn handle_response<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<HandlerResponse, ServerError>> {
        Box::pin(async move {
            Ok(HandlerResponse::Frames(vec![
                self.handle(subject, payload, context).await?,
            ]))
        })
    }
}

impl RequestHandler for Router {
    fn route_token(&self, subject: &str) -> Option<&'static str> {
        Router::route_token(self, subject)
    }

    fn is_unary_rpc_route(&self, subject: &str) -> bool {
        Router::is_unary_rpc_route(self, subject)
    }

    fn is_live_route(&self, subject: &str) -> bool {
        Router::is_live_route(self, subject)
    }

    fn handle<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
        Box::pin(async move { self.handle_request(subject, payload, context).await })
    }

    fn handle_frames<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Vec<Bytes>, ServerError>> {
        Box::pin(async move { self.handle_request_frames(subject, payload, context).await })
    }

    fn handle_response<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<HandlerResponse, ServerError>> {
        Box::pin(async move {
            self.handle_request_response(subject, payload, context)
                .await
        })
    }
}

impl<V> RequestHandler for AuthenticatedRouter<V>
where
    V: RequestValidator + 'static,
{
    fn route_token(&self, subject: &str) -> Option<&'static str> {
        AuthenticatedRouter::route_token(self, subject)
    }

    fn is_unary_rpc_route(&self, subject: &str) -> bool {
        AuthenticatedRouter::is_unary_rpc_route(self, subject)
    }

    fn is_live_route(&self, subject: &str) -> bool {
        AuthenticatedRouter::is_live_route(self, subject)
    }

    fn handle<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
        Box::pin(async move { self.handle_request(subject, payload, context).await })
    }

    fn handle_frames<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<Vec<Bytes>, ServerError>> {
        Box::pin(async move { self.handle_request_frames(subject, payload, context).await })
    }

    fn handle_response<'a>(
        &'a self,
        subject: &'a str,
        payload: Bytes,
        context: RequestContext,
    ) -> BoxFuture<'a, Result<HandlerResponse, ServerError>> {
        Box::pin(async move {
            self.handle_request_response(subject, payload, context)
                .await
        })
    }
}

/// Decode one inbound NATS message into host request fields.
#[doc = concat!("Trellis API operation `", stringify!(decode_nats_request), "`.")]
/// Reads one W3C trace header only when exactly one valid value is present.
///
/// Absent, duplicated, or malformed values yield `None`, which makes the
/// request a new local root without rejecting the business request.
fn single_trace_header(message: &async_nats::Message, name: &str) -> Option<String> {
    let headers = message.headers.as_ref()?;
    let mut matching = headers
        .iter()
        .filter(|(key, _)| key.to_string().eq_ignore_ascii_case(name));
    let (_, values) = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    // Exactly one value must be present; duplicates within a repeated header
    // are rejected the same way as a repeated header name.
    let mut values = values.iter();
    let value = values.next()?.as_str().to_owned();
    if values.next().is_some() {
        return None;
    }
    if name == "traceparent" {
        crate::telemetry::propagation::validate_traceparent(&value).then_some(value)
    } else {
        crate::telemetry::propagation::validate_tracestate(&value).then_some(value)
    }
}

pub fn decode_nats_request(message: &async_nats::Message) -> InboundRequest {
    let subject = message.subject.to_string();
    let reply_to = message.reply.as_ref().map(ToString::to_string);
    let session_key = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("session-key"))
        .map(|value| value.as_str().to_string());
    let proof = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("proof"))
        .map(|value| value.as_str().to_string());
    let authorization_context = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("authorization-context"))
        .map(|value| value.as_str().to_string());
    let iat = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("iat"))
        .and_then(|value| value.as_str().parse::<i64>().ok());
    let request_id = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("request-id"))
        .map(|value| value.as_str().to_string());
    // Trace context is validated at the real carrier boundary while the
    // original header set is available: `HeaderMap::get` returns only the
    // first value, so duplicates must be rejected here rather than silently
    // selecting one.
    let traceparent = single_trace_header(message, "traceparent");
    let tracestate = single_trace_header(message, "tracestate");

    InboundRequest {
        subject: subject.clone(),
        payload: message.payload.clone(),
        reply_to: reply_to.clone(),
        context: RequestContext {
            resuming: false,
            operation_progress: None,
            subject,
            session_key,
            proof,
            authorization_context,
            iat,
            request_id,
            required_capabilities: None,
            required_permission: None,
            reply_to: reply_to.clone(),
            caller: None,
            traceparent,
            tracestate,
        },
    }
}

/// Encode one successful handler payload for reply publishing.
#[doc = concat!("Trellis API operation `", stringify!(encode_success_reply), "`.")]
pub fn encode_success_reply(reply_to: String, payload: Bytes) -> OutboundReply {
    OutboundReply {
        reply_to,
        payload,
        is_error: false,
    }
}

/// Encode one failed handler result for reply publishing.
#[doc = concat!("Trellis API operation `", stringify!(encode_error_reply), "`.")]
pub fn encode_error_reply(reply_to: String, error: &ServerError) -> OutboundReply {
    encode_error_reply_with_context(reply_to, error, &ErrorAnnotationContext::default())
}

#[doc = concat!("Trellis API operation `", stringify!(encode_error_reply_with_context), "`.")]
pub fn encode_error_reply_with_context(
    reply_to: String,
    error: &ServerError,
    annotations: &ErrorAnnotationContext,
) -> OutboundReply {
    match error {
        ServerError::DeclaredRpc(error) => {
            let payload = serde_json::to_vec(&error.to_payload_with_context(
            error_id(),
            annotations.context_map(),
            annotations.trace_id(),
        ))
        .unwrap_or_else(|_| {
            br#"{"id":"rust-server-error","type":"UnexpectedError","message":"An unexpected error has occurred"}"#.to_vec()
        });
            return OutboundReply {
                reply_to,
                payload: Bytes::from(payload),
                is_error: true,
            };
        }
        ServerError::SchemaValidation { issues } => {
            let mut payload = Map::new();
            payload.insert("id".to_string(), Value::String(error_id()));
            payload.insert(
                "type".to_string(),
                Value::String("SchemaValidationError".to_string()),
            );
            payload.insert(
                "message".to_string(),
                Value::String("Schema validation failed.".to_string()),
            );
            payload.insert(
                "issues".to_string(),
                serde_json::to_value(issues).unwrap_or_default(),
            );
            let context = annotations.context_map();
            merge_context(&mut payload, context);
            if let Some(trace_id) = annotations.trace_id() {
                payload.insert("traceId".to_string(), Value::String(trace_id.to_string()));
            }
            let payload_bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| {
            br#"{"id":"rust-server-error","type":"UnexpectedError","message":"An unexpected error has occurred"}"#.to_vec()
        });
            return OutboundReply {
                reply_to,
                payload: Bytes::from(payload_bytes),
                is_error: true,
            };
        }
        ServerError::Validation { issues } => {
            let mut payload = Map::new();
            payload.insert("id".to_string(), Value::String(error_id()));
            payload.insert(
                "type".to_string(),
                Value::String("ValidationError".to_string()),
            );
            payload.insert(
                "message".to_string(),
                Value::String("Data validation failed.".to_string()),
            );
            payload.insert(
                "issues".to_string(),
                serde_json::to_value(issues).unwrap_or_default(),
            );
            let context = annotations.context_map();
            merge_context(&mut payload, context);
            if let Some(trace_id) = annotations.trace_id() {
                payload.insert("traceId".to_string(), Value::String(trace_id.to_string()));
            }
            let payload_bytes = serde_json::to_vec(&payload).unwrap_or_else(|_| {
            br#"{"id":"rust-server-error","type":"UnexpectedError","message":"An unexpected error has occurred"}"#.to_vec()
        });
            return OutboundReply {
                reply_to,
                payload: Bytes::from(payload_bytes),
                is_error: true,
            };
        }
        _ => {}
    }

    #[derive(serde::Serialize)]
    struct ErrorPayload<'a> {
        id: String,
        r#type: &'static str,
        message: &'static str,
        #[serde(rename = "traceId", skip_serializing_if = "Option::is_none")]
        trace_id: Option<&'a str>,
        context: ErrorContext<'a>,
    }

    #[derive(serde::Serialize)]
    struct ErrorContext<'a> {
        #[serde(rename = "causeMessage")]
        cause_message: &'a str,
        #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
        request_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        service: Option<&'a str>,
        #[serde(rename = "contractId", skip_serializing_if = "Option::is_none")]
        contract_id: Option<&'a str>,
        #[serde(rename = "contractDigest", skip_serializing_if = "Option::is_none")]
        contract_digest: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        method: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        live: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation: Option<&'a str>,
    }

    let error_message = error.to_string();
    let payload = match serde_json::to_vec(&ErrorPayload {
    id: error_id(),
    r#type: "UnexpectedError",
    message: "An unexpected error has occurred",
    trace_id: annotations.trace_id(),
    context: ErrorContext {
        cause_message: &error_message,
        request_id: annotations.request_id.as_deref(),
        service: annotations.service.as_deref(),
        contract_id: annotations.contract_id.as_deref(),
        contract_digest: annotations.contract_digest.as_deref(),
        method: annotations.method.as_deref(),
        live: annotations.live.as_deref(),
        operation: annotations.operation.as_deref(),
    },
}) {
    Ok(value) => Bytes::from(value),
    Err(_) => Bytes::from_static(
        br#"{"id":"rust-server-error","type":"UnexpectedError","message":"An unexpected error has occurred"}"#,
    ),
};

    OutboundReply {
        reply_to,
        payload,
        is_error: true,
    }
}

fn insert_string(context: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        context.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn surface_key<'a>(subject: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = subject.strip_prefix(prefix)?.strip_prefix('.')?;
    let (_version, key) = rest.split_once('.')?;
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

fn trim_operation_control(operation: &str) -> &str {
    operation.strip_suffix(".control").unwrap_or(operation)
}

fn trace_id_from_traceparent(traceparent: &str) -> Option<&str> {
    let mut parts = traceparent.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?;
    if parts.next().is_some()
        || version.len() != 2
        || version == "ff"
        || trace_id.len() != 32
        || span_id.len() != 16
        || flags.len() != 2
        || !is_lower_hex(version)
        || !is_lower_hex(trace_id)
        || !is_lower_hex(span_id)
        || !is_lower_hex(flags)
        || trace_id.bytes().all(|byte| byte == b'0')
        || span_id.bytes().all(|byte| byte == b'0')
    {
        return None;
    }
    Some(trace_id)
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn error_id() -> String {
    let sequence = ERROR_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("rust-server-error-{timestamp}-{sequence}")
}

/// Outcome classification for one dispatched server request.
///
/// Typed error categories are used; human-readable messages are never parsed.
fn server_outcome(error: &ServerError) -> &'static str {
    match error {
        ServerError::DeclaredRpc(_) => "declared_error",
        ServerError::SchemaValidation { .. }
        | ServerError::Validation { .. }
        | ServerError::MissingHandler(_)
        | ServerError::MissingReply { .. }
        | ServerError::InvalidOperationControlAction { .. }
        | ServerError::OperationNotFound { .. }
        | ServerError::OperationAlreadyExists { .. }
        | ServerError::OperationInvalidId { .. }
        | ServerError::OperationMismatch { .. }
        | ServerError::OperationAlreadyTerminal { .. }
        | ServerError::OperationUnsupportedControl { .. }
        | ServerError::InvalidResourceBinding { .. }
        | ServerError::TransferObjectMissing { .. }
        | ServerError::InvalidTransferId { .. }
        | ServerError::TransferSequenceOutOfOrder { .. }
        | ServerError::TransferMissingEof { .. }
        | ServerError::TransferAlreadyComplete { .. }
        | ServerError::TransferExpired { .. }
        | ServerError::InvalidTransferExpiry { .. }
        | ServerError::InvalidTransferChunkSize { .. }
        | ServerError::MissingTransferHeader { .. }
        | ServerError::InvalidTransferHeader { .. }
        | ServerError::TransferObjectSizeMismatch { .. }
        | ServerError::StoreObjectTooLarge { .. }
        | ServerError::TransferObjectTooLarge { .. } => "invalid",
        ServerError::RequestDenied { .. }
        | ServerError::MissingSessionKey { .. }
        | ServerError::MissingProof { .. }
        | ServerError::MissingAuthorizationContext { .. }
        | ServerError::ReplyInboxMismatch { .. }
        | ServerError::TransferSessionMismatch { .. }
        | ServerError::TransferDigestMismatch { .. } => "denied",
        ServerError::StoreWaitTimeout { .. } => "timeout",
        ServerError::Json(_) => "error",
        ServerError::OperationCapacityExceeded { .. } => "rate_limited",
        ServerError::Nats(_)
        | ServerError::MissingResourceBinding { .. }
        | ServerError::ResourceUnavailable { .. }
        | ServerError::StoreCommitIndeterminate { .. } => "unavailable",
        ServerError::StoreWaitCanceled { .. }
        | ServerError::StoreWriteCancelled
        | ServerError::StoreReadCancelled
        | ServerError::TransferCancelled { .. } => "cancelled",
        _ => "error",
    }
}

/// Trace carrier pairs retained on one inbound request context.
fn trace_pairs(context: &RequestContext) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    if let Some(traceparent) = &context.traceparent {
        pairs.push(("traceparent".to_owned(), traceparent.clone()));
    }
    if let Some(tracestate) = &context.tracestate {
        pairs.push(("tracestate".to_owned(), tracestate.clone()));
    }
    pairs
}

/// One dispatched request's business outcome before error erasure.
///
/// Dispatch converts handler errors and panics into encoded error replies so
/// callers receive their exact payload. The observation must classify the
/// business result, not the envelope construction, so the typed outcome is
/// carried alongside the reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispatchOutcome {
    /// The handler produced a response without a business error.
    Ok,
    /// The handler returned or panicked with an error category.
    Failed(&'static str),
}

impl DispatchOutcome {
    /// Bounded catalog value for the observation.
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed(outcome) => outcome,
        }
    }
}

/// Runs one handler call and returns its business outcome with the result.
async fn dispatch_outcome<T>(
    call: impl std::future::Future<Output = Result<T, ServerError>>,
) -> (DispatchOutcome, Result<T, ServerError>) {
    match AssertUnwindSafe(call).catch_unwind().await {
        Ok(Ok(value)) => (DispatchOutcome::Ok, Ok(value)),
        Ok(Err(error)) => (DispatchOutcome::Failed(server_outcome(&error)), Err(error)),
        // A handler panic is an internal failure. The wire payload is
        // unchanged, but the observation must not classify it as an
        // unavailable dependency.
        Err(panic) => (
            DispatchOutcome::Failed("error"),
            Err(panic_to_server_error(panic)),
        ),
    }
}

/// One dispatched response ready for publication.
///
/// The unary server observation stays open until the reply is actually handed
/// to the transport, so a failed handoff is not recorded as a success. Stream
/// and live responses never carry the unary observation.
pub(crate) struct DispatchedResponse {
    pub(crate) reply_to: String,
    pub(crate) response: HandlerResponse,
    pub(crate) annotations: ErrorAnnotationContext,
    unary: Option<UnaryRequest>,
    pub(crate) outcome: DispatchOutcome,
}

/// Owns the unary span and counted observation across dispatch and reply handoff.
struct UnaryRequest {
    observation: Option<Observation>,
    span: tracing::Span,
}

impl UnaryRequest {
    fn start(context: &RequestContext, route: &'static str) -> Self {
        let observation = Observation::start_counted(
            DurationFamily::RpcServer,
            UpDownFamily::RpcServerInflight,
            vec![KeyValue::new("trellis.route", route)],
            "cancelled",
        );
        let span = tracing::info_span!(
            "trellis.rpc.server",
            "trellis.route" = route,
            "trellis.request.id" = context.request_id.as_deref().filter(|id| id.len() <= 128),
            "trellis.outcome" = tracing::field::Empty,
        );
        let _ = span.set_parent(propagation::extract_context(&trace_pairs(context)));
        Self {
            observation: Some(observation),
            span,
        }
    }

    fn finish(&mut self, outcome: DispatchOutcome, handoff: Option<&ServerError>) {
        let final_outcome = handoff.map_or(outcome.as_str(), server_outcome);
        self.span.record("trellis.outcome", final_outcome);
        if handoff.is_some() {
            self.span
                .set_status(opentelemetry::trace::Status::error("reply handoff failed"));
        } else if matches!(outcome, DispatchOutcome::Failed(_)) {
            self.span
                .set_status(opentelemetry::trace::Status::error("request failed"));
        }
        if let Some(observation) = self.observation.take() {
            observation.finish(final_outcome);
        }
    }

    fn discard(mut self) {
        if let Some(observation) = self.observation.take() {
            observation.discard();
        }
    }
}

impl Drop for UnaryRequest {
    fn drop(&mut self) {
        if self.observation.is_some() {
            self.span.record("trellis.outcome", "cancelled");
            self.span
                .set_status(opentelemetry::trace::Status::error("request cancelled"));
        }
    }
}

/// Result of one decoded dispatch before reply handoff.
pub(crate) enum DispatchResponse {
    /// The request carried no reply inbox, so nothing is published.
    NoReply,
    /// One response is ready to publish.
    Reply(Box<DispatchedResponse>),
}

impl HandlerResponse {
    /// Whether this response is a unary RPC for the server duration family.
    ///
    /// Stream, Live and live responses have their own lifetimes and must never
    /// be counted as unary RPC successes.
    pub(crate) fn is_unary(&self) -> bool {
        matches!(self, Self::Frames(_) | Self::Error(_))
    }
}

/// Whether one live route's dispatch error must be dropped without any reply.
///
/// The plan forbids reflecting a denial onto a reply that is not provably the
/// authenticated caller's inbox. Unverified, missing-proof, and foreign-reply
/// openings are therefore dropped silently; only setup errors for an already
/// authorized opening are answered by the ordinary error publisher.
fn is_dropped_live_denial(error: &ServerError) -> bool {
    matches!(
        error,
        ServerError::RequestDenied { .. }
            | ServerError::MissingProof { .. }
            | ServerError::MissingSessionKey { .. }
            | ServerError::MissingAuthorizationContext { .. }
            | ServerError::ReplyInboxMismatch { .. }
    )
}

/// Dispatch one decoded request to a request handler and encode a reply.
pub async fn dispatch_one<H>(
    handler: &H,
    request: InboundRequest,
) -> Result<Option<OutboundReply>, ServerError>
where
    H: RequestHandler,
{
    Ok(dispatch_all(handler, request)
        .await?
        .and_then(|mut replies| {
            if replies.is_empty() {
                None
            } else {
                Some(replies.remove(0))
            }
        }))
}

/// Dispatch one decoded request to a request handler and encode all replies.
pub async fn dispatch_all<H>(
    handler: &H,
    request: InboundRequest,
) -> Result<Option<Vec<OutboundReply>>, ServerError>
where
    H: RequestHandler,
{
    let registered_route = handler.route_token(&request.subject);
    let route = registered_route.unwrap_or_else(instruments::unknown_route);
    let live_route = handler.is_live_route(&request.subject);
    let mut unary = (handler.is_unary_rpc_route(&request.subject) && request.reply_to.is_some())
        .then(|| UnaryRequest::start(&request.context, route));
    let reply_to = request.reply_to;
    let annotations =
        ErrorAnnotationContext::from_request(&request.subject, &request.context, handler);
    let call = handler.handle_frames(&request.subject, request.payload, request.context);
    let (dispatch, result) = if let Some(unary) = &unary {
        dispatch_outcome(call.instrument(unary.span.clone())).await
    } else {
        dispatch_outcome(call).await
    };
    let result = match result {
        Ok(payloads) => Ok(reply_to.map(|reply_to| {
            payloads
                .into_iter()
                .map(|payload| encode_success_reply(reply_to.clone(), payload))
                .collect()
        })),
        Err(error) => match reply_to {
            Some(reply_to) if !live_route || !is_dropped_live_denial(&error) => {
                Ok(Some(vec![encode_error_reply_with_context(
                    reply_to,
                    &error,
                    &annotations,
                )]))
            }
            Some(_) => Ok(None),
            None => Err(error),
        },
    };

    if let Some(unary) = &mut unary {
        unary.finish(dispatch, None);
    }
    result
}

pub(crate) async fn dispatch_response<H>(
    handler: &H,
    request: InboundRequest,
) -> Result<DispatchResponse, ServerError>
where
    H: RequestHandler,
{
    let registered_route = handler.route_token(&request.subject);
    let route = registered_route.unwrap_or_else(instruments::unknown_route);
    let live_route = handler.is_live_route(&request.subject);
    let mut unary = handler
        .is_unary_rpc_route(&request.subject)
        .then(|| UnaryRequest::start(&request.context, route));
    let reply_to = request.reply_to;
    let annotations =
        ErrorAnnotationContext::from_request(&request.subject, &request.context, handler);
    let call = handler.handle_response(&request.subject, request.payload, request.context);
    let (dispatch, result) = if let Some(unary) = &unary {
        dispatch_outcome(call.instrument(unary.span.clone())).await
    } else {
        dispatch_outcome(call).await
    };

    let Some(reply_to) = reply_to else {
        // The saved typed outcome is authoritative; no reclassification.
        if let Some(unary) = &mut unary {
            unary.finish(dispatch, None);
        }
        return match result {
            Ok(_) => Ok(DispatchResponse::NoReply),
            Err(error) => Err(error),
        };
    };

    // The typed outcome from dispatch is authoritative even when the error was
    // translated into a wire envelope; only a reply-publication failure may
    // amend it later.
    let (response, outcome) = match result {
        Ok(response) => (response, dispatch),
        Err(error) => {
            if live_route && is_dropped_live_denial(&error) {
                // A live opening whose reply is not provably the caller's inbox
                // must not receive a reflected denial on an unverified subject.
                if let Some(unary) = &mut unary {
                    unary.finish(dispatch, None);
                }
                return Ok(DispatchResponse::NoReply);
            }
            (
                HandlerResponse::Error(
                    encode_error_reply_with_context(reply_to.clone(), &error, &annotations).payload,
                ),
                dispatch,
            )
        }
    };
    // A live response owns its own lifetime; the unary server
    // observation and span must not count it, and only a registered route is a
    // unary surface. A unary observation and span stay open for the caller to
    // finish after the reply handoff.
    if !response.is_unary() {
        if let Some(unary) = unary.take() {
            unary.discard();
        }
    }
    Ok(DispatchResponse::Reply(Box::new(DispatchedResponse {
        reply_to,
        response,
        annotations,
        unary,
        outcome,
    })))
}

async fn publish_reply(
    client: &async_nats::Client,
    reply: OutboundReply,
) -> Result<(), ServerError> {
    if reply.is_error {
        let mut headers = HeaderMap::new();
        headers.insert("status", "error");
        client
            .publish_with_headers(reply.reply_to, headers, reply.payload)
            .await
            .map_err(|error| ServerError::Nats(error.to_string()))?;
        return Ok(());
    }

    client
        .publish(reply.reply_to, reply.payload)
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))?;
    Ok(())
}

async fn publish_response(
    client: &async_nats::Client,
    dispatched: DispatchedResponse,
) -> Result<(), ServerError> {
    let DispatchedResponse {
        reply_to,
        response,
        annotations: _,
        mut unary,
        outcome,
    } = dispatched;
    let result = publish_handler_response(client, reply_to, response).await;
    // The unary observation and its span end with the actual reply handoff. A
    // delivered business failure sets an error status with a static bounded
    // description; a failed publication is an unavailable request.
    if let Some(unary) = &mut unary {
        unary.finish(outcome, result.as_ref().err());
    }
    result
}

async fn publish_handler_response(
    client: &async_nats::Client,
    reply_to: String,
    response: HandlerResponse,
) -> Result<(), ServerError> {
    match response {
        HandlerResponse::Frames(frames) => {
            for payload in frames {
                publish_reply(client, encode_success_reply(reply_to.clone(), payload)).await?;
            }
        }
        HandlerResponse::Error(payload) => {
            let reply = OutboundReply {
                reply_to,
                payload,
                is_error: true,
            };
            publish_reply(client, reply).await?;
        }
        HandlerResponse::LivePrepared(prepared) => {
            // One signed offer, no infinite reply loop. Ownership of the
            // reservation already moved to the connection's live manager.
            let _ = prepared.manager;
            let _ = prepared.request_id;
            client
                .publish_with_headers(reply_to, prepared.headers, prepared.offer)
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
        }
    }
    Ok(())
}

fn panic_to_server_error(panic: Box<dyn Any + Send>) -> ServerError {
    let message = panic
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "request handler panicked".to_string());
    ServerError::Nats(format!("request handler panicked: {message}"))
}

/// Run an inbound NATS request loop until the subscriber closes.
pub(crate) async fn run_nats_request_loop<H>(
    client: async_nats::Client,
    subscriber: impl futures_util::Stream<Item = async_nats::Message>,
    handler: H,
) -> Result<(), ServerError>
where
    H: RequestHandler,
{
    let mut subscriber = Box::pin(subscriber);

    let mut in_flight = FuturesUnordered::new();
    loop {
        tokio::select! {
            message = subscriber.next() => {
                let Some(message) = message else {
                    break;
                };
                let request = decode_nats_request(&message);
                let client = &client;
                let handler = &handler;
                in_flight.push(async move {
                    let subject = request.subject.clone();
                    match dispatch_response(handler, request).await {
                        Ok(DispatchResponse::Reply(dispatched)) => {
                            let reply_to = dispatched.reply_to.clone();
                            let request_id = dispatched.annotations.request_id.clone();
                            tracing::debug!(%subject, %reply_to, request_id = ?request_id, "publishing service request reply");
                            if let Err(error) = publish_response(client, *dispatched).await {
                                tracing::warn!(%subject, %reply_to, request_id = ?request_id, %error, "service request reply publish failed");
                                return Err(error);
                            }
                        }
                        Ok(DispatchResponse::NoReply) => {}
                        Err(_) => {}
                    }
                    Ok::<(), ServerError>(())
                });
            }
            result = in_flight.next(), if !in_flight.is_empty() => {
                if let Some(result) = result {
                    result?;
                }
            }
        }
    }

    while let Some(result) = in_flight.next().await {
        result?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::service::{
        BootstrapBinding, DeclaredRpcError, SchemaValidationIssue, ServiceHost, ValidationIssue,
    };

    const TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

    struct DeclaredErrorHandler;

    impl RequestHandler for DeclaredErrorHandler {
        fn route_token(&self, subject: &str) -> Option<&'static str> {
            route_token_for(subject)
        }

        fn handle<'a>(
            &'a self,
            _subject: &'a str,
            _payload: Bytes,
            _context: RequestContext,
        ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
            Box::pin(async {
                Err(ServerError::DeclaredRpc(DeclaredRpcError::new(
                    "NotFoundError",
                    "Widget not found",
                    [
                        ("code", json!("missing-widget")),
                        (
                            "context",
                            json!({
                                "domain": "inventory",
                                "subject": "rpc.v1.Inventory.Get"
                            }),
                        ),
                    ],
                )))
            })
        }
    }

    struct PanicHandler;

    impl RequestHandler for PanicHandler {
        fn route_token(&self, subject: &str) -> Option<&'static str> {
            route_token_for(subject)
        }

        fn handle<'a>(
            &'a self,
            _subject: &'a str,
            _payload: Bytes,
            _context: RequestContext,
        ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
            Box::pin(async { panic!("boom") })
        }
    }

    #[tokio::test]
    async fn declared_error_reply_preserves_type_and_merges_runtime_context() {
        let host = test_service_host(DeclaredErrorHandler);
        let replies = dispatch_all(&host, test_request("rpc.v1.Inventory.Get"))
            .await
            .expect("dispatch should not fail")
            .expect("reply should be encoded");
        let payload = reply_payload(&replies[0]);

        assert_eq!(payload["type"], "NotFoundError");
        assert_eq!(payload["message"], "Widget not found");
        assert_eq!(payload["code"], "missing-widget");
        assert_eq!(payload["traceId"], TRACE_ID);
        assert!(payload.get("subject").is_none());

        let context = payload["context"]
            .as_object()
            .expect("context should stay an object");
        assert_eq!(context["domain"], "inventory");
        assert_eq!(context["requestId"], "request-123");
        assert_eq!(context["service"], "inventory-service");
        assert_eq!(context["contractId"], "inventory.service@v1");
        assert_eq!(context["contractDigest"], "sha256:inventory");
        assert_eq!(context["method"], "Inventory.Get");
        assert!(context.get("subject").is_none());
    }

    #[tokio::test]
    async fn panic_error_reply_uses_same_runtime_context() {
        let host = test_service_host(PanicHandler);
        let replies = dispatch_all(&host, test_request("rpc.v1.Inventory.Get"))
            .await
            .expect("dispatch should catch panic")
            .expect("reply should be encoded");
        let payload = reply_payload(&replies[0]);

        assert_eq!(payload["type"], "UnexpectedError");
        assert_eq!(payload["traceId"], TRACE_ID);
        assert!(payload.get("subject").is_none());

        let context = payload["context"]
            .as_object()
            .expect("context should be an object");
        assert_eq!(context["requestId"], "request-123");
        assert_eq!(context["service"], "inventory-service");
        assert_eq!(context["contractId"], "inventory.service@v1");
        assert_eq!(context["contractDigest"], "sha256:inventory");
        assert_eq!(context["method"], "Inventory.Get");
        assert!(context["causeMessage"]
            .as_str()
            .expect("cause message should be a string")
            .contains("request handler panicked: boom"));
        assert!(context.get("subject").is_none());
    }

    #[tokio::test]
    async fn schema_validation_error_reply_uses_correct_type() {
        struct SchemaValidationHandler;

        impl RequestHandler for SchemaValidationHandler {
            fn handle<'a>(
                &'a self,
                _subject: &'a str,
                _payload: Bytes,
                _context: RequestContext,
            ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
                Box::pin(async {
                    Err(ServerError::SchemaValidation {
                        issues: Box::new(vec![SchemaValidationIssue {
                            path: "/items".to_string(),
                            schema_path: Some("#/properties/items".to_string()),
                            keyword: "minItems".to_string(),
                            code: "test.items.required".to_string(),
                            message: "Add at least one item.".to_string(),
                            label: Some("Items".to_string()),
                            note: None,
                            i18n_key: None,
                            severity: None,
                            params: Some(
                                vec![("limit".to_string(), serde_json::json!(1))]
                                    .into_iter()
                                    .collect(),
                            ),
                        }]),
                    })
                })
            }
        }

        let host = test_service_host(SchemaValidationHandler);
        let replies = dispatch_all(&host, test_request("rpc.v1.Test.Validate"))
            .await
            .expect("dispatch should not fail")
            .expect("reply should be encoded");
        let payload = reply_payload(&replies[0]);

        assert_eq!(payload["type"], "SchemaValidationError");
        assert_eq!(payload["message"], "Schema validation failed.");

        let issues = payload["issues"]
            .as_array()
            .expect("issues should be an array");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["code"], "test.items.required");
        assert_eq!(issues[0]["keyword"], "minItems");
        assert_eq!(issues[0]["path"], "/items");

        assert_eq!(payload["traceId"], TRACE_ID);
        let context = payload["context"]
            .as_object()
            .expect("context should exist");
        assert_eq!(context["requestId"], "request-123");
        assert_eq!(context["service"], "inventory-service");
    }

    #[tokio::test]
    async fn validation_error_reply_uses_correct_type() {
        struct ValidationHandler;

        impl RequestHandler for ValidationHandler {
            fn handle<'a>(
                &'a self,
                _subject: &'a str,
                _payload: Bytes,
                _context: RequestContext,
            ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
                Box::pin(async {
                    Err(ServerError::Validation {
                        issues: Box::new(vec![ValidationIssue {
                            path: "/name".to_string(),
                            message: "minLength: minimum length is 3".to_string(),
                        }]),
                    })
                })
            }
        }

        let host = test_service_host(ValidationHandler);
        let replies = dispatch_all(&host, test_request("rpc.v1.Test.Validate"))
            .await
            .expect("dispatch should not fail")
            .expect("reply should be encoded");
        let payload = reply_payload(&replies[0]);

        assert_eq!(payload["type"], "ValidationError");
        assert_eq!(payload["message"], "Data validation failed.");

        let issues = payload["issues"]
            .as_array()
            .expect("issues should be an array");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["path"], "/name");

        assert_eq!(payload["traceId"], TRACE_ID);
        let context = payload["context"]
            .as_object()
            .expect("context should exist");
        assert_eq!(context["requestId"], "request-123");
        assert_eq!(context["service"], "inventory-service");
    }

    #[test]
    fn invalid_traceparent_does_not_add_trace_id() {
        let annotations = ErrorAnnotationContext::from_request(
            "rpc.v1.Inventory.Get",
            &RequestContext {
                traceparent: Some(
                    "00-00000000000000000000000000000000-00f067aa0ba902b7-01".to_string(),
                ),
                ..test_context("rpc.v1.Inventory.Get")
            },
            &DeclaredErrorHandler,
        );

        assert_eq!(annotations.trace_id(), None);
    }

    fn test_request(subject: &str) -> InboundRequest {
        InboundRequest {
            subject: subject.to_string(),
            payload: Bytes::new(),
            reply_to: Some("reply.inbox".to_string()),
            context: test_context(subject),
        }
    }

    fn test_context(subject: &str) -> RequestContext {
        RequestContext {
            resuming: false,
            operation_progress: None,
            subject: subject.to_string(),
            session_key: None,
            proof: None,
            authorization_context: None,
            iat: None,
            request_id: Some("request-123".to_string()),
            required_capabilities: None,
            required_permission: None,
            reply_to: Some("reply.inbox".to_string()),
            caller: None,
            traceparent: Some(TRACEPARENT.to_string()),
            tracestate: None,
        }
    }

    fn reply_payload(reply: &OutboundReply) -> Value {
        serde_json::from_slice(&reply.payload).expect("reply should contain json")
    }

    fn test_service_host<H>(handler: H) -> ServiceHost<H> {
        ServiceHost::new(
            "inventory-service",
            BootstrapBinding {
                contract_id: "inventory.service@v1".to_string(),
                digest: "sha256:inventory".to_string(),
            },
            handler,
        )
    }

    /// Distinct route tokens keep concurrent provider tests independent.
    const SUCCESS_ROUTE: &str = "test@v1:Route.Success";
    const DECLARED_ROUTE: &str = "test@v1:Route.Declared";
    const DENIED_ROUTE: &str = "test@v1:Route.Denied";
    const LIVE_ROUTE: &str = "test@v1:Route.Live";
    const PLAIN_ROUTE: &str = "test@v1:Route.Plain";
    const PANIC_ROUTE: &str = "test@v1:Route.Panic";
    const ENVELOPE_ROUTE: &str = "test@v1:Route.Envelope";

    fn route_token_for(subject: &str) -> Option<&'static str> {
        match subject {
            "test.success" => Some(SUCCESS_ROUTE),
            "test.declared" => Some(DECLARED_ROUTE),
            "test.denied" => Some(DENIED_ROUTE),
            "test.live" => Some(LIVE_ROUTE),
            "test.plain" => Some(PLAIN_ROUTE),
            "test.panic" => Some(PANIC_ROUTE),
            "test.envelope" => Some(ENVELOPE_ROUTE),
            _ => None,
        }
    }

    struct DeniedHandler;

    impl RequestHandler for DeniedHandler {
        fn route_token(&self, subject: &str) -> Option<&'static str> {
            route_token_for(subject)
        }

        fn is_live_route(&self, subject: &str) -> bool {
            subject == "test.live"
        }

        fn handle<'a>(
            &'a self,
            subject: &'a str,
            _payload: Bytes,
            _context: RequestContext,
        ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
            Box::pin(async move {
                Err(ServerError::RequestDenied {
                    subject: subject.to_string(),
                    session_key: "session-key".to_string(),
                })
            })
        }
    }

    struct SuccessHandler;

    impl RequestHandler for SuccessHandler {
        fn route_token(&self, subject: &str) -> Option<&'static str> {
            route_token_for(subject)
        }

        fn handle<'a>(
            &'a self,
            _subject: &'a str,
            _payload: Bytes,
            _context: RequestContext,
        ) -> BoxFuture<'a, Result<Bytes, ServerError>> {
            Box::pin(async { Ok(Bytes::from_static(b"{}")) })
        }
    }

    /// Reads `trellis.rpc.server` samples by outcome for one route token.
    ///
    /// Filtering by the exact registered token keeps concurrently running
    /// provider tests from observing each other's samples.
    fn server_outcomes_for(
        capture: &crate::telemetry::capture::MetricCapture,
        route: &str,
    ) -> Vec<(String, u64)> {
        capture
            .points_for("trellis.rpc.server.duration")
            .into_iter()
            .filter(|point| {
                point
                    .attributes
                    .iter()
                    .any(|(key, value)| key == "trellis.route" && value == route)
            })
            .filter_map(|point| {
                let outcome = point
                    .attributes
                    .iter()
                    .find(|(key, _)| key == "trellis.outcome")
                    .map(|(_, value)| value.clone())?;
                Some((outcome, point.count))
            })
            .collect()
    }

    /// Runs one real dispatch and returns its route's cumulative samples.
    async fn recorded_dispatch<H: RequestHandler>(
        handler: H,
        subject: &'static str,
        capture: &crate::telemetry::capture::MetricCapture,
    ) -> Vec<(String, u64)> {
        let host = test_service_host(handler);
        let _ = dispatch_all(&host, test_request(subject)).await;
        capture.flush();
        let route = route_token_for(subject).expect("test subjects carry a route token");
        let mut samples = server_outcomes_for(capture, route);
        samples.sort();
        samples
    }

    #[tokio::test]
    async fn dispatch_outcomes_classify_business_results_not_envelopes() {
        let _guard = crate::telemetry::capture::meter_test_lock().await;
        let capture = crate::telemetry::capture::process_capture();

        // Counts are cumulative across the process, so each phase compares the
        // increase it caused rather than an absolute total.
        capture.flush();
        let baseline = server_outcomes_for(&capture, SUCCESS_ROUTE)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let after_success = recorded_dispatch(SuccessHandler, "test.success", &capture).await;
        let added = added_since(&baseline, &after_success);
        assert_eq!(
            added,
            vec![("ok".to_string(), 1)],
            "one success must add exactly one ok sample"
        );
        let baseline = after_success
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();

        let after_declared =
            recorded_dispatch(DeclaredErrorHandler, "test.declared", &capture).await;
        assert_eq!(
            added_since(&baseline, &after_declared),
            vec![("declared_error".to_string(), 1)],
            "an error reply must add only the declared_error sample"
        );
        let baseline = after_declared
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();

        let after_denied = recorded_dispatch(DeniedHandler, "test.denied", &capture).await;
        assert_eq!(
            added_since(&baseline, &after_denied),
            vec![("denied".to_string(), 1)],
            "a denial must add exactly one denied sample"
        );
        let baseline = after_denied
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();

        let after_panic = recorded_dispatch(PanicHandler, "test.panic", &capture).await;
        assert_eq!(
            added_since(&baseline, &after_panic),
            vec![("error".to_string(), 1)],
            "a panic must add exactly one internal error sample"
        );
    }

    #[tokio::test]
    async fn dispatch_error_envelope_never_records_ok() {
        let _guard = crate::telemetry::capture::meter_test_lock().await;
        let capture = crate::telemetry::capture::process_capture();
        let baseline = std::collections::BTreeMap::new();

        let host = test_service_host(DeclaredErrorHandler);
        let replies = dispatch_all(&host, test_request("test.envelope"))
            .await
            .expect("dispatch should not fail")
            .expect("reply should be encoded");
        // The caller still receives the exact encoded error envelope.
        assert!(replies[0].is_error);
        capture.flush();

        let added = added_since(&baseline, &server_outcomes_for(&capture, ENVELOPE_ROUTE));
        assert_eq!(
            added,
            vec![("declared_error".to_string(), 1)],
            "an error envelope must add the error sample and never an ok sample"
        );
    }

    #[tokio::test]
    async fn denied_live_route_is_dropped_without_a_reflected_reply() {
        let host = test_service_host(DeniedHandler);
        // A registered live route is dropped regardless of the payload shape, so
        // a malformed/nested opening cannot slip past a body discriminator.
        let mut request = test_request("test.live");
        request.payload = Bytes::from_static(
            br#"{"format":"trellis.live.v1","type":"open","openId":"c29tZS1ub25jZS0xNg","receiveMaxPayloadBytes":1048576,"input":{}}"#,
        );
        let dropped = dispatch_all(&host, request)
            .await
            .expect("dispatch should not fail");
        assert!(
            dropped.is_none(),
            "a denied live route must be dropped without a reflected reply"
        );
        let dropped_malformed = dispatch_all(&host, test_request("test.live"))
            .await
            .expect("dispatch should not fail");
        assert!(
            dropped_malformed.is_none(),
            "a live route denial is dropped even for a non-opening payload"
        );

        // An ordinary denied non-live request keeps its encoded error reply.
        let replies = dispatch_all(&host, test_request("test.plain"))
            .await
            .expect("dispatch should not fail")
            .expect("an ordinary denial still replies");
        assert!(
            replies[0].is_error,
            "ordinary denials keep their error reply"
        );
    }

    #[tokio::test]
    async fn dropped_unary_handoff_records_one_cancellation() {
        let _guard = crate::telemetry::capture::meter_test_lock().await;
        let capture = crate::telemetry::capture::process_capture();
        capture.flush();
        let baseline = server_outcomes_for(&capture, SUCCESS_ROUTE)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let host = test_service_host(SuccessHandler);
        let dispatched = dispatch_response(&host, test_request("test.success"))
            .await
            .unwrap();
        let DispatchResponse::Reply(reply) = dispatched else {
            panic!("expected unary reply");
        };
        drop(reply);
        capture.flush();
        assert_eq!(
            added_since(&baseline, &server_outcomes_for(&capture, SUCCESS_ROUTE)),
            vec![("cancelled".to_owned(), 1)]
        );
    }

    /// Outcome counts added between two cumulative samples.
    fn added_since(
        baseline: &std::collections::BTreeMap<String, u64>,
        current: &[(String, u64)],
    ) -> Vec<(String, u64)> {
        let mut added: Vec<(String, u64)> = current
            .iter()
            .filter_map(|(outcome, count)| {
                let previous = baseline.get(outcome).copied().unwrap_or(0);
                (*count > previous).then_some((outcome.clone(), count - previous))
            })
            .collect();
        added.sort();
        added
    }
}
