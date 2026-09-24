mod download;
mod protocol;
pub(crate) use protocol::upload_subject_prefix;
mod upload;

pub use download::run_download_transfer_endpoint;
pub(crate) use protocol::transfer_frame_proof_payload;
use protocol::{download_subject_prefix, UploadTransferCompletionResult, UploadTransferControl};
pub use protocol::{
    DownloadTransferGrant, DownloadTransferGrantPlan, FileTransferInfo, TransferDownloadGrantArgs,
    TransferUploadGrantArgs, UploadTransferAck, UploadTransferChunk, UploadTransferCompletion,
    UploadTransferGrant, UploadTransferGrantPlan, MAX_TRANSFER_CHUNK_BYTES,
    TRANSFER_CONTROL_HEADER, TRANSFER_EOF_HEADER, TRANSFER_SEQUENCE_HEADER,
};
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;
pub use upload::UploadTransferSession;
use upload::UPLOAD_COMMIT;

use async_nats::header::HeaderMap;
use bytes::Bytes;
use futures_util::StreamExt;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::request_loop::encode_error_reply;
use super::{
    OperationTransferProgress, RequestContext, RequestValidator, ServerError,
    ServiceResourceBindings, StoreResourceBinding, StoreResourceClient,
};

/// Decode one upload transfer request frame from NATS headers and payload.
pub fn decode_upload_transfer_chunk(
    headers: Option<&HeaderMap>,
    payload: Bytes,
) -> Result<UploadTransferChunk, ServerError> {
    let seq = parse_transfer_sequence(headers)?;
    if let Some(value) = optional_header(headers, TRANSFER_EOF_HEADER) {
        return Err(ServerError::InvalidTransferHeader {
            header: TRANSFER_EOF_HEADER,
            value: value.to_string(),
        });
    }
    let (eof, cancel) = match optional_header(headers, TRANSFER_CONTROL_HEADER) {
        None => (false, false),
        Some("complete") => (true, false),
        Some("cancel") => (false, true),
        Some(value) => {
            return Err(ServerError::InvalidTransferHeader {
                header: TRANSFER_CONTROL_HEADER,
                value: value.to_string(),
            })
        }
    };

    Ok(UploadTransferChunk {
        seq,
        payload,
        eof,
        cancel,
    })
}

