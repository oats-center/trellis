use serde::Serialize;
use serde_json::{Map, Value};

/// Structured RPC error declared by a service contract.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredRpcError {
    error_type: String,
    message: String,
    fields: Map<String, Value>,
}

impl DeclaredRpcError {
    /// Build a contract-declared RPC error payload.
    pub fn new<K>(
        error_type: impl Into<String>,
        message: impl Into<String>,
        fields: impl IntoIterator<Item = (K, Value)>,
    ) -> Self
    where
        K: Into<String>,
    {
        Self {
            error_type: error_type.into(),
            message: message.into(),
            fields: fields
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        }
    }

    /// Return the declared RPC error type discriminator.
    pub fn error_type(&self) -> &str {
        &self.error_type
    }

    /// Return the human-facing declared RPC error message.
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn to_payload_with_context(
        &self,
        id: String,
        context: Map<String, Value>,
        trace_id: Option<&str>,
    ) -> Value {
        let mut payload = self.fields.clone();
        payload.insert("id".to_string(), Value::String(id));
        payload.insert("type".to_string(), Value::String(self.error_type.clone()));
        payload.insert("message".to_string(), Value::String(self.message.clone()));
        merge_context(&mut payload, context);
        if let Some(trace_id) = trace_id {
            payload.insert("traceId".to_string(), Value::String(trace_id.to_string()));
        }
        Value::Object(payload)
    }
}

pub(crate) fn merge_context(payload: &mut Map<String, Value>, context: Map<String, Value>) {
    if context.is_empty() {
        return;
    }

    match payload.get_mut("context") {
        Some(Value::Object(existing)) => {
            existing.remove("subject");
            for (key, value) in context {
                existing.entry(key).or_insert(value);
            }
        }
        Some(_) => {}
        None => {
            payload.insert("context".to_string(), Value::Object(context));
        }
    }
}

/// One structural validation issue for `ValidationError`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidationIssue {
    /// JSON Pointer locating the invalid value.
    pub path: String,
    /// Human-readable validation failure.
    pub message: String,
}

/// One annotated validation issue for `SchemaValidationError`.
/// Carries stable field-level UX metadata from `x-trellis-validation`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SchemaValidationIssue {
    /// JSON Pointer locating the invalid value.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// JSON Pointer locating the failed schema keyword.
    pub schema_path: Option<String>,
    /// JSON Schema keyword that failed.
    pub keyword: String,
    /// Stable application-facing validation code.
    pub code: String,
    /// Human-readable validation failure.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Optional field label supplied by schema metadata.
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Optional remediation note supplied by schema metadata.
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Optional localization key supplied by schema metadata.
    pub i18n_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Optional severity supplied by schema metadata.
    pub severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Additional structured parameters supplied by schema metadata.
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Errors returned by the Trellis server runtime.
#[derive(thiserror::Error, Debug)]
pub enum ServerError {
    /// A handler returned an error declared by its contract.
    #[error("declared RPC error {0:?}")]
    DeclaredRpc(DeclaredRpcError),

    /// Schema validation of caller input failed and all failures had
    /// annotated `x-trellis-validation` metadata.
    #[error("schema validation failed: {} issues", .issues.len())]
    SchemaValidation {
        /// Annotated schema failures.
        issues: Box<Vec<SchemaValidationIssue>>,
    },

    /// Schema validation of caller input failed with at least one
    /// unannotated failure.
    #[error("validation failed: {} issues", .issues.len())]
    Validation {
        /// Structural validation failures.
        issues: Box<Vec<ValidationIssue>>,
    },

