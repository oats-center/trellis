use super::{TransportLease, TrellisClient, TrellisClientError};
use crate::{
    data_plane::{
        credit::CreditScheduler,
        flow::{FlowLimits, SenderWindow},
    },
    service::transfer::{
        self,
        wire::{self, TransferPeer},
    },
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest as _, Sha256};
use std::{collections::VecDeque, sync::atomic::Ordering};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::watch,
};
use trellis_protocol::transfer::*;

pub use crate::service::transfer::{DownloadTransferGrant, TransferIdentity, UploadTransferGrant};
pub use trellis_protocol::transfer::TransferFileInfo as FileInfo;

/// Cloneable cancellation for an active transfer.
#[derive(Debug, Clone)]
pub struct TransferCancellation {
    sender: watch::Sender<bool>,
}
impl TransferCancellation {
    /// Create an active cancellation signal.
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }
    /// Request terminal cancellation.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
    async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}
impl Default for TransferCancellation {
    fn default() -> Self {
        Self::new()
    }
}

fn fail(value: impl std::fmt::Display) -> TrellisClientError {
    TrellisClientError::TransferProtocol(value.to_string())
}
async fn cancelled(signal: Option<&TransferCancellation>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Exact immutable grant coordinates shared by the two streaming directions.
struct Coordinates<'a> {
    id: &'a str,
    expires: &'a str,
    provider: &'a TransferIdentity,
    consumer: &'a TransferIdentity,
    data: &'a str,
    control: &'a str,
    signal: &'a str,
    direction: TransferDirection,
    max_frame: u64,
    frames: u64,
    bytes: u64,
}

fn validate_coordinates(
    client: &TrellisClient,
    coordinates: &Coordinates<'_>,
    format: &str,
    kind: &str,
    direction: &str,
) -> Result<(), TrellisClientError> {
    if format != TRANSFER_VERSION
        || kind != "TransferGrant"
        || direction
            != match coordinates.direction {
                TransferDirection::Send => "send",
                TransferDirection::Receive => "receive",
            }
    {
        return Err(fail("invalid Transfer v2 grant discriminator"));
    }
    if coordinates.consumer.session_key != client.auth().session_key
        || coordinates.consumer.connection_id != client.own_connection_id()?
    {
        return Err(fail("transfer grant consumer identity mismatch"));
    }
    transfer::transfer_expiry_delay(coordinates.expires).map_err(fail)?;
    Ok(())
}

struct Active {
    lease: TransportLease,
    signals: async_nats::Subscriber,
    data: Option<async_nats::Subscriber>,
    peer: TransferPeer,
    local: TransferPeer,
    max_frame: u64,
}

