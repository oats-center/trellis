use std::future::Future;
use std::marker::PhantomData;

use bytes::Bytes;
use futures_util::Stream;
use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncRead;

use crate::client::transfer::{FileInfo, TransferCancellation, UploadTransferGrant};
use crate::client::TrellisClientError;
use crate::live::subscription::{LiveMapDecision, LiveSubscription};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OperationState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationRefData {
    pub id: String,
    pub service: String,
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OperationSnapshot<TProgress = Value, TOutput = Value> {
    pub revision: u64,
    pub state: OperationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<TProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<OperationTransferProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<TOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationTransferProgress {
    pub chunk_index: u64,
    pub chunk_bytes: u64,
    pub transferred_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AcceptedEnvelope<TProgress = Value, TOutput = Value> {
    kind: String,
    #[serde(rename = "ref")]
    operation_ref: OperationRefData,
    snapshot: OperationSnapshot<TProgress, TOutput>,
    transfer: Option<UploadTransferGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SnapshotFrame<TProgress = Value, TOutput = Value> {
    kind: String,
    snapshot: OperationSnapshot<TProgress, TOutput>,
}

/// Acknowledgement returned after an operation signal is accepted by the provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OperationSignalAccepted<TProgress = Value, TOutput = Value> {
    pub kind: String,
    pub operation_id: String,
    pub signal: String,
    pub signal_sequence: u64,
    pub accepted_at: String,
    pub snapshot: OperationSnapshot<TProgress, TOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct OperationErrorFrame {
    kind: String,
    error: OperationControlError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct OperationControlError {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OperationEvent<TProgress = Value, TOutput = Value, TUpdate = Value> {
    Accepted {
        snapshot: OperationSnapshot<TProgress, TOutput>,
    },
    Started {
        snapshot: OperationSnapshot<TProgress, TOutput>,
    },
    Progress {
        snapshot: OperationSnapshot<TProgress, TOutput>,
    },
    Update {
        #[serde(flatten)]
        update: OperationUpdateEvent<TUpdate>,
    },
    Transfer {
        snapshot: OperationSnapshot<TProgress, TOutput>,
        transfer: OperationTransferProgress,
    },
    Completed {
        snapshot: OperationSnapshot<TProgress, TOutput>,
    },
    Failed {
        snapshot: OperationSnapshot<TProgress, TOutput>,
    },
    Cancelled {
        snapshot: OperationSnapshot<TProgress, TOutput>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct EventFrame<TProgress = Value, TOutput = Value, TUpdate = Value> {
    kind: String,
    event: OperationEvent<TProgress, TOutput, TUpdate>,
}

/// One live-only typed operation update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OperationUpdateEvent<TUpdate = Value> {
    /// Durable operation id associated with the update.
    pub operation_id: String,
    /// Monotonic live event sequence for this operation process.
    pub sequence: u64,
    /// RFC 3339 publication timestamp.
    pub timestamp: String,
    /// Cumulative contract-defined update payload.
    pub update: TUpdate,
}

/// Compile-time evidence that an operation does not declare live updates.
pub struct NoOperationUpdates;

/// Compile-time evidence that an operation declares live updates.
pub struct DeclaredOperationUpdates;

/// Evidence carried by an operation descriptor's update type.
pub trait OperationUpdateEvidence: Send + 'static {}

impl OperationUpdateEvidence for NoOperationUpdates {}
impl OperationUpdateEvidence for DeclaredOperationUpdates {}

/// Evidence required by APIs that publish or decode declared live updates.
pub trait HasOperationUpdates: OperationUpdateEvidence {}

impl HasOperationUpdates for DeclaredOperationUpdates {}

/// Static operation contract metadata shared by caller and service APIs.
pub trait OperationDescriptor {
    type Input: Serialize;
    type Progress: DeserializeOwned + Send + 'static;
    type Output: DeserializeOwned + Send + 'static;
    /// Contract-defined live update payload, or `Value` when updates are not declared.
    type Update: Serialize + DeserializeOwned + Send + 'static;
    /// Whether this descriptor authored a live update schema.
    type UpdateEvidence: OperationUpdateEvidence;
    type Error: Send + 'static;

    const API_ID: &'static str = "";
    const KEY: &'static str;
    const SUBJECT: &'static str;
    const CALLER_CAPABILITIES: &'static [&'static str];
    const OBSERVE_CAPABILITIES: &'static [&'static str];
    const CANCEL_CAPABILITIES: &'static [&'static str];
    const CONTROL_CAPABILITIES: &'static [&'static str] = &[];
    const CANCELABLE: bool;
    /// Whether the operation requires a runtime-owned upload before handler execution.
    const UPLOAD: bool = false;
    const ERRORS: &'static [&'static str] = &[];

    const INPUT_SCHEMA_JSON: &'static str;
    const PROGRESS_SCHEMA_JSON: Option<&'static str>;
    const OUTPUT_SCHEMA_JSON: &'static str;
    /// JSON Schema for declared live updates.
    const UPDATE_SCHEMA_JSON: Option<&'static str>;
    const SIGNAL_INPUT_SCHEMAS_JSON: &'static str;
}

/// Marker trait for operations that declare an upload transfer.
pub trait TransferOperationDescriptor: OperationDescriptor {}

#[doc(hidden)]
pub trait OperationTransport {
    fn operation_subject(
        &self,
        _api_id: &str,
        _operation: &str,
        subject: &str,
    ) -> Result<String, TrellisClientError> {
        Ok(subject.to_string())
    }

    fn request_json_value<'a>(
        &'a self,
        subject: String,
        body: Value,
    ) -> impl Future<Output = Result<Value, TrellisClientError>> + Send + 'a;

    fn put_upload_transfer<'a>(
        &'a self,
        grant: UploadTransferGrant,
        body: Vec<u8>,
    ) -> impl Future<Output = Result<FileInfo, TrellisClientError>> + Send + 'a;

    fn put_upload_transfer_from<'a, R>(
        &'a self,
        grant: UploadTransferGrant,
        reader: &'a mut R,
        expected_size: Option<u64>,
    ) -> impl Future<Output = Result<FileInfo, TrellisClientError>> + Send + 'a
    where
        R: AsyncRead + Unpin + Send + ?Sized + 'a;

    fn put_upload_transfer_from_with_cancel<'a, R>(
        &'a self,
        grant: UploadTransferGrant,
        reader: &'a mut R,
        expected_size: Option<u64>,
        cancellation: &'a TransferCancellation,
    ) -> impl Future<Output = Result<FileInfo, TrellisClientError>> + Send + 'a
    where
        R: AsyncRead + Unpin + Send + ?Sized + 'a;
}

