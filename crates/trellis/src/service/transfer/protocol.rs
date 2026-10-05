use super::super::{ServerError, ServiceResourceBindings};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, future::Future};
use tokio::{sync::oneshot, task::JoinHandle};

pub use crate::client::FileInfo as FileTransferInfo;

/// Identity pinned for an entire transfer, independent of physical attachments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferIdentity {
    /// Signed logical runtime connection id.
    pub connection_id: String,
    /// Runtime signing public key.
    pub session_key: String,
}

/// One prepared caller-to-service v2 streaming grant. Storage identity stays private.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadTransferGrant {
    /// Canonical Transfer protocol format.
    pub format: String,
    /// Grant discriminator.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Direction, `send`.
    pub direction: String,
    /// Owning service.
    pub service: String,
    /// Runtime-generated high entropy session id.
    pub transfer_id: String,
    /// Absolute RFC3339 expiry.
    pub expires_at: String,
    /// Provider identity.
    pub provider: TransferIdentity,
    /// Consumer identity.
    pub consumer: TransferIdentity,
    /// Exact raw DATA subject.
    pub data_subject: String,
    /// Exact owner-control subject.
    pub control_subject: String,
    /// Exact provider-signal subject.
    pub signal_subject: String,
    /// Negotiated maximum DATA payload size.
    pub max_frame_bytes: u64,
    /// Maximum outstanding frames.
    pub window_frames: u64,
    /// Maximum outstanding bytes.
    pub window_bytes: u64,
    /// Complete-object limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Retained media type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Retained application metadata.
    pub metadata: BTreeMap<String, String>,
}

/// One prepared provider-to-consumer v2 streaming grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadTransferGrant {
    /// Canonical Transfer protocol format.
    pub format: String,
    /// Grant discriminator.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Direction, `receive`.
    pub direction: String,
    /// Owning service.
    pub service: String,
    /// Runtime-generated high entropy session id.
    pub transfer_id: String,
    /// Absolute RFC3339 expiry.
    pub expires_at: String,
    /// Provider identity.
    pub provider: TransferIdentity,
    /// Consumer identity.
    pub consumer: TransferIdentity,
    /// Exact raw DATA subject.
    pub data_subject: String,
    /// Exact owner-control subject.
    pub control_subject: String,
    /// Exact provider-signal subject.
    pub signal_subject: String,
    /// Negotiated maximum DATA payload size.
    pub max_frame_bytes: u64,
    /// Maximum outstanding frames.
    pub window_frames: u64,
    /// Maximum outstanding bytes.
    pub window_bytes: u64,
    /// Independently verified logical file metadata.
    pub info: FileTransferInfo,
}

/// Inputs to storage-neutral upload planning.
#[derive(Debug)]
pub struct TransferUploadGrantArgs<'a> {
    /// Verified request that admitted this transfer and its exact permission.
    pub admission: &'a super::super::RequestContext,
    /// Owning service.
    pub service_name: &'a str,
    /// Caller public key.
    pub session_key: &'a str,
    /// Provider public key.
    pub service_session_key: &'a str,
    /// Caller logical connection.
    pub consumer_connection_id: &'a str,
    /// Provider logical connection.
    pub provider_connection_id: &'a str,
    /// Provider usable NATS payload limit.
    pub max_payload: u64,
    /// Resolved private storage bindings.
    pub resources: &'a ServiceResourceBindings,
    /// Contract-local store alias.
    pub store: &'a str,
    /// Logical destination key.
    pub key: &'a str,
    /// Runtime-generated transfer id.
    pub transfer_id: &'a str,
    /// RFC3339 expiry.
    pub expires_at: &'a str,
    /// Operation-level size cap.
    pub max_bytes: Option<u64>,
    /// Media type.
    pub content_type: Option<&'a str>,
    /// Application metadata.
    pub metadata: BTreeMap<String, String>,
}

