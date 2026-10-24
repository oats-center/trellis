//! Framework-private Operation persistence shared by native and WASM consumers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AuthenticatedCaller, SessionBinding};

/// Representation version inside the existing format-1 TRKV envelope.
pub const OPERATION_RECORD_VERSION: u32 = 2;

/// Maximum descriptor-encoded input bytes.
pub const MAX_OPERATION_INPUT_BYTES: usize = 256 * 1024;
/// Maximum descriptor-encoded progress bytes.
pub const MAX_OPERATION_PROGRESS_BYTES: usize = 64 * 1024;
/// Maximum descriptor-encoded output bytes.
pub const MAX_OPERATION_OUTPUT_BYTES: usize = 512 * 1024;
/// Maximum descriptor-encoded error bytes.
pub const MAX_OPERATION_ERROR_BYTES: usize = 32 * 1024;
/// Maximum retained signals.
pub const MAX_OPERATION_SIGNALS: usize = 100;
/// Maximum JSON bytes in one signal.
pub const MAX_OPERATION_SIGNAL_BYTES: usize = 64 * 1024;
/// Maximum complete framed record bytes.
pub const MAX_OPERATION_RECORD_BYTES: usize = 1024 * 1024;

/// Invalid or oversized persisted state. Failures never mean record absence.
#[derive(Debug, thiserror::Error)]
pub enum OperationRecordError {
    /// Structurally or semantically inconsistent state.
    #[error("invalid operation record: {0}")]
    Invalid(String),
    /// A retained value exceeds its supported limit.
    #[error("operation {field} has {actual_bytes} bytes; maximum is {max_bytes}")]
    Capacity {
        /// Oversized component.
        field: &'static str,
        /// Encoded size.
        actual_bytes: usize,
        /// Supported maximum.
        max_bytes: usize,
    },
    /// Invalid JSON representation.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Invalid canonical invocation identity.
    #[error(transparent)]
    Protocol(#[from] crate::ProtocolError),
}

/// Stable invocation digest; diagnostic and executor fields never contribute.
pub fn operation_invocation_digest(
    api_id: &str,
    operation: &str,
    principal_id: &str,
    binding: &SessionBinding,
    input: &Value,
) -> Result<String, crate::ProtocolError> {
    crate::digest_json(&serde_json::json!({
        "apiId": api_id, "operation": operation,
        "creatorPrincipalId": principal_id, "creatorBinding": binding, "input": input,
    }))
}

impl DurableOperationRecord {
    /// Validate retained identity, lifecycle consistency, signal order and limits.
    pub fn validate(&self) -> Result<(), OperationRecordError> {
        let invalid = |message: &str| OperationRecordError::Invalid(message.to_owned());
        // These are framework counters shared with JS, not descriptor-owned
        // uint64 values (whose generated codecs use decimal strings).
        if [
            self.revision,
            self.owner_epoch,
            self.next_signal_sequence,
            self.update_sequence,
            self.snapshot.revision,
        ]
        .into_iter()
        .any(|counter| counter > 9_007_199_254_740_991)
        {
            return Err(invalid(
                "framework counter exceeds exact JSON integer range",
            ));
        }
        if self.cancellation_requested && self.cancellation_requested_at.is_none() {
            return Err(invalid(
                "cancellation intent is missing its committed timestamp",
            ));
        }
        if let Some(timestamp) = &self.cancellation_requested_at {
            time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
                .map_err(|_| invalid("invalid cancellation timestamp"))?;
        }
        if self.invocation_id.parse::<ulid::Ulid>().is_err()
            || self.invocation_digest
                != operation_invocation_digest(
                    &self.api_id,
                    &self.operation,
                    &self.creator_principal_id,
                    &self.creator_binding,
                    &self.input,
                )?
        {
            return Err(invalid("invocation identity or digest does not match"));
        }
        if self.snapshot.id.as_deref() != Some(&self.invocation_id)
            || self.snapshot.operation.as_deref() != Some(&self.operation)
            || self.snapshot.revision != self.revision
        {
            return Err(invalid("snapshot identity or revision does not match"));
        }
        for value in [&self.snapshot.created_at, &self.snapshot.updated_at] {
            let value = value
                .as_deref()
                .ok_or_else(|| invalid("missing checkpoint timestamp"))?;
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .map_err(|_| invalid("invalid checkpoint timestamp"))?;
        }
        if self.signals.len() > MAX_OPERATION_SIGNALS
            || self.next_signal_sequence != self.signals.len() as u64 + 1
        {
            return Err(invalid("inconsistent signal count or next sequence"));
        }
        for (index, signal) in self.signals.iter().enumerate() {
            if signal.sequence != index as u64 + 1
                || self.signals[..index]
                    .iter()
                    .any(|other| other.request_id == signal.request_id)
            {
                return Err(invalid(
                    "signals must have contiguous sequences and unique request ids",
                ));
            }
            time::OffsetDateTime::parse(
                &signal.accepted_at,
                &time::format_description::well_known::Rfc3339,
            )
            .map_err(|_| invalid("invalid signal acceptance timestamp"))?;
            let actual_bytes = serde_json::to_vec(&signal.payload)?.len();
            if actual_bytes > MAX_OPERATION_SIGNAL_BYTES {
                return Err(OperationRecordError::Capacity {
                    field: "signal",
                    actual_bytes,
                    max_bytes: MAX_OPERATION_SIGNAL_BYTES,
                });
            }
        }
        if let Some(value) = &self.transfer {
            let upload: OperationUpload = serde_json::from_value(value.clone())?;
            // Staged attempts own a staging object; stream attempts deliver
            // bytes straight to the running handler and stage nothing.
            let valid = match upload.state.as_str() {
                "pending" | "uploading" | "committed" | "invalid" => !upload.staging_key.is_empty(),
                "streaming" | "streamed" => upload.staging_key.is_empty(),
                _ => false,
            };
            if !valid {
                return Err(invalid("invalid upload attempt"));
            }
            crate::transfer::parse_transfer_grant(&serde_json::to_vec(&upload.grant)?)?;
            if matches!(upload.state.as_str(), "committed" | "streamed") {
                let info: crate::transfer::TransferFileInfo =
                    serde_json::from_value(upload.file_info.ok_or_else(|| {
                        invalid("committed upload is missing logical file metadata")
                    })?)?;
                if Some(info.size) != upload.size
                    || Some(&info.digest) != upload.digest.as_ref()
                    || Some(&info.updated_at) != upload.updated_at.as_ref()
                {
                    return Err(invalid(
                        "committed upload metadata does not match staged object",
                    ));
                }
            }
        }
        let error = self
            .snapshot
            .error
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        for (field, value, max_bytes) in [
            ("input", Some(&self.input), MAX_OPERATION_INPUT_BYTES),
            (
                "progress",
                self.snapshot.progress.as_ref(),
                MAX_OPERATION_PROGRESS_BYTES,
            ),
            (
                "output",
                self.snapshot.output.as_ref(),
                MAX_OPERATION_OUTPUT_BYTES,
            ),
            ("error", error.as_ref(), MAX_OPERATION_ERROR_BYTES),
        ] {
            if let Some(value) = value {
                let actual_bytes = serde_json::to_vec(&value)?.len();
                if actual_bytes > max_bytes {
                    return Err(OperationRecordError::Capacity {
                        field,
                        actual_bytes,
                        max_bytes,
                    });
                }
            }
        }
        Ok(())
    }