#[derive(Debug)]
pub struct OperationInvoker<'a, T, D> {
    transport: &'a T,
    _descriptor: PhantomData<D>,
}

/// Builder for operation calls with a captured input payload.
#[derive(Debug)]
pub struct OperationInputBuilder<'a, 'b, T, D: OperationDescriptor> {
    invoker: &'b OperationInvoker<'a, T, D>,
    input: &'b D::Input,
}

/// Builder for operation calls that upload bytes after the operation is accepted.
#[derive(Debug)]
pub struct OperationTransferInputBuilder<'a, 'b, T, D: OperationDescriptor> {
    invoker: &'b OperationInvoker<'a, T, D>,
    input: &'b D::Input,
    body: Vec<u8>,
}

/// Builder for operation calls that stream upload bytes after acceptance.
#[derive(Debug)]
pub struct OperationTransferReaderInputBuilder<'a, 'b, T, D: OperationDescriptor, R: ?Sized> {
    invoker: &'b OperationInvoker<'a, T, D>,
    input: &'b D::Input,
    reader: &'b mut R,
    expected_size: Option<u64>,
}

/// Successful result for starting an operation and uploading its transfer body.
pub struct StartedOperationTransfer<'a, T, D> {
    operation_ref: OperationRef<'a, T, D>,
    file_info: FileInfo,
}

/// Error returned when starting or uploading an operation transfer fails.
pub enum OperationTransferStartError<'a, T, D> {
    Start(TrellisClientError),
    Upload {
        operation_ref: Box<OperationRef<'a, T, D>>,
        source: TrellisClientError,
    },
}

impl<'a, T, D> std::fmt::Debug for StartedOperationTransfer<'a, T, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedOperationTransfer")
            .field("operation_ref", &self.operation_ref)
            .field("file_info", &self.file_info)
            .finish()
    }
}

impl<'a, T, D> std::fmt::Debug for OperationTransferStartError<'a, T, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(source) => f.debug_tuple("Start").field(source).finish(),
            Self::Upload {
                operation_ref,
                source,
            } => f
                .debug_struct("Upload")
                .field("operation_ref", operation_ref)
                .field("source", source)
                .finish(),
        }
    }
}

pub struct OperationRef<'a, T, D> {
    transport: &'a T,
    data: OperationRefData,
    accepted_transfer: Option<UploadTransferGrant>,
    _descriptor: PhantomData<D>,
}

impl<'a, T, D> std::fmt::Debug for OperationRef<'a, T, D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperationRef")
            .field("data", &self.data)
            .field("accepted_transfer", &self.accepted_transfer)
            .finish_non_exhaustive()
    }
}

impl<'a, T, D> OperationInvoker<'a, T, D> {
    /// Create a typed operation reference for an existing operation id.
    ///
    /// This does not send a start request or run the operation handler. Follow-up
    /// methods on the returned reference use the descriptor-derived control
    /// subject and preserve the descriptor's progress and output types.
    pub fn control(
        &self,
        operation_id: impl Into<String>,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError>
    where
        D: OperationDescriptor,
    {
        let operation_id = operation_id.into();
        if operation_id.trim().is_empty() {
            return Err(TrellisClientError::OperationProtocol(
                "operation id must not be empty".to_string(),
            ));
        }

        Ok(OperationRef {
            transport: self.transport,
            data: OperationRefData {
                id: operation_id,
                service: String::new(),
                operation: D::KEY.to_string(),
            },
            accepted_transfer: None,
            _descriptor: PhantomData,
        })
    }

    pub fn new(transport: &'a T) -> Self {
        Self {
            transport,
            _descriptor: PhantomData,
        }
    }

    /// Captures an operation input for ergonomic chained calls.
    pub fn input<'b>(&'b self, input: &'b D::Input) -> OperationInputBuilder<'a, 'b, T, D>
    where
        D: OperationDescriptor,
    {
        OperationInputBuilder {
            invoker: self,
            input,
        }
    }
}

impl<'a, T, D> OperationInvoker<'a, T, D>
where
    T: OperationTransport,
    D: OperationDescriptor,
    D::Progress: Send,
    D::Output: Send,
{
    pub async fn start(
        &self,
        input: &D::Input,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        self.start_with_invocation_id(ulid::Ulid::new().to_string(), input)
            .await
    }

    /// Start or replay an operation with a caller-selected ULID idempotency key.
    pub async fn start_with_invocation_id(
        &self,
        invocation_id: impl Into<String>,
        input: &D::Input,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        let invocation_id = invocation_id.into();
        invocation_id.parse::<ulid::Ulid>().map_err(|_| {
            TrellisClientError::OperationProtocol(
                "operation invocation id must be a ULID".to_owned(),
            )
        })?;
        let body = serde_json::to_value(input)?;
        validate_operation_schema(D::INPUT_SCHEMA_JSON, &body, "operation input")?;
        self.start_encoded(serde_json::json!({
            "invocationId": invocation_id,
            "input": body,
        }))
        .await
    }

    pub(crate) async fn start_encoded(
        &self,
        body: Value,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        let response = self
            .transport
            .request_json_value(
                self.transport
                    .operation_subject(D::API_ID, D::KEY, D::SUBJECT)?,
                body,
            )
            .await?;
        validate_snapshot_at::<D>(&response, "/snapshot")?;
        let accepted: AcceptedEnvelope<D::Progress, D::Output> = serde_json::from_value(response)?;
        if accepted.kind != "accepted" {
            return Err(TrellisClientError::OperationProtocol(format!(
                "expected accepted envelope, got '{}'",
                accepted.kind
            )));
        }
        Ok(OperationRef {
            transport: self.transport,
            data: accepted.operation_ref,
            accepted_transfer: accepted.transfer,
            _descriptor: PhantomData,
        })
    }
}

impl<'a, 'b, T, D> OperationInputBuilder<'a, 'b, T, D>
where
    T: OperationTransport,
    D: OperationDescriptor,
    D::Progress: Send,
    D::Output: Send,
{
    /// Starts the operation with the captured input.
    pub async fn start(self) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        self.invoker.start(self.input).await
    }

