use std::collections::BTreeMap;
use std::time::Duration;

use async_nats::header::HeaderMap;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;

use crate::client::connection::signed_headers;
use crate::client::{SessionAuth, TrellisClient, TrellisClientError};

const TRANSFER_SEQUENCE_HEADER: &str = "trellis-transfer-seq";
const TRANSFER_EOF_HEADER: &str = "trellis-transfer-eof";
const TRANSFER_CONTROL_HEADER: &str = "trellis-transfer-control";
const MAX_TRANSFER_CHUNK_BYTES: u64 = 1024 * 1024;

/// Cloneable cancellation signal for an active transfer.
#[derive(Debug, Clone)]
pub struct TransferCancellation {
    sender: watch::Sender<bool>,
}

impl TransferCancellation {
    /// Create a cancellation signal in the active state.
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    /// Request transfer cancellation.
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

    fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
}

impl Default for TransferCancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata verified after a completed download or committed upload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    /// Logical object key within the authorized store resource.
    pub key: String,
    /// Complete object size in bytes.
    pub size: u64,
    /// Backend commit timestamp encoded as RFC 3339.
    pub updated_at: String,
    /// Required SHA-256 digest used for independent end-to-end verification.
    pub digest: String,
    /// Media type retained with the object when one was supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Application metadata retained with the object.
    pub metadata: BTreeMap<String, String>,
}

/// Short-lived, session-bound authority to upload one object.
///
/// The client must use the declared subject and frame size unchanged. Completion
/// is accepted only after the receiver verifies the authenticated size and digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UploadTransferGrant {
    /// Wire discriminator, always `TransferGrant`.
    #[serde(rename = "type")]
    pub type_name: TransferGrantType,
    /// Caller-to-service transfer direction, always `send`.
    pub direction: UploadTransferDirection,
    /// Service that issued and serves this grant.
    pub service: String,
    /// Session public key to which this grant is bound.
    pub session_key: String,
    /// Unique transfer identifier included in signed frame proofs.
    pub transfer_id: String,
    /// Exact NATS endpoint authorized for this upload.
    pub subject: String,
    /// Grant expiry encoded as RFC 3339.
    pub expires_at: String,
    /// Maximum bytes in one non-completion data frame.
    #[serde(deserialize_with = "deserialize_chunk_bytes")]
    pub chunk_bytes: u64,
    /// Optional complete-object size limit enforced by the receiver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Media type to retain with the committed object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Application metadata to retain with the committed object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
}

/// Short-lived, session-bound authority to download one verified object.
///
/// Download requests pull one frame at a time. The client verifies the final
/// byte count and digest against [`DownloadTransferGrant::info`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTransferGrant {
    /// Wire discriminator, always `TransferGrant`.
    #[serde(rename = "type")]
    pub type_name: TransferGrantType,
    /// Service-to-caller transfer direction, always `receive`.
    pub direction: DownloadTransferDirection,
    /// Service that issued and serves this grant.
    pub service: String,
    /// Session public key to which this grant is bound.
    pub session_key: String,
    /// Unique transfer identifier included in signed frame proofs.
    pub transfer_id: String,
    /// Exact NATS endpoint authorized for this download.
    pub subject: String,
    /// Grant expiry encoded as RFC 3339.
    pub expires_at: String,
    /// Maximum bytes returned in one data frame.
    #[serde(deserialize_with = "deserialize_chunk_bytes")]
    pub chunk_bytes: u64,
    /// Expected committed object metadata used for final verification.
    pub info: FileInfo,
}

/// Transfer grant wire discriminator.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferGrantType {
    /// Identifies a transfer grant.
    #[serde(rename = "TransferGrant")]
    TransferGrant,
}

/// Caller-to-service transfer direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UploadTransferDirection {
    /// The caller sends bytes to the service.
    #[serde(rename = "send")]
    Send,
}

/// Service-to-caller transfer direction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DownloadTransferDirection {
    /// The caller receives bytes from the service.
    #[serde(rename = "receive")]
    Receive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "lowercase")]
