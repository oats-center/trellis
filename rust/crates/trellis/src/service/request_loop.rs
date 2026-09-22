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

use super::error::merge_context;
use super::{AuthenticatedRouter, RequestContext, RequestValidator, Router, ServerError};

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
    Stream(ResponseStream),
    FeedStream {
        stream: ResponseStream,
        control_subject: String,
        feed_id: String,
    },
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
    feed: Option<String>,
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
        insert_string(&mut context, "feed", self.feed.as_deref());
        insert_string(&mut context, "operation", self.operation.as_deref());
        context
    }

    fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    fn set_surface(&mut self, subject: &str) {
        if let Some(method) = surface_key(subject, "rpc") {
            self.method = Some(method.to_string());
        } else if let Some(feed) = surface_key(subject, "feed") {
            self.feed = Some(feed.to_string());
        } else if let Some(feed) = surface_key(subject, "feeds") {
            self.feed = Some(feed.to_string());
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
                HandlerResponse::Stream(mut stream) => {
                    let mut frames = Vec::new();
                    while let Some(frame) = stream.next().await {
                        frames.push(frame?);
                    }
                    Ok(frames)
                }
                HandlerResponse::FeedStream { mut stream, .. } => {
                    let mut frames = Vec::new();
                    while let Some(frame) = stream.next().await {
                        frames.push(frame?);
                    }
                    Ok(frames)
                }
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
    let traceparent = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("traceparent"))
        .map(|value| value.as_str().to_string());
    let tracestate = message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("tracestate"))
        .map(|value| value.as_str().to_string());

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
        feed: Option<&'a str>,
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
        feed: annotations.feed.as_deref(),
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
    let reply_to = request.reply_to;
    let annotations =
        ErrorAnnotationContext::from_request(&request.subject, &request.context, handler);
    let result =
        AssertUnwindSafe(handler.handle_frames(&request.subject, request.payload, request.context))
            .catch_unwind()
            .await;

    match result {
        Ok(Ok(payloads)) => Ok(reply_to.map(|reply_to| {
            payloads
                .into_iter()
                .map(|payload| encode_success_reply(reply_to.clone(), payload))
                .collect()
        })),
        Ok(Err(error)) => match reply_to {
            Some(reply_to) => Ok(Some(vec![encode_error_reply_with_context(
                reply_to,
                &error,
                &annotations,
            )])),
            None => Err(error),
        },
        Err(panic) => {
            let error = panic_to_server_error(panic);
            match reply_to {
                Some(reply_to) => Ok(Some(vec![encode_error_reply_with_context(
                    reply_to,
                    &error,
                    &annotations,
                )])),
                None => Err(error),
            }
        }
    }
}

pub(crate) async fn dispatch_response<H>(
    handler: &H,
    request: InboundRequest,
) -> Result<Option<(String, HandlerResponse, ErrorAnnotationContext)>, ServerError>
where
    H: RequestHandler,
{
    let reply_to = request.reply_to;
    let annotations =
        ErrorAnnotationContext::from_request(&request.subject, &request.context, handler);
    let result = AssertUnwindSafe(handler.handle_response(
        &request.subject,
        request.payload,
        request.context,
    ))
    .catch_unwind()
    .await;

    let Some(reply_to) = reply_to else {
        return match result {
            Ok(Ok(_)) => Ok(None),
            Ok(Err(error)) => Err(error),
            Err(panic) => Err(panic_to_server_error(panic)),
        };
    };

    match result {
        Ok(Ok(response)) => Ok(Some((reply_to, response, annotations))),
        Ok(Err(error)) => Ok(Some((
            reply_to.clone(),
            HandlerResponse::Error(
                encode_error_reply_with_context(reply_to, &error, &annotations).payload,
            ),
            annotations,
        ))),
        Err(panic) => {
            let error = panic_to_server_error(panic);
            Ok(Some((
                reply_to.clone(),
                HandlerResponse::Error(
                    encode_error_reply_with_context(reply_to, &error, &annotations).payload,
                ),
                annotations,
            )))
        }
    }
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
    reply_to: String,
    response: HandlerResponse,
    annotations: &ErrorAnnotationContext,
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
        HandlerResponse::Stream(mut stream) => loop {
            let frame = AssertUnwindSafe(stream.next()).catch_unwind().await;
            match frame {
                Ok(Some(Ok(payload))) => {
                    publish_reply(client, encode_success_reply(reply_to.clone(), payload)).await?;
                }
                Ok(Some(Err(error))) => {
                    publish_reply(
                        client,
                        encode_error_reply_with_context(reply_to.clone(), &error, annotations),
                    )
                    .await?;
                    break;
                }
                Ok(None) => break,
                Err(panic) => {
                    let error = panic_to_server_error(panic);
                    publish_reply(
                        client,
                        encode_error_reply_with_context(reply_to.clone(), &error, annotations),
                    )
                    .await?;
                    break;
                }
            }
        },
        HandlerResponse::FeedStream {
            mut stream,
            control_subject,
            feed_id,
        } => {
            let mut headers = HeaderMap::new();
            headers.insert("feed-status", "ready");
            headers.insert("feed-control-subject", control_subject.as_str());
            headers.insert("feed-id", feed_id.as_str());
            client
                .publish_with_headers(reply_to.clone(), headers, Bytes::new())
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;

            loop {
                let frame = AssertUnwindSafe(stream.next()).catch_unwind().await;
                match frame {
                    Ok(Some(Ok(payload))) => {
                        publish_reply(client, encode_success_reply(reply_to.clone(), payload))
                            .await?;
                    }
                    Ok(Some(Err(error))) => {
                        publish_reply(
                            client,
                            encode_error_reply_with_context(reply_to.clone(), &error, annotations),
                        )
                        .await?;
                        break;
                    }
                    Ok(None) => break,
                    Err(panic) => {
                        let error = panic_to_server_error(panic);
                        publish_reply(
                            client,
                            encode_error_reply_with_context(reply_to.clone(), &error, annotations),
                        )
                        .await?;
                        break;
                    }
                }
            }
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
                        Ok(Some((reply_to, response, annotations))) => {
                            tracing::debug!(%subject, %reply_to, request_id = ?annotations.request_id, "publishing service request reply");
                            if let Err(error) = publish_response(client, reply_to.clone(), response, &annotations).await {
                                tracing::warn!(%subject, %reply_to, request_id = ?annotations.request_id, %error, "service request reply publish failed");
                                return Err(error);
                            }
                        }
                        Ok(None) => {}
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
}