    /// Captures upload bytes to send after the operation is accepted.
    pub fn transfer(self, body: impl AsRef<[u8]>) -> OperationTransferInputBuilder<'a, 'b, T, D>
    where
        D: TransferOperationDescriptor,
    {
        OperationTransferInputBuilder {
            invoker: self.invoker,
            input: self.input,
            body: body.as_ref().to_vec(),
        }
    }

    /// Captures a borrowed asynchronous reader to stream after operation acceptance.
    pub fn transfer_from<R>(
        self,
        reader: &'b mut R,
        expected_size: Option<u64>,
    ) -> OperationTransferReaderInputBuilder<'a, 'b, T, D, R>
    where
        D: TransferOperationDescriptor,
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        OperationTransferReaderInputBuilder {
            invoker: self.invoker,
            input: self.input,
            reader,
            expected_size,
        }
    }
}

impl<'a, 'b, T, D> OperationTransferInputBuilder<'a, 'b, T, D>
where
    T: OperationTransport,
    D: TransferOperationDescriptor,
    D::Progress: Send,
    D::Output: Send,
{
    /// Starts the operation, uploads the captured bytes, and returns the operation and file info.
    pub async fn start(
        self,
    ) -> Result<StartedOperationTransfer<'a, T, D>, OperationTransferStartError<'a, T, D>> {
        let operation_ref = self
            .invoker
            .start(self.input)
            .await
            .map_err(OperationTransferStartError::Start)?;
        let file_info = match operation_ref.transfer_vec(self.body).await {
            Ok(file_info) => file_info,
            Err(source) => {
                return Err(OperationTransferStartError::Upload {
                    operation_ref: Box::new(operation_ref),
                    source,
                })
            }
        };
        Ok(StartedOperationTransfer {
            operation_ref,
            file_info,
        })
    }
}

impl<'a, 'b, T, D, R> OperationTransferReaderInputBuilder<'a, 'b, T, D, R>
where
    T: OperationTransport,
    D: TransferOperationDescriptor,
    D::Progress: Send,
    D::Output: Send,
    R: AsyncRead + Unpin + Send + ?Sized,
{
    /// Starts the operation, streams the reader, and returns the operation and file info.
    pub async fn start(
        self,
    ) -> Result<StartedOperationTransfer<'a, T, D>, OperationTransferStartError<'a, T, D>> {
        let operation_ref = self
            .invoker
            .start(self.input)
            .await
            .map_err(OperationTransferStartError::Start)?;
        let file_info = match operation_ref
            .transfer_from(self.reader, self.expected_size)
            .await
        {
            Ok(file_info) => file_info,
            Err(source) => {
                return Err(OperationTransferStartError::Upload {
                    operation_ref: Box::new(operation_ref),
                    source,
                })
            }
        };
        Ok(StartedOperationTransfer {
            operation_ref,
            file_info,
        })
    }
}

impl<'a, T, D> StartedOperationTransfer<'a, T, D> {
    /// Return the accepted operation reference.
    pub fn operation_ref(&self) -> &OperationRef<'a, T, D> {
        &self.operation_ref
    }

    /// Return information about the uploaded transfer body.
    pub fn file_info(&self) -> &FileInfo {
        &self.file_info
    }

    /// Consume the result and return the accepted operation reference.
    pub fn into_operation_ref(self) -> OperationRef<'a, T, D> {
        self.operation_ref
    }
}

impl<'a, T, D> OperationTransferStartError<'a, T, D> {
    /// Return the accepted operation reference when the operation was accepted before upload failed.
    pub fn operation_ref(&self) -> Option<&OperationRef<'a, T, D>> {
        match self {
            Self::Start(_) => None,
            Self::Upload { operation_ref, .. } => Some(operation_ref),
        }
    }

    /// Return the underlying client error.
    pub fn source(&self) -> &TrellisClientError {
        match self {
            Self::Start(source) | Self::Upload { source, .. } => source,
        }
    }
}

impl<'a, T, D> OperationRef<'a, T, D> {
    /// Return the durable operation id.
    pub fn id(&self) -> &str {
        &self.data.id
    }

    /// Return the owning service name when known from the accepted envelope.
    ///
    /// References resumed with [`OperationInvoker::control`] are scoped by the
    /// typed descriptor and operation id, so this value is empty until the
    /// runtime receives service metadata from a start response.
    pub fn service(&self) -> &str {
        &self.data.service
    }

    /// Return the operation key for this typed reference.
    pub fn operation(&self) -> &str {
        &self.data.operation
    }
}