enum UploadAck {
    Continue,
    Complete { info: FileInfo },
    Cancelled,
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum UploadControl<'a> {
    Complete { size: u64, digest: &'a str },
    Cancel,
}

fn upload_headers(
    auth: &SessionAuth,
    context_digest: &str,
    subject: &str,
    reply: &str,
    payload: &[u8],
    seq: u64,
    control: Option<&str>,
) -> Result<HeaderMap, TrellisClientError> {
    let proof_payload = crate::service::transfer_frame_proof_payload(seq, control, payload);
    let mut headers = signed_headers(auth, context_digest, subject, reply, &proof_payload)?;
    headers.insert(TRANSFER_SEQUENCE_HEADER, seq.to_string().as_str());
    if let Some(control) = control {
        headers.insert(TRANSFER_CONTROL_HEADER, control);
    }
    Ok(headers)
}

fn transfer_chunk_size(chunk_bytes: u64) -> Result<usize, TrellisClientError> {
    if !(1..=MAX_TRANSFER_CHUNK_BYTES).contains(&chunk_bytes) {
        return Err(TrellisClientError::TransferProtocol(format!(
            "transfer chunk size must be between 1 and {MAX_TRANSFER_CHUNK_BYTES} bytes, got {chunk_bytes}"
        )));
    }
    usize::try_from(chunk_bytes).map_err(|_| {
        TrellisClientError::TransferProtocol(
            "transfer chunk size does not fit in usize".to_string(),
        )
    })
}

fn deserialize_chunk_bytes<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let chunk_bytes = u64::deserialize(deserializer)?;
    if !(1..=MAX_TRANSFER_CHUNK_BYTES).contains(&chunk_bytes) {
        return Err(serde::de::Error::custom(format!(
            "transfer chunk size must be between 1 and {MAX_TRANSFER_CHUNK_BYTES} bytes"
        )));
    }
    Ok(chunk_bytes)
}

pub(crate) async fn put_upload_grant(
    client: &TrellisClient,
    grant: &UploadTransferGrant,
    body: impl AsRef<[u8]>,
) -> Result<FileInfo, TrellisClientError> {
    let bytes = body.as_ref();
    let expected_size = u64::try_from(bytes.len()).map_err(|_| {
        TrellisClientError::TransferProtocol("upload length does not fit in u64".to_string())
    })?;
    let mut reader = std::io::Cursor::new(bytes);
    put_upload_grant_from(client, grant, &mut reader, Some(expected_size)).await
}

pub(crate) async fn put_upload_grant_from<R>(
    client: &TrellisClient,
    grant: &UploadTransferGrant,
    reader: &mut R,
    expected_size: Option<u64>,
) -> Result<FileInfo, TrellisClientError>
where
    R: AsyncRead + Unpin + Send + ?Sized,
{
    put_upload_grant_from_with_cancel(client, grant, reader, expected_size, None).await
}

