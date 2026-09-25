//! Reusable Trellis OpenTelemetry ownership and instrumentation.
//!
//! The crate's instrumentation only uses the OpenTelemetry API: importing
//! Trellis never installs providers or starts network activity. Native
//! processes that want Trellis-owned OTLP export enable the non-default
//! `telemetry-otlp` feature and call [`init_from_env`] once at process entry.
//! Applications that already own OpenTelemetry call [`use_host_providers`]
//! instead, after their own providers are installed and before their first
//! Trellis connection or mount.
//!
//! Telemetry is never authority: observations follow completed decisions, may
//! be lost, and never change an authorization, readiness, or durability
//! outcome.

pub mod instruments;
pub mod lifecycle;
pub mod propagation;

#[cfg(test)]
pub(crate) mod capture;

#[cfg(feature = "telemetry-otlp")]
pub mod export;

#[doc(hidden)]
pub mod internal {
    pub use super::instruments::*;
    pub use super::lifecycle::*;
    pub use super::propagation::*;
}

use std::sync::{Mutex, OnceLock};

/// Document-hidden tracer/instrument scope name for Trellis instrumentation.
pub const INSTRUMENTATION_SCOPE: &str = "@oatscenter/trellis";

/// Attribute pair for host-side instrumentation call sites.
pub use opentelemetry::KeyValue;

/// Role of the process that owns the telemetry providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryRole {
    /// The bundled Trellis server process.
    Server,
    /// The Trellis CLI.
    Cli,
    /// A connected Trellis service or application host.
    Service,
    /// A connected device or device companion host.
    Device,
}

impl TelemetryRole {
    /// Stable resource attribute value for this role.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Cli => "cli",
            Self::Service => "service",
            Self::Device => "device",
        }
    }
}

/// Process identity defaults for one telemetry owner.
///
/// Application hosts provide a stable service name before opening
/// connections. Environment variables can override the deployment identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TelemetryIdentity {
    /// Stable service name for the deployment.
    pub service_name: String,
    /// Service version reported as `service.version`.
    pub service_version: String,
    /// Role of the process that owns the providers.
    pub role: TelemetryRole,
}

impl TelemetryIdentity {
    /// Builds one process identity.
    pub fn new(
        service_name: impl Into<String>,
        role: TelemetryRole,
        service_version: impl Into<String>,
    ) -> Self {
        Self {
            service_name: service_name.into(),
            service_version: service_version.into(),
            role,
        }
    }
}

/// Records the process-wide provider ownership decision.
#[derive(Clone)]
enum GuardMode {
    /// No providers were installed by Trellis.
    Disabled,
    /// A host application owns the providers.
    HostOwned {
        metrics_enabled: bool,
        traces_enabled: bool,
    },
    /// Trellis installed owned providers.
    #[cfg(feature = "telemetry-otlp")]
    Owned(std::sync::Arc<export::OwnedProviders>),
}

static MODE: OnceLock<GuardMode> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Whether any process telemetry mode has been claimed and metrics are on.
///
/// Runtime samplers consult this before polling so a disabled or unconfigured
/// process performs no telemetry database or broker work at all.
pub fn process_metrics_enabled() -> bool {
    MODE.get().is_some_and(|mode| match mode {
        GuardMode::Disabled => false,
        GuardMode::HostOwned {
            metrics_enabled, ..
        } => *metrics_enabled,
        #[cfg(feature = "telemetry-otlp")]
        GuardMode::Owned(providers) => providers.metrics_enabled,
    })
}

/// Claims the process telemetry mode exactly once.
fn claim_mode(build: impl FnOnce() -> GuardMode) -> GuardMode {
    if let Some(mode) = MODE.get() {
        return mode.clone();
    }
    let _guard = INIT_LOCK.lock().expect("telemetry init lock");
    if let Some(mode) = MODE.get() {
        return mode.clone();
    }
    let mode = build();
    let _ = MODE.set(mode);
    MODE.get().expect("telemetry mode installed").clone()
}

