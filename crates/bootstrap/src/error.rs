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
    /// A generated NATS listener port was not positive.
    #[error("generated NATS {listener} listener port must be greater than zero")]
    InvalidListenerPort {
        /// Listener label.
        listener: &'static str,
    },
    /// Two generated NATS listeners were assigned the same port.
    #[error("generated NATS {first} and {second} listeners both use port {port}")]
    DuplicateListenerPort {
        /// First listener label.
        first: &'static str,
        /// Second listener label.
        second: &'static str,
        /// Shared port.
        port: u16,
    },
    /// A generated NATS listener collided with the locally hosted Trellis HTTP port.
    #[error("generated NATS {listener} listener port {port} collides with the Trellis HTTP port")]
    TrellisListenerPortCollision {
        /// Listener label.
        listener: &'static str,
        /// Colliding port.
        port: u16,
    },
}

/// Error resolving the required listener subset from a managed bundle's `nats.conf`.
#[derive(Debug, Diagnostic, Error)]
pub enum NatsConfigError {
    /// Reading the managed bundle's NATS config failed.
    #[error("failed to read managed NATS config {path}: {source}")]
    Read {
        /// Path to the managed bundle's `nats.conf`.
        path: PathBuf,
        /// Underlying read failure.
        #[source]
        source: std::io::Error,
    },
    /// A required listener was not declared.
    #[error(
        "managed NATS config {path} does not declare a top-level {listener} listener; the managed bundle requires explicit supported listener literals"
    )]
    MissingListener {
        /// Path to the managed bundle's `nats.conf`.
        path: PathBuf,
        /// Missing listener label.
        listener: &'static str,
    },
    /// A required listener was declared more than once.
    #[error(
        "managed NATS config {path} declares {listener} more than once (lines {first} and {second})"
    )]
    DuplicateListener {
        /// Path to the managed bundle's `nats.conf`.
        path: PathBuf,
        /// Duplicated listener label.
        listener: &'static str,
        /// First declaration line.
        first: usize,
        /// Second declaration line.
        second: usize,
    },
    /// A listener value was not a supported numeric host:port literal.
    #[error(
        "managed NATS config {path} has an unsupported {listener} value {value:?} on line {line}; the managed bundle requires explicit supported listener literals"
    )]
    InvalidListener {
        /// Path to the managed bundle's `nats.conf`.
        path: PathBuf,
        /// Listener label.
        listener: &'static str,
        /// Declaration line.
        line: usize,
        /// Rejected value.
        value: String,
    },
    /// The managed config structure was malformed.
    #[error("managed NATS config {path} is malformed at line {line}: {message}")]
    Malformed {
        /// Path to the managed bundle's `nats.conf`.
        path: PathBuf,
        /// Line where the malformed structure was found.
        line: usize,
        /// What was wrong.
        message: &'static str,
    },
}
