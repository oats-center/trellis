use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt;

use super::super::{RequestValidator, ServerError, StoreResourceClient};
use super::{
    abort_store_task, decode_upload_transfer_chunk, publish_download_chunk, publish_error_reply,
    transfer_digests_match, transfer_expiry_delay, validate_chunk_bytes,
    validate_download_transfer_message, DownloadTransferGrantPlan, UploadTransferControl,
};

/// Serve one pull-based download grant until completion, cancellation, or expiry.
///
/// The backend reader is opened only after an authenticated pull. At most one
/// frame is in flight, and final size plus both declared and backend SHA-256
/// digests are verified before the empty completion response is published.
pub async fn run_download_transfer_endpoint<C, V>(
    client: async_nats::Client,
    subscriber: impl futures_util::Stream<Item = async_nats::Message>,
    plan: DownloadTransferGrantPlan,
    store: C,
    validator: V,
) -> Result<(), ServerError>
where
    C: StoreResourceClient,
    V: RequestValidator + 'static,
{
    let mut subscriber = Box::pin(subscriber);
    let capacity = usize::try_from(plan.grant.chunk_bytes).map_err(|_| {
        ServerError::InvalidTransferChunkSize {
            chunk_bytes: plan.grant.chunk_bytes,
        }
    })?;
    validate_chunk_bytes(plan.grant.chunk_bytes)?;
    let mut store = Some(store);
    let mut pipe_reader = None;
    let mut store_task = None;
    let mut seq = 0_u64;
    let mut transferred = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; capacity];
    let expiry = tokio::time::sleep(transfer_expiry_delay(&plan.grant.expires_at)?);
    tokio::pin!(expiry);

    loop {
        let message = tokio::select! {
            _ = &mut expiry => {
                abort_store_task(&mut store_task).await;
                return Ok(());
            }
            message = subscriber.next() => {
                let Some(message) = message else {
                    abort_store_task(&mut store_task).await;
                    return Ok(());
                };
                message
            }
        };
        let Some(reply_to) = message.reply.as_ref().map(ToString::to_string) else {
            continue;
        };
        if let Err(error) = validate_download_transfer_message(&plan, &validator, &message).await {
            publish_error_reply(&client, reply_to, &error).await?;
            if matches!(error, ServerError::TransferExpired { .. }) {
                abort_store_task(&mut store_task).await;
                return Ok(());
            }
            continue;
        }
        let frame =
            decode_upload_transfer_chunk(message.headers.as_ref(), message.payload.clone())?;
        if frame.cancel {
            if !matches!(
                serde_json::from_slice(&message.payload),
                Ok(UploadTransferControl::Cancel)
            ) {
                publish_error_reply(
                    &client,
                    reply_to,
                    &ServerError::Nats("invalid download cancellation control".to_string()),
                )
                .await?;
                continue;
            }
            abort_store_task(&mut store_task).await;
            client
                .publish(reply_to, Bytes::from_static(b"{\"status\":\"cancelled\"}"))
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
            return Ok(());
        }
        if frame.seq != seq {
            publish_error_reply(
                &client,
                reply_to,
                &ServerError::TransferSequenceOutOfOrder {
                    transfer_id: plan.grant.transfer_id.clone(),
                    expected_seq: seq,
                    actual_seq: frame.seq,
                },
            )
            .await?;
            continue;
        }
        if frame.eof || !message.payload.is_empty() {
            publish_error_reply(
                &client,
                reply_to,
                &ServerError::Nats("invalid download transfer control".to_string()),
            )
            .await?;
            continue;
        }

        if pipe_reader.is_none() {
            let (mut pipe_writer, reader) = tokio::io::duplex(capacity);
            let key = plan.grant.info.key.clone();
            let store = store.take().ok_or_else(|| {
                ServerError::Nats("download transfer store already started".to_string())
            })?;
            pipe_reader = Some(reader);
            store_task = Some(tokio::spawn(async move {
                store.read_into(&key, &mut pipe_writer).await
            }));
        }

        let count = match loop {
            tokio::select! {
                _ = &mut expiry => {
                    abort_store_task(&mut store_task).await;
                    return Ok(());
                }
                result = pipe_reader.as_mut().expect("download pipe initialized").read(&mut buffer) => break result,
                control = subscriber.next() => {
                    let Some(control) = control else {
                        abort_store_task(&mut store_task).await;
                        return Ok(());
                    };
                    let Some(control_reply) = control.reply.as_ref().map(ToString::to_string) else {
                        continue;
                    };
                    if let Err(error) = validate_download_transfer_message(&plan, &validator, &control).await {
                        publish_error_reply(&client, control_reply, &error).await?;
                        continue;
                    }
                    let frame = decode_upload_transfer_chunk(
                        control.headers.as_ref(),
                        control.payload.clone(),
                    )?;
                    if frame.cancel && matches!(
                        serde_json::from_slice(&control.payload),
                        Ok(UploadTransferControl::Cancel)
                    ) {
                        abort_store_task(&mut store_task).await;
                        client
                            .publish(
                                control_reply,
                                Bytes::from_static(b"{\"status\":\"cancelled\"}"),
                            )
                            .await
                            .map_err(|error| ServerError::Nats(error.to_string()))?;
                        return Ok(());
                    }
                    publish_error_reply(
                        &client,
                        control_reply,
                        &ServerError::Nats("download transfer already has a pending pull".to_string()),
                    )
                    .await?;
                }
            }
        } {
            Ok(count) => count,
            Err(error) => {
                abort_store_task(&mut store_task).await;
                publish_error_reply(&client, reply_to, &ServerError::Nats(error.to_string()))
                    .await?;
                return Ok(());
            }
        };
        if count == 0 {
            let store_info = match store_task
                .take()
                .ok_or_else(|| ServerError::Nats("download transfer task missing".to_string()))?
                .await
            {
                Ok(Ok(Some(info))) => info,
                Ok(Ok(None)) => {
                    publish_error_reply(
                        &client,
                        reply_to,
                        &ServerError::TransferObjectMissing {
                            store: plan.store_alias.clone(),
                            key: plan.grant.info.key.clone(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
                Ok(Err(error)) => {
                    publish_error_reply(&client, reply_to, &error).await?;
                    return Ok(());
                }
                Err(error) => {
                    publish_error_reply(&client, reply_to, &ServerError::Nats(error.to_string()))
                        .await?;
                    return Ok(());
                }
            };
            if store_info.key != plan.grant.info.key {
                publish_error_reply(
                    &client,
                    reply_to,
                    &ServerError::Nats(format!(
                        "download stored key mismatch: expected {}, got {}",
                        plan.grant.info.key, store_info.key
                    )),
                )
                .await?;
                return Ok(());
            }
            if transferred != plan.grant.info.size || store_info.size != transferred {
                publish_error_reply(
                    &client,
                    reply_to,
                    &ServerError::TransferObjectSizeMismatch {
                        store: plan.store_alias.clone(),
                        key: plan.grant.info.key.clone(),
                        expected_size: plan.grant.info.size,
                        actual_size: transferred,
                    },
                )
                .await?;
                return Ok(());
            }
            let digest = format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.finalize()));
            if !transfer_digests_match(&plan.grant.info.digest, &digest) {
                publish_error_reply(
                    &client,
                    reply_to,
                    &ServerError::TransferDigestMismatch {
                        transfer_id: plan.grant.transfer_id.clone(),
                        expected_digest: plan.grant.info.digest.clone(),
                        actual_digest: digest,
                    },
                )
                .await?;
                return Ok(());
            }
            let Some(expected) = store_info.digest.as_ref() else {
                publish_error_reply(
                    &client,
                    reply_to,
                    &ServerError::TransferDigestMismatch {
                        transfer_id: plan.grant.transfer_id.clone(),
                        expected_digest: plan.grant.info.digest.clone(),
                        actual_digest: "missing backend digest".to_string(),
                    },
                )
                .await?;
                return Ok(());
            };
            if !transfer_digests_match(expected, &digest) {
                publish_error_reply(
                    &client,
                    reply_to,
                    &ServerError::TransferDigestMismatch {
                        transfer_id: plan.grant.transfer_id.clone(),
                        expected_digest: expected.clone(),
                        actual_digest: digest,
                    },
                )
                .await?;
                return Ok(());
            }
            publish_download_chunk(&client, &reply_to, seq, Bytes::new(), true).await?;
            return Ok(());
        }

        let next = transferred
            .checked_add(count as u64)
            .ok_or_else(|| ServerError::Nats("download transfer size overflow".to_string()))?;
        if next > plan.grant.info.size || plan.max_object_bytes.is_some_and(|max| next > max) {
            abort_store_task(&mut store_task).await;
            publish_error_reply(
                &client,
                reply_to,
                &ServerError::TransferObjectTooLarge {
                    service_name: plan.grant.service.clone(),
                    store: plan.store_alias.clone(),
                    key: plan.grant.info.key.clone(),
                    size: next,
                    max_bytes: plan.max_object_bytes.unwrap_or(plan.grant.info.size),
                },
            )
            .await?;
            return Ok(());
        }
        publish_download_chunk(
            &client,
            &reply_to,
            seq,
            Bytes::copy_from_slice(&buffer[..count]),
            false,
        )
        .await?;
        hasher.update(&buffer[..count]);
        transferred = next;
        seq = seq
            .checked_add(1)
            .ok_or_else(|| ServerError::Nats("download transfer sequence overflow".to_string()))?;
    }
}