/// Run a NATS upload transfer endpoint and report operation progress for accepted body chunks.
pub async fn run_upload_transfer_endpoint_with_progress<C, V, F>(
    client: async_nats::Client,
    subscriber: impl futures_util::Stream<Item = async_nats::Message>,
    session: UploadTransferSession,
    store: C,
    validator: V,
    on_progress: F,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
    F: Fn(OperationTransferProgress) + Send + Sync + 'static,
{
    run_upload_transfer_endpoint_inner(
        client,
        subscriber,
        session,
        store,
        validator,
        on_progress,
        None,
    )
    .await
}

async fn run_upload_transfer_endpoint_inner<C, V, F>(
    client: async_nats::Client,
    subscriber: impl futures_util::Stream<Item = async_nats::Message>,
    mut session: UploadTransferSession,
    store: C,
    validator: V,
    on_progress: F,
    mut completion: Option<oneshot::Sender<UploadTransferCompletionResult>>,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
    F: Fn(OperationTransferProgress) + Send + Sync + 'static,
{
    let mut subscriber = Box::pin(subscriber);
    session.start(store).await?;
    let expiry = tokio::time::sleep(transfer_expiry_delay(&session.plan.grant.expires_at)?);
    tokio::pin!(expiry);
    loop {
        let message = tokio::select! {
            _ = &mut expiry => {
                session.abort().await;
                if let Some(sender) = completion.take() {
                    let _ = sender.send((
                        Err(ServerError::TransferExpired {
                            transfer_id: session.plan.grant.transfer_id.clone(),
                            expires_at: session.plan.grant.expires_at.clone(),
                        }),
                        None,
                    ));
                }
                return Ok(());
            }
            maybe_message = subscriber.next() => {
                let Some(message) = maybe_message else {
                    break;
                };
                message
            }
        };
        let reply_to = message.reply.as_ref().map(ToString::to_string);
        let subject = session.subject().to_string();
        let session_key = session.session_key().to_string();
        let upload_state = Arc::clone(&session.upload_state);
        enum Outcome {
            Handled(Result<(UploadTransferAck, OperationTransferProgress), ServerError>),
            Cancelled(Option<String>),
            Expired,
            Closed,
        }
        let outcome = {
            let handling = handle_upload_transfer_message(&mut session, &validator, &message);
            tokio::pin!(handling);
            loop {
                tokio::select! {
                _ = &mut expiry, if upload_state.load(Ordering::Acquire) < UPLOAD_COMMIT => {
                    break Outcome::Expired;
                }
                result = &mut handling => break Outcome::Handled(result),
                pending = subscriber.next() => {
                    let Some(pending) = pending else {
                        break Outcome::Closed;
                    };
                    let pending_reply = pending.reply.as_ref().map(ToString::to_string);
                    match decode_authenticated_upload_transfer_message(
                        &subject,
                        &session_key,
                        &validator,
                        &pending,
                    ).await {
                        Ok(chunk) if chunk.cancel => {
                            if upload_state.load(Ordering::Acquire) >= UPLOAD_COMMIT {
                                if let Some(pending_reply) = pending_reply {
                                    publish_error_reply(
                                        &client,
                                        pending_reply,
                                        &ServerError::Nats(
                                            "upload transfer cannot be cancelled after validated EOF"
                                                .to_string(),
                                        ),
                                    ).await?;
                                }
                            } else {
                                break Outcome::Cancelled(pending_reply);
                            }
                        }
                        Ok(_) => {
                            if let Some(pending_reply) = pending_reply {
                                publish_error_reply(
                                    &client,
                                    pending_reply,
                                    &ServerError::Nats(
                                        "upload transfer already has a pending frame".to_string(),
                                    ),
                                ).await?;
                            }
                        }
                        Err(error) => {
                            if let Some(pending_reply) = pending_reply {
                                publish_error_reply(&client, pending_reply, &error).await?;
                            }
                        }
                    }
                }
                }
            }
        };
        match outcome {
            Outcome::Expired => {
                session.abort().await;
                if let Some(sender) = completion.take() {
                    let _ = sender.send((
                        Err(ServerError::TransferExpired {
                            transfer_id: session.plan.grant.transfer_id.clone(),
                            expires_at: session.plan.grant.expires_at.clone(),
                        }),
                        None,
                    ));
                }
                return Ok(());
            }
            Outcome::Closed => break,
            Outcome::Cancelled(cancel_reply) => {
                session.abort().await;
                if let Some(sender) = completion.take() {
                    let _ = sender.send((
                        Err(ServerError::TransferCancelled {
                            transfer_id: session.plan.grant.transfer_id.clone(),
                        }),
                        None,
                    ));
                }
                if let Some(cancel_reply) = cancel_reply {
                    client
                        .publish(
                            cancel_reply,
                            Bytes::from_static(b"{\"status\":\"cancelled\"}"),
                        )
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                }
                return Ok(());
            }
            Outcome::Handled(result) => match result {
                Ok((ack, progress)) => {
                    match &ack {
                        UploadTransferAck::Continue if progress.chunk_bytes > 0 => {
                            on_progress(progress);
                        }
                        UploadTransferAck::Complete { info } => {
                            if let Some(sender) = completion.take() {
                                let (persisted, persisted_result) = oneshot::channel();
                                if sender.send((Ok(info.clone()), Some(persisted))).is_err() {
                                    return Err(ServerError::Nats(
                                        "upload transfer completion receiver closed".to_string(),
                                    ));
                                }
                                match persisted_result.await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(error)) => {
                                        if let Some(reply_to) = reply_to {
                                            publish_error_reply(&client, reply_to, &error).await?;
                                        }
                                        return Ok(());
                                    }
                                    Err(_) => {
                                        return Err(ServerError::Nats(
                                            "upload transfer persistence acknowledgement closed"
                                                .to_string(),
                                        ));
                                    }
                                }
                            }
                        }
                        UploadTransferAck::Continue | UploadTransferAck::Cancelled => {}
                    }
                    if let Some(reply_to) = reply_to {
                        client
                            .publish(reply_to, Bytes::from(serde_json::to_vec(&ack)?))
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                    }
                    if matches!(
                        ack,
                        UploadTransferAck::Complete { .. } | UploadTransferAck::Cancelled
                    ) {
                        return Ok(());
                    }
                }
                Err(error) => {
                    session.abort().await;
                    if let Some(sender) = completion.take() {
                        let _ = sender.send((Err(transfer_completion_error(&error)), None));
                    }
                    if let Some(reply_to) = reply_to {
                        if matches!(error, ServerError::TransferCancelled { .. }) {
                            client
                                .publish(
                                    reply_to,
                                    Bytes::from_static(b"{\"status\":\"cancelled\"}"),
                                )
                                .await
                                .map_err(|error| ServerError::Nats(error.to_string()))?;
                        } else {
                            publish_error_reply(&client, reply_to, &error).await?;
                        }
                    }
                    return Ok(());
                }
            },
        }
    }

    session.abort().await;
    if let Some(sender) = completion.take() {
        let _ = sender.send((
            Err(ServerError::TransferMissingEof {
                transfer_id: session.plan.grant.transfer_id.clone(),
            }),
            None,
        ));
    }

    Ok(())
}