pub(crate) async fn put_upload_grant_from_with_cancel<R>(
    client: &TrellisClient,
    grant: &UploadTransferGrant,
    reader: &mut R,
    expected_size: Option<u64>,
    cancellation: Option<&TransferCancellation>,
) -> Result<FileInfo, TrellisClientError>
where
    R: AsyncRead + Unpin + Send + ?Sized,
{
    validate_grant(&grant.session_key, client)?;
    if let (Some(expected_size), Some(max_bytes)) = (expected_size, grant.max_bytes) {
        if expected_size > max_bytes {
            return Err(TrellisClientError::TransferProtocol(format!(
                "upload exceeds max bytes: attempted {expected_size}, max {max_bytes}"
            )));
        }
    }
    let max_chunk = transfer_chunk_size(grant.chunk_bytes)?;
    let context_digest = client.authorization_context_digest()?;
    let mut seq: u64 = 0;
    let mut transferred = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; max_chunk];

    loop {
        let count = if let Some(cancellation) = cancellation {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    send_transfer_cancel(client, &grant.subject, &context_digest, seq).await?;
                    return Err(TrellisClientError::TransferCancelled);
                }
                count = reader.read(&mut buffer) => match count {
                    Ok(count) => count,
                    Err(error) => {
                        let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                        return Err(error.into());
                    }
                },
            }
        } else {
            match reader.read(&mut buffer).await {
                Ok(count) => count,
                Err(error) => {
                    let _ =
                        send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                    return Err(error.into());
                }
            }
        };
        if count == 0 {
            break;
        }
        let chunk = &buffer[..count];
        let next = transferred.checked_add(count as u64).ok_or_else(|| {
            TrellisClientError::TransferProtocol("upload size overflow".to_string())
        })?;
        if let Some(expected_size) = expected_size {
            if next > expected_size {
                let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                return Err(TrellisClientError::TransferProtocol(format!(
                    "upload size mismatch: expected {expected_size}, got at least {next}"
                )));
            }
        }
        if let Some(max_bytes) = grant.max_bytes {
            if next > max_bytes {
                let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                return Err(TrellisClientError::TransferProtocol(format!(
                    "upload exceeds max bytes: attempted {next}, max {max_bytes}"
                )));
            }
        }
        let reply = client.nats().new_inbox();
        let headers = upload_headers(
            client.auth(),
            &context_digest,
            &grant.subject,
            &reply,
            chunk,
            seq,
            None,
        )?;
        let request = async_nats::Request::new()
            .inbox(reply)
            .headers(headers)
            .payload(Bytes::copy_from_slice(chunk));
        let nats = client.nats();
        let response = {
            let request = tokio::time::timeout(
                Duration::from_millis(client.timeout_ms()),
                nats.send_request(grant.subject.clone(), request),
            );
            tokio::pin!(request);
            if let Some(cancellation) = cancellation {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => None,
                    response = &mut request => Some(response),
                }
            } else {
                Some(request.await)
            }
        };
        let Some(response) = response else {
            send_transfer_cancel(client, &grant.subject, &context_digest, seq).await?;
            return Err(TrellisClientError::TransferCancelled);
        };
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                return Err(TrellisClientError::NatsRequest(error.to_string()));
            }
            Err(_) => {
                let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                return Err(TrellisClientError::Timeout);
            }
        };

        let ack = match parse_upload_ack(response) {
            Ok(ack) => ack,
            Err(error) => {
                let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
                return Err(error);
            }
        };
        if !matches!(ack, UploadAck::Continue) {
            let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
            return Err(TrellisClientError::TransferProtocol(
                "upload completed before eof frame".into(),
            ));
        }
        hasher.update(chunk);
        transferred = next;
        seq = seq.checked_add(1).ok_or_else(|| {
            TrellisClientError::TransferProtocol("upload sequence overflow".to_string())
        })?;
    }

    if let Some(expected_size) = expected_size {
        if transferred != expected_size {
            let _ = send_transfer_cancel(client, &grant.subject, &context_digest, seq).await;
            return Err(TrellisClientError::TransferProtocol(format!(
                "upload size mismatch: expected {expected_size}, got {transferred}"
            )));
        }
    }

    let digest = format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.finalize()));
    let completion = serde_json::to_vec(&UploadControl::Complete {
        size: transferred,
        digest: &digest,
    })?;

    let reply = client.nats().new_inbox();
    let headers = upload_headers(
        client.auth(),
        &context_digest,
        &grant.subject,
        &reply,
        &completion,
        seq,
        Some("complete"),
    )?;
    let request = async_nats::Request::new()
        .inbox(reply)
        .headers(headers)
        .payload(Bytes::from(completion));
    let response = tokio::time::timeout(
        Duration::from_millis(client.timeout_ms()),
        client.nats().send_request(grant.subject.clone(), request),
    )
    .await
    .map_err(|_| TrellisClientError::Timeout)?
    .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

    match parse_upload_ack(response)? {
        UploadAck::Continue | UploadAck::Cancelled => Err(TrellisClientError::TransferProtocol(
            "upload finished without completion payload".into(),
        )),
        UploadAck::Complete { info } => {
            if info.size != transferred {
                return Err(TrellisClientError::TransferProtocol(format!(
                    "upload result size mismatch: expected {transferred}, got {}",
                    info.size
                )));
            }
            if !transfer_digests_match(&info.digest, &digest) {
                return Err(TrellisClientError::TransferProtocol(format!(
                    "upload result digest mismatch: expected {digest}, got {:?}",
                    info.digest
                )));
            }
            Ok(info)
        }
    }
}

