use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};
use tokio::task::JoinHandle;

use super::super::{OperationTransferProgress, ServerError, StoreObjectInfo, StoreResourceClient};
use super::{
    abort_store_task, enforce_transfer_not_expired, enforce_upload_max_bytes,
    transfer_digests_match, FileTransferInfo, UploadTransferAck, UploadTransferChunk,
    UploadTransferControl, UploadTransferGrantPlan,
};

/// One bounded, store-backed upload session.
///
/// Data is backpressured through a single-frame pipe. The store sees EOF only
/// after an authenticated completion control validates size and SHA-256; drop
/// or cancellation before that point aborts the backend reader instead.
pub struct UploadTransferSession {
    pub(super) plan: UploadTransferGrantPlan,
    next_seq: u64,
    transferred_bytes: u64,
    hasher: Sha256,
    pipe: Option<tokio::io::DuplexStream>,
    pub(super) upload_state: Arc<AtomicU8>,
    upload_task: Option<JoinHandle<Result<StoreObjectInfo, ServerError>>>,
    complete: bool,
    updated_at: String,
}

impl std::fmt::Debug for UploadTransferSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UploadTransferSession")
            .field("plan", &self.plan)
            .field("next_seq", &self.next_seq)
            .field("transferred_bytes", &self.transferred_bytes)
            .field("complete", &self.complete)
            .field("updated_at", &self.updated_at)
            .finish_non_exhaustive()
    }
}

const UPLOAD_OPEN: u8 = 0;
pub(super) const UPLOAD_COMMIT: u8 = 1;
const UPLOAD_ABORT: u8 = 2;

struct UploadPipeReader {
    reader: tokio::io::DuplexStream,
    state: Arc<AtomicU8>,
}

impl AsyncRead for UploadPipeReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.reader).poll_read(cx, buf) {
            Poll::Ready(Ok(()))
                if buf.filled().len() == before
                    && this.state.load(Ordering::Acquire) != UPLOAD_COMMIT =>
            {
                Poll::Ready(Err(std::io::Error::other("upload transfer aborted")))
            }
            result => result,
        }
    }
}

impl UploadTransferSession {
    /// Create an unopened session reporting `updated_at` after durable commit.
    pub fn new(plan: UploadTransferGrantPlan, updated_at: impl Into<String>) -> Self {
        Self {
            plan,
            next_seq: 0,
            transferred_bytes: 0,
            hasher: Sha256::new(),
            pipe: None,
            upload_state: Arc::new(AtomicU8::new(UPLOAD_OPEN)),
            upload_task: None,
            complete: false,
            updated_at: updated_at.into(),
        }
    }

    /// Return the private NATS endpoint for this session.
    pub fn subject(&self) -> &str {
        &self.plan.grant.subject
    }

    /// Return the caller session key required by every frame proof.
    pub fn session_key(&self) -> &str {
        &self.plan.grant.session_key
    }

    /// Compute progress after accepting `chunk`, without mutating the session.
    pub fn progress_for_chunk(&self, chunk: &UploadTransferChunk) -> OperationTransferProgress {
        OperationTransferProgress {
            chunk_index: chunk.seq,
            chunk_bytes: chunk.payload.len() as u64,
            transferred_bytes: self
                .transferred_bytes
                .saturating_add(chunk.payload.len() as u64),
        }
    }

    pub(super) async fn start<C>(&mut self, store: C) -> Result<(), ServerError>
    where
        C: StoreResourceClient,
    {
        let capacity = usize::try_from(self.plan.grant.chunk_bytes)
            .unwrap_or(usize::MAX)
            .max(1);
        let (writer, reader) = tokio::io::duplex(capacity);
        let mut reader = UploadPipeReader {
            reader,
            state: Arc::clone(&self.upload_state),
        };
        let key = self.plan.key.clone();
        self.pipe = Some(writer);
        self.upload_task = Some(tokio::spawn(async move {
            store.write_from(&key, &mut reader).await
        }));
        Ok(())
    }