fn transfer_completion_error(error: &ServerError) -> ServerError {
    match error {
        ServerError::TransferSessionMismatch {
            subject,
            actual_session_key,
        } => ServerError::TransferSessionMismatch {
            subject: subject.clone(),
            actual_session_key: actual_session_key.clone(),
        },
        ServerError::MissingSessionKey { subject } => ServerError::MissingSessionKey {
            subject: subject.clone(),
        },
        ServerError::MissingProof { subject } => ServerError::MissingProof {
            subject: subject.clone(),
        },
        ServerError::RequestDenied {
            subject,
            session_key,
        } => ServerError::RequestDenied {
            subject: subject.clone(),
            session_key: session_key.clone(),
        },
        ServerError::ReplyInboxMismatch {
            subject,
            session_key,
            reply_to,
            expected_prefix,
        } => ServerError::ReplyInboxMismatch {
            subject: subject.clone(),
            session_key: session_key.clone(),
            reply_to: reply_to.clone(),
            expected_prefix: expected_prefix.clone(),
        },
        ServerError::TransferObjectTooLarge {
            service_name,
            store,
            key,
            size,
            max_bytes,
        } => ServerError::TransferObjectTooLarge {
            service_name: service_name.clone(),
            store: store.clone(),
            key: key.clone(),
            size: *size,
            max_bytes: *max_bytes,
        },
        ServerError::TransferSequenceOutOfOrder {
            transfer_id,
            expected_seq,
            actual_seq,
        } => ServerError::TransferSequenceOutOfOrder {
            transfer_id: transfer_id.clone(),
            expected_seq: *expected_seq,
            actual_seq: *actual_seq,
        },
        ServerError::TransferMissingEof { transfer_id } => ServerError::TransferMissingEof {
            transfer_id: transfer_id.clone(),
        },
        ServerError::TransferAlreadyComplete { transfer_id } => {
            ServerError::TransferAlreadyComplete {
                transfer_id: transfer_id.clone(),
            }
        }
        ServerError::InvalidTransferId { value } => ServerError::InvalidTransferId {
            value: value.clone(),
        },
        ServerError::TransferExpired {
            transfer_id,
            expires_at,
        } => ServerError::TransferExpired {
            transfer_id: transfer_id.clone(),
            expires_at: expires_at.clone(),
        },
        ServerError::InvalidTransferExpiry {
            expires_at,
            details,
        } => ServerError::InvalidTransferExpiry {
            expires_at: expires_at.clone(),
            details: details.clone(),
        },
        ServerError::TransferObjectMissing { store, key } => ServerError::TransferObjectMissing {
            store: store.clone(),
            key: key.clone(),
        },
        ServerError::InvalidTransferChunkSize { chunk_bytes } => {
            ServerError::InvalidTransferChunkSize {
                chunk_bytes: *chunk_bytes,
            }
        }
        ServerError::MissingTransferHeader { header } => {
            ServerError::MissingTransferHeader { header }
        }
        ServerError::InvalidTransferHeader { header, value } => {
            ServerError::InvalidTransferHeader {
                header,
                value: value.clone(),
            }
        }
        ServerError::TransferObjectSizeMismatch {
            store,
            key,
            expected_size,
            actual_size,
        } => ServerError::TransferObjectSizeMismatch {
            store: store.clone(),
            key: key.clone(),
            expected_size: *expected_size,
            actual_size: *actual_size,
        },
        ServerError::TransferDigestMismatch {
            transfer_id,
            expected_digest,
            actual_digest,
        } => ServerError::TransferDigestMismatch {
            transfer_id: transfer_id.clone(),
            expected_digest: expected_digest.clone(),
            actual_digest: actual_digest.clone(),
        },
        ServerError::TransferCancelled { transfer_id } => ServerError::TransferCancelled {
            transfer_id: transfer_id.clone(),
        },
        ServerError::StoreCommitIndeterminate { key, message } => {
            ServerError::StoreCommitIndeterminate {
                key: key.clone(),
                message: message.clone(),
            }
        }
        ServerError::StoreCommittedCleanupFailed { key, message } => {
            ServerError::StoreCommittedCleanupFailed {
                key: key.clone(),
                message: message.clone(),
            }
        }
        _ => ServerError::Nats(error.to_string()),
    }
}

async fn abort_store_task<T>(task: &mut Option<JoinHandle<T>>) {
    if let Some(task) = task.take() {
        task.abort();
        let _ = task.await;
    }
}

/// Subscribe and run an upload transfer endpoint that reports operation progress.
pub async fn spawn_upload_transfer_endpoint_with_progress<C, V, F>(
    client: async_nats::Client,
    session: UploadTransferSession,
    store: C,
    validator: V,
    on_progress: F,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
    F: Fn(OperationTransferProgress) + Send + Sync + 'static,
{
    let subject = session.subject().to_string();
    tracing::info!(subject = %subject, "subscribing upload transfer endpoint");
    let subscriber = client.subscribe(subject.clone()).await.map_err(|error| {
        ServerError::Nats(format!(
            "failed to subscribe to upload transfer subject '{subject}': {error}"
        ))
    })?;
    tokio::spawn(async move {
        tracing::debug!(subject = %subject, "upload transfer endpoint task started");
        if let Err(error) = run_upload_transfer_endpoint_with_progress(
            client,
            subscriber,
            session,
            store,
            validator,
            on_progress,
        )
        .await
        {
            tracing::error!(subject = %subject, error = %error, "upload transfer endpoint failed");
        }
        tracing::debug!(subject = %subject, "upload transfer endpoint task ended");
    });
    Ok(())
}

/// Subscribe and run an upload transfer endpoint that can be awaited until durable storage.
pub async fn spawn_upload_transfer_endpoint_with_completion<C, V>(
    client: async_nats::Client,
    session: UploadTransferSession,
    store: C,
    validator: V,
) -> Result<UploadTransferCompletion, ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
{
    spawn_upload_transfer_endpoint_with_progress_and_completion(
        client,
        session,
        store,
        validator,
        |_| {},
    )
    .await
}

/// Subscribe and run an upload transfer endpoint with progress and durable completion reporting.
pub async fn spawn_upload_transfer_endpoint_with_progress_and_completion<C, V, F>(
    client: async_nats::Client,
    session: UploadTransferSession,
    store: C,
    validator: V,
    on_progress: F,
) -> Result<UploadTransferCompletion, ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
    F: Fn(OperationTransferProgress) + Send + Sync + 'static,
{
    let subject = session.subject().to_string();
    tracing::info!(subject = %subject, "subscribing upload transfer endpoint");
    let subscriber = client.subscribe(subject.clone()).await.map_err(|error| {
        ServerError::Nats(format!(
            "failed to subscribe to upload transfer subject '{subject}': {error}"
        ))
    })?;
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        tracing::debug!(subject = %subject, "upload transfer endpoint task started");
        if let Err(error) = run_upload_transfer_endpoint_inner(
            client,
            subscriber,
            session,
            store,
            validator,
            on_progress,
            Some(sender),
        )
        .await
        {
            tracing::error!(subject = %subject, error = %error, "upload transfer endpoint failed");
        }
        tracing::debug!(subject = %subject, "upload transfer endpoint task ended");
    });
    Ok(UploadTransferCompletion { receiver })
}