async fn send_transfer_cancel(
    client: &TrellisClient,
    subject: &str,
    context_digest: &str,
    seq: u64,
) -> Result<(), TrellisClientError> {
    let payload = Bytes::from(serde_json::to_vec(&UploadControl::Cancel)?);
    let reply = client.nats().new_inbox();
    let headers = upload_headers(
        client.auth(),
        context_digest,
        subject,
        &reply,
        &payload,
        seq,
        Some("cancel"),
    )?;
    let response = tokio::time::timeout(
        Duration::from_millis(client.timeout_ms()),
        client.nats().send_request(
            subject.to_string(),
            async_nats::Request::new()
                .inbox(reply)
                .headers(headers)
                .payload(payload),
        ),
    )
    .await
    .map_err(|_| TrellisClientError::Timeout)?
    .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;
    if !matches!(parse_upload_ack(response)?, UploadAck::Cancelled) {
        return Err(TrellisClientError::TransferProtocol(
            "transfer cancellation was not acknowledged".to_string(),
        ));
    }
    Ok(())
}

pub(crate) async fn get_download_grant(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
) -> Result<Vec<u8>, TrellisClientError> {
    let mut writer = std::io::Cursor::new(Vec::new());
    get_download_grant_into(client, grant, &mut writer).await?;
    Ok(writer.into_inner())
}

pub(crate) async fn get_download_grant_into<W>(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
    writer: &mut W,
) -> Result<FileInfo, TrellisClientError>
where
    W: AsyncWrite + Unpin + Send + ?Sized,
{
    get_download_grant_into_with_cancel(client, grant, writer, None).await
}

pub(crate) async fn get_download_grant_into_with_cancel<W>(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
    writer: &mut W,
    cancellation: Option<&TransferCancellation>,
) -> Result<FileInfo, TrellisClientError>
where
    W: AsyncWrite + Unpin + Send + ?Sized,
{
    let result = get_download_grant_into_inner(client, grant, writer, cancellation).await;
    if result.is_err()
        && !matches!(&result, Err(TrellisClientError::TransferCancelled))
        && grant.session_key == client.auth().session_key
    {
        if let Ok(context_digest) = client.authorization_context_digest() {
            let _ = send_transfer_cancel(client, &grant.subject, &context_digest, 0).await;
        }
    }
    result
}

