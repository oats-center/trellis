use std::future::Future;
use std::marker::PhantomData;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use futures_util::Stream;
use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncRead;

use crate::client::transfer::{FileInfo, TransferCancellation, UploadTransferGrant};
use crate::client::TrellisClientError;
use crate::live::subscription::{LiveMapDecision, LiveSubscription};
use crate::service::schema_validation::PreparedSchema;

/// Client-owned validation keeps the existing automatic draft for live updates.
#[derive(Debug)]
struct ClientOperationSchemas {
    input: PreparedSchema,
    progress: Option<PreparedSchema>,
    output: PreparedSchema,
    signals: std::collections::BTreeMap<String, PreparedSchema>,
    update: Option<jsonschema::Validator>,
}

impl ClientOperationSchemas {
    fn new<D: OperationDescriptor>() -> Result<Self, TrellisClientError> {
        let compile = |schema: &str, label: &str| {
            PreparedSchema::new(schema).map_err(|error| {
                TrellisClientError::OperationProtocol(format!(
                    "{label} failed schema validation: {error}"
                ))
            })
        };
        let signal_schemas: std::collections::BTreeMap<String, Value> =
            serde_json::from_str(D::SIGNAL_INPUT_SCHEMAS_JSON)?;
        let update = D::UPDATE_SCHEMA_JSON
            .map(|schema| {
                let schema: Value = serde_json::from_str(schema)?;
                jsonschema::validator_for(&schema).map_err(|error| {
                    TrellisClientError::OperationProtocol(format!(
                        "failed to compile operation update schema: {error}"
                    ))
                })
            })
            .transpose()?;
        Ok(Self {
            input: compile(D::INPUT_SCHEMA_JSON, "operation input")?,
            progress: D::PROGRESS_SCHEMA_JSON
                .map(|schema| compile(schema, "operation progress"))
                .transpose()?,
            output: compile(D::OUTPUT_SCHEMA_JSON, "operation output")?,
            signals: signal_schemas
                .into_iter()
                .map(|(name, schema)| {
                    Ok((
                        name,
                        compile(&schema.to_string(), "operation signal input")?,
                    ))
                })
                .collect::<Result<_, TrellisClientError>>()?,
            update,
        })
    }
}

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
    ) -> impl Future<Output = Result<FileInfo, TrellisClientError>> + Send + 'a
    where
        R: AsyncRead + Unpin + Send + ?Sized + 'a;

    fn put_upload_transfer_from_with_cancel<'a, R>(
        &'a self,
        grant: UploadTransferGrant,
        reader: &'a mut R,
        cancellation: &'a TransferCancellation,
    ) -> impl Future<Output = Result<FileInfo, TrellisClientError>> + Send + 'a
    where
        R: AsyncRead + Unpin + Send + ?Sized + 'a;
}