async fn activate(
    client: &TrellisClient,
    coordinates: &Coordinates<'_>,
    cancellation: Option<&TransferCancellation>,
) -> Result<Active, TrellisClientError> {
    let publish = match coordinates.direction {
        TransferDirection::Send => {
            vec![coordinates.data.to_owned(), coordinates.control.to_owned()]
        }
        TransferDirection::Receive => vec![coordinates.control.to_owned()],
    };
    let mut subscribe = vec![coordinates.signal.to_owned()];
    if coordinates.direction == TransferDirection::Receive {
        subscribe.push(coordinates.data.to_owned());
    }
    let deadline = client.transport_deadline();
    let lease = tokio::select! { biased; _ = cancelled(cancellation) => return Err(TrellisClientError::TransferCancelled), result = client.acquire_transport(&publish, &subscribe, deadline) => result? };
    let mut signals = lease
        .nats()
        .subscribe(coordinates.signal.to_owned())
        .await
        .map_err(fail)?;
    let data = if coordinates.direction == TransferDirection::Receive {
        Some(
            lease
                .nats()
                .subscribe(coordinates.data.to_owned())
                .await
                .map_err(fail)?,
        )
    } else {
        None
    };
    lease.nats().flush().await.map_err(fail)?;
    let max_frame = coordinates.max_frame.min(
        negotiate_transfer_max_frame_bytes(
            lease.nats().server_info().max_payload as u64,
            lease.nats().server_info().max_payload as u64,
        )
        .map_err(fail)?,
    );
    let body = TransferControl::Activate {
        format: TransferFormat::V2,
        kind: TransferControlKind::Control,
        transfer_id: coordinates.id.into(),
        control_seq: U64s::new(1),
        received_seq: U64s::new(0),
        consumed_seq: U64s::new(0),
        receive_max_frame_bytes: max_frame,
    };
    let payload = Bytes::from(serde_json::to_vec(&body)?);
    let descriptor = TransferFrameDescriptor {
        transfer_id: coordinates.id.into(),
        direction: coordinates.direction,
        sequence: U64s::new(1),
        kind: TransferFrameKind::Control,
        terminal: None,
    };
    let headers = wire::caller_headers(
        client,
        coordinates.control,
        coordinates.signal,
        &descriptor,
        &payload,
    )
    .map_err(fail)?;
    let request_id = wire::header(&headers, "request-id").map_err(fail)?;
    let local = TransferPeer::retain(
        client.authorization_provider(),
        &client.authorization_context_digest()?,
        coordinates.consumer,
        None,
    )
    .await
    .map_err(fail)?;
    lease
        .nats()
        .publish_with_reply_and_headers(
            coordinates.control.to_owned(),
            coordinates.signal.to_owned(),
            headers,
            payload.clone(),
        )
        .await
        .map_err(fail)?;
    let wait = async {
        let mut cancelled_sent = false;
        loop {
            let message = tokio::select! {
                _ = cancelled(cancellation), if !cancelled_sent => {
                    local.guard.reconcile().await.map_err(|error| fail(format!("activation cancellation authority: {error:?}")))?;
                    // Activation has not returned: no DATA has been authenticated or consumed.
                    let body = TransferControl::Cancel { format: TransferFormat::V2, kind: TransferControlKind::Control, transfer_id: coordinates.id.into(), control_seq: U64s::new(2), received_seq: U64s::new(0), consumed_seq: U64s::new(0), consumed_bytes: U64s::new(0) };
                    let payload = Bytes::from(serde_json::to_vec(&body)?);
                    let descriptor = TransferFrameDescriptor { transfer_id: coordinates.id.into(), direction: coordinates.direction, sequence: U64s::new(2), kind: TransferFrameKind::Control, terminal: None };
                    let headers = wire::caller_headers(client, coordinates.control, coordinates.signal, &descriptor, &payload).map_err(fail)?;
                    lease.nats().publish_with_reply_and_headers(coordinates.control.to_owned(), coordinates.signal.to_owned(), headers, payload.clone()).await.map_err(fail)?;
                    transfer::telemetry::frame(&descriptor, &payload, "client-tx");
                    cancelled_sent = true;
                    continue;
                },
                message = signals.next() => message,
            }
                .ok_or_else(|| fail("transfer signal subscription closed"))?;
            if message.subject.as_str() != coordinates.signal {
                continue;
            }
            let descriptor = match wire::descriptor(&message, coordinates.id, coordinates.direction)
            {
                Ok(value) if value.kind == TransferFrameKind::Signal => value,
                _ => continue,
            };
            let headers = message
                .headers
                .as_ref()
                .ok_or_else(|| fail("signal headers missing"))?;
            let digest = wire::header(headers, "authorization-context").map_err(fail)?;
            let peer = match TransferPeer::retain(
                client.authorization_provider(),
                &digest,
                coordinates.provider,
                None,
            )
            .await
            {
                Ok(peer) => peer,
                Err(_) => continue,
            };
            if peer.verify_provider(&message, &descriptor).await.is_err() {
                continue;
            }
            match parse_transfer_signal(&message.payload).map_err(fail)? {
                TransferSignal::Activated {
                    request_id: received_request,
                    control_seq,
                    max_frame_bytes,
                    window_frames,
                    window_bytes,
                    ..
                } if !cancelled_sent
                    && received_request == request_id
                    && control_seq.get() == 1
                    && max_frame_bytes <= max_frame
                    && window_frames == coordinates.frames
                    && window_bytes == coordinates.bytes =>
                {
                    return Ok((peer, max_frame_bytes))
                }
                TransferSignal::Error { code, .. } => {
                    return Err(fail(format!("transfer activation failed: {code:?}")))
                }
                TransferSignal::Cancelled { .. } if cancelled_sent => {
                    return Err(TrellisClientError::TransferCancelled)
                }
                _ => continue,
            }
        }
    };
    let (peer, max_frame) = tokio::select! {
        _ = lease.wait_lost() => return Err(fail("transfer pinned transport lost during activation")),
        result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), wait) => result.map_err(|_| TrellisClientError::Timeout)??,
    };
    Ok(Active {
        lease,
        signals,
        data,
        peer,
        local,
        max_frame,
    })
}