impl<'a, T, D> OperationRef<'a, T, D>
where
    T: OperationTransport,
    D: OperationDescriptor,
{
    pub async fn get(
        &self,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, TrellisClientError> {
        let body = json!({
            "action": "get",
            "operationId": self.id(),
        });
        let response = self
            .transport
            .request_json_value(
                control_subject(&self.transport.operation_subject(
                    D::API_ID,
                    D::KEY,
                    D::SUBJECT,
                )?),
                body,
            )
            .await?;
        decode_snapshot_response::<D>(response)
    }

    pub async fn cancel(
        &self,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, TrellisClientError> {
        if !D::CANCELABLE {
            return Err(TrellisClientError::OperationProtocol(
                "operation is not cancelable".to_owned(),
            ));
        }
        let body = json!({
            "action": "cancel",
            "operationId": self.id(),
        });
        let subject = control_subject(&self.transport.operation_subject(
            D::API_ID,
            D::KEY,
            D::SUBJECT,
        )?);
        loop {
            let response = self
                .transport
                .request_json_value(subject.clone(), body.clone())
                .await?;
            let snapshot = decode_snapshot_response::<D>(response)?;
            if matches!(
                snapshot.state,
                OperationState::Completed | OperationState::Failed | OperationState::Cancelled
            ) {
                return Ok(snapshot);
            }
            // ponytail: retry with cancel authority; observe is a separate grant.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// Send a control signal to the running operation.
    pub async fn signal(
        &self,
        signal: impl Into<String>,
        input: Option<Value>,
    ) -> Result<OperationSignalAccepted<D::Progress, D::Output>, TrellisClientError> {
        let signal = signal.into();
        let signal_schemas: serde_json::Map<String, Value> =
            serde_json::from_str(D::SIGNAL_INPUT_SCHEMAS_JSON)?;
        let schema = signal_schemas.get(&signal).ok_or_else(|| {
            TrellisClientError::OperationProtocol(format!("undeclared operation signal '{signal}'"))
        })?;
        validate_operation_schema(
            &serde_json::to_string(schema)?,
            input.as_ref().unwrap_or(&Value::Null),
            "operation signal input",
        )?;
        let mut body = json!({
            "action": "signal",
            "operationId": self.id(),
            "signal": signal,
        });
        if let Some(input) = input {
            body["input"] = input;
        }
        self.send_signal(body).await
    }

    pub(crate) async fn signal_encoded(
        &self,
        signal: String,
        input: Option<Value>,
    ) -> Result<OperationSignalAccepted<D::Progress, D::Output>, TrellisClientError> {
        let mut body = json!({
            "action": "signal",
            "operationId": self.id(),
            "signal": signal,
        });
        if let Some(input) = input {
            body["input"] = input;
        }
        self.send_signal(body).await
    }

    async fn send_signal(
        &self,
        body: Value,
    ) -> Result<OperationSignalAccepted<D::Progress, D::Output>, TrellisClientError> {
        let response = self
            .transport
            .request_json_value(
                control_subject(&self.transport.operation_subject(
                    D::API_ID,
                    D::KEY,
                    D::SUBJECT,
                )?),
                body,
            )
            .await?;
        decode_signal_response::<D>(response)
    }

    pub async fn transfer(&self, body: impl AsRef<[u8]>) -> Result<FileInfo, TrellisClientError> {
        self.transfer_vec(body.as_ref().to_vec()).await
    }

    /// Upload a transfer from a borrowed asynchronous reader after this operation is accepted.
    pub async fn transfer_from<R>(
        &self,
        reader: &mut R,
        expected_size: Option<u64>,
    ) -> Result<FileInfo, TrellisClientError>
    where
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        let grant = self.accepted_transfer.clone().ok_or_else(|| {
            TrellisClientError::OperationProtocol(
                "operation does not have an accepted transfer session".into(),
            )
        })?;
        self.transport
            .put_upload_transfer_from(grant, reader, expected_size)
            .await
    }

    /// Upload from a borrowed reader and authenticate cancellation when requested.
    pub async fn transfer_from_with_cancel<R>(
        &self,
        reader: &mut R,
        expected_size: Option<u64>,
        cancellation: &TransferCancellation,
    ) -> Result<FileInfo, TrellisClientError>
    where
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        let grant = self.accepted_transfer.clone().ok_or_else(|| {
            TrellisClientError::OperationProtocol(
                "operation does not have an accepted transfer session".into(),
            )
        })?;
        self.transport
            .put_upload_transfer_from_with_cancel(grant, reader, expected_size, cancellation)
            .await
    }

    async fn transfer_vec(&self, body: Vec<u8>) -> Result<FileInfo, TrellisClientError> {
        let grant = self.accepted_transfer.clone().ok_or_else(|| {
            TrellisClientError::OperationProtocol(
                "operation does not have an accepted transfer session".into(),
            )
        })?;
        self.transport.put_upload_transfer(grant, body).await
    }
}

impl<'a, D> OperationRef<'a, super::TrellisClient, D>
where
    D: OperationDescriptor,
{
    /// Watch until a terminal snapshot, then dispose the observer.
    ///
    /// Business `failed` and `cancelled` snapshots are returned as success.
    /// Transport failures stay as client errors.
    pub async fn wait(
        &self,
    ) -> Result<OperationSnapshot<D::Progress, D::Output>, TrellisClientError> {
        let mut events = self.live().await?;
        let result = wait_for_terminal_snapshot(&mut events).await;
        let _ = events.close().await;
        result
    }

    /// Open a live Operation observation without declared update envelopes.
    pub async fn live(
        &self,
    ) -> Result<LiveSubscription<OperationEvent<D::Progress, D::Output, Value>>, TrellisClientError>
    {
        self.open_operation_watch::<Value>(false, None).await
    }

    /// Observe durable lifecycle events plus declared live-only updates.
    pub async fn live_with_updates(
        &self,
    ) -> Result<
        LiveSubscription<OperationEvent<D::Progress, D::Output, D::Update>>,
        TrellisClientError,
    >
    where
        D::UpdateEvidence: HasOperationUpdates,
    {
        let update_schema = D::UPDATE_SCHEMA_JSON.ok_or_else(|| {
            TrellisClientError::OperationProtocol("operation does not declare live updates".into())
        })?;
        self.open_operation_watch::<D::Update>(true, Some(update_schema))
            .await
    }

    /// Subscribe to declared live-only updates until the operation becomes terminal.
    pub async fn updates(
        &self,
    ) -> Result<
        impl Stream<Item = Result<OperationUpdateEvent<D::Update>, TrellisClientError>> + Unpin,
        TrellisClientError,
    >
    where
        D::UpdateEvidence: HasOperationUpdates,
    {
        let events = self.live_with_updates().await?;
        Ok(events.map_items(|event| match event {
            OperationEvent::Update { update } => LiveMapDecision::Emit(update),
            event if is_terminal_event(&event) => LiveMapDecision::Complete,
            _ => LiveMapDecision::Skip,
        }))
    }

    async fn open_operation_watch<TUpdate>(
        &self,
        include_updates: bool,
        update_schema_json: Option<&'static str>,
    ) -> Result<LiveSubscription<OperationEvent<D::Progress, D::Output, TUpdate>>, TrellisClientError>
    where
        TUpdate: DeserializeOwned + Send + 'static,
    {
        let base_subject = self
            .transport
            .operation_subject(D::API_ID, D::KEY, D::SUBJECT)?;
        let publish_subject = control_subject(&base_subject);
        let open_id = trellis_protocol::generate_nonce()
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
        let receive_max_payload_bytes = self.transport.nats().max_payload() as u64;
        let body = operation_watch_open_value(
            self.id(),
            include_updates,
            &open_id,
            receive_max_payload_bytes,
        );
        let action_name = D::KEY.split_once('.').map_or(D::KEY, |(_, action)| action);
        let permission = trellis_protocol::PermissionAtom::new(
            trellis_protocol::PermissionTarget::api_surface(
                D::API_ID,
                trellis_protocol::ApiSurfaceKind::Operation,
                action_name.to_owned(),
            )
            .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?,
            trellis_protocol::PermissionAction::Observe,
        )
        .map_err(|error| TrellisClientError::LiveProtocol(error.to_string()))?;
        let open = crate::live::client_open::ClientOpen {
            kind: trellis_protocol::LiveSessionKind::Operation,
            api_id: D::API_ID,
            base_subject: &base_subject,
            publish_subject: &publish_subject,
            body: Bytes::from(serde_json::to_vec(&body)?),
            open_id,
            receive_max_payload_bytes,
            permission,
        };
        let prepared = crate::live::client_open::open_client_session(
            self.transport,
            self.transport.authorization_provider(),
            open,
        )
        .await?;
        let progress_schema = D::PROGRESS_SCHEMA_JSON;
        let output_schema = D::OUTPUT_SCHEMA_JSON;
        crate::live::client_open::install_operation_watch_handle(
            self.transport,
            prepared,
            move |value| {
                decode_watch_frame::<D::Progress, TUpdate, D::Output>(
                    value,
                    progress_schema,
                    update_schema_json,
                    output_schema,
                )
            },
        )
        .await
    }
}

fn operation_watch_open_value(
    operation_id: &str,
    include_updates: bool,
    open_id: &str,
    receive_max_payload_bytes: u64,
) -> Value {
    let mut body = json!({
        "action": "watch",
        "operationId": operation_id,
        "observation": {
            "format": trellis_protocol::LIVE_VERSION,
            "type": "open",
            "openId": open_id,
            "receiveMaxPayloadBytes": receive_max_payload_bytes,
        }
    });
    if include_updates {
        body["includeUpdates"] = json!(true);
    }
    body
}

async fn wait_for_terminal_snapshot<S, TProgress, TOutput, TUpdate>(
    events: &mut S,
) -> Result<OperationSnapshot<TProgress, TOutput>, TrellisClientError>
where
    S: Stream<Item = Result<OperationEvent<TProgress, TOutput, TUpdate>, TrellisClientError>>
        + Unpin,
{
    while let Some(event) = events.next().await {
        match event? {
            OperationEvent::Completed { snapshot }
            | OperationEvent::Failed { snapshot }
            | OperationEvent::Cancelled { snapshot } => return Ok(snapshot),
            _ => {}
        }
    }
    Err(TrellisClientError::OperationProtocol(
        "operation watch ended before a terminal snapshot".to_string(),
    ))
}

fn decode_watch_frame<
    TProgress: DeserializeOwned,
    TUpdate: DeserializeOwned,
    TOutput: DeserializeOwned,
>(
    value: Value,
    progress_schema_json: Option<&str>,
    update_schema_json: Option<&str>,
    output_schema_json: &str,
) -> Result<Option<OperationEvent<TProgress, TOutput, TUpdate>>, TrellisClientError> {
    if value.get("kind").and_then(Value::as_str) == Some("keepalive") {
        return Ok(None);
    }

    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        TrellisClientError::OperationProtocol("expected watch frame kind".to_string())
    })?;

    match kind {
        "snapshot" => {
            validate_snapshot_value(
                value.pointer("/snapshot"),
                progress_schema_json,
                output_schema_json,
            )?;
            let frame: SnapshotFrame<TProgress, TOutput> = serde_json::from_value(value)?;
            Ok(Some(snapshot_to_event(frame.snapshot)))
        }
        "event" => {
            validate_snapshot_value(
                value.pointer("/event/snapshot"),
                progress_schema_json,
                output_schema_json,
            )?;
            if value.pointer("/event/type").and_then(Value::as_str) == Some("update") {
                let update = value.pointer("/event/update").ok_or_else(|| {
                    TrellisClientError::OperationProtocol(
                        "operation update event is missing its payload".to_string(),
                    )
                })?;
                let schema_json = update_schema_json.ok_or_else(|| {
                    TrellisClientError::OperationProtocol(
                        "received an undeclared operation update".to_string(),
                    )
                })?;
                validate_update_schema(schema_json, update)?;
            }
            let frame: EventFrame<TProgress, TOutput, TUpdate> = serde_json::from_value(value)?;
            Ok(Some(frame.event))
        }
        "error" => Err(operation_error_frame(value)),
        _ => Err(TrellisClientError::OperationProtocol(
            "expected snapshot/event/keepalive frame".to_string(),
        )),
    }
}