    /// Whether a replay addresses the same immutable invocation.
    pub fn same_invocation(&self, other: &Self) -> bool {
        self.invocation_id == other.invocation_id
            && self.invocation_digest == other.invocation_digest
    }
}

/// Encode one validated record in TRKV format 1, representation 2.
pub fn encode_operation_record(
    record: &DurableOperationRecord,
) -> Result<Vec<u8>, OperationRecordError> {
    record.validate()?;
    let mut bytes = b"TRKV\x01".to_vec();
    bytes.extend_from_slice(&OPERATION_RECORD_VERSION.to_be_bytes());
    serde_json::to_writer(&mut bytes, record)?;
    if bytes.len() > MAX_OPERATION_RECORD_BYTES {
        return Err(OperationRecordError::Capacity {
            field: "record",
            actual_bytes: bytes.len(),
            max_bytes: MAX_OPERATION_RECORD_BYTES,
        });
    }
    Ok(bytes)
}

/// Decode and validate without rewriting, migrating or deleting retained state.
pub fn decode_operation_record(
    bytes: &[u8],
) -> Result<DurableOperationRecord, OperationRecordError> {
    if bytes.len() < 9
        || &bytes[..5] != b"TRKV\x01"
        || bytes[5..9] != OPERATION_RECORD_VERSION.to_be_bytes()
    {
        return Err(OperationRecordError::Invalid(
            "unsupported record representation".into(),
        ));
    }
    if bytes.len() > MAX_OPERATION_RECORD_BYTES {
        return Err(OperationRecordError::Capacity {
            field: "record",
            actual_bytes: bytes.len(),
            max_bytes: MAX_OPERATION_RECORD_BYTES,
        });
    }
    let record: DurableOperationRecord = serde_json::from_slice(&bytes[9..])?;
    record.validate()?;
    Ok(record)
}

/// Persisted invocation identity, state, and executor fence. Application values
/// are already encoded by their generated descriptor codecs, not encoded again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableOperationRecord {
    /// Stable invocation ULID.
    pub invocation_id: String,
    /// Digest of immutable input and initiating principal/binding.
    pub invocation_digest: String,
    /// Qualified API identity.
    pub api_id: String,
    /// Generated Operation identity.
    pub operation: String,
    /// Deployment owning the physical repository.
    pub deployment_id: String,
    /// Initiating principal.
    pub creator_principal_id: String,
    /// Initiating application or deployment binding.
    pub creator_binding: SessionBinding,
    /// Original accepted session key, never fresh application authority.
    pub caller_session_key: String,
    /// Retained authenticated caller projection for accepted work.
    pub caller: Option<AuthenticatedCaller>,
    /// Descriptor-encoded input.
    pub input: Value,
    /// Durable lifecycle checkpoint.
    pub snapshot: OperationSnapshot,
    /// Monotonic record revision.
    pub revision: u64,
    /// Current process executor, absent before acquisition or after release.
    pub owner_executor_id: Option<String>,
    /// Logical connection holding the fence.
    pub owner_connection_id: Option<String>,
    /// Monotonic fencing epoch.
    pub owner_epoch: u64,
    /// Lease expiry in Unix milliseconds.
    pub lease_expires_at_ms: Option<i64>,
    /// Monotone cancellation intent.
    pub cancellation_requested: bool,
    /// Time cancellation intent was first committed, retained across owners.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_requested_at: Option<String>,
    /// Next contiguous signal sequence.
    pub next_signal_sequence: u64,
    /// Last published application update sequence within the executor epoch.
    pub update_sequence: u64,
    /// Accepted signals and acknowledgement state.
    pub signals: Vec<DurableOperationSignal>,
    /// Physical upload staging and commitment state.
    pub transfer: Option<Value>,
    /// Diagnostic carrier; never contributes to identity or authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<OperationTraceCarrier>,
}