async fn verified_signal(
    active: &TransferPeer,
    message: async_nats::Message,
    coordinates: &Coordinates<'_>,
) -> Result<Option<TransferSignal>, TrellisClientError> {
    if message.subject.as_str() != coordinates.signal {
        return Ok(None);
    }
    let descriptor = match wire::descriptor(&message, coordinates.id, coordinates.direction) {
        Ok(value) if value.kind == TransferFrameKind::Signal => value,
        _ => return Ok(None),
    };
    if active.verify_provider(&message, &descriptor).await.is_err() {
        return Ok(None);
    }
    Ok(Some(parse_transfer_signal(&message.payload).map_err(fail)?))
}

async fn publish_control(
    client: &TrellisClient,
    active: &Active,
    coordinates: &Coordinates<'_>,
    body: TransferControl,
) -> Result<(), TrellisClientError> {
    active
        .local
        .guard
        .reconcile()
        .await
        .map_err(|e| fail(format!("transfer local authority: {e:?}")))?;
    active
        .peer
        .guard
        .check_now()
        .map_err(|e| fail(format!("transfer peer authority: {e:?}")))?;
    let payload = Bytes::from(serde_json::to_vec(&body)?);
    let descriptor = TransferFrameDescriptor {
        transfer_id: coordinates.id.into(),
        direction: coordinates.direction,
        sequence: body.control_seq(),
        kind: TransferFrameKind::Control,
        terminal: None,
    };
    let headers = wire::caller_headers(
        client,
        coordinates.control,
        coordinates.signal,
        &descriptor,
        &payload,
    )
    .map_err(fail)?;
    active
        .lease
        .nats()
        .publish_with_reply_and_headers(
            coordinates.control.to_owned(),
            coordinates.signal.to_owned(),
            headers,
            payload.clone(),
        )
        .await
        .map_err(fail)?;
    transfer::telemetry::frame(&descriptor, &payload, "client-tx");
    Ok(())
}

pub(crate) async fn put_upload_grant(
    client: &TrellisClient,
    grant: &UploadTransferGrant,
    body: impl AsRef<[u8]>,
) -> Result<FileInfo, TrellisClientError> {
    put_upload_grant_from(client, grant, &mut std::io::Cursor::new(body.as_ref())).await
}
pub(crate) async fn put_upload_grant_from<R: AsyncRead + Unpin + Send + ?Sized>(
    client: &TrellisClient,
    grant: &UploadTransferGrant,
    reader: &mut R,
) -> Result<FileInfo, TrellisClientError> {
    put_upload_grant_from_with_cancel(client, grant, reader, None).await
}