async fn get_download_grant_into_inner<W>(
    client: &TrellisClient,
    grant: &DownloadTransferGrant,
    writer: &mut W,
    cancellation: Option<&TransferCancellation>,
) -> Result<FileInfo, TrellisClientError>
where
    W: AsyncWrite + Unpin + Send + ?Sized,
{
    validate_grant(&grant.session_key, client)?;
    transfer_chunk_size(grant.chunk_bytes)?;
    let expected_digest = &grant.info.digest;
    let context_digest = client.authorization_context_digest()?;

    let mut expected_seq = 0_u64;
    let mut transferred = 0_u64;
    let mut hasher = Sha256::new();
    loop {
        if cancellation.is_some_and(TransferCancellation::is_cancelled) {
            send_transfer_cancel(client, &grant.subject, &context_digest, expected_seq).await?;
            return Err(TrellisClientError::TransferCancelled);
        }
        let reply = client.nats().new_inbox();
        let headers = upload_headers(
            client.auth(),
            &context_digest,
            &grant.subject,
            &reply,
            &[],
            expected_seq,
            None,
        )?;
        let request = async_nats::Request::new()
            .inbox(reply)
            .headers(headers)
            .payload(Bytes::new());
        let nats = client.nats();
        let response = nats.send_request(grant.subject.clone(), request);
        let message = if let Some(cancellation) = cancellation {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    send_transfer_cancel(
                        client,
                        &grant.subject,
                        &context_digest,
                        expected_seq,
                    ).await?;
                    return Err(TrellisClientError::TransferCancelled);
                }
                response = tokio::time::timeout(Duration::from_millis(client.timeout_ms()), response) => {
                    response.map_err(|_| TrellisClientError::Timeout)?
                }
            }
        } else {
            tokio::time::timeout(Duration::from_millis(client.timeout_ms()), response)
                .await
                .map_err(|_| TrellisClientError::Timeout)?
        }
        .map_err(|error| TrellisClientError::NatsRequest(error.to_string()))?;

        if message
            .headers
            .as_ref()
            .and_then(|headers| headers.get("status"))
            .is_some_and(|status| status.as_str() == "error")
        {
            let value: serde_json::Value = serde_json::from_slice(&message.payload)?;
            return Err(TrellisClientError::TransferProtocol(value.to_string()));
        }

        let actual_seq = message
            .headers
            .as_ref()
            .and_then(|headers| headers.get(TRANSFER_SEQUENCE_HEADER))
            .ok_or_else(|| {
                TrellisClientError::TransferProtocol(
                    "download frame missing transfer sequence".to_string(),
                )
            })?
            .as_str()
            .parse::<u64>()
            .map_err(|_| {
                TrellisClientError::TransferProtocol(
                    "download frame has invalid transfer sequence".to_string(),
                )
            })?;
        if actual_seq != expected_seq {
            return Err(TrellisClientError::TransferProtocol(format!(
                "download sequence mismatch: expected {expected_seq}, got {actual_seq}"
            )));
        }
        if message.payload.len() as u64 > grant.chunk_bytes {
            return Err(TrellisClientError::TransferProtocol(format!(
                "download frame exceeds chunk size: attempted {}, max {}",
                message.payload.len(),
                grant.chunk_bytes
            )));
        }
        let next = transferred
            .checked_add(message.payload.len() as u64)
            .ok_or_else(|| {
                TrellisClientError::TransferProtocol("download size overflow".to_string())
            })?;
        if next > grant.info.size {
            return Err(TrellisClientError::TransferProtocol(format!(
                "download exceeds declared size: attempted {next}, expected {}",
                grant.info.size
            )));
        }

        let eof = message
            .headers
            .as_ref()
            .and_then(|headers| headers.get(TRANSFER_EOF_HEADER))
            .is_some_and(|value| value.as_str() == "true");
        if eof {
            if !message.payload.is_empty() {
                return Err(TrellisClientError::TransferProtocol(
                    "download eof frame must be empty".to_string(),
                ));
            }
            if transferred != grant.info.size {
                return Err(TrellisClientError::TransferProtocol(format!(
                    "download size mismatch: expected {}, got {transferred}",
                    grant.info.size
                )));
            }
            let actual_digest = format!("SHA-256={}", URL_SAFE_NO_PAD.encode(hasher.finalize()));
            if !transfer_digests_match(&actual_digest, expected_digest) {
                return Err(TrellisClientError::TransferProtocol(format!(
                    "download digest mismatch: expected {expected_digest}, got {actual_digest}"
                )));
            }
            return Ok(grant.info.clone());
        }

        let write = writer.write_all(&message.payload);
        if let Some(cancellation) = cancellation {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    send_transfer_cancel(client, &grant.subject, &context_digest, expected_seq).await?;
                    return Err(TrellisClientError::TransferCancelled);
                }
                result = write => {
                    if let Err(error) = result {
                        let _ = send_transfer_cancel(client, &grant.subject, &context_digest, expected_seq).await;
                        return Err(error.into());
                    }
                }
            }
        } else if let Err(error) = write.await {
            let _ =
                send_transfer_cancel(client, &grant.subject, &context_digest, expected_seq).await;
            return Err(error.into());
        }
        hasher.update(&message.payload);
        transferred = next;
        expected_seq = expected_seq.checked_add(1).ok_or_else(|| {
            TrellisClientError::TransferProtocol("download sequence overflow".to_string())
        })?;
    }
}

/// Parse a receive transfer grant from generated SDK or raw JSON values.
pub fn download_transfer_grant_from_value(
    value: serde_json::Value,
) -> Result<DownloadTransferGrant, TrellisClientError> {
    Ok(serde_json::from_value(value)?)
}

fn validate_grant(
    expected_session_key: &str,
    client: &TrellisClient,
) -> Result<(), TrellisClientError> {
    if expected_session_key != client.auth().session_key {
        return Err(TrellisClientError::TransferProtocol(
            "transfer grant session key does not match client session".into(),
        ));
    }
    Ok(())
}

fn transfer_digests_match(left: &str, right: &str) -> bool {
    left.trim_end_matches('=') == right.trim_end_matches('=')
}

fn parse_upload_ack(message: async_nats::Message) -> Result<UploadAck, TrellisClientError> {
    if message
        .headers
        .as_ref()
        .and_then(|headers| headers.get("status"))
        .is_some_and(|status| status.as_str() == "error")
    {
        let value: serde_json::Value = serde_json::from_slice(&message.payload)?;
        return Err(TrellisClientError::TransferProtocol(value.to_string()));
    }

    Ok(serde_json::from_slice(&message.payload)?)
}

