use std::collections::BTreeMap;
use std::future::Future;

use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::oneshot;

use super::super::{ServerError, ServiceResourceBindings};

const UPLOAD_SUBJECT_PREFIX: &str = "transfer.v1.upload";
const DOWNLOAD_SUBJECT_PREFIX: &str = "transfer.v1.download";

/// Header carrying the zero-based transfer frame sequence number.
pub const TRANSFER_SEQUENCE_HEADER: &str = "trellis-transfer-seq";
/// Legacy EOF header, rejected by the authenticated control-frame protocol.
pub const TRANSFER_EOF_HEADER: &str = "trellis-transfer-eof";
/// Header distinguishing signed completion and cancellation controls from data.
pub const TRANSFER_CONTROL_HEADER: &str = "trellis-transfer-control";
/// Largest individual frame accepted by either Trellis runtime.
pub const MAX_TRANSFER_CHUNK_BYTES: u64 = 1024 * 1024;
const TRANSFER_FRAME_PROOF_DOMAIN: &[u8] = b"trellis.transfer.v1.frame\0";

pub(crate) fn transfer_frame_proof_payload(
    seq: u64,
    control: Option<&str>,
    payload: &[u8],
) -> Bytes {
    let control = control.unwrap_or_default().as_bytes();
    let mut framed = Vec::with_capacity(
        TRANSFER_FRAME_PROOF_DOMAIN.len() + 8 + 4 + control.len() + payload.len(),
    );
    framed.extend_from_slice(TRANSFER_FRAME_PROOF_DOMAIN);
    framed.extend_from_slice(&seq.to_be_bytes());
    framed.extend_from_slice(&(control.len() as u32).to_be_bytes());
    framed.extend_from_slice(control);
    framed.extend_from_slice(payload);
    Bytes::from(framed)
}

/// Store metadata authenticated by a receive grant and verified after streaming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileTransferInfo {
    /// Logical object key within the authorized store binding.
    pub key: String,
    /// Declared complete-object byte count, verified against streamed bytes.
    pub size: u64,
    /// Store-reported update timestamp encoded as ISO 8601.
    pub updated_at: String,
    /// Required SHA-256 digest, verified independently by the receiver.
    pub digest: String,
    /// Object content type, when one was stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Store metadata authenticated with the transfer grant.
    pub metadata: BTreeMap<String, String>,
}

/// Inputs for planning a service-owned upload transfer grant.
#[derive(Debug)]
pub struct TransferUploadGrantArgs<'a> {
    /// Service exposed in the grant.
    pub service_name: &'a str,
    /// Caller session that alone may use the grant.
    pub session_key: &'a str,
    /// Service session used to scope the private transfer subject.
    pub service_session_key: &'a str,
    /// Resolved bootstrap resource bindings.
    pub resources: &'a ServiceResourceBindings,
    /// Contract-local store alias.
    pub store: &'a str,
    /// Logical destination object key.
    pub key: &'a str,
    /// Preallocated transfer identifier.
    pub transfer_id: &'a str,
    /// ISO-8601 instant after which the endpoint rejects frames.
    pub expires_at: &'a str,
    /// Maximum payload bytes permitted in one data frame.
    pub chunk_bytes: u64,
    /// Operation-level size cap before the store binding cap is applied.
    pub max_bytes: Option<u64>,
    /// Content type to attach only after a complete upload commits.
    pub content_type: Option<&'a str>,
    /// Metadata to attach only after a complete upload commits.
    pub metadata: BTreeMap<String, String>,
}

/// Inputs for planning a service-owned download transfer grant.
#[derive(Debug)]
pub struct TransferDownloadGrantArgs<'a> {
    /// Service exposed in the grant.
    pub service_name: &'a str,
    /// Caller session that alone may use the grant.
    pub session_key: &'a str,
    /// Service session used to scope the private transfer subject.
    pub service_session_key: &'a str,
    /// Resolved bootstrap resource bindings.
    pub resources: &'a ServiceResourceBindings,
    /// Contract-local store alias.
    pub store: &'a str,
    /// Preallocated transfer identifier.
    pub transfer_id: &'a str,
    /// ISO-8601 instant after which the endpoint rejects requests.
    pub expires_at: &'a str,
    /// Maximum payload bytes permitted in one response frame.
    pub chunk_bytes: u64,
    /// Complete-object metadata that the receiver must verify.
    pub info: FileTransferInfo,
}

/// Wire grant for a caller-to-service upload session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadTransferGrant {
    /// Wire discriminator.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Transfer direction, always `send`.
    pub direction: String,
    /// Service accepting the upload.
    pub service: String,
    /// Caller session bound to the grant proof.
    pub session_key: String,
    /// Unique transfer identifier.
    pub transfer_id: String,
    /// Private NATS endpoint scoped to the service session.
    pub subject: String,
    /// ISO-8601 grant expiry.
    pub expires_at: String,
    /// Maximum payload bytes in one data frame.
    #[serde(deserialize_with = "deserialize_chunk_bytes")]
    pub chunk_bytes: u64,
    /// Effective complete-object cap after applying operation and store limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Content type committed with a successful upload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Metadata committed with a successful upload.
    pub metadata: BTreeMap<String, String>,
}