/// Diagnostic trace carrier retained with accepted work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationTraceCarrier {
    /// W3C trace parent.
    pub traceparent: String,
    /// Optional W3C trace state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
}

/// One durably accepted signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableOperationSignal {
    /// Contiguous sequence starting at one.
    pub sequence: u64,
    /// Caller-selected idempotency key.
    pub request_id: String,
    /// Declared signal name.
    pub name: String,
    /// Generated-codec JSON value, retained without a second binary encoding.
    pub payload: Value,
    /// Server timestamp of original acceptance, preserved on replay.
    pub accepted_at: String,
    /// Durable processing acknowledgement.
    pub acknowledged: bool,
}

/// Physical upload attempt retained with an invocation. The grant uses the
/// existing Transfer protocol; it is not a second authority format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationUpload<G = Value> {
    /// Exact object-store locator for a staged attempt; empty for a stream.
    pub staging_key: String,
    /// Staged: pending, uploading, committed, or invalid. Stream: streaming
    /// until verified delivery, then streamed.
    pub state: String,
    /// Existing send grant.
    pub grant: G,
    /// Committed object size.
    pub size: Option<u64>,
    /// Committed object digest.
    pub digest: Option<String>,
    /// Object-store update timestamp.
    pub updated_at: Option<String>,
    /// Media type, when supplied.
    pub content_type: Option<String>,
    /// Exact logical file metadata returned by the committing producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_info: Option<Value>,
}

/// Operation lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OperationState {
    /// Accepted, awaiting execution or upload.
    Pending,
    /// Executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Completed unsuccessfully.
    Failed,
    /// Cancellation cleanup completed.
    Cancelled,
}