#[cfg(test)]
mod tests {
    use crate::client::proof::{verify_event_proof, VerifyEventProofInput};
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use trellis_protocol::build_authorization_request_proof_input;

    use super::*;

    fn test_auth() -> SessionAuth {
        SessionAuth::from_seed_base64url("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("session auth")
    }

    const TEST_CONTEXT_DIGEST: &str = "byhVYTUxr4iVywgon-utTJesrl5WZVm1MC0PXqCU06c";

    #[tokio::test]
    async fn cancellation_cannot_lose_registration_wakeups() {
        for _ in 0..2_000 {
            let cancellation = TransferCancellation::new();
            let waiter = {
                let cancellation = cancellation.clone();
                tokio::spawn(async move { cancellation.cancelled().await })
            };
            tokio::task::yield_now().await;
            cancellation.cancel();
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("cancellation waiter must wake")
                .expect("cancellation waiter task");
        }
    }

    #[test]
    fn transfer_chunk_size_is_bounded() {
        assert!(transfer_chunk_size(0).is_err());
        assert_eq!(transfer_chunk_size(6).unwrap(), 6);
        assert!(transfer_chunk_size(MAX_TRANSFER_CHUNK_BYTES + 1).is_err());
    }

    #[test]
    fn transfer_grants_validate_wire_literals_bounds_and_digest() {
        let upload = serde_json::json!({
            "type": "TransferGrant",
            "direction": "send",
            "service": "service",
            "sessionKey": "session",
            "transferId": "transfer",
            "subject": "transfer.v1.upload.service.transfer",
            "expiresAt": "2099-01-01T00:00:00Z",
            "chunkBytes": 1
        });
        let upload_grant = serde_json::from_value::<UploadTransferGrant>(upload.clone()).unwrap();
        assert_eq!(serde_json::to_value(upload_grant).unwrap(), upload);

        let grant = serde_json::json!({
            "type": "TransferGrant",
            "direction": "receive",
            "service": "service",
            "sessionKey": "session",
            "transferId": "transfer",
            "subject": "transfer.v1.download.service.transfer",
            "expiresAt": "2099-01-01T00:00:00Z",
            "chunkBytes": 1,
            "info": {
                "key": "object",
                "size": 0,
                "updatedAt": "2099-01-01T00:00:00Z",
                "digest": "SHA-256=47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU",
                "metadata": {}
            }
        });
        let download_grant =
            serde_json::from_value::<DownloadTransferGrant>(grant.clone()).unwrap();
        assert_eq!(serde_json::to_value(download_grant).unwrap(), grant);

        for (field, value) in [("type", "transfer.v1"), ("direction", "download")] {
            let mut invalid = grant.clone();
            invalid[field] = value.into();
            assert!(serde_json::from_value::<DownloadTransferGrant>(invalid).is_err());
        }
        for (field, value) in [("type", "transfer.v1"), ("direction", "upload")] {
            let mut invalid = upload.clone();
            invalid[field] = value.into();
            assert!(serde_json::from_value::<UploadTransferGrant>(invalid).is_err());
        }

        for chunk_bytes in [0, MAX_TRANSFER_CHUNK_BYTES + 1] {
            let mut invalid = grant.clone();
            invalid["chunkBytes"] = chunk_bytes.into();
            assert!(serde_json::from_value::<DownloadTransferGrant>(invalid).is_err());
        }
        let mut missing_digest = grant;
        missing_digest["info"]
            .as_object_mut()
            .unwrap()
            .remove("digest");
        assert!(serde_json::from_value::<DownloadTransferGrant>(missing_digest).is_err());
    }

    #[test]
    fn upload_chunks_match_raw_transfer_sequence() {
        let body = b"hello world";
        let chunks: Vec<&[u8]> = body.chunks(transfer_chunk_size(6).unwrap()).collect();

        assert_eq!(chunks, vec![b"hello ".as_slice(), b"world".as_slice()]);
        assert_eq!(chunks.len() as u64, 2);
    }

    fn verify_request_proof_headers(
        auth: &SessionAuth,
        subject: &str,
        reply: &str,
        payload: &[u8],
        headers: &HeaderMap,
    ) -> bool {
        let context_digest: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(TEST_CONTEXT_DIGEST)
            .expect("context digest")
            .try_into()
            .expect("context digest bytes");
        let seq = headers
            .get(TRANSFER_SEQUENCE_HEADER)
            .expect("sequence")
            .as_str()
            .parse()
            .expect("sequence integer");
        let proof_payload = crate::service::transfer_frame_proof_payload(
            seq,
            headers
                .get(TRANSFER_CONTROL_HEADER)
                .map(async_nats::HeaderValue::as_str),
            payload,
        );
        let input = build_authorization_request_proof_input(
            &context_digest,
            subject,
            Some(reply),
            &proof_payload,
            headers
                .get("iat")
                .expect("iat")
                .as_str()
                .parse()
                .expect("iat integer"),
            headers.get("request-id").expect("request-id").as_str(),
        )
        .expect("proof input");
        let public_key = VerifyingKey::from_bytes(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&auth.session_key)
                .expect("session key")
                .try_into()
                .expect("session key bytes"),
        )
        .expect("public key");
        let signature = Signature::from_bytes(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(headers.get("proof").expect("proof").as_str())
                .expect("proof bytes")
                .try_into()
                .expect("proof signature"),
        );
        public_key.verify(input.digest(), &signature).is_ok()
    }

