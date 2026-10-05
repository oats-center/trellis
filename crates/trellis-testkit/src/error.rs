//! Stable public error surface for the testkit.
//!
//! Every failure carries a broad [`TrellisTestErrorKind`] category and the
//! [`TrellisTestStage`] that produced it, plus bounded, already-redacted
//! diagnostics. Secret material (passwords, seeds, portal bindings, bootstrap
//! tokens) is never stored on an error; callers redact output before attaching
//! it.

use std::fmt;
use std::path::{Path, PathBuf};

/// Broad failure category for a testkit operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TrellisTestErrorKind {
    /// The host platform is not supported by the testkit.
    UnsupportedPlatform,
    /// A required executable could not be found.
    MissingBinary,
    /// A resolved executable is not a regular executable file.
    InvalidBinary,
    /// The production runtime task or managed broker failed.
    Runtime,
    /// A builder value or generated configuration is invalid.
    InvalidConfiguration,
    /// A loopback port could not be reserved.
    PortAllocation,
    /// A selected loopback port was already bound by another process.
    PortConflict,
    /// A stage exceeded its deadline.
    Timeout,
    /// The bootstrap bundle could not be generated or validated.
    Bootstrap,
    /// A login/authorization step failed.
    Authentication,
    /// A participant installed with a conflicting package digest.
    ParticipantConflict,
    /// The participant kind is not supported by this release.
    UnsupportedParticipantKind,
    /// A registration name was already reserved in this runtime.
    DuplicateName,
    /// A generated administration RPC failed.
    AdminRpc,
    /// The runtime has already been shut down.
    RuntimeStopped,
    /// Filesystem or process I/O failed.
    Io,
    /// Cleanup of owned infrastructure failed.
    Cleanup,
}

impl TrellisTestErrorKind {
    /// Stable machine-readable code for this kind.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::MissingBinary => "missing_binary",
            Self::InvalidBinary => "invalid_binary",
            Self::Runtime => "runtime",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::PortAllocation => "port_allocation",
            Self::PortConflict => "port_conflict",
            Self::Timeout => "timeout",
            Self::Bootstrap => "bootstrap",
            Self::Authentication => "authentication",
            Self::ParticipantConflict => "participant_conflict",
            Self::UnsupportedParticipantKind => "unsupported_participant_kind",
            Self::DuplicateName => "duplicate_name",
            Self::AdminRpc => "admin_rpc",
            Self::RuntimeStopped => "runtime_stopped",
            Self::Io => "io",
            Self::Cleanup => "cleanup",
        }
    }
}

impl fmt::Display for TrellisTestErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Phase of the testkit state machine that produced an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TrellisTestStage {
    /// Builder input and platform validation.
    Validation,
    /// Loopback port reservation.
    PortAllocation,
    /// Bootstrap configuration generation.
    ConfigGeneration,
    /// Production runtime and managed broker startup and readiness.
    RuntimeStart,
    /// First-administrator bootstrap.
    AdministratorBootstrap,
    /// Administrator login and client connection.
    AdministratorLogin,
    /// Participant installation.
    ParticipantInstallation,
    /// Deployment creation and participant apply.
    DeploymentCreateApply,
    /// Service instance provisioning.
    ServiceProvisioning,
    /// Application/agent caller login.
    ClientLogin,
    /// Runtime shutdown and cleanup.
    Shutdown,
}

impl TrellisTestStage {
    /// Stable machine-readable code for this stage.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::PortAllocation => "port_allocation",
            Self::ConfigGeneration => "config_generation",
            Self::RuntimeStart => "runtime_start",
            Self::AdministratorBootstrap => "administrator_bootstrap",
            Self::AdministratorLogin => "administrator_login",
            Self::ParticipantInstallation => "participant_installation",
            Self::DeploymentCreateApply => "deployment_create_apply",
            Self::ServiceProvisioning => "service_provisioning",
            Self::ClientLogin => "client_login",
            Self::Shutdown => "shutdown",
        }
    }
}

impl fmt::Display for TrellisTestStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// A structured testkit failure.
pub struct TrellisTestError {
    kind: TrellisTestErrorKind,
    stage: TrellisTestStage,
    message: String,
    workdir: Option<PathBuf>,
    server_code: Option<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    cleanup_error: Option<Box<TrellisTestError>>,
}

impl TrellisTestError {
    /// Creates an error for `kind` at `stage`.
    pub(crate) fn new(
        kind: TrellisTestErrorKind,
        stage: TrellisTestStage,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            stage,
            message: message.into(),
            workdir: None,
            server_code: None,
            source: None,
            cleanup_error: None,
        }
    }

    /// Attaches the owned sandbox directory, if one exists.
    #[must_use]
    pub(crate) fn with_workdir(mut self, workdir: impl Into<PathBuf>) -> Self {
        self.workdir = Some(workdir.into());
        self
    }

    /// Attaches a stable server error code.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn with_server_code(mut self, code: impl Into<String>) -> Self {
        self.server_code = Some(code.into());
        self
    }

    /// Attaches a retained source error that carries no secret-bearing payload.
    #[must_use]
    pub(crate) fn with_source(
        mut self,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Attaches a secondary cleanup failure without displacing this error.
    #[must_use]
    pub(crate) fn with_cleanup(mut self, cleanup: TrellisTestError) -> Self {
        self.cleanup_error = Some(Box::new(cleanup));
        self
    }

    /// Broad failure category.
    #[must_use]
    pub fn kind(&self) -> TrellisTestErrorKind {
        self.kind
    }

    /// Testkit stage that produced the failure.
    #[must_use]
    pub fn stage(&self) -> TrellisTestStage {
        self.stage
    }

    /// Human-readable message (contains no secret material).
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Owned sandbox directory, when one was created.
    #[must_use]
    pub fn workdir(&self) -> Option<&Path> {
        self.workdir.as_deref()
    }

    /// Stable server error code, when the failure came from a server response.
    #[must_use]
    pub fn server_code(&self) -> Option<&str> {
        self.server_code.as_deref()
    }

    /// Secondary cleanup failure, when cleanup also failed.
    #[must_use]
    pub fn cleanup_error(&self) -> Option<&TrellisTestError> {
        self.cleanup_error.as_deref()
    }
}

impl fmt::Display for TrellisTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({}): {}",
            self.stage, self.kind, self.message
        )?;
        if let Some(code) = &self.server_code {
            write!(formatter, " [server_code={code}]")?;
        }
        if let Some(workdir) = &self.workdir {
            write!(formatter, " [workdir={}]", workdir.display())?;
        }
        if let Some(cleanup) = &self.cleanup_error {
            write!(formatter, " [cleanup failed: {cleanup}]")?;
        }
        Ok(())
    }
}

impl fmt::Debug for TrellisTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrellisTestError")
            .field("kind", &self.kind)
            .field("stage", &self.stage)
            .field("message", &self.message)
            .field("server_code", &self.server_code)
            .field("workdir", &self.workdir)
            .field(
                "cleanup_error",
                &self.cleanup_error.as_ref().map(|e| e.kind()),
            )
            .finish()
    }
}

impl std::error::Error for TrellisTestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for TrellisTestError {
    fn from(source: std::io::Error) -> Self {
        let message = source.to_string();
        Self::new(
            TrellisTestErrorKind::Io,
            TrellisTestStage::Validation,
            message,
        )
        .with_source(source)
    }
}