/// Inputs to storage-neutral download planning.
#[derive(Debug)]
pub struct TransferDownloadGrantArgs<'a> {
    /// Verified request that admitted this transfer and its exact permission.
    pub admission: &'a super::super::RequestContext,
    /// Owning service.
    pub service_name: &'a str,
    /// Caller public key.
    pub session_key: &'a str,
    /// Provider public key.
    pub service_session_key: &'a str,
    /// Caller logical connection.
    pub consumer_connection_id: &'a str,
    /// Provider logical connection.
    pub provider_connection_id: &'a str,
    /// Provider usable NATS payload limit.
    pub max_payload: u64,
    /// Resolved private storage bindings.
    pub resources: &'a ServiceResourceBindings,
    /// Contract-local store alias.
    pub store: &'a str,
    /// Runtime-generated transfer id.
    pub transfer_id: &'a str,
    /// RFC3339 expiry.
    pub expires_at: &'a str,
    /// Expected logical object metadata.
    pub info: FileTransferInfo,
}

/// Caller-visible grant and private provider storage mapping.
#[derive(Debug, Clone)]
pub struct UploadTransferGrantPlan {
    /// Verified admission retained throughout the transfer.
    pub admission: super::super::RequestContext,
    /// Prepared grant.
    pub grant: UploadTransferGrant,
    /// Contract-local alias.
    pub store_alias: String,
    /// Physical storage identifier, never serialized to the caller.
    pub store: String,
    /// Logical destination key.
    pub key: String,
}

/// Caller-visible grant and private provider storage mapping.
#[derive(Debug, Clone)]
pub struct DownloadTransferGrantPlan {
    /// Verified admission retained throughout the transfer.
    pub admission: super::super::RequestContext,
    /// Prepared grant.
    pub grant: DownloadTransferGrant,
    /// Contract-local alias.
    pub store_alias: String,
    /// Physical storage identifier, never serialized to the caller.
    pub store: String,
    /// Installed backend object limit.
    pub max_object_bytes: Option<u64>,
}

pub(super) type UploadTransferCompletionResult = (
    Result<FileTransferInfo, ServerError>,
    Option<oneshot::Sender<Result<(), ServerError>>>,
);

/// Owned endpoint completion and final Operation persistence barrier.
#[derive(Debug)]
pub struct UploadTransferCompletion {
    pub(super) receiver: oneshot::Receiver<UploadTransferCompletionResult>,
    pub(super) task: Option<JoinHandle<Result<(), ServerError>>>,
}

impl UploadTransferCompletion {
    /// Await backend commit and permit the committed signal for a non-Operation upload.
    pub async fn completed(self) -> Result<FileTransferInfo, ServerError> {
        self.completed_after(|_| async { Ok(()) }).await
    }

    pub(crate) async fn completed_after<F, Fut>(
        mut self,
        persist: F,
    ) -> Result<FileTransferInfo, ServerError>
    where
        F: FnOnce(FileTransferInfo) -> Fut,
        Fut: Future<Output = Result<(), ServerError>>,
    {
        let result = match (&mut self.receiver).await {
            Ok((Ok(info), persisted)) => {
                let result = persist(info.clone()).await;
                if let Some(persisted) = persisted {
                    let _ = persisted.send(
                        result
                            .as_ref()
                            .map(|_| ())
                            .map_err(|error| ServerError::Nats(error.to_string())),
                    );
                }
                result.map(|_| info)
            }
            Ok((Err(error), _)) => Err(error),
            Err(_) => Err(ServerError::Nats("upload completion channel closed".into())),
        };
        // Even a failed backend/barrier must finish endpoint cleanup and its terminal signal.
        // Keep the handle here so cancelling this wait still aborts the owned endpoint.
        let endpoint = match self.task.as_mut() {
            Some(task) => task
                .await
                .map_err(|error| ServerError::Nats(error.to_string()))
                .and_then(|result| result),
            None => Ok(()),
        };
        self.task.take();
        let info = result?;
        endpoint?;
        Ok(info)
    }
}

impl Drop for UploadTransferCompletion {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