    /// JSON encoding or decoding failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    #[doc(hidden)]
    Subject(#[from] crate::client::SubjectError),

    #[error("nats error: {0}")]
    #[doc(hidden)]
    Nats(String),

    /// A KV revision check failed because the key is no longer at the expected revision.
    #[error("kv key '{key}' revision mismatch: expected {expected}, actual {actual:?}")]
    #[doc(hidden)]
    KvRevisionMismatch {
        key: String,
        expected: u64,
        actual: Option<u64>,
    },

    #[error("missing handler for subject '{0}'")]
    #[doc(hidden)]
    MissingHandler(String),

    #[error("missing session key for authenticated subject '{subject}'")]
    #[doc(hidden)]
    MissingSessionKey { subject: String },

    #[error("missing proof for authenticated subject '{subject}'")]
    #[doc(hidden)]
    MissingProof { subject: String },

    #[error("missing authorization-context for authenticated subject '{subject}'")]
    #[doc(hidden)]
    MissingAuthorizationContext { subject: String },

    #[error("missing reply inbox for authenticated request subject '{subject}'")]
    #[doc(hidden)]
    MissingReply { subject: String },

    #[error("request denied for subject '{subject}' and session '{session_key}'")]
    #[doc(hidden)]
    RequestDenied {
        subject: String,
        session_key: String,
    },

    #[error(
        "reply inbox '{reply_to}' does not match authorized prefix '{expected_prefix}' for session '{session_key}' on subject '{subject}'"
    )]
    #[doc(hidden)]
    ReplyInboxMismatch {
        subject: String,
        session_key: String,
        reply_to: String,
        expected_prefix: String,
    },

    #[error(
        "transfer request for subject '{subject}' used a session that does not match the grant"
    )]
    #[doc(hidden)]
    TransferSessionMismatch {
        subject: String,
        actual_session_key: String,
    },

    #[error("invalid operation control action '{action}' for subject '{subject}'")]
    #[doc(hidden)]
    InvalidOperationControlAction { subject: String, action: String },

    #[error("operation '{operation_id}' was not found")]
    #[doc(hidden)]
    OperationNotFound { operation_id: String },

    #[error("operation '{operation_id}' already exists")]
    #[doc(hidden)]
    OperationAlreadyExists { operation_id: String },

    #[error("invalid operation id '{operation_id}'")]
    #[doc(hidden)]
    OperationInvalidId { operation_id: String },

    #[error(
        "operation '{operation_id}' belongs to service '{actual_service}' operation '{actual_operation}', expected service '{expected_service}' operation '{expected_operation}'"
    )]
    #[doc(hidden)]
    OperationMismatch {
        operation_id: String,
        expected_service: String,
        expected_operation: String,
        actual_service: Box<str>,
        actual_operation: Box<str>,
    },

    #[error("operation '{operation_id}' is already terminal in state '{state}'")]
    #[doc(hidden)]
    OperationAlreadyTerminal { operation_id: String, state: String },

    #[error("operation '{operation}' does not support '{action}'")]
    #[doc(hidden)]
    OperationUnsupportedControl { operation: String, action: String },

    #[error(
        "service '{service_name}' is missing {resource_kind} resource binding '{resource_name}'"
    )]
    #[doc(hidden)]
    MissingResourceBinding {
        service_name: String,
        resource_kind: String,
        resource_name: String,
    },

    #[error(
        "service '{service_name}' has invalid {resource_kind} resource binding '{resource_name}': {reason}"
    )]
    #[doc(hidden)]
    InvalidResourceBinding {
        service_name: String,
        resource_kind: String,
        resource_name: String,
        reason: String,
    },

    /// A retained resource handle no longer names the current usable binding.
    #[error("{resource_kind} resource binding '{resource_name}' is no longer available")]
    ResourceUnavailable {
        /// Resource family.
        resource_kind: String,
        /// Participant-local resource name.
        resource_name: String,
    },

    /// Waiting for an object-store key exceeded the configured timeout.
    #[error(
        "service '{service_name}' timed out waiting {timeout_ms}ms for store '{store}' object '{key}'"
    )]
    #[doc(hidden)]
    StoreWaitTimeout {
        service_name: String,
        store: String,
        key: String,
        timeout_ms: u128,
    },

    /// Waiting for an object-store key was canceled by the caller.
    #[error("service '{service_name}' canceled waiting for store '{store}' object '{key}'")]
    #[doc(hidden)]
    StoreWaitCanceled {
        service_name: String,
        store: String,
        key: String,
    },

    #[error(
        "service '{service_name}' transfer object '{key}' in store '{store}' is {size} bytes, exceeding max object size {max_bytes}"
    )]
    #[doc(hidden)]
    TransferObjectTooLarge {
        service_name: String,
        store: String,
        key: String,
        size: u64,
        max_bytes: u64,
    },

    /// A bound object-store upload exceeded its configured maximum.
    #[error("store object exceeds configured maximum: attempted {attempted_bytes} bytes, max {max_bytes} bytes")]
    StoreObjectTooLarge {
        /// First size known to exceed the limit.
        attempted_bytes: u64,
        /// Configured object-size limit.
        max_bytes: u64,
    },

    /// A bound object-store upload did not match its declared exact size.
    #[error(
        "store object size mismatch: expected {expected_bytes} bytes, got {actual_bytes} bytes"
    )]
    StoreObjectSizeMismatch {
        /// Declared exact object size.
        expected_bytes: u64,
        /// Observed size, or the first size known to exceed the declaration.
        actual_bytes: u64,
    },

    /// A bound object-store upload was cancelled before validated source EOF.
    #[error("store object upload cancelled")]
    StoreWriteCancelled,

    /// A bound object-store download was cancelled.
    #[error("store object download cancelled")]
    StoreReadCancelled,

    /// A durable operation value exceeded its storage capacity.
    #[error("operation {field} exceeds capacity: {actual_bytes} bytes, max {max_bytes} bytes")]
    OperationCapacityExceeded {
        /// Persisted field that exceeded its limit.
        field: &'static str,
        /// Encoded size that was rejected.
        actual_bytes: usize,
        /// Maximum encoded size.
        max_bytes: usize,
    },

    /// An idempotency key was reused with different operation content.
    #[error("operation {kind} id '{request_id}' was already accepted with different content")]
    OperationIdempotencyConflict {
        /// Kind of idempotent request.
        kind: &'static str,
        /// Reused request identifier.
        request_id: String,
    },

    /// Object metadata publication may have committed without a definitive acknowledgement.
    #[error("store object commit is indeterminate for key {key}: {message}")]
    StoreCommitIndeterminate {
        /// Logical object key.
        key: String,
        /// Backend failure detail.
        message: String,
    },

    /// Replacement metadata committed but cleanup of the previous chunks failed.
    #[error("store object replacement committed but cleanup failed for key {key}: {message}")]
    StoreCommittedCleanupFailed {
        /// Logical object key.
        key: String,
        /// Backend failure detail.
        message: String,
    },

    #[error("invalid transfer id '{value}': expected a single safe NATS subject token")]
    #[doc(hidden)]
    InvalidTransferId { value: String },

    #[error("transfer '{transfer_id}' expected chunk sequence {expected_seq}, got {actual_seq}")]
    #[doc(hidden)]
    TransferSequenceOutOfOrder {
        transfer_id: String,
        expected_seq: u64,
        actual_seq: u64,
    },

    #[error("transfer '{transfer_id}' has not received an EOF frame")]
    #[doc(hidden)]
    TransferMissingEof { transfer_id: String },

    #[error("transfer '{transfer_id}' is already complete")]
    #[doc(hidden)]
    TransferAlreadyComplete { transfer_id: String },

    #[error("transfer '{transfer_id}' expired at '{expires_at}'")]
    #[doc(hidden)]
    TransferExpired {
        transfer_id: String,
        expires_at: String,
    },

    #[error("invalid transfer expiration '{expires_at}': {details}")]
    #[doc(hidden)]
    InvalidTransferExpiry { expires_at: String, details: String },

    #[error("transfer object '{key}' is missing from store '{store}'")]
    #[doc(hidden)]
    TransferObjectMissing { store: String, key: String },

    #[error("transfer chunk size must be between 1 and 1048576 bytes, got {chunk_bytes}")]
    #[doc(hidden)]
    InvalidTransferChunkSize { chunk_bytes: u64 },

    #[error("transfer request is missing required header '{header}'")]
    #[doc(hidden)]
    MissingTransferHeader { header: &'static str },

    #[error("transfer request has invalid header '{header}': '{value}'")]
    #[doc(hidden)]
    InvalidTransferHeader { header: &'static str, value: String },

    #[error(
        "transfer object '{key}' in store '{store}' is {actual_size} bytes, but grant expected {expected_size} bytes"
    )]
    #[doc(hidden)]
    TransferObjectSizeMismatch {
        store: String,
        key: String,
        expected_size: u64,
        actual_size: u64,
    },

    /// Transfer completion metadata did not match the independently hashed bytes.
    #[error(
        "transfer '{transfer_id}' digest mismatch: expected {expected_digest}, got {actual_digest}"
    )]
    TransferDigestMismatch {
        /// Transfer session identifier.
        transfer_id: String,
        /// Digest declared by the sender or grant.
        expected_digest: String,
        /// Digest computed by the receiver.
        actual_digest: String,
    },

    /// An authenticated transfer cancellation was accepted.
    #[error("transfer '{transfer_id}' cancelled")]
    TransferCancelled {
        /// Transfer session identifier.
        transfer_id: String,
    },
}

/// Result alias used by descriptor-backed RPC handlers.
pub type HandlerResult<T> = Result<T, ServerError>;