pub(crate) async fn put_upload_grant_from_with_cancel<R: AsyncRead + Unpin + Send + ?Sized>(
    client: &TrellisClient,
    grant: &UploadTransferGrant,
    reader: &mut R,
    cancellation: Option<&TransferCancellation>,
) -> Result<FileInfo, TrellisClientError> {
    parse_transfer_grant(&serde_json::to_vec(grant)?).map_err(fail)?;
    let coordinates = Coordinates {
        id: &grant.transfer_id,
        expires: &grant.expires_at,
        provider: &grant.provider,
        consumer: &grant.consumer,
        data: &grant.data_subject,
        control: &grant.control_subject,
        signal: &grant.signal_subject,
        direction: TransferDirection::Send,
        max_frame: grant.max_frame_bytes,
        frames: grant.window_frames,
        bytes: grant.window_bytes,
    };
    validate_coordinates(
        client,
        &coordinates,
        &grant.format,
        &grant.type_name,
        &grant.direction,
    )?;
    let mut active = activate(client, &coordinates, cancellation).await?;
    let window = SenderWindow::new(FlowLimits {
        max_frame_bytes: active.max_frame,
        window_frames: grant.window_frames,
        window_bytes: grant.window_bytes,
    });
    let mut changes = active.peer.guard.subscribe_changes();
    let mut local_changes = active.local.guard.subscribe_changes();
    let expiry =
        tokio::time::sleep(transfer::transfer_expiry_delay(&grant.expires_at).map_err(fail)?);
    tokio::pin!(expiry);
    let mut source_done = false;
    let mut complete_sent = false;
    let mut cancelled_sent = false;
    let mut transferred = 0u64;
    let mut consumed_bytes = 0;
    let mut hasher = Sha256::new();
    let observation = transfer::telemetry::observation(TransferDirection::Send, "client");
    let mut staged: Option<transfer::telemetry::Payload> = None;
    let mut buffer = vec![0u8; active.max_frame as usize];
    let outcome = async {
        loop {
            if let Some(bytes) = &staged {
                if window.validate_frame_slot(bytes.len() as u64, active.max_frame).is_ok() {
                    active.local.guard.reconcile().await.map_err(|e| fail(format!("upload local authority: {e:?}")))?;
                    active.peer.guard.check_now().map_err(|e| fail(format!("upload peer authority: {e:?}")))?;
                    let seq = window.next_frame_seq().map_err(|e| fail(format!("upload sequence: {e:?}")))?;
                    let bytes = staged.take().ok_or_else(|| fail("upload staged frame missing"))?;
                    let next = transferred.checked_add(bytes.len() as u64).ok_or_else(|| fail("upload size overflow"))?;
                    if grant.max_bytes.is_some_and(|max| next > max) { return Err(fail("upload exceeds object limit")); }
                    let descriptor = TransferFrameDescriptor { transfer_id: grant.transfer_id.clone(), direction: TransferDirection::Send, sequence: U64s::new(seq), kind: TransferFrameKind::Data, terminal: None };
                    let headers = wire::caller_headers(client, &grant.data_subject, &grant.signal_subject, &descriptor, &bytes).map_err(fail)?;
                    hasher.update(&bytes);
                    let len = bytes.len() as u64;
                    active.lease.nats().publish_with_reply_and_headers(grant.data_subject.clone(), grant.signal_subject.clone(), headers, bytes.bytes.clone()).await.map_err(fail)?;
                    transfer::telemetry::frame(&descriptor, &bytes, "client-tx");
                    window.commit_frame(seq, len).map_err(|e| fail(format!("upload sender window: {e:?}")))?;
                    transferred = next;
                    continue;
                }
            }
            if source_done && !complete_sent && staged.is_none() && !cancelled_sent {
                let body = TransferComplete { format: TransferFormat::V2, kind: TransferCompleteKind::Complete, transfer_id: grant.transfer_id.clone(), final_seq: U64s::new(window.highest_sent.load(Ordering::Acquire)), size: transferred, digest: format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.clone().finalize())) };
                let payload = Bytes::from(serde_json::to_vec(&body)?);
                let descriptor = TransferFrameDescriptor { transfer_id: grant.transfer_id.clone(), direction: TransferDirection::Send, sequence: body.final_seq, kind: TransferFrameKind::Complete, terminal: None };
                let headers = wire::caller_headers(client, &grant.data_subject, &grant.signal_subject, &descriptor, &payload).map_err(fail)?;
                active.lease.nats().publish_with_reply_and_headers(grant.data_subject.clone(), grant.signal_subject.clone(), headers, payload).await.map_err(fail)?;
                complete_sent = true;
            }
            tokio::select! {
                _ = &mut expiry => return Err(fail("upload grant expired")),
                _ = active.lease.wait_lost() => return Err(fail("upload pinned transport lost")),
                failure = wire::authority_failure(&active.local, &active.peer) => return Err(fail(failure)),
                _ = changes.recv() => { active.peer.guard.check_now().map_err(|e| fail(format!("upload authority lost: {e:?}")))?; },
                _ = local_changes.recv() => { active.local.guard.reconcile().await.map_err(|e| fail(format!("upload local authority lost: {e:?}")))?; },
                _ = cancelled(cancellation), if !cancelled_sent => {
                    publish_control(client, &active, &coordinates, TransferControl::Cancel { format: TransferFormat::V2, kind: TransferControlKind::Control, transfer_id: grant.transfer_id.clone(), control_seq: U64s::new(2), received_seq: U64s::new(window.highest_received.load(Ordering::Acquire)), consumed_seq: U64s::new(window.highest_consumed.load(Ordering::Acquire)), consumed_bytes: U64s::new(consumed_bytes) }).await?;
                    cancelled_sent = true;
                    staged.take();
                },
                count = reader.read(&mut buffer), if !source_done && staged.is_none() && !cancelled_sent => {
                    let count = count?;
                    if count == 0 { source_done = true; }
                    else { let mut frame = std::mem::replace(&mut buffer, vec![0u8; active.max_frame as usize]); frame.truncate(count); staged = Some(transfer::telemetry::Payload::new(Bytes::from(frame.into_boxed_slice()), TransferDirection::Send, "client-tx")); }
                },
                message = active.signals.next() => {
                    let message = message.ok_or_else(|| fail("upload signal subscription closed"))?;
                    match verified_signal(&active.peer, message, &coordinates).await? {
                        Some(TransferSignal::Credit { received_seq, consumed_seq, consumed_bytes: bytes, .. }) => {
                            if bytes.get() < consumed_bytes || bytes.get() > transferred { return Err(fail("invalid upload consumed byte cursor")); }
                            window.apply_credit(received_seq.get(), consumed_seq.get(), Some(bytes.get())).map_err(|e| fail(format!("upload credit: {e:?}")))?;
                            consumed_bytes = bytes.get();
                        },
                        Some(TransferSignal::Committed { final_seq, info, .. }) => {
                            if !complete_sent || final_seq.get() != window.highest_sent.load(Ordering::Acquire) || info.size != transferred || !transfer::transfer_digests_match(&info.digest, &format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.clone().finalize()))) { return Err(fail("upload committed metadata mismatch")); }
                            return Ok(info);
                        },
                        Some(TransferSignal::Cancelled { .. }) if cancelled_sent => return Err(TrellisClientError::TransferCancelled),
                        Some(TransferSignal::Error { code, .. }) => return Err(fail(format!("upload failed: {code:?}"))),
                        _ => {},
                    }
                },
            }
        }
    }.await;
    if outcome.is_err() && !cancelled_sent {
        let _ = publish_control(
            client,
            &active,
            &coordinates,
            TransferControl::Cancel {
                format: TransferFormat::V2,
                kind: TransferControlKind::Control,
                transfer_id: grant.transfer_id.clone(),
                control_seq: U64s::new(2),
                received_seq: U64s::new(window.highest_received.load(Ordering::Acquire)),
                consumed_seq: U64s::new(window.highest_consumed.load(Ordering::Acquire)),
                consumed_bytes: U64s::new(consumed_bytes),
            },
        )
        .await;
    }
    observation.finish(if outcome.is_ok() { "ok" } else { "error" });
    outcome
}