/// Upload grant plus the private binding data needed by its endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTransferGrantPlan {
    /// Caller-visible grant.
    pub grant: UploadTransferGrant,
    /// Contract-local alias used in errors and authorization.
    pub store_alias: String,
    /// Resolved physical store used only inside the service runtime.
    pub store: String,
    /// Logical destination key.
    pub key: String,
}

/// Wire grant for a service-to-caller download session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTransferGrant {
    /// Wire discriminator.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Transfer direction, always `receive`.
    pub direction: String,
    /// Service providing the object.
    pub service: String,
    /// Caller session bound to the grant proof.
    pub session_key: String,
    /// Unique transfer identifier.
    pub transfer_id: String,
    /// Private NATS endpoint scoped to the service session.
    pub subject: String,
    /// ISO-8601 grant expiry.
    pub expires_at: String,
    /// Maximum payload bytes in one response frame.
    #[serde(deserialize_with = "deserialize_chunk_bytes")]
    pub chunk_bytes: u64,
    /// Required final metadata independently verified by the receiver.
    pub info: FileTransferInfo,
}

/// Download grant plus the private binding data needed by its endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTransferGrantPlan {
    /// Caller-visible grant.
    pub grant: DownloadTransferGrant,
    /// Contract-local alias used in errors and authorization.
    pub store_alias: String,
    /// Resolved physical store used only inside the service runtime.
    pub store: String,
    /// Effective complete-object limit from the binding.
    pub max_object_bytes: Option<u64>,
}

/// One authenticated upload frame after header decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTransferChunk {
    /// Zero-based frame sequence.
    pub seq: u64,
    /// Raw data or signed control payload.
    pub payload: Bytes,
    /// Whether this is a completion control frame.
    pub eof: bool,
    /// Whether this is a cancellation control frame.
    pub cancel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "lowercase")]
pub(super) enum UploadTransferControl {
    Complete { size: u64, digest: String },
    Cancel,
}

/// Service acknowledgement for one upload request frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum UploadTransferAck {
    /// The frame committed to the bounded service pipe and another is expected.
    Continue,
    /// The completion control was verified and the backend committed metadata.
    Complete {
        /// Final independently verified object metadata.
        info: FileTransferInfo,
    },
    /// Cancellation was authenticated and backend work was stopped and joined.
    Cancelled,
}

/// Provider-side completion signal for a spawned upload endpoint.
#[derive(Debug)]
pub struct UploadTransferCompletion {
    pub(super) receiver: oneshot::Receiver<UploadTransferCompletionResult>,
}

pub(super) type UploadTransferCompletionResult = (
    Result<FileTransferInfo, ServerError>,
    Option<oneshot::Sender<Result<(), ServerError>>>,
);

impl UploadTransferCompletion {
    /// Wait for durable object metadata or the terminal transfer failure.
    pub async fn completed(self) -> Result<FileTransferInfo, ServerError> {
        let (result, persisted) = self.receiver.await.map_err(|_| {
            ServerError::Nats("upload transfer completion channel closed".to_string())
        })?;
        if let Some(persisted) = persisted {
            let _ = persisted.send(Ok(()));
        }
        result
    }

    pub(crate) async fn completed_after<F, Fut>(
        self,
        persist: F,
    ) -> Result<FileTransferInfo, ServerError>
    where
        F: FnOnce(FileTransferInfo) -> Fut,
        Fut: Future<Output = Result<(), ServerError>>,
    {
        let (result, persisted) = self.receiver.await.map_err(|_| {
            ServerError::Nats("upload transfer completion channel closed".to_string())
        })?;
        let info = result?;
        let result = persist(info.clone()).await;
        if let Some(persisted) = persisted {
            let _ = persisted.send(
                result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|error| ServerError::Nats(error.to_string())),
            );
        }
        result?;
        Ok(info)
    }
}

pub(crate) fn upload_subject_prefix() -> &'static str {
    UPLOAD_SUBJECT_PREFIX
}

pub(super) fn download_subject_prefix() -> &'static str {
    DOWNLOAD_SUBJECT_PREFIX
}

fn deserialize_chunk_bytes<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let chunk_bytes = u64::deserialize(deserializer)?;
    if chunk_bytes == 0 || chunk_bytes > MAX_TRANSFER_CHUNK_BYTES {
        return Err(serde::de::Error::custom(format!(
            "chunkBytes must be between 1 and {MAX_TRANSFER_CHUNK_BYTES}"
        )));
    }
    Ok(chunk_bytes)
}