/// Subscribe and run one planned download transfer endpoint in the background.
pub async fn spawn_download_transfer_endpoint<C, V>(
    client: async_nats::Client,
    plan: DownloadTransferGrantPlan,
    store: C,
    validator: V,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
{
    let subject = plan.grant.subject.clone();
    tracing::info!(subject = %subject, "subscribing download transfer endpoint");
    let subscriber = client.subscribe(subject.clone()).await.map_err(|error| {
        ServerError::Nats(format!(
            "failed to subscribe to download transfer subject '{subject}': {error}"
        ))
    })?;
    tokio::spawn(async move {
        tracing::debug!(subject = %subject, "download transfer endpoint task started");
        if let Err(error) =
            run_download_transfer_endpoint(client, subscriber, plan, store, validator).await
        {
            tracing::error!(subject = %subject, error = %error, "download transfer endpoint failed");
        }
        tracing::debug!(subject = %subject, "download transfer endpoint task ended");
    });
    Ok(())
}

async fn handle_upload_transfer_message<V>(
    session: &mut UploadTransferSession,
    validator: &V,
    message: &async_nats::Message,
) -> Result<(UploadTransferAck, OperationTransferProgress), ServerError>
where
    V: RequestValidator,
{
    let chunk = decode_authenticated_upload_transfer_message(
        session.subject(),
        session.session_key(),
        validator,
        message,
    )
    .await?;
    if chunk.cancel {
        session.abort().await;
        return Err(ServerError::TransferCancelled {
            transfer_id: session.plan.grant.transfer_id.clone(),
        });
    }
    let progress = session.progress_for_chunk(&chunk);
    tracing::debug!(
        subject = %session.subject(),
        seq = chunk.seq,
        bytes = chunk.payload.len(),
        eof = chunk.eof,
        "received upload transfer chunk"
    );
    let now = current_time_iso()?;
    let ack = session.receive_at(chunk, &now).await?;
    Ok((ack, progress))
}

async fn decode_authenticated_upload_transfer_message<V>(
    subject: &str,
    session_key: &str,
    validator: &V,
    message: &async_nats::Message,
) -> Result<UploadTransferChunk, ServerError>
where
    V: RequestValidator,
{
    let context = transfer_request_context(message);
    let seq = parse_transfer_sequence(message.headers.as_ref())?;
    let control = optional_header(message.headers.as_ref(), TRANSFER_CONTROL_HEADER);
    let proof_payload = transfer_frame_proof_payload(seq, control, &message.payload);
    validate_transfer_request(subject, &proof_payload, &context, session_key, validator).await?;
    let chunk = decode_upload_transfer_chunk(message.headers.as_ref(), message.payload.clone())?;
    if chunk.cancel {
        if !matches!(
            serde_json::from_slice(&message.payload),
            Ok(UploadTransferControl::Cancel)
        ) {
            return Err(ServerError::Nats(
                "invalid upload cancellation control".to_string(),
            ));
        }
        return Ok(chunk);
    }
    Ok(chunk)
}

async fn validate_download_transfer_message<V>(
    plan: &DownloadTransferGrantPlan,
    validator: &V,
    message: &async_nats::Message,
) -> Result<(), ServerError>
where
    V: RequestValidator,
{
    let context = transfer_request_context(message);
    let seq = parse_transfer_sequence(message.headers.as_ref())?;
    let control = optional_header(message.headers.as_ref(), TRANSFER_CONTROL_HEADER);
    let proof_payload = transfer_frame_proof_payload(seq, control, &message.payload);
    validate_transfer_request(
        &plan.grant.subject,
        &proof_payload,
        &context,
        &plan.grant.session_key,
        validator,
    )
    .await?;
    let now = current_time_iso()?;
    enforce_transfer_not_expired(&plan.grant.transfer_id, &plan.grant.expires_at, &now)
}

fn transfer_request_context(message: &async_nats::Message) -> RequestContext {
    RequestContext {
        resuming: false,
        operation_progress: None,
        subject: message.subject.to_string(),
        session_key: message
            .headers
            .as_ref()
            .and_then(|headers| headers.get("session-key"))
            .map(|value| value.as_str().to_owned()),
        proof: optional_header(message.headers.as_ref(), "proof").map(ToString::to_string),
        authorization_context: optional_header(message.headers.as_ref(), "authorization-context")
            .map(ToString::to_string),
        iat: optional_header(message.headers.as_ref(), "iat").and_then(|value| value.parse().ok()),
        request_id: optional_header(message.headers.as_ref(), "request-id")
            .map(ToString::to_string),
        required_capabilities: None,
        required_permission: None,
        reply_to: message.reply.as_ref().map(ToString::to_string),
        caller: None,
        traceparent: optional_header(message.headers.as_ref(), "traceparent")
            .map(ToString::to_string),
        tracestate: optional_header(message.headers.as_ref(), "tracestate")
            .map(ToString::to_string),
    }
}

async fn validate_transfer_request<V>(
    subject: &str,
    payload: &Bytes,
    context: &RequestContext,
    expected_session_key: &str,
    validator: &V,
) -> Result<(), ServerError>
where
    V: RequestValidator,
{
    if context.proof.as_deref().is_none_or(str::is_empty) {
        return Err(ServerError::MissingProof {
            subject: subject.to_string(),
        });
    }
    let validation = validator
        .validate_possession(subject, payload, context)
        .await?;
    let actual_session_key = validation
        .caller
        .as_ref()
        .map(|caller| caller.session_key.clone())
        .unwrap_or_default();
    if actual_session_key != expected_session_key {
        return Err(ServerError::TransferSessionMismatch {
            subject: subject.to_string(),
            actual_session_key,
        });
    }

    if validation.allowed {
        Ok(())
    } else {
        Err(ServerError::RequestDenied {
            subject: subject.to_string(),
            session_key: actual_session_key,
        })
    }
}

async fn publish_download_chunk(
    client: &async_nats::Client,
    reply_to: &str,
    seq: u64,
    payload: Bytes,
    eof: bool,
) -> Result<(), ServerError> {
    let mut headers = HeaderMap::new();
    headers.insert(TRANSFER_SEQUENCE_HEADER, seq.to_string().as_str());
    if eof {
        headers.insert(TRANSFER_EOF_HEADER, "true");
    }
    client
        .publish_with_headers(reply_to.to_string(), headers, payload)
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))
}