    #[test]
    fn upload_headers_include_context_proof_sequence_and_control_kind() {
        let auth = test_auth();
        let subject = "transfer.v1.upload.test.tx1";
        let reply = "_INBOX.test.reply";
        let payload = b"hello ";

        let chunk_headers =
            upload_headers(&auth, TEST_CONTEXT_DIGEST, subject, reply, payload, 0, None)
                .expect("chunk headers");

        assert_eq!(
            chunk_headers
                .get("session-key")
                .expect("session-key")
                .as_str(),
            auth.session_key
        );
        assert_eq!(
            chunk_headers
                .get("authorization-context")
                .expect("authorization-context")
                .as_str(),
            TEST_CONTEXT_DIGEST
        );
        assert!(verify_request_proof_headers(
            &auth,
            subject,
            reply,
            payload,
            &chunk_headers
        ));
        assert_eq!(
            chunk_headers
                .get(TRANSFER_SEQUENCE_HEADER)
                .expect("sequence")
                .as_str(),
            "0"
        );
        assert!(chunk_headers.get(TRANSFER_EOF_HEADER).is_none());

        let eof_headers = upload_headers(
            &auth,
            TEST_CONTEXT_DIGEST,
            subject,
            reply,
            &[],
            2,
            Some("complete"),
        )
        .expect("control headers");

        assert_eq!(
            eof_headers
                .get(TRANSFER_SEQUENCE_HEADER)
                .expect("eof sequence")
                .as_str(),
            "2"
        );
        assert_eq!(
            eof_headers
                .get(TRANSFER_CONTROL_HEADER)
                .expect("control marker")
                .as_str(),
            "complete"
        );
        assert!(verify_request_proof_headers(
            &auth,
            subject,
            reply,
            &[],
            &eof_headers
        ));
    }

    #[test]
    fn event_proof_verifies_with_context_digest() {
        let auth = test_auth();
        let subject = "events.v1.Documents.Changed.doc-1";
        let payload = br#"{"id":"doc-1"}"#;
        let event_id = "evt_doc_1";
        let event_time = "1970-01-01T00:19:10Z";
        let proof = auth
            .create_event_proof(
                TEST_CONTEXT_DIGEST,
                "test-event-descriptor",
                subject,
                payload,
                event_id,
                event_time,
            )
            .expect("event proof");
        assert!(verify_event_proof(VerifyEventProofInput {
            public_session_key: &auth.session_key,
            context_digest: TEST_CONTEXT_DIGEST,
            descriptor_identity: "test-event-descriptor",
            subject,
            payload,
            event_id,
            event_time,
            proof_base64url: proof.as_str(),
        })
        .expect("event proof verifies"));
        assert!(!verify_event_proof(VerifyEventProofInput {
            public_session_key: &auth.session_key,
            context_digest: TEST_CONTEXT_DIGEST,
            descriptor_identity: "test-event-descriptor",
            subject,
            payload,
            event_id: "evt_other",
            event_time,
            proof_base64url: proof.as_str(),
        })
        .expect("changed event id rejects"));
    }
}
