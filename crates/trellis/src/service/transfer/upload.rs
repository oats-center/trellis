use std::sync::atomic::Ordering;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use sha2::{Digest as _, Sha256};
use tokio::sync::{mpsc, watch};

use super::super::{OperationTransferProgress, ServerError, StoreObjectInfo, StoreResourceClient};
use super::{
    ingress::{Consumed, FrameReader, Ingress},
    transfer_digests_match, FileTransferInfo, UploadTransferGrantPlan,
};
use crate::data_plane::flow::{FlowLimits, SenderWindow};

/// One frame-aware bounded storage upload. Its backend future is driven by the endpoint.
pub struct UploadTransferSession {
    pub(super) plan: UploadTransferGrantPlan,
    pub(super) received: SenderWindow,
    pub(super) consumed: watch::Receiver<Consumed>,
    sender: Option<mpsc::Sender<Ingress>>,
    pub(super) upload_future:
        Option<futures_util::future::BoxFuture<'static, Result<StoreObjectInfo, ServerError>>>,
    transferred_bytes: u64,
    hasher: Sha256,
    complete: bool,
    updated_at: String,
}

impl UploadTransferSession {
    /// Prepare storage ingress; no backend work begins until authenticated activation.
    pub fn new(plan: UploadTransferGrantPlan, updated_at: impl Into<String>) -> Self {
        let (_, consumed) = watch::channel(Consumed::default());
        let received = SenderWindow::new(FlowLimits {
            max_frame_bytes: plan.grant.max_frame_bytes,
            window_frames: plan.grant.window_frames,
            window_bytes: plan.grant.window_bytes,
        });
        Self {
            plan,
            received,
            consumed,
            sender: None,
            upload_future: None,
            transferred_bytes: 0,
            hasher: Sha256::new(),
            complete: false,
            updated_at: updated_at.into(),
        }
    }

    /// Exact one-way DATA subject.
    pub fn subject(&self) -> &str {
        &self.plan.grant.data_subject
    }

    /// Session public key pinned by the grant.
    pub fn session_key(&self) -> &str {
        &self.plan.grant.consumer.session_key
    }

    pub(super) fn start<C: StoreResourceClient>(&mut self, store: C) -> Result<(), ServerError> {
        let (sender, receiver) = mpsc::channel(self.plan.grant.window_frames as usize + 1);
        let (consumed, progress) = watch::channel(Consumed::default());
        let mut reader = FrameReader::new(receiver, consumed);
        let key = self.plan.key.clone();
        self.consumed = progress;
        self.sender = Some(sender);
        self.upload_future = Some(Box::pin(async move {
            store.write_from(&key, &mut reader).await
        }));
        Ok(())
    }

    pub(super) fn consumption(&self) -> Consumed {
        *self.consumed.borrow()
    }

    pub(super) fn progress(&self) -> OperationTransferProgress {
        let consumed = self.consumption();
        OperationTransferProgress {
            chunk_index: consumed.seq.saturating_sub(1),
            chunk_bytes: consumed.frame_bytes,
            transferred_bytes: consumed.bytes,
        }
    }