async fn publish_error_reply(
    client: &async_nats::Client,
    reply_to: String,
    error: &ServerError,
) -> Result<(), ServerError> {
    let reply = encode_error_reply(reply_to, error);
    let mut headers = HeaderMap::new();
    headers.insert("status", "error");
    client
        .publish_with_headers(reply.reply_to, headers, reply.payload)
        .await
        .map_err(|error| ServerError::Nats(error.to_string()))
}

fn required_header<'a>(
    headers: Option<&'a HeaderMap>,
    header: &'static str,
) -> Result<&'a str, ServerError> {
    optional_header(headers, header).ok_or(ServerError::MissingTransferHeader { header })
}

fn parse_transfer_sequence(headers: Option<&HeaderMap>) -> Result<u64, ServerError> {
    let seq = required_header(headers, TRANSFER_SEQUENCE_HEADER)?;
    seq.parse::<u64>()
        .map_err(|_| ServerError::InvalidTransferHeader {
            header: TRANSFER_SEQUENCE_HEADER,
            value: seq.to_string(),
        })
}

fn optional_header<'a>(headers: Option<&'a HeaderMap>, header: &str) -> Option<&'a str> {
    headers
        .and_then(|headers| headers.get(header))
        .map(async_nats::header::HeaderValue::as_str)
}

/// Build upload transfer grant metadata from resolved service resource bindings.
pub fn plan_upload_transfer_grant(
    args: TransferUploadGrantArgs<'_>,
) -> Result<UploadTransferGrantPlan, ServerError> {
    validate_chunk_bytes(args.chunk_bytes)?;
    let store = store_binding(args.service_name, args.resources, args.store)?;
    let max_bytes = effective_upload_max_bytes(args.max_bytes, store.max_object_bytes);
    validate_transfer_id(args.transfer_id)?;

    Ok(UploadTransferGrantPlan {
        grant: UploadTransferGrant {
            type_name: "TransferGrant".to_string(),
            direction: "send".to_string(),
            service: args.service_name.to_string(),
            session_key: args.session_key.to_string(),
            transfer_id: args.transfer_id.to_string(),
            subject: transfer_subject(
                upload_subject_prefix(),
                args.service_session_key,
                args.transfer_id,
            ),
            expires_at: args.expires_at.to_string(),
            chunk_bytes: args.chunk_bytes,
            max_bytes,
            content_type: args.content_type.map(ToString::to_string),
            metadata: args.metadata,
        },
        store_alias: args.store.to_string(),
        store: store.name.clone(),
        key: args.key.to_string(),
    })
}

/// Build download transfer grant metadata from resolved service resource bindings.
pub fn plan_download_transfer_grant(
    args: TransferDownloadGrantArgs<'_>,
) -> Result<DownloadTransferGrantPlan, ServerError> {
    validate_chunk_bytes(args.chunk_bytes)?;
    let store = store_binding(args.service_name, args.resources, args.store)?;
    enforce_max_object_bytes(
        args.service_name,
        args.store,
        &args.info,
        store.max_object_bytes,
    )?;
    validate_transfer_id(args.transfer_id)?;

    Ok(DownloadTransferGrantPlan {
        grant: DownloadTransferGrant {
            type_name: "TransferGrant".to_string(),
            direction: "receive".to_string(),
            service: args.service_name.to_string(),
            session_key: args.session_key.to_string(),
            transfer_id: args.transfer_id.to_string(),
            subject: transfer_subject(
                download_subject_prefix(),
                args.service_session_key,
                args.transfer_id,
            ),
            expires_at: args.expires_at.to_string(),
            chunk_bytes: args.chunk_bytes,
            info: args.info,
        },
        store_alias: args.store.to_string(),
        store: store.name.clone(),
        max_object_bytes: store
            .max_object_bytes
            .and_then(|value| u64::try_from(value).ok()),
    })
}

fn store_binding<'a>(
    service_name: &str,
    resources: &'a ServiceResourceBindings,
    store: &str,
) -> Result<&'a StoreResourceBinding, ServerError> {
    resources
        .store
        .get(store)
        .ok_or_else(|| ServerError::MissingResourceBinding {
            service_name: service_name.to_string(),
            resource_kind: "store".to_string(),
            resource_name: store.to_string(),
        })
}