impl OperationState {
    /// Whether execution has reached a durable terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Public checkpoint, also used inside the framework's durable record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    rename_all = "camelCase",
    bound(deserialize = "TProgress: Deserialize<'de>, TOutput: Deserialize<'de>")
)]
pub struct OperationSnapshot<TProgress = Value, TOutput = Value> {
    /// Invocation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Service identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Operation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Durable checkpoint revision.
    pub revision: u64,
    /// Lifecycle state.
    pub state: OperationState,
    /// RFC3339 creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC3339 update time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// RFC3339 completion time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Descriptor-encoded progress.
    #[serde(
        default,
        deserialize_with = "crate::transfer::present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub progress: Option<TProgress>,
    /// Upload checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<OperationTransferProgress>,
    /// Descriptor-encoded output.
    #[serde(
        default,
        deserialize_with = "crate::transfer::present_optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub output: Option<TOutput>,
    /// Failure payload, including application-defined fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<OperationError>,
}

impl<P, O> Default for OperationSnapshot<P, O> {
    fn default() -> Self {
        Self {
            id: None,
            service: None,
            operation: None,
            revision: 0,
            state: OperationState::Pending,
            created_at: None,
            updated_at: None,
            completed_at: None,
            progress: None,
            transfer: None,
            output: None,
            error: None,
        }
    }
}

/// Durable failure payload. Unknown application fields survive repository writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationError {
    /// Wire error discriminator.
    #[serde(rename = "type")]
    pub error_type: String,
    /// Public failure message.
    pub message: String,
    /// Descriptor-owned error fields, retained without interpretation.
    #[serde(flatten)]
    pub fields: Box<serde_json::Map<String, Value>>,
}

/// Backend-consumed upload checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationTransferProgress {
    /// Latest consumed frame.
    pub chunk_index: u64,
    /// Bytes in the latest frame.
    pub chunk_bytes: u64,
    /// Total consumed bytes.
    pub transferred_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codec_preserves_descriptor_values_and_rejects_inexact_framework_counters() {
        let id = ulid::Ulid::new().to_string();
        let binding = SessionBinding::Browser {
            client_id: "caller".into(),
            origin: "https://example.test".into(),
        };
        let input = json!({"count": "18446744073709551615", "bytes": "AP8="});
        let mut record = DurableOperationRecord {
            invocation_digest: operation_invocation_digest(
                "example@v1",
                "run",
                "alice",
                &binding,
                &input,
            )
            .unwrap(),
            invocation_id: id.clone(),
            api_id: "example@v1".into(),
            operation: "run".into(),
            deployment_id: ulid::Ulid::new().to_string(),
            creator_principal_id: "alice".into(),
            creator_binding: binding,
            caller_session_key: "retained-session".into(),
            caller: None,
            input,
            snapshot: OperationSnapshot {
                id: Some(id),
                operation: Some("run".into()),
                revision: 1,
                state: OperationState::Failed,
                created_at: Some("2026-10-18T00:00:00Z".into()),
                updated_at: Some("2026-10-18T00:00:00Z".into()),
                completed_at: Some("2026-10-18T00:00:00Z".into()),
                progress: Some(json!({"nested": [null, "18446744073709551615"]})),
                error: Some(OperationError {
                    error_type: "ApplicationError".into(),
                    message: "declared failure".into(),
                    fields: Box::new(serde_json::Map::from_iter([(
                        "details".into(),
                        json!({"retryable": false}),
                    )])),
                }),
                ..Default::default()
            },
            revision: 1,
            owner_executor_id: None,
            owner_connection_id: None,
            owner_epoch: 0,
            lease_expires_at_ms: None,
            cancellation_requested: false,
            cancellation_requested_at: None,
            next_signal_sequence: 1,
            update_sequence: 0,
            signals: vec![],
            transfer: None,
            telemetry: None,
        };
        let bytes = encode_operation_record(&record).unwrap();
        assert_eq!(decode_operation_record(&bytes).unwrap(), record);
        record.snapshot.state = OperationState::Completed;
        record.snapshot.error = None;
        record.snapshot.progress = Some(Value::Null);
        record.snapshot.output = Some(Value::Null);
        let bytes = encode_operation_record(&record).unwrap();
        assert_eq!(decode_operation_record(&bytes).unwrap(), record);
        record.owner_epoch = 9_007_199_254_740_992;
        assert!(encode_operation_record(&record).is_err());
        // Failed decoding/encoding must never turn the retained checkpoint into absence.
        assert_eq!(
            decode_operation_record(&bytes).unwrap().snapshot.progress,
            record.snapshot.progress
        );
    }
}