#[derive(Debug)]
pub struct OperationInvoker<'a, T, D> {
    transport: &'a T,
    schemas: OnceLock<Result<Arc<ClientOperationSchemas>, String>>,
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
    schemas: Arc<ClientOperationSchemas>,
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
    fn schemas(&self) -> Result<&Arc<ClientOperationSchemas>, TrellisClientError>
    where
        D: OperationDescriptor,
    {
        self.schemas
            .get_or_init(|| {
                ClientOperationSchemas::new::<D>()
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|message| TrellisClientError::OperationProtocol(message.clone()))
    }
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
            schemas: Arc::clone(self.schemas()?),
            _descriptor: PhantomData,
        })
    }

    pub fn new(transport: &'a T) -> Self {
        Self {
            transport,
            schemas: OnceLock::new(),
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
        self.start_invocation(invocation_id.into(), input, false)
            .await
    }

    /// Admit or replay an invocation with durable cancellation requested.
    ///
    /// A previously absent invocation enters its handler for cleanup only. This
    /// requires Invoke and Cancel authority and does not wait for cleanup; use
    /// the returned reference's `wait()` to observe the terminal result.
    pub async fn start_cancelled_with_invocation_id(
        &self,
        invocation_id: impl Into<String>,
        input: &D::Input,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        if !D::CANCELABLE {
            return Err(TrellisClientError::OperationProtocol(
                "operation is not cancelable".to_owned(),
            ));
        }
        self.start_invocation(invocation_id.into(), input, true)
            .await
    }

    async fn start_invocation(
        &self,
        invocation_id: String,
        input: &D::Input,
        cancellation_requested: bool,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        invocation_id.parse::<ulid::Ulid>().map_err(|_| {
            TrellisClientError::OperationProtocol(
                "operation invocation id must be a ULID".to_owned(),
            )
        })?;
        let body = serde_json::to_value(input)?;
        validate_operation_schema(&self.schemas()?.input, &body, "operation input")?;
        let mut envelope = serde_json::json!({
            "invocationId": invocation_id,
            "input": body,
        });
        if cancellation_requested {
            envelope["cancellationRequested"] = Value::Bool(true);
        }
        self.start_encoded(envelope).await
    }

    pub(crate) async fn start_encoded(
        &self,
        body: Value,
    ) -> Result<OperationRef<'a, T, D>, TrellisClientError> {
        let schemas = self.schemas()?;
        let subject = self
            .transport
            .operation_subject(D::API_ID, D::KEY, D::SUBJECT)?;
        let response = self.transport.request_json_value(subject, body).await?;
        validate_snapshot_at(&response, "/snapshot", schemas)?;
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
            schemas: Arc::clone(schemas),
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
    ) -> OperationTransferReaderInputBuilder<'a, 'b, T, D, R>
    where
        D: TransferOperationDescriptor,
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        OperationTransferReaderInputBuilder {
            invoker: self.invoker,
            input: self.input,
            reader,
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
        let file_info = match operation_ref.transfer_from(self.reader).await {
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
        decode_snapshot_response::<D>(response, &self.schemas)
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
            let snapshot = decode_snapshot_response::<D>(response, &self.schemas)?;
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
        let schema = self.schemas.signals.get(&signal).ok_or_else(|| {
            TrellisClientError::OperationProtocol(format!("undeclared operation signal '{signal}'"))
        })?;
        validate_operation_schema(
            schema,
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
        decode_signal_response::<D>(response, &self.schemas)
    }

    pub async fn transfer(&self, body: impl AsRef<[u8]>) -> Result<FileInfo, TrellisClientError> {
        self.transfer_vec(body.as_ref().to_vec()).await
    }

    /// Upload a transfer from a borrowed asynchronous reader after this operation is accepted.
    pub async fn transfer_from<R>(&self, reader: &mut R) -> Result<FileInfo, TrellisClientError>
    where
        R: AsyncRead + Unpin + Send + ?Sized,
    {
        let grant = self.accepted_transfer.clone().ok_or_else(|| {
            TrellisClientError::OperationProtocol(
                "operation does not have an accepted transfer session".into(),
            )
        })?;
        self.transport.put_upload_transfer_from(grant, reader).await
    }

    /// Upload from a borrowed reader and authenticate cancellation when requested.
    pub async fn transfer_from_with_cancel<R>(
        &self,
        reader: &mut R,
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
            .put_upload_transfer_from_with_cancel(grant, reader, cancellation)
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
        let result = async {
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
        .await;
        let _ = events.close().await;
        result
    }

    /// Open a live Operation observation without declared update envelopes.
    pub async fn live(
        &self,
    ) -> Result<LiveSubscription<OperationEvent<D::Progress, D::Output, Value>>, TrellisClientError>
    {
        self.open_operation_watch::<Value>(false).await
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
        self.schemas.update.as_ref().ok_or_else(|| {
            TrellisClientError::OperationProtocol("operation does not declare live updates".into())
        })?;
        self.open_operation_watch::<D::Update>(true).await
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
        // Pin one generation for the whole operation observation.
        let deadline = self.transport.transport_deadline();
        let lease = self
            .transport
            .acquire_transport(
                std::slice::from_ref(&publish_subject),
                &[format!("{}.>", self.transport.inbox_prefix())],
                deadline,
            )
            .await?;
        let receive_max_payload_bytes = lease.nats().max_payload() as u64;
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
            lease,
            deadline,
        )
        .await?;
        let schemas = Arc::clone(&self.schemas);
        crate::live::client_open::install_operation_watch_handle(
            self.transport,
            prepared,
            move |value| {
                decode_watch_frame::<D::Progress, TUpdate, D::Output>(
                    value,
                    &schemas,
                    include_updates,
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

fn decode_watch_frame<
    TProgress: DeserializeOwned,
    TUpdate: DeserializeOwned,
    TOutput: DeserializeOwned,
>(
    value: Value,
    schemas: &ClientOperationSchemas,
    include_updates: bool,
) -> Result<Option<OperationEvent<TProgress, TOutput, TUpdate>>, TrellisClientError> {
    if value.get("kind").and_then(Value::as_str) == Some("keepalive") {
        return Ok(None);
    }

    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        TrellisClientError::OperationProtocol("expected watch frame kind".to_string())
    })?;

    match kind {
        "snapshot" => {
            validate_snapshot_value(value.pointer("/snapshot"), schemas)?;
            let frame: SnapshotFrame<TProgress, TOutput> = serde_json::from_value(value)?;
            Ok(Some(snapshot_to_event(frame.snapshot)))
        }
        "event" => {
            validate_snapshot_value(value.pointer("/event/snapshot"), schemas)?;
            if value.pointer("/event/type").and_then(Value::as_str) == Some("update") {
                let update = value.pointer("/event/update").ok_or_else(|| {
                    TrellisClientError::OperationProtocol(
                        "operation update event is missing its payload".to_string(),
                    )
                })?;
                let validator = schemas
                    .update
                    .as_ref()
                    .filter(|_| include_updates)
                    .ok_or_else(|| {
                        TrellisClientError::OperationProtocol(
                            "received an undeclared operation update".to_string(),
                        )
                    })?;
                if let Some(error) = validator.iter_errors(update).next() {
                    return Err(TrellisClientError::OperationProtocol(format!(
                        "operation update failed schema validation: {error}"
                    )));
                }
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
    schemas: &ClientOperationSchemas,
) -> Result<OperationSnapshot<D::Progress, D::Output>, TrellisClientError> {
    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        TrellisClientError::OperationProtocol("expected control frame kind".to_string())
    })?;

    match kind {
        "snapshot" => {
            validate_snapshot_at(&value, "/snapshot", schemas)?;
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
    schemas: &ClientOperationSchemas,
) -> Result<OperationSignalAccepted<D::Progress, D::Output>, TrellisClientError> {
    let kind = value.get("kind").and_then(Value::as_str).ok_or_else(|| {
        TrellisClientError::OperationProtocol("expected signal frame kind".to_string())
    })?;

    match kind {
        "signal-accepted" => {
            validate_snapshot_at(&value, "/snapshot", schemas)?;
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

fn validate_snapshot_at(
    value: &Value,
    pointer: &str,
    schemas: &ClientOperationSchemas,
) -> Result<(), TrellisClientError> {
    validate_snapshot_value(value.pointer(pointer), schemas)
}

fn validate_snapshot_value(
    snapshot: Option<&Value>,
    schemas: &ClientOperationSchemas,
) -> Result<(), TrellisClientError> {
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    if let (Some(schema), Some(progress)) = (&schemas.progress, snapshot.get("progress")) {
        if !progress.is_null() {
            validate_operation_schema(schema, progress, "operation progress")?;
        }
    }
    if let Some(output) = snapshot.get("output") {
        if !output.is_null() {
            validate_operation_schema(&schemas.output, output, "operation output")?;
        }
    }
    Ok(())
}

fn validate_operation_schema(
    schema: &PreparedSchema,
    value: &Value,
    label: &str,
) -> Result<(), TrellisClientError> {
    schema.validate(value).map_err(|error| {
        TrellisClientError::OperationProtocol(format!("{label} failed schema validation: {error}"))
    })
}

#[cfg(test)]
mod update_tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Update {
        processed: Vec<u64>,
    }

    struct UpdateOperation;

    impl OperationDescriptor for UpdateOperation {
        type Input = Value;
        type Progress = Value;
        type Output = Value;
        type Update = Update;
        type UpdateEvidence = DeclaredOperationUpdates;
        type Error = String;
        const KEY: &'static str = "Test.Update";
        const SUBJECT: &'static str = "operations.v1.Test.Update";
        const CALLER_CAPABILITIES: &'static [&'static str] = &[];
        const OBSERVE_CAPABILITIES: &'static [&'static str] = &[];
        const CANCEL_CAPABILITIES: &'static [&'static str] = &[];
        const CANCELABLE: bool = false;
        const INPUT_SCHEMA_JSON: &'static str = "{}";
        const PROGRESS_SCHEMA_JSON: Option<&'static str> = None;
        const OUTPUT_SCHEMA_JSON: &'static str = "{}";
        const SIGNAL_INPUT_SCHEMAS_JSON: &'static str = "{}";
        const UPDATE_SCHEMA_JSON: Option<&'static str> = Some(
            r#"{
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object","required":["processed"],
            "properties":{"processed":{"type":"array","prefixItems":[{"type":"integer"}],"items":false}}
        }"#,
        );
    }

    #[test]
    fn decodes_and_validates_typed_update_frame() {
        let schemas = ClientOperationSchemas::new::<UpdateOperation>().unwrap();
        let event = decode_watch_frame::<Value, Update, Value>(
            json!({
                "kind": "event",
                "sequence": 2,
                "event": {
                    "type": "update",
                    "operationId": "op-1",
                    "sequence": 2,
                    "timestamp": "2026-07-10T12:00:00Z",
                    "update": { "processed": [3] }
                }
            }),
            &schemas,
            true,
        )
        .expect("decode update frame")
        .expect("event frame");

        assert!(matches!(
            event,
            OperationEvent::Update { update } if update.update.processed == [3]
        ));

        for update in [
            json!({"processed":["3"]}),
            json!({"processed":[3,4]}),
            json!({}),
        ] {
            let error = decode_watch_frame::<Value, Update, Value>(
                json!({
                    "kind":"event", "event":{"type":"update", "update":update}
                }),
                &schemas,
                true,
            )
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("operation update failed schema validation"));
        }
        let undeclared = decode_watch_frame::<Value, Update, Value>(
            json!({
                "kind":"event", "event":{"type":"update", "update":{"processed":[3]}}
            }),
            &schemas,
            false,
        )
        .unwrap_err();
        assert!(undeclared
            .to_string()
            .contains("undeclared operation update"));
    }
}

pub fn control_subject(subject: &str) -> String {
    format!("{subject}.control")
}

#[cfg(test)]
mod tests {
    use super::{decode_watch_frame, ClientOperationSchemas, OperationDescriptor, OperationEvent};
    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};

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

    #[test]
    fn watch_decode_skips_keepalive_and_preserves_typed_output() {
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
        ];
        let mut decoded = Vec::new();
        let schemas = ClientOperationSchemas::new::<RefundOperation>().unwrap();
        for frame in frames {
            if let Some(event) =
                decode_watch_frame::<RefundProgress, Value, RefundOutput>(frame, &schemas, false)
                    .expect("decode watch frame")
            {
                decoded.push(event);
            }
        }
        assert_eq!(decoded.len(), 3);
        assert!(matches!(decoded[0], OperationEvent::Started { .. }));
        assert!(matches!(decoded[1], OperationEvent::Progress { .. }));
        let OperationEvent::Completed { snapshot } = &decoded[2] else {
            panic!("completed frame must decode as a business terminal")
        };
        assert_eq!(
            snapshot.output,
            Some(RefundOutput {
                refund_id: "rf_123".to_string(),
            })
        );
    }

    #[test]
    fn failed_frame_decodes_as_a_business_event() {
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
            &ClientOperationSchemas::new::<RefundOperation>().unwrap(),
            false,
        )
        .expect("decode failed")
        .expect("event");
        let OperationEvent::Failed { snapshot } = event else {
            panic!("failed frame must decode as a business terminal")
        };
        assert_eq!(snapshot.state, super::OperationState::Failed);
    }
}