/// Process telemetry handle returned by an initialization entrypoint.
///
/// Dropping the guard does not shut providers down; call [`Self::shutdown`] or
/// [`Self::force_flush`] from the process stop path.
#[derive(Clone)]
pub struct TelemetryGuard {
    mode: GuardMode,
}

impl TelemetryGuard {
    /// A handle for a process that does not export telemetry.
    pub fn disabled() -> Self {
        Self {
            mode: GuardMode::Disabled,
        }
    }

    /// Records an explicit host-owned provider mode without installing or
    /// shutting down any global provider. Idempotent per process.
    pub fn use_host_providers(
        identity: TelemetryIdentity,
        metrics_enabled: bool,
        traces_enabled: bool,
    ) -> Self {
        let _ = identity;
        let mode = claim_mode(|| GuardMode::HostOwned {
            metrics_enabled,
            traces_enabled,
        });
        if metrics_enabled {
            // The host installed its meter provider before connecting; rebind
            // observable families so instruments created before that install
            // export through the host meter like Trellis-owned initialization.
            instruments::note_meter_provider_changed();
        }
        Self { mode }
    }

    /// Whether Trellis-owned or host-owned metric instruments are active.
    pub fn metrics_enabled(&self) -> bool {
        match &self.mode {
            GuardMode::Disabled => false,
            GuardMode::HostOwned {
                metrics_enabled, ..
            } => *metrics_enabled,
            #[cfg(feature = "telemetry-otlp")]
            GuardMode::Owned(providers) => providers.metrics_enabled,
        }
    }

    /// Whether Trellis-owned or host-owned tracing is active.
    pub fn traces_enabled(&self) -> bool {
        match &self.mode {
            GuardMode::Disabled => false,
            GuardMode::HostOwned { traces_enabled, .. } => *traces_enabled,
            #[cfg(feature = "telemetry-otlp")]
            GuardMode::Owned(providers) => providers.traces_enabled,
        }
    }

    /// Optional owned tracer for attaching the `tracing_opentelemetry` layer.
    ///
    /// Host-owned and disabled modes return `None`: an embedding application
    /// keeps responsibility for its own layers.
    #[cfg(feature = "telemetry-otlp")]
    pub fn tracer(&self) -> Option<opentelemetry_sdk::trace::Tracer> {
        match &self.mode {
            GuardMode::Owned(providers) => providers.tracer(),
            GuardMode::Disabled | GuardMode::HostOwned { .. } => None,
        }
    }

    /// Flushes owned providers within the process telemetry budget.
    ///
    /// Never fails business work and never blocks a Tokio worker thread.
    pub async fn force_flush(&self) {
        #[cfg(feature = "telemetry-otlp")]
        if let GuardMode::Owned(providers) = &self.mode {
            export::flush_bounded(providers).await;
        }
    }

    /// Flushes and shuts down owned providers within the process budget.
    pub async fn shutdown(&self) {
        #[cfg(feature = "telemetry-otlp")]
        if let GuardMode::Owned(providers) = &self.mode {
            export::shutdown_bounded(providers).await;
        }
    }

    /// Whether the process telemetry mode has been claimed.
    pub fn is_initialized(&self) -> bool {
        MODE.get().is_some()
    }
}

/// Installs Trellis-owned OTLP providers from the standard environment.
///
/// Only available with the `telemetry-otlp` feature. Initialization is
/// idempotent: later calls reuse the process providers and never replace them.
#[cfg(feature = "telemetry-otlp")]
pub fn init_from_env(identity: TelemetryIdentity) -> TelemetryGuard {
    TelemetryGuard {
        mode: claim_mode(|| GuardMode::Owned(std::sync::Arc::new(export::build_owned(&identity)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_guard_never_reports_active_signals() {
        let guard = TelemetryGuard::disabled();
        assert!(!guard.metrics_enabled());
        assert!(!guard.traces_enabled());
        guard.force_flush().await;
        guard.shutdown().await;
    }
}