fn effective_upload_max_bytes(
    requested: Option<u64>,
    store_max_object_bytes: Option<i64>,
) -> Option<u64> {
    match (
        requested,
        store_max_object_bytes.and_then(|value| u64::try_from(value).ok()),
    ) {
        (Some(requested), Some(store_max)) => Some(requested.min(store_max)),
        (Some(requested), None) => Some(requested),
        (None, Some(store_max)) => Some(store_max),
        (None, None) => None,
    }
}

fn enforce_max_object_bytes(
    service_name: &str,
    store: &str,
    info: &FileTransferInfo,
    store_max_object_bytes: Option<i64>,
) -> Result<(), ServerError> {
    let Some(max_bytes) = store_max_object_bytes.and_then(|value| u64::try_from(value).ok()) else {
        return Ok(());
    };

    if info.size > max_bytes {
        return Err(ServerError::TransferObjectTooLarge {
            service_name: service_name.to_string(),
            store: store.to_string(),
            key: info.key.clone(),
            size: info.size,
            max_bytes,
        });
    }

    Ok(())
}

fn enforce_upload_max_bytes(plan: &UploadTransferGrantPlan, size: u64) -> Result<(), ServerError> {
    let Some(max_bytes) = plan.grant.max_bytes else {
        return Ok(());
    };

    if size > max_bytes {
        return Err(ServerError::TransferObjectTooLarge {
            service_name: plan.grant.service.clone(),
            store: plan.store_alias.clone(),
            key: plan.key.clone(),
            size,
            max_bytes,
        });
    }

    Ok(())
}

fn validate_chunk_bytes(chunk_bytes: u64) -> Result<(), ServerError> {
    if !(1..=MAX_TRANSFER_CHUNK_BYTES).contains(&chunk_bytes) {
        return Err(ServerError::InvalidTransferChunkSize { chunk_bytes });
    }
    Ok(())
}

fn current_time_iso() -> Result<String, ServerError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| ServerError::InvalidTransferExpiry {
            expires_at: "now".to_string(),
            details: error.to_string(),
        })
}

fn enforce_transfer_not_expired(
    transfer_id: &str,
    expires_at: &str,
    now_iso: &str,
) -> Result<(), ServerError> {
    let expires_at_time = parse_transfer_time(expires_at)?;
    let now = parse_transfer_time(now_iso)?;
    if now >= expires_at_time {
        return Err(ServerError::TransferExpired {
            transfer_id: transfer_id.to_string(),
            expires_at: expires_at.to_string(),
        });
    }
    Ok(())
}

fn transfer_expiry_delay(expires_at: &str) -> Result<Duration, ServerError> {
    let expires_at_time = parse_transfer_time(expires_at)?;
    let remaining = expires_at_time - OffsetDateTime::now_utc();
    let millis = remaining.whole_milliseconds();
    if millis <= 0 {
        return Ok(Duration::ZERO);
    }
    Ok(Duration::from_millis(millis.min(u64::MAX as i128) as u64))
}

fn parse_transfer_time(value: &str) -> Result<OffsetDateTime, ServerError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|error| ServerError::InvalidTransferExpiry {
        expires_at: value.to_string(),
        details: error.to_string(),
    })
}

pub(crate) fn transfer_subject(prefix: &str, session_key: &str, transfer_id: &str) -> String {
    let session_prefix: String = session_key.chars().take(16).collect();
    format!("{prefix}.{session_prefix}.{transfer_id}")
}

fn validate_transfer_id(transfer_id: &str) -> Result<(), ServerError> {
    let invalid = transfer_id.is_empty()
        || transfer_id
            .chars()
            .any(|ch| matches!(ch, '.' | '*' | '>' | '/') || ch.is_whitespace() || ch.is_control());

    if invalid {
        return Err(ServerError::InvalidTransferId {
            value: transfer_id.to_string(),
        });
    }

    Ok(())
}