fn decode_snapshot_response<D: OperationDescriptor>(
    value: Value,
) -> Result<OperationSnapshot<D::Progress, D::Output>, TrellisClientError> {
    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        TrellisClientError::OperationProtocol("expected control frame kind".to_string())
    })?;

    match kind {
        "snapshot" => {
            validate_snapshot_at::<D>(&value, "/snapshot")?;
            let frame: SnapshotFrame<D::Progress, D::Output> = serde_json::from_value(value)?;
            Ok(frame.snapshot)
        }
        "error" => Err(operation_error_frame(value)),
        _ => Err(TrellisClientError::OperationProtocol(format!(
            "expected snapshot frame, got '{kind}'"
        ))),
    }
}

fn decode_signal_response<D: OperationDescriptor>(
    value: Value,
) -> Result<OperationSignalAccepted<D::Progress, D::Output>, TrellisClientError> {
    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        TrellisClientError::OperationProtocol("expected signal frame kind".to_string())
    })?;

    match kind {
        "signal-accepted" => {
            validate_snapshot_at::<D>(&value, "/snapshot")?;
            Ok(serde_json::from_value(value)?)
        }
        "error" => Err(operation_error_frame(value)),
        _ => Err(TrellisClientError::OperationProtocol(format!(
            "expected signal-accepted frame, got '{kind}'"
        ))),
    }
}

