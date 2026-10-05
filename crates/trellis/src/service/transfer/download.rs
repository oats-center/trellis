use super::{
    wire::{self, error, publish_signal, TransferPeer},
    DownloadTransferGrantPlan, ServerError, StoreResourceClient,
};
use crate::{
    client::{TransportLease, TrellisClient},
    data_plane::flow::{FlowLimits, SenderWindow},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest as _, Sha256};
use std::sync::{atomic::Ordering, Arc};
use tokio::io::AsyncReadExt;
use trellis_protocol::transfer::*;

/// Stream a storage-neutral push download on its exact pinned attachment.
pub async fn run_download_transfer_endpoint<C>(
    client: Arc<TrellisClient>,
    lease: TransportLease,
    mut control: async_nats::Subscriber,
    plan: DownloadTransferGrantPlan,
    store: C,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
{
    let grant = &plan.grant;
    let observation = super::telemetry::observation(TransferDirection::Receive, "provider");
    let permission = plan
        .admission
        .required_permission
        .as_ref()
        .ok_or_else(|| error("download admission permission missing"))?
        .permission_atom()
        .map_err(error)?;
    let peer = TransferPeer::retain(
        client.authorization_provider(),
        plan.admission
            .authorization_context
            .as_deref()
            .ok_or_else(|| error("download admission context missing"))?,
        &grant.consumer,
        Some(permission),
    )
    .await?;
    let local = TransferPeer::retain(
        client.authorization_provider(),
        &client.authorization_context_digest().map_err(error)?,
        &grant.provider,
        None,
    )
    .await?;
    let mut changes = peer.guard.subscribe_changes();
    let mut local_changes = local.guard.subscribe_changes();
    let expiry = tokio::time::sleep(super::transfer_expiry_delay(&grant.expires_at)?);
    tokio::pin!(expiry);
    let mut store = Some(store);
    let mut backend: Option<
        futures_util::future::BoxFuture<
            'static,
            Result<Option<crate::service::StoreObjectInfo>, ServerError>,
        >,
    > = None;
    let mut backend_result = None;
    let mut reader: Option<super::telemetry::Pipe> = None;
    let mut activated = false;
    let mut max_frame_bytes = grant.max_frame_bytes;
    let mut window = SenderWindow::new(FlowLimits {
        max_frame_bytes,
        window_frames: grant.window_frames,
        window_bytes: grant.window_bytes,
    });
    let mut buffer = vec![0; max_frame_bytes as usize];
    let mut staged: Option<super::telemetry::Payload> = None;
    let mut transferred = 0u64;
    let mut hasher = Sha256::new();
    let mut eof = false;
    let mut control_seq = 0;
    let mut control_body = None;
    let mut activate_body = None;
    let result: Result<(), ServerError> = async {
        loop {
            if let Some(bytes) = staged.as_ref() {
                if window.validate_frame_slot(bytes.len() as u64, max_frame_bytes).is_ok() {
                    peer.guard.check_now().map_err(|e| error(format!("download authority: {e:?}")))?;
                    let seq = window.next_frame_seq().map_err(|e| error(format!("download sequence: {e:?}")))?;
                    let bytes = staged.take().ok_or_else(|| error("staged download frame missing"))?;
                    let len = bytes.len() as u64;
                    hasher.update(&bytes);
                    let next = transferred.checked_add(len).ok_or_else(|| error("download size overflow"))?;
                    if next > grant.info.size || plan.max_object_bytes.is_some_and(|max| next > max) { return Err(error("download exceeds declared object size")); }
                    wire::publish_provider(lease.nats(), client.auth(), &local.guard, &grant.data_subject, &TransferFrameDescriptor { transfer_id: grant.transfer_id.clone(), direction: TransferDirection::Receive, sequence: U64s::new(seq), kind: TransferFrameKind::Data, terminal: None }, bytes.into_bytes()).await?;
                    // Single event owner: credit cannot be processed between handoff and commit.
                    window.commit_frame(seq, len).map_err(|e| error(format!("download sender window: {e:?}")))?;
                    transferred = next;
                    continue;
                }
            }
            tokio::select! {
                _ = &mut expiry => return Err(error("download grant expired")),
                _ = lease.wait_lost() => return Err(error("download pinned transport lost")),
                failure = wire::authority_failure(&local, &peer) => return Err(failure),
                _ = changes.recv() => { peer.guard.check_now().map_err(|e| error(format!("download authority: {e:?}")))?; },
                _ = local_changes.recv() => { local.guard.reconcile().await.map_err(|e| error(format!("download local authority: {e:?}")))?; },
                message = control.next() => {
                    let message = message.ok_or_else(|| error("download control subscription closed"))?;
                    let descriptor = match wire::descriptor(&message, &grant.transfer_id, TransferDirection::Receive) { Ok(value) => value, Err(_) => continue };
                    if descriptor.kind != TransferFrameKind::Control || peer.verify_caller(&message, &descriptor, &grant.provider.connection_id, &grant.signal_subject).await.is_err() { continue; }
                    let body = parse_transfer_control(&message.payload).map_err(error)?;
                    let seq = body.control_seq().get();
                    let repeated_activate = activated && seq == 1 && matches!(body, TransferControl::Activate { .. });
                    if repeated_activate {
                        if activate_body.as_ref() != Some(&message.payload) { return Err(error("download activation conflict")); }
                    } else if seq == control_seq {
                        if control_body.as_ref() != Some(&message.payload) { return Err(error("download control conflict")); }
                        if !matches!(body, TransferControl::Activate { .. }) { continue; }
                    } else if seq != control_seq + 1 { return Err(error("download control gap")); }
                    if !repeated_activate { control_seq = seq; control_body = Some(message.payload.clone()); }
                    match body {
                        TransferControl::Activate { receive_max_frame_bytes, .. } => {
                             if !activated {
                                 activate_body = Some(message.payload.clone());
                                max_frame_bytes = max_frame_bytes.min(receive_max_frame_bytes);
                                buffer.resize(max_frame_bytes as usize, 0);
                                window = SenderWindow::new(FlowLimits { max_frame_bytes, window_frames: grant.window_frames, window_bytes: grant.window_bytes });
                            }
                            publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Receive, TransferSignal::Activated { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone(), control_seq: U64s::new(1), request_id: wire::header(message.headers.as_ref().ok_or_else(|| error("headers missing"))?, "request-id")?, max_frame_bytes, window_frames: grant.window_frames, window_bytes: grant.window_bytes }).await?;
                            if !activated {
                                let (mut writer, pipe) = super::telemetry::pipe(max_frame_bytes as usize, TransferDirection::Receive, "provider-tx");
                                let store = store.take().ok_or_else(|| error("download backend already started"))?;
                                let key = grant.info.key.clone();
                                reader = Some(pipe);
                                backend = Some(Box::pin(async move { store.read_into(&key, &mut writer).await }));
                                activated = true;
                            }
                        },
                        TransferControl::Credit { received_seq, consumed_seq, consumed_bytes, .. } => {
                            if !activated || consumed_bytes.get() > transferred { return Err(error("invalid download credit bytes")); }
                            window.apply_credit(received_seq.get(), consumed_seq.get(), Some(consumed_bytes.get())).map_err(|e| error(format!("download credit: {e:?}")))?;
                        },
                        TransferControl::Cancel { .. } => {
                             backend.take();
                            publish_signal(lease.nats(), &client, &local.guard, &grant.signal_subject, TransferDirection::Receive, TransferSignal::Cancelled { format: TransferFormat::V2, transfer_id: grant.transfer_id.clone() }).await?;
                            return Ok(());
                        },
                        TransferControl::EndAck { final_seq, received_seq, consumed_seq, consumed_bytes, .. } => {
                            if !eof || consumed_bytes.get() != transferred { return Err(error("invalid download EOF acknowledgement")); }
                            window.apply_credit(received_seq.get(), consumed_seq.get(), Some(consumed_bytes.get())).map_err(|e| error(format!("download final credit: {e:?}")))?;
                            window.validate_complete(final_seq.get()).map_err(|e| error(format!("download final sequence: {e:?}")))?;
                            return Ok(());
                        },
                    }
                },
                result = async { backend.as_mut().ok_or_else(|| error("download backend missing"))?.await }, if backend.is_some() => {
                    backend.take();
                    backend_result = Some(result?.ok_or_else(|| error("download object missing"))?);
                },
                count = async { reader.as_mut().ok_or_else(|| error("download reader missing"))?.read(&mut buffer).await.map_err(error) }, if activated && !eof && staged.is_none() => {
                    let count = count?;
                    if count > 0 { let mut frame = std::mem::replace(&mut buffer, vec![0; max_frame_bytes as usize]); frame.truncate(count); staged = Some(super::telemetry::Payload::new(Bytes::from(frame.into_boxed_slice()), TransferDirection::Receive, "provider-tx")); continue; }
                    let info = match backend_result.take() {
                        Some(info) => info,
                        None => backend.as_mut().ok_or_else(|| error("download backend missing"))?.await?.ok_or_else(|| error("download object missing"))?,
                    };
                    backend.take();
                    let digest = format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.clone().finalize()));
                    if info.key != grant.info.key || info.size != transferred || transferred != grant.info.size || !super::transfer_digests_match(&grant.info.digest, &digest) || !info.digest.as_deref().is_some_and(|value| super::transfer_digests_match(value, &digest)) { return Err(error("download backend metadata/digest mismatch")); }
                    let final_seq = U64s::new(window.highest_sent.load(Ordering::Acquire));
                    wire::publish_provider(lease.nats(), client.auth(), &local.guard, &grant.data_subject, &TransferFrameDescriptor { transfer_id: grant.transfer_id.clone(), direction: TransferDirection::Receive, sequence: final_seq, kind: TransferFrameKind::Eof, terminal: Some(TransferTerminal { final_seq, size: transferred, digest: grant.info.digest.clone() }) }, Bytes::new()).await?;
                    eof = true;
                },
            }
        }
    }.await;
    backend.take();
    if result.is_err() {
        if let Err(error) = &result {
            tracing::warn!(%error, "transfer download endpoint failed");
        }
        let _ = publish_signal(
            lease.nats(),
            &client,
            &local.guard,
            &grant.signal_subject,
            TransferDirection::Receive,
            TransferSignal::Error {
                format: TransferFormat::V2,
                transfer_id: grant.transfer_id.clone(),
                code: TransferErrorCode::StorageFailed,
            },
        )
        .await;
    }
    observation.finish(if result.is_ok() { "ok" } else { "error" });
    result
}