    pub(super) fn admit(&mut self, seq: u64, bytes: Bytes) -> Result<(), ServerError> {
        if bytes.is_empty() || self.complete {
            return Err(ServerError::Nats("invalid upload DATA state".into()));
        }
        let consumed = self.consumption();
        let received = self.received.highest_sent.load(Ordering::Acquire);
        self.received
            .apply_credit(received, consumed.seq, Some(consumed.bytes))
            .map_err(|e| ServerError::Nats(format!("upload credit: {e:?}")))?;
        self.received
            .validate_frame_slot(bytes.len() as u64, self.plan.grant.max_frame_bytes)
            .map_err(|e| ServerError::Nats(format!("upload window violation: {e:?}")))?;
        if seq
            != self
                .received
                .next_frame_seq()
                .map_err(|e| ServerError::Nats(format!("upload sequence: {e:?}")))?
        {
            return Err(ServerError::TransferSequenceOutOfOrder {
                transfer_id: self.plan.grant.transfer_id.clone(),
                expected_seq: received + 1,
                actual_seq: seq,
            });
        }
        let total = self
            .transferred_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| ServerError::Nats("upload size overflow".into()))?;
        super::enforce_upload_max_bytes(&self.plan, total)?;
        self.hasher.update(&bytes);
        let len = bytes.len() as u64;
        self.sender
            .as_ref()
            .ok_or_else(|| ServerError::Nats("upload ingress not activated".into()))?
            .try_send(Ingress::Frame {
                seq,
                bytes: super::telemetry::Payload::new(
                    bytes,
                    trellis_protocol::transfer::TransferDirection::Send,
                    "provider-rx",
                ),
            })
            .map_err(|e| ServerError::Nats(format!("upload ingress closed/full: {e}")))?;
        self.received
            .commit_frame(seq, len)
            .map_err(|e| ServerError::Nats(format!("upload sequence: {e:?}")))?;
        self.transferred_bytes = total;
        Ok(())
    }

    pub(super) async fn finish(
        &mut self,
        final_seq: u64,
        size: u64,
        digest: &str,
    ) -> Result<FileTransferInfo, ServerError> {
        let actual_digest = format!(
            "SHA-256={}",
            URL_SAFE_NO_PAD.encode(self.hasher.clone().finalize())
        );
        if final_seq != self.received.highest_sent.load(Ordering::Acquire)
            || size != self.transferred_bytes
            || !transfer_digests_match(digest, &actual_digest)
        {
            tracing::warn!(
                sequence_matches = final_seq == self.received.highest_sent.load(Ordering::Acquire),
                size_matches = size == self.transferred_bytes,
                digest_matches = digest == actual_digest,
                digest_bytes_match = transfer_digests_match(digest, &actual_digest),
                "upload completion integrity mismatch"
            );
            return Err(ServerError::Nats(
                "upload completion sequence/size/digest mismatch".into(),
            ));
        }
        self.sender
            .as_ref()
            .ok_or_else(|| ServerError::Nats("upload ingress not activated".into()))?
            .send(Ingress::Complete)
            .await
            .map_err(|e| ServerError::Nats(e.to_string()))?;
        self.sender.take();
        // Keep the future in self while awaiting; dropping the endpoint cancels it.
        let stored = self
            .upload_future
            .as_mut()
            .ok_or_else(|| ServerError::Nats("upload backend missing".into()))?
            .await;
        self.upload_future.take();
        let stored = stored?;
        if stored.key != self.plan.key
            || stored.size != size
            || !stored
                .digest
                .as_deref()
                .is_some_and(|value| transfer_digests_match(value, &actual_digest))
        {
            return Err(ServerError::Nats("upload backend metadata mismatch".into()));
        }
        let consumed = self.consumption();
        self.received
            .apply_credit(final_seq, consumed.seq, Some(consumed.bytes))
            .map_err(|e| ServerError::Nats(format!("upload credit: {e:?}")))?;
        self.received.validate_complete(final_seq).map_err(|e| {
            ServerError::Nats(format!("upload incomplete storage consumption: {e:?}"))
        })?;
        self.complete = true;
        Ok(FileTransferInfo {
            key: self.plan.key.clone(),
            size,
            updated_at: match stored.modified_at {
                Some(timestamp) => timestamp
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|error| ServerError::Nats(error.to_string()))?,
                None => self.updated_at.clone(),
            },
            digest: actual_digest,
            content_type: self.plan.grant.content_type.clone(),
            metadata: self.plan.grant.metadata.clone(),
        })
    }

    pub(super) async fn abort(&mut self) {
        self.sender.take();
        self.upload_future.take();
    }

    pub(super) async fn early_backend_failure(&mut self) -> ServerError {
        let result = match self.upload_future.as_mut() {
            Some(future) => future.await,
            None => return ServerError::Nats("upload backend missing".into()),
        };
        self.upload_future.take();
        match result {
            Err(error) => error,
            Ok(_) => {
                ServerError::Nats("upload backend returned before validated completion".into())
            }
        }
    }

    /// Verify that authenticated completion reached backend commit.
    pub fn ensure_complete(&self) -> Result<(), ServerError> {
        if self.complete {
            Ok(())
        } else {
            Err(ServerError::TransferMissingEof {
                transfer_id: self.plan.grant.transfer_id.clone(),
            })
        }
    }
}

impl std::fmt::Debug for UploadTransferSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UploadTransferSession")
            .field("plan", &self.plan)
            .field("transferred_bytes", &self.transferred_bytes)
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}
