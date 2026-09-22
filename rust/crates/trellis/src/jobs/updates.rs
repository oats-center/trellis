//! Typed live-only job update wire support.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

/// Contract-generated descriptor for one jobs queue.
pub trait JobDescriptor {
    /// Queued payload type.
    type Payload: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;
    /// Successful result type.
    type Result: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;

    /// Contract-local queue type.
    const QUEUE_TYPE: &'static str;
}

/// Descriptor extension implemented only by jobs queues that declare live updates.
pub trait JobUpdateDescriptor: JobDescriptor {
    /// Contract-defined cumulative update payload.
    type Update: Serialize + DeserializeOwned + Send + 'static;

    /// Declared update schema name in the contract manifest.
    const UPDATE_SCHEMA: &'static str;
    /// Declared update JSON Schema.
    const UPDATE_SCHEMA_JSON: &'static str;
}

/// Live-only job update envelope published outside the durable `JOBS` stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobUpdate<TUpdate = Value> {
    /// Job id receiving this update.
    pub job_id: String,
    /// One-based worker attempt number.
    pub attempt: u64,
    /// Monotonic sequence within this attempt.
    pub sequence: u64,
    /// RFC 3339 publication timestamp.
    pub timestamp: String,
    /// Cumulative contract-defined update payload.
    pub update: TUpdate,
}

/// Errors returned while validating typed job updates.
#[derive(Debug, thiserror::Error)]
pub enum JobUpdateError {
    /// Update serialization or decoding failed.
    #[error("failed to encode or decode job update: {0}")]
    Json(#[from] serde_json::Error),
    /// The declared update schema is invalid.
    #[error("failed to compile job update schema: {0}")]
    InvalidSchema(String),
    /// The update payload does not satisfy its declared schema.
    #[error("job update failed schema validation: {0}")]
    Validation(String),
}

pub(crate) fn validate_update(schema_json: &str, update: &Value) -> Result<(), JobUpdateError> {
    let schema: Value = serde_json::from_str(schema_json)?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| JobUpdateError::InvalidSchema(error.to_string()))?;
    if let Some(error) = validator.iter_errors(update).next() {
        return Err(JobUpdateError::Validation(error.to_string()));
    }
    Ok(())
}