pub(super) fn transfer_digests_match(left: &str, right: &str) -> bool {
    left.trim_end_matches('=') == right.trim_end_matches('=')
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use futures_util::future::BoxFuture;
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

    use crate::service::{RequestValidation, StoreObjectInfo, VerifiedCaller};

    use super::*;

    #[test]
    fn transfer_digest_comparison_accepts_object_store_padding() {
        assert!(transfer_digests_match("abc=", "abc"));
        assert!(!transfer_digests_match("SHA-256=abc", "abc"));
    }

    #[derive(Debug, Clone)]
    struct CountingValidator {
        calls: Arc<AtomicUsize>,
        allowed: bool,
        session_key: &'static str,
    }

    #[derive(Debug, Clone, Default)]
    struct CommitOnEofStore {
        bytes: Arc<Mutex<Option<Vec<u8>>>>,
        reported_key: Option<String>,
        reported_size: Option<u64>,
        reported_digest: Option<String>,
    }

    impl StoreResourceClient for CommitOnEofStore {
        async fn read_into<W>(
            &self,
            _key: &str,
            _writer: &mut W,
        ) -> Result<Option<StoreObjectInfo>, ServerError>
        where
            W: AsyncWrite + Unpin + Send,
        {
            Ok(None)
        }

        async fn write_from<R>(
            &self,
            key: &str,
            reader: &mut R,
        ) -> Result<StoreObjectInfo, ServerError>
        where
            R: AsyncRead + Unpin + Send,
        {
            let mut bytes = Vec::new();
            reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| ServerError::Nats(format!("test store upload failed: {error}")))?;
            let size = bytes.len() as u64;
            *self.bytes.lock().expect("test store lock") = Some(bytes);
            Ok(StoreObjectInfo {
                key: self.reported_key.clone().unwrap_or_else(|| key.to_string()),
                size: self.reported_size.unwrap_or(size),
                digest: self.reported_digest.clone().or_else(|| {
                    Some(format!(
                        "SHA-256={}",
                        URL_SAFE_NO_PAD.encode(Sha256::digest(
                            self.bytes
                                .lock()
                                .expect("test store lock")
                                .as_deref()
                                .unwrap_or_default()
                        ))
                    ))
                }),
                modified_at: None,
            })
        }

        async fn list(&self) -> Result<Vec<String>, ServerError> {
            Ok(Vec::new())
        }

        async fn delete(&self, _key: &str) -> Result<(), ServerError> {
            Ok(())
        }
    }

    fn upload_plan(chunk_bytes: u64) -> UploadTransferGrantPlan {
        UploadTransferGrantPlan {
            grant: UploadTransferGrant {
                type_name: "TransferGrant".to_string(),
                direction: "send".to_string(),
                service: "test-service".to_string(),
                session_key: "session".to_string(),
                transfer_id: "transfer-1".to_string(),
                subject: "transfer.v1.upload.service.transfer-1".to_string(),
                expires_at: "2099-01-01T00:00:00Z".to_string(),
                chunk_bytes,
                max_bytes: Some(32),
                content_type: None,
                metadata: BTreeMap::new(),
            },
            store_alias: "objects".to_string(),
            store: "physical_store".to_string(),
            key: "object".to_string(),
        }
    }

    #[tokio::test]
    async fn upload_session_streams_and_commits_only_after_authenticated_completion() {
        let store = CommitOnEofStore::default();
        let mut session = UploadTransferSession::new(upload_plan(4), "2026-08-26T00:00:00Z");
        session.start(store.clone()).await.unwrap();
        session
            .receive_at(
                UploadTransferChunk {
                    seq: 0,
                    payload: Bytes::from_static(b"1234"),
                    eof: false,
                    cancel: false,
                },
                "2026-08-26T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(store.bytes.lock().expect("test store lock").is_none());

        let digest = format!(
            "SHA-256={}",
            URL_SAFE_NO_PAD.encode(Sha256::digest(b"1234"))
        );
        let completion =
            serde_json::to_vec(&UploadTransferControl::Complete { size: 4, digest }).unwrap();
        let ack = session
            .receive_at(
                UploadTransferChunk {
                    seq: 1,
                    payload: Bytes::from(completion),
                    eof: true,
                    cancel: false,
                },
                "2026-08-26T00:00:00Z",
            )
            .await
            .unwrap();

        assert!(matches!(ack, UploadTransferAck::Complete { .. }));
        assert_eq!(
            store.bytes.lock().expect("test store lock").as_deref(),
            Some(b"1234".as_slice())
        );
    }

    #[tokio::test]
    async fn upload_session_abort_never_commits_a_partial_object() {
        let store = CommitOnEofStore::default();
        let mut session = UploadTransferSession::new(upload_plan(4), "2026-08-26T00:00:00Z");
        session.start(store.clone()).await.unwrap();
        session
            .receive_at(
                UploadTransferChunk {
                    seq: 0,
                    payload: Bytes::from_static(b"1234"),
                    eof: false,
                    cancel: false,
                },
                "2026-08-26T00:00:00Z",
            )
            .await
            .unwrap();
        session.abort().await;
        tokio::task::yield_now().await;
        assert!(store.bytes.lock().expect("test store lock").is_none());
    }

    #[test]
    fn cancellation_json_is_data_without_an_explicit_control_header() {
        let mut headers = HeaderMap::new();
        headers.insert(TRANSFER_SEQUENCE_HEADER, "0");
        let chunk = decode_upload_transfer_chunk(
            Some(&headers),
            Bytes::from_static(br#"{"action":"cancel"}"#),
        )
        .expect("decode arbitrary data frame");

        assert!(!chunk.cancel);
        assert!(!chunk.eof);
        assert_eq!(chunk.payload, Bytes::from_static(br#"{"action":"cancel"}"#));
    }

    #[test]
    fn authenticated_framing_matches_shared_transfer_v1_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../integration/fixtures/protocol/transfer-v1/vectors.json"
        ))
        .unwrap();
        for vector in vectors["vectors"].as_array().unwrap() {
            let payload = URL_SAFE_NO_PAD
                .decode(vector["payloadBase64url"].as_str().unwrap())
                .unwrap();
            let framed = transfer_frame_proof_payload(
                vector["seq"].as_u64().unwrap(),
                vector["control"].as_str(),
                &payload,
            );
            assert_eq!(
                URL_SAFE_NO_PAD.encode(framed),
                vector["framedBase64url"].as_str().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn upload_rejects_backend_final_key_size_and_digest_mismatches() {
        let expected_digest = format!(
            "SHA-256={}",
            URL_SAFE_NO_PAD.encode(Sha256::digest(b"1234"))
        );
        for (store, expected_error) in [
            (
                CommitOnEofStore {
                    reported_size: Some(5),
                    ..Default::default()
                },
                "size",
            ),
            (
                CommitOnEofStore {
                    reported_digest: Some("SHA-256=wrong".to_string()),
                    ..Default::default()
                },
                "digest",
            ),
            (
                CommitOnEofStore {
                    reported_key: Some("wrong-key".to_string()),
                    ..Default::default()
                },
                "key",
            ),
        ] {
            let mut session = UploadTransferSession::new(upload_plan(4), "2026-08-26T00:00:00Z");
            session.start(store).await.unwrap();
            session
                .receive_at(
                    UploadTransferChunk {
                        seq: 0,
                        payload: Bytes::from_static(b"1234"),
                        eof: false,
                        cancel: false,
                    },
                    "2026-08-26T00:00:00Z",
                )
                .await
                .unwrap();
            let completion = serde_json::to_vec(&UploadTransferControl::Complete {
                size: 4,
                digest: expected_digest.clone(),
            })
            .unwrap();
            let error = session
                .receive_at(
                    UploadTransferChunk {
                        seq: 1,
                        payload: Bytes::from(completion),
                        eof: true,
                        cancel: false,
                    },
                    "2026-08-26T00:00:00Z",
                )
                .await
                .unwrap_err();
            assert!(match expected_error {
                "size" => matches!(&error, ServerError::TransferObjectSizeMismatch { .. }),
                "digest" => matches!(&error, ServerError::TransferDigestMismatch { .. }),
                "key" =>
                    matches!(&error, ServerError::Nats(message) if message.contains("key mismatch")),
                _ => unreachable!(),
            });
        }
    }

    impl RequestValidator for CountingValidator {
        fn validate<'a>(
            &'a self,
            _subject: &'a str,
            _payload: &'a Bytes,
            _context: &'a RequestContext,
        ) -> BoxFuture<'a, Result<RequestValidation, ServerError>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(RequestValidation {
                    allowed: self.allowed,
                    caller: Some(VerifiedCaller {
                        session_key: self.session_key.to_owned(),
                        inbox_prefix: "_INBOX.test".to_owned(),
                        context_digest: "context".to_owned(),
                        connection_id: "con_test".to_owned(),
                        login_session_id: None,
                        principal_id: "usr_test".to_owned(),
                        principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
                        participant_id: "test@v1".to_owned(),
                        platform_privileges: Vec::new(),
                        deployment_id: None,
                        instance_id: None,
                    }),
                    inbox_prefix: Some("_INBOX.test".to_owned()),
                })
            })
        }

        fn revalidate_current<'a>(
            &'a self,
            _context: &'a RequestContext,
        ) -> BoxFuture<'a, Result<bool, ServerError>> {
            Box::pin(async { Ok(self.allowed) })
        }
    }

    #[tokio::test]
    async fn transfer_validation_rejects_verified_context_session_mismatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let validator = CountingValidator {
            calls: Arc::clone(&calls),
            allowed: true,
            session_key: "wrong-session",
        };
        let context = RequestContext {
            resuming: false,
            operation_progress: None,
            subject: "transfer.v1.upload.session.transfer-1".to_string(),
            session_key: Some("expected-session".to_string()),
            proof: Some("proof".to_string()),
            authorization_context: None,
            iat: None,
            request_id: None,
            required_capabilities: None,
            required_permission: None,
            reply_to: None,
            caller: None,
            traceparent: None,
            tracestate: None,
        };

        let error = validate_transfer_request(
            "transfer.v1.upload.session.transfer-1",
            &Bytes::new(),
            &context,
            "expected-session",
            &validator,
        )
        .await
        .expect_err("session mismatch");

        assert!(matches!(
            error,
            ServerError::TransferSessionMismatch { actual_session_key, .. }
                if actual_session_key == "wrong-session"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transfer_validation_requires_proof_before_session_mismatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let validator = CountingValidator {
            calls: Arc::clone(&calls),
            allowed: true,
            session_key: "wrong-session",
        };
        let context = RequestContext {
            resuming: false,
            operation_progress: None,
            subject: "transfer.v1.upload.session.transfer-1".to_string(),
            session_key: None,
            proof: None,
            authorization_context: None,
            iat: None,
            request_id: None,
            required_capabilities: None,
            required_permission: None,
            reply_to: None,
            caller: None,
            traceparent: None,
            tracestate: None,
        };

        let error = validate_transfer_request(
            "transfer.v1.upload.session.transfer-1",
            &Bytes::new(),
            &context,
            "expected-session",
            &validator,
        )
        .await
        .expect_err("missing proof");

        assert!(matches!(error, ServerError::MissingProof { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transfer_validation_maps_denied_validator_to_request_denied() {
        let calls = Arc::new(AtomicUsize::new(0));
        let validator = CountingValidator {
            calls: Arc::clone(&calls),
            allowed: false,
            session_key: "expected-session",
        };
        let context = RequestContext {
            resuming: false,
            operation_progress: None,
            subject: "transfer.v1.download.session.transfer-1".to_string(),
            session_key: Some("expected-session".to_string()),
            proof: Some("proof".to_string()),
            authorization_context: None,
            iat: None,
            request_id: None,
            required_capabilities: None,
            required_permission: None,
            reply_to: None,
            caller: None,
            traceparent: None,
            tracestate: None,
        };

        let error = validate_transfer_request(
            "transfer.v1.download.session.transfer-1",
            &Bytes::new(),
            &context,
            "expected-session",
            &validator,
        )
        .await
        .expect_err("denied");

        assert!(matches!(
            error,
            ServerError::RequestDenied { session_key, .. } if session_key == "expected-session"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