pub(crate) async fn get_download_grant(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
) -> Result<Vec<u8>, TrellisClientError> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    get_download_grant_into(client, grant, &mut bytes).await?;
    Ok(bytes.into_inner())
}
pub(crate) async fn get_download_grant_into<W: AsyncWrite + Unpin + Send + ?Sized>(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
    writer: &mut W,
) -> Result<FileInfo, TrellisClientError> {
    get_download_grant_into_with_cancel(client, grant, writer, None).await
}

pub(crate) async fn get_download_grant_into_with_cancel<W: AsyncWrite + Unpin + Send + ?Sized>(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
    writer: &mut W,
    cancellation: Option<&TransferCancellation>,
) -> Result<FileInfo, TrellisClientError> {
    parse_transfer_grant(&serde_json::to_vec(grant)?).map_err(fail)?;
    let coordinates = Coordinates {
        id: &grant.transfer_id,
        expires: &grant.expires_at,
        provider: &grant.provider,
        consumer: &grant.consumer,
        data: &grant.data_subject,
        control: &grant.control_subject,
        signal: &grant.signal_subject,
        direction: TransferDirection::Receive,
        max_frame: grant.max_frame_bytes,
        frames: grant.window_frames,
        bytes: grant.window_bytes,
    };
    validate_coordinates(
        client,
        &coordinates,
        &grant.format,
        &grant.type_name,
        &grant.direction,
    )?;
    validate_transfer_digest(&grant.info.digest).map_err(fail)?;
    let mut active = activate(client, &coordinates, cancellation).await?;
    let mut data = active
        .data
        .take()
        .ok_or_else(|| fail("download data subscription missing"))?;
    let received = SenderWindow::new(FlowLimits {
        max_frame_bytes: active.max_frame,
        window_frames: grant.window_frames,
        window_bytes: grant.window_bytes,
    });
    let observation = transfer::telemetry::observation(TransferDirection::Receive, "client");
    let mut queue = VecDeque::<(u64, transfer::telemetry::Payload)>::new();
    let mut transferred = 0u64;
    let mut consumed = 0u64;
    let mut admitted_bytes = 0u64;
    let mut hasher = Sha256::new();
    let mut eof: Option<TransferTerminal> = None;
    let mut control_seq = 1u64;
    let mut scheduler: CreditScheduler = transfer::credit_scheduler();
    let mut reported = 0u64;
    let mut reported_bytes = 0u64;
    let mut changes = active.peer.guard.subscribe_changes();
    let mut local_changes = active.local.guard.subscribe_changes();
    let expiry =
        tokio::time::sleep(transfer::transfer_expiry_delay(&grant.expires_at).map_err(fail)?);
    tokio::pin!(expiry);
    let outcome = async {
        loop {
            if let Some((seq, bytes)) = queue.pop_front() {
                let write = writer.write_all(&bytes);
                tokio::pin!(write);
                loop {
                    tokio::select! {
                        result = &mut write => { result?; break; },
                        _ = cancelled(cancellation) => return Err(TrellisClientError::TransferCancelled),
                        _ = active.lease.wait_lost() => return Err(fail("download pinned transport lost")),
                        failure = wire::authority_failure(&active.local, &active.peer) => return Err(fail(failure)),
                        _ = &mut expiry => return Err(fail("download grant expired")),
                        _ = changes.recv() => { active.peer.guard.check_now().map_err(|e| fail(format!("download authority lost: {e:?}")))?; },
                        _ = local_changes.recv() => { active.local.guard.reconcile().await.map_err(|e| fail(format!("download local authority lost: {e:?}")))?; },
                        _ = transfer::credit_due(&scheduler) => {
                            control_seq = control_seq.checked_add(1).ok_or_else(|| fail("control sequence overflow"))?;
                            publish_control(client, &active, &coordinates, TransferControl::Credit { format: TransferFormat::V2, kind: TransferControlKind::Control, transfer_id: grant.transfer_id.clone(), control_seq: U64s::new(control_seq), received_seq: U64s::new(received.highest_sent.load(Ordering::Acquire)), consumed_seq: U64s::new(consumed), consumed_bytes: U64s::new(transferred) }).await?;
                            reported = consumed; reported_bytes = transferred; scheduler.clear();
                        },
                        message = active.signals.next() => {
                            match verified_signal(&active.peer, message.ok_or_else(|| fail("download signal subscription closed"))?, &coordinates).await? {
                                Some(TransferSignal::Error { code, .. }) => return Err(fail(format!("download failed: {code:?}"))),
                                Some(TransferSignal::Cancelled { .. }) => return Err(TrellisClientError::TransferCancelled),
                                _ => {},
                            }
                        },
                        message = data.next() => {
                            let message = message.ok_or_else(|| fail("download data subscription closed"))?;
                            admit_download(&active.peer, message, &coordinates, &received, &mut queue, &mut eof, &mut admitted_bytes, grant.info.size).await?;
                        },
                    }
                }
                hasher.update(&bytes);
                transferred = transferred.checked_add(bytes.len() as u64).ok_or_else(|| fail("download size overflow"))?;
                consumed = seq;
                received.apply_credit(received.highest_sent.load(Ordering::Acquire), consumed, Some(transferred)).map_err(|e| fail(format!("download consumer window: {e:?}")))?;
                scheduler.note_pending(tokio::time::Instant::now(), consumed - reported, transferred - reported_bytes);
            }
            if queue.is_empty() {
                if let Some(terminal) = &eof {
                    if terminal.final_seq.get() != consumed || terminal.size != transferred || transferred != grant.info.size || !transfer::transfer_digests_match(&terminal.digest, &grant.info.digest) || !transfer::transfer_digests_match(&terminal.digest, &format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.clone().finalize()))) { return Err(fail("download EOF integrity mismatch")); }
                    control_seq = control_seq.checked_add(1).ok_or_else(|| fail("control sequence overflow"))?;
                    publish_control(client, &active, &coordinates, TransferControl::EndAck { format: TransferFormat::V2, kind: TransferControlKind::Control, transfer_id: grant.transfer_id.clone(), control_seq: U64s::new(control_seq), received_seq: terminal.final_seq, consumed_seq: terminal.final_seq, consumed_bytes: U64s::new(transferred), final_seq: terminal.final_seq }).await?;
                    active.lease.nats().flush().await.map_err(fail)?;
                    return Ok(grant.info.clone());
                }
            }
            if scheduler.is_due(tokio::time::Instant::now()) {
                control_seq = control_seq.checked_add(1).ok_or_else(|| fail("control sequence overflow"))?;
                publish_control(client, &active, &coordinates, TransferControl::Credit { format: TransferFormat::V2, kind: TransferControlKind::Control, transfer_id: grant.transfer_id.clone(), control_seq: U64s::new(control_seq), received_seq: U64s::new(received.highest_sent.load(Ordering::Acquire)), consumed_seq: U64s::new(consumed), consumed_bytes: U64s::new(transferred) }).await?;
                reported = consumed; reported_bytes = transferred; scheduler.clear();
            }
            if !queue.is_empty() { continue; }
            tokio::select! {
                _ = cancelled(cancellation) => return Err(TrellisClientError::TransferCancelled),
                _ = active.lease.wait_lost() => return Err(fail("download pinned transport lost")),
                failure = wire::authority_failure(&active.local, &active.peer) => return Err(fail(failure)),
                _ = &mut expiry => return Err(fail("download grant expired")),
                _ = changes.recv() => { active.peer.guard.check_now().map_err(|e| fail(format!("download authority lost: {e:?}")))?; },
                _ = local_changes.recv() => { active.local.guard.reconcile().await.map_err(|e| fail(format!("download local authority lost: {e:?}")))?; },
                _ = transfer::credit_due(&scheduler) => {},
                message = data.next() => {
                    admit_download(&active.peer, message.ok_or_else(|| fail("download data subscription closed"))?, &coordinates, &received, &mut queue, &mut eof, &mut admitted_bytes, grant.info.size).await?;
                },
                message = active.signals.next() => {
                    match verified_signal(&active.peer, message.ok_or_else(|| fail("download signal subscription closed"))?, &coordinates).await? {
                        Some(TransferSignal::Error { code, .. }) => return Err(fail(format!("download failed: {code:?}"))),
                        Some(TransferSignal::Cancelled { .. }) => return Err(TrellisClientError::TransferCancelled),
                        _ => {},
                    }
                },
            }
        }
    }.await;
    if outcome.is_err() {
        control_seq = control_seq
            .checked_add(1)
            .ok_or_else(|| fail("control sequence overflow"))?;
        let _ = publish_control(
            client,
            &active,
            &coordinates,
            TransferControl::Cancel {
                format: TransferFormat::V2,
                kind: TransferControlKind::Control,
                transfer_id: grant.transfer_id.clone(),
                control_seq: U64s::new(control_seq),
                received_seq: U64s::new(received.highest_sent.load(Ordering::Acquire)),
                consumed_seq: U64s::new(consumed),
                consumed_bytes: U64s::new(transferred),
            },
        )
        .await;
    }
    observation.finish(if outcome.is_ok() { "ok" } else { "error" });
    outcome
}