fn operation_error_frame(value: Value) -> TrellisClientError {
    match serde_json::from_value::<OperationErrorFrame>(value) {
        Ok(frame) => TrellisClientError::OperationProtocol(format!(
            "{}: {}",
            frame.error.error_type, frame.error.message
        )),
        Err(error) => TrellisClientError::Json(error),
    }
}

fn snapshot_to_event<TProgress, TUpdate, TOutput>(
    snapshot: OperationSnapshot<TProgress, TOutput>,
) -> OperationEvent<TProgress, TOutput, TUpdate> {
    match snapshot.state {
        OperationState::Pending => OperationEvent::Accepted { snapshot },
        OperationState::Running => OperationEvent::Started { snapshot },
        OperationState::Completed => OperationEvent::Completed { snapshot },
        OperationState::Failed => OperationEvent::Failed { snapshot },
        OperationState::Cancelled => OperationEvent::Cancelled { snapshot },
    }
}

fn is_terminal_event<TProgress, TUpdate, TOutput>(
    event: &OperationEvent<TProgress, TOutput, TUpdate>,
) -> bool {
    matches!(
        event,
        OperationEvent::Completed { .. }
            | OperationEvent::Failed { .. }
            | OperationEvent::Cancelled { .. }
    )
}

fn validate_update_schema(schema_json: &str, update: &Value) -> Result<(), TrellisClientError> {
    let schema: Value = serde_json::from_str(schema_json)?;
    let validator = jsonschema::validator_for(&schema).map_err(|error| {
        TrellisClientError::OperationProtocol(format!(
            "failed to compile operation update schema: {error}"
        ))
    })?;
    if let Some(error) = validator.iter_errors(update).next() {
        return Err(TrellisClientError::OperationProtocol(format!(
            "operation update failed schema validation: {error}"
        )));
    }
    Ok(())
}

fn validate_snapshot_at<D: OperationDescriptor>(
    value: &Value,
    pointer: &str,
) -> Result<(), TrellisClientError> {
    validate_snapshot_value(
        value.pointer(pointer),
        D::PROGRESS_SCHEMA_JSON,
        D::OUTPUT_SCHEMA_JSON,
    )
}

fn validate_snapshot_value(
    snapshot: Option<&Value>,
    progress_schema_json: Option<&str>,
    output_schema_json: &str,
) -> Result<(), TrellisClientError> {
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    if let (Some(schema), Some(progress)) = (progress_schema_json, snapshot.get("progress")) {
        if !progress.is_null() {
            validate_operation_schema(schema, progress, "operation progress")?;
        }
    }
    if let Some(output) = snapshot.get("output") {
        if !output.is_null() {
            validate_operation_schema(output_schema_json, output, "operation output")?;
        }
    }
    Ok(())
}

fn validate_operation_schema(
    schema_json: &str,
    value: &Value,
    label: &str,
) -> Result<(), TrellisClientError> {
    crate::service::validate_input_schema(schema_json, value).map_err(|error| {
        TrellisClientError::OperationProtocol(format!("{label} failed schema validation: {error}"))
    })
}

#[cfg(test)]
mod update_tests {
    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Update {
        processed: u64,
    }