    pub(super) async fn receive_at(
        &mut self,
        chunk: UploadTransferChunk,
        now_iso: &str,
    ) -> Result<UploadTransferAck, ServerError> {
        if self.complete {
            return Err(ServerError::TransferAlreadyComplete {
                transfer_id: self.plan.grant.transfer_id.clone(),
            });
        }
        enforce_transfer_not_expired(
            &self.plan.grant.transfer_id,
            &self.plan.grant.expires_at,
            now_iso,
        )?;
        if chunk.seq != self.next_seq {
            return Err(ServerError::TransferSequenceOutOfOrder {
                transfer_id: self.plan.grant.transfer_id.clone(),
                expected_seq: self.next_seq,
                actual_seq: chunk.seq,
            });
        }
        let chunk_limit = self.plan.grant.chunk_bytes;
        if !chunk.eof && chunk.payload.len() as u64 > chunk_limit {
            return Err(ServerError::TransferObjectTooLarge {
                service_name: self.plan.grant.service.clone(),
                store: self.plan.store_alias.clone(),
                key: self.plan.key.clone(),
                size: chunk.payload.len() as u64,
                max_bytes: chunk_limit,
            });
        }
        if !chunk.eof {
            let next_size = self
                .transferred_bytes
                .checked_add(chunk.payload.len() as u64)
                .ok_or_else(|| ServerError::Nats("upload transfer size overflow".to_string()))?;
            enforce_upload_max_bytes(&self.plan, next_size)?;
            self.pipe
                .as_mut()
                .ok_or_else(|| ServerError::Nats("upload transfer pipe is not open".to_string()))?
                .write_all(&chunk.payload)
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))?;
            self.hasher.update(&chunk.payload);
            self.transferred_bytes = next_size;
            self.next_seq = self.next_seq.checked_add(1).ok_or_else(|| {
                ServerError::Nats("upload transfer sequence overflow".to_string())
            })?;
            return Ok(UploadTransferAck::Continue);
        }

        let control: UploadTransferControl = serde_json::from_slice(&chunk.payload)?;
        let UploadTransferControl::Complete { size, digest } = control else {
            self.abort().await;
            return Err(ServerError::TransferCancelled {
                transfer_id: self.plan.grant.transfer_id.clone(),
            });
        };
        if size != self.transferred_bytes {
            self.abort().await;
            return Err(ServerError::TransferObjectSizeMismatch {
                store: self.plan.store_alias.clone(),
                key: self.plan.key.clone(),
                expected_size: size,
                actual_size: self.transferred_bytes,
            });
        }
        let actual_digest = format!(
            "SHA-256={}",
            URL_SAFE_NO_PAD.encode(self.hasher.clone().finalize())
        );
        if digest != actual_digest {
            self.abort().await;
            return Err(ServerError::TransferDigestMismatch {
                transfer_id: self.plan.grant.transfer_id.clone(),
                expected_digest: digest,
                actual_digest,
            });
        }

        self.upload_state.store(UPLOAD_COMMIT, Ordering::Release);
        self.pipe.take();
        let stored = self
            .upload_task
            .take()
            .ok_or_else(|| ServerError::Nats("upload transfer task is not running".to_string()))?
            .await
            .map_err(|error| {
                ServerError::Nats(format!("upload transfer task failed: {error}"))
            })??;
        if stored.key != self.plan.key {
            return Err(ServerError::Nats(format!(
                "upload stored key mismatch: expected {}, got {}",
                self.plan.key, stored.key
            )));
        }
        if stored.size != self.transferred_bytes {
            return Err(ServerError::TransferObjectSizeMismatch {
                store: self.plan.store_alias.clone(),
                key: self.plan.key.clone(),
                expected_size: self.transferred_bytes,
                actual_size: stored.size,
            });
        }
        let stored_digest = stored
            .digest
            .ok_or_else(|| ServerError::TransferDigestMismatch {
                transfer_id: self.plan.grant.transfer_id.clone(),
                expected_digest: actual_digest.clone(),
                actual_digest: "missing backend digest".to_string(),
            })?;
        if !transfer_digests_match(&stored_digest, &actual_digest) {
            return Err(ServerError::TransferDigestMismatch {
                transfer_id: self.plan.grant.transfer_id.clone(),
                expected_digest: actual_digest,
                actual_digest: stored_digest,
            });
        }
        let info = FileTransferInfo {
            key: self.plan.key.clone(),
            size: self.transferred_bytes,
            updated_at: self.updated_at.clone(),
            digest: actual_digest,
            content_type: self.plan.grant.content_type.clone(),
            metadata: self.plan.grant.metadata.clone(),
        };
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or_else(|| ServerError::Nats("upload transfer sequence overflow".to_string()))?;
        self.complete = true;
        Ok(UploadTransferAck::Complete { info })
    }

    pub(super) async fn abort(&mut self) {
        self.upload_state.store(UPLOAD_ABORT, Ordering::Release);
        self.pipe.take();
        abort_store_task(&mut self.upload_task).await;
    }

    /// Verify that an authenticated completion frame committed the backend object.
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
