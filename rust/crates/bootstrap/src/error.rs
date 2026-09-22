use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

/// Error returned while generating bootstrap output.
#[derive(Debug, Diagnostic, Error)]
pub enum BootstrapError {
    /// The output directory exists and contains files while force is disabled.
    #[error("output directory {path} is not empty; pass --force to replace it")]
    OutputDirectoryNotEmpty {
        /// Existing non-empty output directory.
        path: PathBuf,
    },
    /// A required bootstrap option was missing or empty.
    #[error("missing required bootstrap option {0}")]
    MissingRequiredOption(&'static str),
    /// A generated text value would break config or environment file rendering.
    #[error("bootstrap option {0} must not contain control characters")]
    InvalidGeneratedTextValue(&'static str),
    /// NATS JWT generation failed.
    #[error("failed to generate NATS JWT material: {0}")]
    Jwt(String),
    /// NATS NKEY generation failed.
    #[error(transparent)]
    Nkey(#[from] nkeys::error::Error),
    /// Authorization trust generation failed.
    #[error("failed to generate authorization trust: {0}")]
    AuthorizationTrust(String),
    /// Filesystem I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Generated JSON could not be parsed or written.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