    #[test]
    fn decodes_and_validates_typed_update_frame() {
        let event = decode_watch_frame::<Value, Update, Value>(
            json!({
                "kind": "event",
                "sequence": 2,
                "event": {
                    "type": "update",
                    "operationId": "op-1",
                    "sequence": 2,
                    "timestamp": "2026-07-10T12:00:00Z",
                    "update": { "processed": 3 }
                }
            }),
            None,
            Some(r#"{"type":"object","required":["processed"],"properties":{"processed":{"type":"integer"}}}"#),
            "{}",
        )
        .expect("decode update frame")
        .expect("event frame");

        assert!(matches!(
            event,
            OperationEvent::Update { update } if update.update.processed == 3
        ));
    }
}

pub fn control_subject(subject: &str) -> String {
    format!("{subject}.control")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_util::stream;
    use futures_util::StreamExt;
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use tokio::io::AsyncRead;

    use super::{
        control_subject, decode_watch_frame, operation_watch_open_value,
        wait_for_terminal_snapshot, FileInfo, OperationDescriptor, OperationEvent,
        OperationInvoker, OperationSignalAccepted, OperationTransferProgress, OperationTransport,
        TransferCancellation, TransferOperationDescriptor, UploadTransferGrant,
    };
    use crate::client::TrellisClientError;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct RefundInput {
        charge_id: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct RefundProgress {
        message: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct RefundOutput {
        refund_id: String,
    }

    struct RefundOperation;

    impl OperationDescriptor for RefundOperation {
        type Input = RefundInput;
        type Progress = RefundProgress;
        type Output = RefundOutput;
        type Update = Value;
        type UpdateEvidence = super::NoOperationUpdates;
        type Error = String;

        const KEY: &'static str = "Billing.Refund";
        const SUBJECT: &'static str = "operations.v1.Billing.Refund";
        const CALLER_CAPABILITIES: &'static [&'static str] = &["billing.refund"];
        const OBSERVE_CAPABILITIES: &'static [&'static str] = &["billing.read"];
        const CANCEL_CAPABILITIES: &'static [&'static str] = &["billing.cancel"];
        const CANCELABLE: bool = true;
        const ERRORS: &'static [&'static str] = &[];
        const INPUT_SCHEMA_JSON: &'static str =
            r#"{"type":"object","properties":{},"required":[]}"#;
        const PROGRESS_SCHEMA_JSON: Option<&'static str> = None;
        const OUTPUT_SCHEMA_JSON: &'static str =
            r#"{"type":"object","properties":{},"required":[]}"#;
        const UPDATE_SCHEMA_JSON: Option<&'static str> = None;
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = r#"{"selectWorkspace":{"type":"object","required":["workspaceId"],"properties":{"workspaceId":{"type":"string"}}}}"#;
    }

    impl TransferOperationDescriptor for RefundOperation {}

    #[derive(Debug, Default)]
    struct RecordingTransport {
        requests: Mutex<Vec<(String, Value)>>,
        responses: Mutex<Vec<Value>>,
    }

    impl RecordingTransport {
        fn with_responses(responses: Vec<Value>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses),
            }
        }

        fn requests(&self) -> Vec<(String, Value)> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    impl OperationTransport for RecordingTransport {
        async fn request_json_value(
            &self,
            subject: String,
            body: Value,
        ) -> Result<Value, TrellisClientError> {
            self.requests
                .lock()
                .expect("requests lock")
                .push((subject, body));
            let response = self.responses.lock().expect("responses lock").remove(0);
            Ok(response)
        }

        async fn put_upload_transfer(
            &self,
            _grant: UploadTransferGrant,
            _body: Vec<u8>,
        ) -> Result<FileInfo, TrellisClientError> {
            Err(TrellisClientError::TransferProtocol(
                "not implemented in test transport".to_string(),
            ))
        }

        async fn put_upload_transfer_from<'a, R>(
            &'a self,
            _grant: UploadTransferGrant,
            _reader: &'a mut R,
            _expected_size: Option<u64>,
        ) -> Result<FileInfo, TrellisClientError>
        where
            R: AsyncRead + Unpin + Send + ?Sized + 'a,
        {
            Err(TrellisClientError::OperationProtocol(
                "recording transport does not stream transfers".to_string(),
            ))
        }

        async fn put_upload_transfer_from_with_cancel<'a, R>(
            &'a self,
            _grant: UploadTransferGrant,
            _reader: &'a mut R,
            _expected_size: Option<u64>,
            _cancellation: &'a TransferCancellation,
        ) -> Result<FileInfo, TrellisClientError>
        where
            R: AsyncRead + Unpin + Send + ?Sized + 'a,
        {
            Err(TrellisClientError::OperationProtocol(
                "recording transport does not stream transfers".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn control_by_operation_id_uses_typed_control_subject_without_starting() {
        let transport = RecordingTransport::with_responses(vec![json!({
            "kind": "snapshot",
            "snapshot": {
                "revision": 7,
                "state": "running",
                "progress": { "message": "job resumed" }
            }
        })]);
        let invoker = OperationInvoker::<_, RefundOperation>::new(&transport);

        let operation = invoker
            .control("op_resumed")
            .expect("operation id is valid");
        let snapshot = operation.get().await.expect("get succeeds");

        assert_eq!(operation.id(), "op_resumed");
        assert_eq!(operation.operation(), "Billing.Refund");
        assert_eq!(snapshot.revision, 7);
        assert_eq!(
            snapshot.progress,
            Some(RefundProgress {
                message: "job resumed".to_string(),
            })
        );
        assert_eq!(
            transport.requests(),
            vec![(
                control_subject(RefundOperation::SUBJECT),
                json!({ "action": "get", "operationId": "op_resumed" })
            )]
        );
    }

    #[test]
    fn control_by_operation_id_rejects_empty_id_as_result_error() {
        let transport = RecordingTransport::default();
        let invoker = OperationInvoker::<_, RefundOperation>::new(&transport);

        let error = invoker.control("   ").expect_err("empty id is rejected");

        assert!(matches!(error, TrellisClientError::OperationProtocol(_)));
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn resumed_operation_reference_preserves_typed_output() {
        let frame = decode_watch_frame::<RefundProgress, Value, RefundOutput>(
            json!({
                "kind": "snapshot",
                "snapshot": {
                    "revision": 8,
                    "state": "completed",
                    "output": { "refund_id": "rf_resumed" }
                }
            }),
            None,
            None,
            RefundOperation::OUTPUT_SCHEMA_JSON,
        )
        .expect("decode snapshot")
        .expect("event");
        let mut events = stream::iter([Ok(frame)]);

        let snapshot = wait_for_terminal_snapshot(&mut events)
            .await
            .expect("wait succeeds");

        assert_eq!(
            snapshot.output,
            Some(RefundOutput {
                refund_id: "rf_resumed".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn resumed_transfer_attempt_returns_result_error_without_payload_mutation() {
        let transport = RecordingTransport::default();
        let invoker = OperationInvoker::<_, RefundOperation>::new(&transport);

        let error = invoker
            .control("op_transfer")
            .expect("operation id is valid")
            .transfer(Vec::new())
            .await
            .expect_err("resumed refs do not carry accepted transfer grants");

        assert!(matches!(error, TrellisClientError::OperationProtocol(_)));
        assert!(transport.requests().is_empty());

        let _ = OperationTransferProgress {
            chunk_index: 0,
            chunk_bytes: 0,
            transferred_bytes: 0,
        };
    }

    #[tokio::test]
    async fn control_error_frame_returns_result_error_for_invalid_operation_state() {
        let transport = RecordingTransport::with_responses(vec![json!({
            "kind": "error",
            "error": {
                "type": "TerminalOperation",
                "message": "operation is already terminal"
            }
        })]);
        let invoker = OperationInvoker::<_, RefundOperation>::new(&transport);

        let error = invoker
            .control("op_done")
            .expect("operation id is valid")
            .cancel()
            .await
            .expect_err("terminal control returns expected error");

        match error {
            TrellisClientError::OperationProtocol(message) => {
                assert!(message.contains("TerminalOperation"));
                assert!(message.contains("already terminal"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn signal_sends_control_signal_and_decodes_ack() {
        let transport = RecordingTransport::with_responses(vec![json!({
            "kind": "signal-accepted",
            "operationId": "op_signal",
            "signal": "selectWorkspace",
            "signalSequence": 1,
            "acceptedAt": "2026-05-15T00:00:00Z",
            "snapshot": {
                "revision": 2,
                "state": "running",
                "progress": { "message": "waiting" }
            }
        })]);
        let invoker = OperationInvoker::<_, RefundOperation>::new(&transport);

        let ack: OperationSignalAccepted<RefundProgress, RefundOutput> = invoker
            .control("op_signal")
            .expect("operation id is valid")
            .signal("selectWorkspace", Some(json!({ "workspaceId": "ws_1" })))
            .await
            .expect("signal succeeds");

        assert_eq!(ack.signal, "selectWorkspace");
        assert_eq!(ack.signal_sequence, 1);
        assert_eq!(
            transport.requests(),
            vec![(
                control_subject(RefundOperation::SUBJECT),
                json!({
                    "action": "signal",
                    "operationId": "op_signal",
                    "signal": "selectWorkspace",
                    "input": { "workspaceId": "ws_1" }
                })
            )]
        );
    }

    #[test]
    fn watch_uses_control_subject_skips_keepalive_and_stops_after_terminal_event() {
        assert_eq!(
            control_subject(RefundOperation::SUBJECT),
            "operations.v1.Billing.Refund.control"
        );
        let body = operation_watch_open_value("op_123", false, "open-nonce", 1024);
        assert_eq!(body["action"], "watch");
        assert_eq!(body["operationId"], "op_123");
        assert!(body.get("includeUpdates").is_none());
        assert_eq!(
            body["observation"]["format"],
            trellis_protocol::LIVE_VERSION
        );
        assert_eq!(body["observation"]["type"], "open");
        assert_eq!(body["observation"]["openId"], "open-nonce");
        assert_eq!(body["observation"]["receiveMaxPayloadBytes"], 1024);

        let with_updates = operation_watch_open_value("op_123", true, "open-nonce", 1024);
        assert_eq!(with_updates["includeUpdates"], true);
    }

    #[tokio::test]
    async fn watch_decode_skips_keepalive_and_wait_stops_after_terminal_event() {
        let frames = [
            json!({
                "kind": "snapshot",
                "snapshot": {
                    "revision": 2,
                    "state": "running",
                    "progress": { "message": "working" }
                }
            }),
            json!({
                "kind": "event",
                "event": {
                    "type": "progress",
                    "snapshot": {
                        "revision": 3,
                        "state": "running",
                        "progress": { "message": "almost there" }
                    }
                }
            }),
            json!({ "kind": "keepalive" }),
            json!({
                "kind": "event",
                "event": {
                    "type": "completed",
                    "snapshot": {
                        "revision": 4,
                        "state": "completed",
                        "output": { "refund_id": "rf_123" }
                    }
                }
            }),
            json!({
                "kind": "event",
                "event": {
                    "type": "progress",
                    "snapshot": {
                        "revision": 5,
                        "state": "running",
                        "progress": { "message": "ignored" }
                    }
                }
            }),
        ];
        let mut decoded = Vec::new();
        for frame in frames {
            if let Some(event) = decode_watch_frame::<RefundProgress, Value, RefundOutput>(
                frame,
                None,
                None,
                RefundOperation::OUTPUT_SCHEMA_JSON,
            )
            .expect("decode watch frame")
            {
                decoded.push(event);
            }
        }
        assert_eq!(decoded.len(), 4);
        assert!(matches!(decoded[0], OperationEvent::Started { .. }));
        assert!(matches!(decoded[1], OperationEvent::Progress { .. }));
        assert!(matches!(decoded[2], OperationEvent::Completed { .. }));
        assert!(matches!(decoded[3], OperationEvent::Progress { .. }));

        let mut events = stream::iter(decoded.into_iter().map(Ok));
        let snapshot = wait_for_terminal_snapshot(&mut events)
            .await
            .expect("wait succeeds");
        assert_eq!(
            snapshot.output,
            Some(RefundOutput {
                refund_id: "rf_123".to_string(),
            })
        );
        // Remaining frames after the terminal event are not consumed by wait.
        assert!(matches!(
            events.next().await,
            Some(Ok(OperationEvent::Progress { .. }))
        ));
    }

    #[tokio::test]
    async fn wait_returns_failed_snapshot_without_transport_error() {
        let event = decode_watch_frame::<RefundProgress, Value, RefundOutput>(
            json!({
                "kind": "event",
                "event": {
                    "type": "failed",
                    "snapshot": {
                        "revision": 2,
                        "state": "failed"
                    }
                }
            }),
            None,
            None,
            RefundOperation::OUTPUT_SCHEMA_JSON,
        )
        .expect("decode failed")
        .expect("event");
        let mut events = stream::iter([Ok(event)]);
        let snapshot = wait_for_terminal_snapshot(&mut events)
            .await
            .expect("failed is a business terminal");
        assert_eq!(snapshot.state, super::OperationState::Failed);
    }
}