#[expect(
    clippy::too_many_arguments,
    reason = "admission borrows the existing receive-loop state without introducing another owner"
)]
async fn admit_download(
    peer: &TransferPeer,
    message: async_nats::Message,
    coordinates: &Coordinates<'_>,
    window: &SenderWindow,
    queue: &mut VecDeque<(u64, transfer::telemetry::Payload)>,
    eof: &mut Option<TransferTerminal>,
    admitted_bytes: &mut u64,
    expected_size: u64,
) -> Result<(), TrellisClientError> {
    if message.subject.as_str() != coordinates.data {
        return Ok(());
    }
    let descriptor = match wire::descriptor(&message, coordinates.id, coordinates.direction) {
        Ok(value) => value,
        Err(_) => return Ok(()),
    };
    if peer.verify_provider(&message, &descriptor).await.is_err() {
        return Ok(());
    }
    if eof.is_some() {
        return Err(fail("download DATA after EOF"));
    }
    match descriptor.kind {
        TransferFrameKind::Data => {
            window
                .validate_frame_slot(message.payload.len() as u64, coordinates.max_frame)
                .map_err(|e| fail(format!("download window violation: {e:?}")))?;
            if descriptor.sequence.get()
                != window
                    .next_frame_seq()
                    .map_err(|e| fail(format!("download sequence: {e:?}")))?
            {
                return Err(fail("verified download delivery gap"));
            }
            *admitted_bytes = admitted_bytes
                .checked_add(message.payload.len() as u64)
                .ok_or_else(|| fail("download size overflow"))?;
            if *admitted_bytes > expected_size {
                return Err(fail("download exceeds declared size"));
            }
            window
                .commit_frame(descriptor.sequence.get(), message.payload.len() as u64)
                .map_err(|e| fail(format!("download window: {e:?}")))?;
            queue.push_back((
                descriptor.sequence.get(),
                transfer::telemetry::Payload::new(
                    message.payload,
                    TransferDirection::Receive,
                    "client-rx",
                ),
            ));
        }
        TransferFrameKind::Eof => {
            let terminal = descriptor
                .terminal
                .ok_or_else(|| fail("download EOF missing terminal"))?;
            if terminal.final_seq.get() != window.highest_sent.load(Ordering::Acquire) {
                return Err(fail("verified download EOF delivery gap"));
            }
            *eof = Some(terminal);
        }
        _ => return Err(fail("invalid download frame kind")),
    }
    Ok(())
}

/// Decode a generated SDK receive grant; runtime validates identities before use.
pub fn download_transfer_grant_from_value(
    value: serde_json::Value,
) -> Result<DownloadTransferGrant, TrellisClientError> {
    if !matches!(
        parse_transfer_grant(&serde_json::to_vec(&value)?).map_err(fail)?,
        TransferGrant::Receive(_)
    ) {
        return Err(fail("expected receive transfer grant"));
    }
    Ok(serde_json::from_value(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn cancellation_cannot_lose_registration_wakeups() {
        let cancellation = TransferCancellation::new();
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), cancellation.cancelled())
            .await
            .expect("cancellation before registration remains observable");

        let cancellation = TransferCancellation::new();
        let mut waiter = Box::pin(cancellation.cancelled());
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("registered cancel waiter wakes");
    }
}
