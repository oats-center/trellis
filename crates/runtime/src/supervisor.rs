#![cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use trellis_rs::__internal::RuntimeOwner as _;

use serde::Serialize;
use thiserror::Error;
use tokio_util::task::AbortOnDropHandle;
use ulid::Ulid;

// Twice the listener's drain bound, so a connection it closes at that bound
// still lets the server finish within this one.
const HTTP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const SUBSYSTEM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const OWNERSHIP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const NATS_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

use crate::leases::{LeaseError, LeaseKey};
use crate::ownership::{OwnerContext, OwnerGroup, RuntimeOwnership};
use crate::resources::{stream_is_compatible, ExpectedRuntimeResources};
use crate::shutdown::StopHandle;
use crate::storage::{RuntimeStores, StoreError};
use crate::{
    events, health, jobs, platform, RuntimeConfig, RuntimeMode, ServerError, SubsystemName,
};

/// Replacement for configured NATS endpoints used by managed `trellis-server` startup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsEndpointOverride {
    /// Native NATS connectivity used by runtime internals, never a client proxy;
    /// omission dials the configured servers.
    pub runtime_servers: Option<String>,
    /// Advertised native client endpoints; omission retains configured hints.
    pub advertised_native: Option<Vec<String>>,
    /// Advertised WebSocket client endpoints; omission retains configured hints.
    pub advertised_websocket: Option<Vec<String>>,
}

/// Runtime startup options for `trellis-server`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeOptions {
    /// Runtime mode selected by the command line.
    pub mode: RuntimeMode,
    /// Runtime configuration already loaded with host-selected path defaults.
    pub config: RuntimeConfig,
    /// Issue a one-time password-reset URL for the sole active administrator.
    pub reset_admin: bool,
    /// Optional replacement for the configured NATS endpoints (the managed local NATS
    /// server used by `trellis-server`). `None` keeps the configured values.
    pub nats_override: Option<NatsEndpointOverride>,
}

/// Status of one read-only runtime preflight check.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCheckStatus {
    /// The required condition is satisfied.
    Ok,
    /// The condition is safe but needs operator attention.
    Warning,
    /// A required resource does not exist.
    Missing,
    /// Existing state is incompatible with this runtime.
    Incompatible,
    /// Trust material has expired.
    Expired,
    /// A monotonic trust floor would move backwards.
    Rollback,
    /// Validation could not complete.
    Error,
}

/// Result of one named runtime preflight check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCheckResult {
    /// Stable check name.
    pub name: String,
    /// Machine-readable status.
    pub status: RuntimeCheckStatus,
    /// Operator-readable result without secrets.
    pub detail: String,
}

/// Complete structured report returned by [`check`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCheckReport {
    /// Whether every required check succeeded.
    pub valid: bool,
    /// Runtime mode that was checked.
    pub mode: String,
    /// Configuration path that was checked.
    pub config: PathBuf,
    /// Ordered per-check results.
    pub checks: Vec<RuntimeCheckResult>,
}

impl RuntimeCheckReport {
    fn push(&mut self, name: &str, status: RuntimeCheckStatus, detail: impl Into<String>) {
        if !matches!(status, RuntimeCheckStatus::Ok | RuntimeCheckStatus::Warning) {
            self.valid = false;
        }
        self.checks.push(RuntimeCheckResult {
            name: name.to_owned(),
            status,
            detail: detail.into(),
        });
    }
}

/// Validate host-resolved configuration, migrations, NATS, trust, and registries.
pub async fn check(
    mode: RuntimeMode,
    config_path: PathBuf,
    config: RuntimeConfig,
    nats_servers: Option<&str>,
) -> Result<RuntimeCheckReport, RuntimeError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut report = RuntimeCheckReport {
        valid: true,
        mode: mode.to_string(),
        config: config_path.clone(),
        checks: Vec::new(),
    };
    let config = match config.validate_for_mode(mode) {
        Ok(()) => {
            report.push("config", RuntimeCheckStatus::Ok, "configuration is valid");
            config
        }
        Err(error) => {
            report.push("config", RuntimeCheckStatus::Error, error.to_string());
            return Ok(report);
        }
    };
    let stores = match RuntimeStores::from_config(&config, mode) {
        Ok(stores) => stores,
        Err(error) => {
            report.push("migrations", RuntimeCheckStatus::Error, error.to_string());
            return Ok(report);
        }
    };
    match stores.check_all() {
        Ok(()) => report.push(
            "migrations",
            RuntimeCheckStatus::Ok,
            "configured databases accept all pending migrations on temporary copies",
        ),
        Err(error) => {
            let status = match error {
                StoreError::MissingSqlite { .. } => RuntimeCheckStatus::Missing,
                StoreError::SqliteSnapshotChanged { .. } => RuntimeCheckStatus::Error,
                _ => RuntimeCheckStatus::Incompatible,
            };
            report.push("migrations", status, error.to_string());
            return Ok(report);
        }
    }
    let nats = match config.resolve_nats_runtime_with(nats_servers) {
        Ok(nats) => nats,
        Err(error) => {
            report.push("nats.config", RuntimeCheckStatus::Error, error.to_string());
            return Ok(report);
        }
    };
    let trellis_nats = match check_nats_connection(&nats, &nats.trellis_creds).await {
        Ok(client) => {
            report.push(
                "nats.trellis",
                RuntimeCheckStatus::Ok,
                "connection and flush succeeded",
            );
            client
        }
        Err(error) => {
            report.push("nats.trellis", RuntimeCheckStatus::Error, error.to_string());
            return Ok(report);
        }
    };
    let jetstream = async_nats::jetstream::new(trellis_nats.clone());
    let expected_resources = ExpectedRuntimeResources::for_mode(
        mode,
        &config,
        async_nats::jetstream::stream::StorageType::File,
    );
    for expected in expected_resources.streams() {
        let check_name = format!("nats.stream.{}", expected.name.to_ascii_lowercase());
        match jetstream.get_stream(&expected.name).await {
            Ok(mut stream) => match stream.info().await {
                Ok(info) if stream_is_compatible(&info.config, expected) => report.push(
                    &check_name,
                    RuntimeCheckStatus::Ok,
                    "stream exists with compatible Trellis-owned policy",
                ),
                Ok(_) => report.push(
                    &check_name,
                    RuntimeCheckStatus::Incompatible,
                    "stream policy differs from the selected runtime mode",
                ),
                Err(error) => {
                    report.push(&check_name, RuntimeCheckStatus::Error, error.to_string())
                }
            },
            Err(error) => report.push(&check_name, RuntimeCheckStatus::Missing, error.to_string()),
        }
    }
    let leases = config.resolve_leases()?;
    match jetstream.get_key_value(&leases.bucket).await {
        Ok(store) => match store.status().await {
            Ok(status)
                if status.max_age() == Duration::from_millis(leases.ttl_ms)
                    && (leases.replicas == 0
                        || status.info.config.num_replicas == usize::from(leases.replicas)) =>
            {
                report.push(
                    "nats.kv.leases",
                    RuntimeCheckStatus::Ok,
                    "lease bucket exists with compatible TTL and replicas",
                );
            }
            Ok(_) => report.push(
                "nats.kv.leases",
                RuntimeCheckStatus::Incompatible,
                "lease bucket settings differ from runtime configuration",
            ),
            Err(error) => report.push(
                "nats.kv.leases",
                RuntimeCheckStatus::Error,
                error.to_string(),
            ),
        },
        Err(error) => report.push(
            "nats.kv.leases",
            RuntimeCheckStatus::Missing,
            error.to_string(),
        ),
    }
    if matches!(mode, RuntimeMode::All | RuntimeMode::Platform) {
        let keys = crate::platform::callout_keys(&config)?;
        config.resolve_authorization()?;
        if let Err(error) = crate::platform::auth::issuer_signing_key(&config) {
            report.push(
                "authorization.issuer",
                RuntimeCheckStatus::Error,
                error.to_string(),
            );
            return Ok(report);
        }
        report.push(
            "authorization.issuer",
            RuntimeCheckStatus::Ok,
            "configured issuer signing material is valid",
        );
        if let Err(error) = crate::platform::auth::oauth_storage_key(&config) {
            report.push(
                "authorization.oauth_custody",
                RuntimeCheckStatus::Error,
                error.to_string(),
            );
            return Ok(report);
        }
        report.push(
            "authorization.oauth_custody",
            RuntimeCheckStatus::Ok,
            "configured OAuth storage custody is valid",
        );
        if stores.platform()?.exists() {
            let (platform, store_config, account) = (
                stores.platform()?.clone(),
                config.clone(),
                keys.target_account().to_owned(),
            );
            let issuer = crate::platform::auth::blocking(move || {
                let auth_store = crate::platform::auth::SqliteAuthorizationStore::open_read_only(
                    &platform,
                    &store_config,
                    &account,
                )?;
                Ok::<_, RuntimeError>(auth_store.runtime_authorization_trust())
            })
            .await?;
            match issuer {
                Ok(_) => report.push(
                    "authorization.issuer_record",
                    RuntimeCheckStatus::Ok,
                    "configured public issuer key is retained and active",
                ),
                Err(crate::platform::auth::AuthError::Denied) => report.push(
                    "authorization.issuer_record",
                    RuntimeCheckStatus::Incompatible,
                    "configured issuer is not the active retained signing key",
                ),
                Err(error) => report.push(
                    "authorization.issuer_record",
                    RuntimeCheckStatus::Error,
                    error.to_string(),
                ),
            }
        }
        for name in [
            crate::platform::auth::MATERIAL_BUCKET,
            crate::platform::auth::REVOCATION_BUCKET,
        ] {
            let check = format!("authorization.registry.{name}");
            match jetstream.get_key_value(name).await {
                Ok(bucket) => match bucket.status().await {
                    Ok(status)
                        if status.info.config.storage
                            == async_nats::jetstream::stream::StorageType::File
                            && status.max_age().is_zero() =>
                    {
                        report.push(
                            &check,
                            RuntimeCheckStatus::Ok,
                            "durable nonexpiring verification evidence is retained",
                        )
                    }
                    Ok(_) => report.push(
                        &check,
                        RuntimeCheckStatus::Incompatible,
                        "verification evidence requires durable nonexpiring storage",
                    ),
                    Err(error) => report.push(&check, RuntimeCheckStatus::Error, error.to_string()),
                },
                Err(error) => report.push(&check, RuntimeCheckStatus::Missing, error.to_string()),
            }
        }
        let (auth_creds, system_creds) = nats.platform_creds()?;
        for (name, credentials) in [("nats.auth", auth_creds), ("nats.system", system_creds)] {
            match check_nats_connection(&nats, credentials).await {
                Ok(client) => {
                    let _ = client.drain().await;
                    report.push(
                        name,
                        RuntimeCheckStatus::Ok,
                        "connection and flush succeeded",
                    );
                }
                Err(error) => report.push(name, RuntimeCheckStatus::Error, error.to_string()),
            }
        }
    }
    trellis_nats
        .drain()
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    Ok(report)
}

/// Options for the runtime's own connections to `nats` as `credentials`.
pub(crate) fn nats_connect_options(
    nats: &crate::config::ResolvedRuntimeNatsConfig,
    credentials: &str,
) -> Result<async_nats::ConnectOptions, RuntimeError> {
    let options = async_nats::ConnectOptions::with_credentials(credentials)
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    let roots: Vec<_> = nats.ca_certificate.iter().cloned().collect();
    Ok(match &nats.tls_server_name {
        Some(name) => options.tls_client_config(
            trellis_rs::__internal::named_server_tls(&roots, name)
                .map_err(|error| RuntimeError::Nats(error.to_string()))?,
        ),
        None => roots
            .into_iter()
            .fold(options, async_nats::ConnectOptions::add_root_certificates),
    })
}

async fn check_nats_connection(
    nats: &crate::config::ResolvedRuntimeNatsConfig,
    credentials: &str,
) -> Result<async_nats::Client, RuntimeError> {
    let options = nats_connect_options(nats, credentials)?;
    let client = tokio::time::timeout(Duration::from_secs(10), options.connect(nats.server_urls()))
        .await
        .map_err(|_| RuntimeError::Nats("connection timed out".to_owned()))?
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    Ok(client)
}

/// Error returned while starting or running the Trellis runtime.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Runtime configuration could not be loaded or validated.
    #[error(transparent)]
    Config(#[from] crate::ConfigError),
    /// Runtime HTTP server failed.
    #[error(transparent)]
    Server(#[from] ServerError),
    /// Runtime storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Runtime NATS connection failed.
    #[error("runtime NATS connection failed: {0}")]
    Nats(String),
    /// An existing runtime-owned stream cannot be changed without changing its identity or
    /// reducing its retained data.
    #[error("incompatible runtime stream {name}: {reason}")]
    IncompatibleResource {
        /// Existing stream name.
        name: String,
        /// Configuration incompatibility.
        reason: &'static str,
    },
    /// The final runtime NATS flush failed during shutdown.
    #[error("runtime NATS flush failed during shutdown: {0}")]
    NatsFlush(String),
    /// The final runtime NATS flush exceeded its shutdown bound.
    #[error("runtime NATS flush did not complete within the shutdown bound")]
    NatsFlushTimeout,
    /// The runtime lease KV bucket could not be opened or created.
    #[error("failed to open runtime lease bucket for owner {owner_id}: {source}")]
    LeaseBucketOpen {
        /// Process-unique owner identity.
        owner_id: String,
        /// Low-level lease failure.
        #[source]
        source: Box<LeaseError>,
    },
    /// A selected singleton owner lease is already held.
    #[error(
        "runtime owner lease {key:?} for {subsystem} is already held; owner \
        {owner_id} cannot start: {source}"
    )]
    OwnerHeld {
        /// Selected subsystem owner group.
        subsystem: SubsystemName,
        /// Stable owner lease key.
        key: LeaseKey,
        /// Process-unique owner identity.
        owner_id: String,
        /// Low-level held error.
        #[source]
        source: Box<LeaseError>,
    },
    /// A selected singleton owner lease could not be acquired.
    #[error(
        "failed to acquire runtime owner lease {key:?} for {subsystem} as {owner_id}: {source}"
    )]
    OwnerAcquire {
        /// Selected subsystem owner group.
        subsystem: SubsystemName,
        /// Stable owner lease key.
        key: LeaseKey,
        /// Process-unique owner identity.
        owner_id: String,
        /// Low-level lease failure.
        #[source]
        source: Box<LeaseError>,
    },
    /// A held owner lease was lost or could not be renewed.
    #[error("runtime owner lease {key:?} for {subsystem} was lost by {owner_id}: {source}")]
    OwnerRenewal {
        /// Selected subsystem owner group.
        subsystem: SubsystemName,
        /// Stable owner lease key.
        key: LeaseKey,
        /// Process-unique owner identity.
        owner_id: String,
        /// Low-level lease failure.
        #[source]
        source: Box<LeaseError>,
    },
    /// A complete owner renewal round exceeded the configured renew interval.
    #[error("runtime owner renewal round timed out for {owner_id}")]
    OwnerRenewalRoundTimeout {
        /// Process-unique owner identity.
        owner_id: String,
    },
    /// The critical owner renewal task exited without a stop request.
    #[error("runtime owner renewal task exited unexpectedly for {owner_id}")]
    OwnerRenewalTaskExited {
        /// Process-unique owner identity.
        owner_id: String,
    },
    /// The critical owner renewal task panicked or was cancelled.
    #[error("runtime owner renewal task failed for {owner_id}: {source}")]
    OwnerRenewalTaskFailed {
        /// Process-unique owner identity.
        owner_id: String,
        /// Tokio task join failure.
        #[source]
        source: tokio::task::JoinError,
    },
    /// The ownership renewal task did not stop within the shutdown bound.
    #[error("runtime owner renewal task did not stop within the shutdown bound for {owner_id}")]
    OwnerRenewalShutdownTimeout {
        /// Process-unique owner identity.
        owner_id: String,
    },
    /// A held owner lease could not be released safely.
    #[error(
        "failed to release runtime owner lease {key:?} for {subsystem} as {owner_id}: {source}"
    )]
    OwnerRelease {
        /// Selected subsystem owner group.
        subsystem: SubsystemName,
        /// Stable owner lease key.
        key: LeaseKey,
        /// Process-unique owner identity.
        owner_id: String,
        /// Low-level lease failure.
        #[source]
        source: Box<LeaseError>,
    },
    /// A selected subsystem was not given its acquired owner context.
    #[error("runtime owner context is missing for {subsystem}")]
    OwnerContextMissing {
        /// Selected subsystem owner group.
        subsystem: SubsystemName,
    },
    /// Health subsystem setup or runtime failed.
    #[error("health subsystem failed: {0}")]
    Health(String),
    /// Platform authorization setup or runtime failed.
    #[error("platform subsystem failed: {0}")]
    Platform(String),
    /// A subsystem scaffold failed to start.
    #[error("failed to start runtime subsystem {subsystem}: {reason}")]
    Subsystem {
        /// Subsystem that failed to start.
        subsystem: SubsystemName,
        /// Failure reason.
        reason: &'static str,
    },
    /// A subsystem task failed after startup.
    #[error("runtime subsystem {subsystem} task failed: {source}")]
    SubsystemTask {
        /// Subsystem whose task failed.
        subsystem: SubsystemName,
        /// Tokio task join failure.
        #[source]
        source: tokio::task::JoinError,
    },
    /// A long-running subsystem exited without a stop request.
    #[error("runtime subsystem {subsystem} exited unexpectedly")]
    SubsystemExited {
        /// Subsystem that stopped unexpectedly.
        subsystem: SubsystemName,
    },
    /// The runtime HTTP server did not drain within the shutdown bound.
    #[error("runtime HTTP server did not drain within the shutdown bound")]
    HttpShutdownTimeout,
    /// A subsystem did not stop within the cooperative shutdown bound.
    #[error("runtime subsystem {subsystem} did not stop within the shutdown bound")]
    SubsystemShutdownTimeout {
        /// Subsystem that exceeded the shutdown bound.
        subsystem: SubsystemName,
    },
}

/// Shared runtime context passed to subsystem startup scaffolds.
pub(crate) struct RuntimeContext {
    /// Loaded and validated runtime configuration.
    pub(crate) config: RuntimeConfig,
    /// Runtime mode selected for this process.
    pub(crate) mode: RuntimeMode,
    /// Whether the next platform start must rotate the pending first-administrator flow.
    reset_admin: std::sync::atomic::AtomicBool,
    /// Optional replacement for the configured NATS endpoints (managed local NATS).
    pub(crate) nats_override: Option<NatsEndpointOverride>,
    /// SQLite stores opened for the selected subsystems.
    pub(crate) stores: RuntimeStores,
    /// Trellis-account runtime NATS client shared by built-in subsystems.
    pub(crate) trellis_nats: async_nats::Client,
    /// Owner contexts for the groups this process currently owns.
    owners: std::sync::Mutex<BTreeMap<OwnerGroup, OwnerContext>>,
    /// Routes the starting platform registers, before they are served.
    http_router: std::sync::Mutex<axum::Router>,
    /// Routes of the running platform, served by the process-wide listener.
    platform_router: Arc<std::sync::RwLock<Option<axum::Router>>>,
    /// Whether this process serves, reported by every listener's `/readyz`.
    pub(crate) http_ready: Arc<std::sync::atomic::AtomicBool>,
    /// The running platform's aggregate store; other modes may only read provisioned identity.
    pub(crate) authorization_store:
        std::sync::Mutex<Option<Arc<crate::platform::auth::SqliteAuthorizationStore>>>,
    /// Per-role delivery slots for built-in live provider owners.
    pub(crate) live_providers: crate::platform::LiveProviderSlots,
    /// Where each selected owner group stands.
    groups: GroupStates,
}

impl std::fmt::Debug for RuntimeContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeContext")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl RuntimeContext {
    pub(crate) fn owner(&self, group: OwnerGroup) -> Result<OwnerContext, RuntimeError> {
        self.owners
            .lock()
            .ok()
            .and_then(|owners| owners.get(&group).cloned())
            .ok_or(RuntimeError::OwnerContextMissing {
                subsystem: group.subsystem(),
            })
    }

    /// Whether this platform start rotates the first-administrator flow; true once per process.
    pub(crate) fn take_reset_admin(&self) -> bool {
        self.reset_admin
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn register_http_router(&self, router: axum::Router) -> Result<(), RuntimeError> {
        let mut registered = self
            .http_router
            .lock()
            .map_err(|_| RuntimeError::Platform("HTTP router lock poisoned".to_owned()))?;
        *registered = std::mem::take(&mut *registered).merge(router);
        Ok(())
    }

    fn take_http_router(&self) -> Result<axum::Router, RuntimeError> {
        let mut registered = self
            .http_router
            .lock()
            .map_err(|_| RuntimeError::Platform("HTTP router lock poisoned".to_owned()))?;
        Ok(std::mem::take(&mut *registered))
    }

    fn serve_platform_routes(&self, router: Option<axum::Router>) {
        if let Ok(mut served) = self.platform_router.write() {
            *served = router;
        }
    }
}

/// Where one owner group stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupState {
    /// Taking its lease or starting its work.
    Starting,
    /// Doing its work.
    Running,
    /// Waiting for its lease while another runtime or a broker failure keeps it.
    StandingBy,
}

/// Every selected owner group's state, and the process readiness it implies.
///
/// The process is ready once each group has first settled, while at least one
/// group runs and, when the platform is selected, the platform runs: its routes
/// are the only ones the listener serves. A group restarting on its own after a
/// lost lease leaves the others, and readiness, alone.
struct GroupStates {
    states: tokio::sync::watch::Sender<BTreeMap<OwnerGroup, GroupState>>,
    ready: Arc<std::sync::atomic::AtomicBool>,
    components: Arc<crate::telemetry::snapshots::ComponentReadiness>,
    settled: std::sync::atomic::AtomicBool,
    stopping: std::sync::atomic::AtomicBool,
}

impl GroupStates {
    fn new(mode: RuntimeMode, ready: Arc<std::sync::atomic::AtomicBool>) -> Self {
        let states = OwnerGroup::for_mode(mode)
            .iter()
            .map(|&group| (group, GroupState::Starting))
            .collect();
        Self {
            states: tokio::sync::watch::channel(states).0,
            ready,
            components: Arc::default(),
            settled: std::sync::atomic::AtomicBool::new(false),
            stopping: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn set(&self, group: OwnerGroup, state: GroupState) {
        use std::sync::atomic::Ordering::SeqCst;
        self.states.send_modify(|states| {
            states.insert(group, state);
        });
        self.components.set(
            component_label(group.subsystem()),
            if state == GroupState::Running {
                1.0
            } else {
                0.0
            },
        );
        let states = self.states.borrow();
        if !states.values().any(|state| *state == GroupState::Starting) {
            self.settled.store(true, SeqCst);
        }
        let platform = states
            .get(&OwnerGroup::Platform)
            .is_none_or(|state| *state == GroupState::Running);
        let running = states.values().any(|state| *state == GroupState::Running);
        self.ready.store(
            !self.stopping.load(SeqCst) && self.settled.load(SeqCst) && platform && running,
            SeqCst,
        );
    }

    /// Marks the process stopping; it is not ready from here on.
    fn stop(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.stopping.store(true, SeqCst);
        self.ready.store(false, SeqCst);
        for name in self.states.borrow().keys() {
            self.components.set(component_label(name.subsystem()), 0.0);
        }
    }

    /// Waits until `group` has started or stands by.
    async fn settled(&self, group: OwnerGroup) {
        let mut states = self.states.subscribe();
        let _ = states
            .wait_for(|states| states.get(&group) != Some(&GroupState::Starting))
            .await;
    }
}

impl Drop for RuntimeContext {
    fn drop(&mut self) {
        self.live_providers.abort();
    }
}

/// Handle for a started subsystem scaffold.
#[derive(Debug)]
pub(crate) struct SubsystemHandle {
    /// Started subsystem name.
    pub(crate) name: SubsystemName,
    /// Cooperative stop request handle for the subsystem task.
    pub(crate) stop: StopHandle,
    /// Join handle for the subsystem task.
    pub(crate) join: AbortOnDropHandle<Result<(), RuntimeError>>,
}

impl Drop for SubsystemHandle {
    fn drop(&mut self) {
        self.stop.stop();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeStopCause {
    Signal,
    HttpFinished,
    SubsystemFailed,
    OwnershipLost,
}

/// Loads configuration, validates selected subsystem storage, and runs the runtime.
pub async fn run(options: RuntimeOptions) -> Result<(), RuntimeError> {
    run_with_stop(options, None).await
}

/// Runs the runtime until the host shutdown signal or the owner's stop handle fires.
///
/// An owner-supplied handle lets an in-process caller (a test harness owning the runtime,
/// a supervisor restarting it) request the same cooperative shutdown a host signal triggers.
///
/// Each selected owner group runs on its own: it takes its lease, starts its
/// subsystem and built-in provider, and on losing the lease stops that work and
/// stands by to take it back, while the other groups and the listener carry on.
pub async fn run_with_stop(
    options: RuntimeOptions,
    stop: Option<crate::shutdown::StopHandle>,
) -> Result<(), RuntimeError> {
    // NATS TLS connects before the OAuth/Console listeners initialize rustls.
    // Select our provider before any TLS consumer builds a client configuration.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = options.config;
    config.validate_for_mode(options.mode)?;
    if let Some(notice) =
        crate::tls::GeneratedTlsNotice::from_config(&config, config.http_transport())
            .map_err(|error| RuntimeError::Platform(error.to_string()))?
    {
        notice.warn();
    }
    let nats = config.resolve_nats_runtime_with(
        options
            .nats_override
            .as_ref()
            .and_then(|o| o.runtime_servers.as_deref()),
    )?;
    let leases = config.resolve_leases()?;
    let trellis_nats = nats_connect_options(&nats, &nats.trellis_creds)?
        .connect(nats.server_urls())
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    let owner_id = format!(
        "{}-{}",
        config.instance_name.as_deref().unwrap_or("trellis-runtime"),
        Ulid::generate()
    );
    let manager = RuntimeOwnership::open(trellis_nats.clone(), &leases, owner_id).await?;
    // Register the OS shutdown signal before any listener becomes observable. A
    // host can connect to a bound port immediately, so a SIGTERM delivered
    // during startup must already stop the runtime cooperatively instead of
    // terminating it with its leases held. One stop serves the whole process, so
    // a signal is observed whether a group is serving or standing by.
    let process_stop = StopHandle::new();
    let signal = shutdown_or_stop(stop);
    let _signal = {
        let mut signal = Box::pin(signal);
        std::future::poll_fn(|cx| {
            let _ = Future::poll(signal.as_mut(), cx);
            std::task::Poll::Ready(())
        })
        .await;
        let process_stop = process_stop.clone();
        AbortOnDropHandle::new(tokio::spawn(async move {
            signal.await;
            process_stop.stop();
        }))
    };
    // Streams and local stores belong to the process, whichever groups it owns.
    ExpectedRuntimeResources::for_mode(
        options.mode,
        &config,
        async_nats::jetstream::stream::StorageType::File,
    )
    .converge_streams(trellis_nats.clone())
    .await?;
    let stores = RuntimeStores::from_config(&config, options.mode)?;
    stores.migrate_all()?;
    let listener = crate::bind_http_listener(&config).await?;
    let origin = url::Url::parse(&config.public_origin()).map_err(|error| {
        RuntimeError::Config(crate::ConfigError::InvalidPublicOrigin {
            origin: format!("{}: {error}", config.public_origin()),
        })
    })?;
    let http_ready = Arc::<std::sync::atomic::AtomicBool>::default();
    let context = Arc::new(RuntimeContext {
        groups: GroupStates::new(options.mode, Arc::clone(&http_ready)),
        config,
        mode: options.mode,
        reset_admin: std::sync::atomic::AtomicBool::new(options.reset_admin),
        nats_override: options.nats_override,
        stores,
        trellis_nats: trellis_nats.clone(),
        owners: std::sync::Mutex::default(),
        http_router: std::sync::Mutex::new(axum::Router::new()),
        platform_router: Arc::default(),
        http_ready,
        authorization_store: std::sync::Mutex::default(),
        live_providers: crate::platform::LiveProviderSlots::new(),
    });
    let result = serve(&context, &manager, listener, &origin, &process_stop).await;
    tracing::info!("runtime shutdown finished; flushing NATS");
    let flush_result = bounded_flush(trellis_nats.flush(), NATS_FLUSH_TIMEOUT).await;
    tracing::info!("runtime NATS flush finished");
    preserve_primary(result, Ok(()), flush_result)
}

async fn bounded_flush<F, E>(flush: F, timeout: Duration) -> Result<(), RuntimeError>
where
    F: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(timeout, flush).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(RuntimeError::NatsFlush(error.to_string())),
        Err(_) => Err(RuntimeError::NatsFlushTimeout),
    }
}

fn preserve_primary(
    result: Result<(), RuntimeError>,
    release: Result<(), RuntimeError>,
    flush: Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    let mut primary = result.err();
    for (secondary, message) in [
        (release.err(), "runtime ownership shutdown also failed"),
        (flush.err(), "runtime NATS flush also failed"),
    ] {
        if let Some(error) = secondary {
            if primary.is_some() {
                tracing::error!(error = %error, "{message}");
            } else {
                primary = Some(error);
            }
        }
    }
    primary.map_or(Ok(()), Err)
}

/// Resolves when the host shutdown signal fires or the owner requests a stop. With no owner
/// handle the signal is the only trigger, preserving host behavior exactly.
async fn shutdown_or_stop(stop: Option<crate::shutdown::StopHandle>) {
    match stop {
        Some(handle) => {
            tokio::select! {
                () = crate::shutdown::shutdown_signal() => {}
                () = handle.stopped() => {}
            }
        }
        None => crate::shutdown::shutdown_signal().await,
    }
}

/// Serves the listener and runs every selected owner group until the process
/// stops or one of them fails for a reason ownership cannot explain.
async fn serve(
    context: &Arc<RuntimeContext>,
    manager: &crate::leases::LeaseManager,
    listener: tokio::net::TcpListener,
    origin: &url::Url,
    stop: &StopHandle,
) -> Result<(), RuntimeError> {
    let _component_sampler = crate::telemetry::snapshots::spawn_component_sampler(
        context
            .mode
            .subsystems()
            .iter()
            .map(|&subsystem| component_label(subsystem))
            .collect(),
        Arc::clone(&context.groups.components),
        stop.clone(),
    );
    let server_stop = stop.clone();
    let mut server = std::pin::pin!(async {
        // The bound port accepts once the platform's routes are served or it
        // stands by, so an open port never means routes still to come.
        if context.mode.subsystems().contains(&SubsystemName::Platform) {
            tokio::select! {
                () = context.groups.settled(OwnerGroup::Platform) => {}
                () = stop.stopped() => return Ok(()),
            }
        }
        crate::serve_http_listener(
            listener,
            origin,
            context.config.http_transport(),
            context.config.http_trusted_proxies(),
            context.mode,
            application_router(context),
            Arc::clone(&context.http_ready),
            async move { server_stop.stopped().await },
        )
        .await
    });
    let mut groups = tokio::task::JoinSet::new();
    let mut names = BTreeMap::new();
    for &group in OwnerGroup::for_mode(context.mode) {
        let task = groups.spawn(run_group(
            Arc::clone(context),
            manager.clone(),
            group,
            stop.clone(),
        ));
        names.insert(task.id(), group);
    }
    let group_result =
        |joined: Result<(tokio::task::Id, Result<(), RuntimeError>), tokio::task::JoinError>| {
            match joined {
                Ok((_, Ok(()))) if stop.is_stopped() => Ok(()),
                Ok((id, Ok(()))) => Err(RuntimeError::SubsystemExited {
                    subsystem: names[&id].subsystem(),
                }),
                Ok((_, Err(error))) => Err(error),
                Err(source) => Err(RuntimeError::SubsystemTask {
                    subsystem: names[&source.id()].subsystem(),
                    source,
                }),
            }
        };
    let (primary, server_finished) = tokio::select! {
        () = stop.stopped() => (Ok(()), false),
        result = server.as_mut() => (result.map_err(RuntimeError::from), true),
        Some(joined) = groups.join_next_with_id() => (group_result(joined), false),
    };
    tracing::info!("runtime shutdown requested");
    context.groups.stop();
    stop.stop();
    let http = finish_http_shutdown(server, server_finished, HTTP_SHUTDOWN_TIMEOUT);
    // Each group bounds its own subsystem stop, provider close and lease release.
    let owned = async {
        let mut first_error = None;
        while let Some(joined) = groups.join_next_with_id().await {
            if let Err(error) = group_result(joined) {
                if first_error.is_some() {
                    tracing::error!(error = %error, "additional runtime group shutdown failed");
                } else {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    };
    let (http, owned) = tokio::join!(http, owned);
    preserve_run_primary(primary, preserve_run_primary(owned, http))
}

/// Routes the process listener serves beside readiness: the running platform's,
/// answered unavailable while it stands by or restarts.
fn application_router(context: &RuntimeContext) -> axum::Router {
    use axum::response::IntoResponse as _;
    let router = if context.mode.subsystems().contains(&SubsystemName::Platform) {
        let served = Arc::clone(&context.platform_router);
        axum::Router::new().fallback(move |request: axum::extract::Request| {
            let router = served.read().ok().and_then(|router| router.clone());
            async move {
                match router {
                    Some(router) => match tower::ServiceExt::oneshot(router, request).await {
                        Ok(response) => response,
                        Err(never) => match never {},
                    },
                    None => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
        })
    } else {
        // An upgrade may name any path, including ones no route serves, so a
        // fallback gives the WebSocket rule below every path to wrap.
        axum::Router::new().fallback(|| std::future::ready(axum::http::StatusCode::NOT_FOUND))
    };
    match context.config.nats_websocket_upstream() {
        Some(upstream) => router.layer(axum::middleware::from_fn_with_state(
            upstream.to_owned(),
            crate::nats_websocket::relay,
        )),
        None => router,
    }
}

/// Runs one owner group for the life of the process: owns its lease, runs its
/// work, and after losing the lease stands by to take it back.
async fn run_group(
    context: Arc<RuntimeContext>,
    manager: crate::leases::LeaseManager,
    group: OwnerGroup,
    stop: StopHandle,
) -> Result<(), RuntimeError> {
    let subsystem = group.subsystem();
    // The platform starts first, as other roles prepare their providers through
    // it; once it runs or stands by, they start whichever way it went.
    if group != OwnerGroup::Platform && context.mode.subsystems().contains(&SubsystemName::Platform)
    {
        tokio::select! {
            () = context.groups.settled(OwnerGroup::Platform) => {}
            () = stop.stopped() => return Ok(()),
        }
    }
    loop {
        let Some(mut ownership) =
            RuntimeOwnership::acquire_when_free(&manager, &[group], &stop, || {
                context.groups.set(group, GroupState::StandingBy);
            })
            .await
        else {
            return Ok(());
        };
        context.groups.set(group, GroupState::Starting);
        let result = run_owned_group(&context, group, &mut ownership, &stop).await;
        tracing::info!(%subsystem, "runtime subsystem stopped; releasing its ownership");
        let release = ownership.shutdown().await;
        match result {
            Err(
                lost @ (RuntimeError::OwnerRenewal { .. }
                | RuntimeError::OwnerRenewalRoundTimeout { .. }),
            ) if !stop.is_stopped() => {
                tracing::warn!(
                    %subsystem,
                    error = %lost,
                    "runtime lost ownership of this work; it stopped and stands by to resume"
                );
                if let Err(release) = release {
                    tracing::warn!(error = %release, "runtime ownership release also failed");
                }
            }
            result => return preserve_primary(result, release, Ok(())),
        }
    }
}

/// Starts one owned group's subsystem and built-in provider, runs them until
/// the lease is lost, the process stops, or the subsystem fails, and stops them.
async fn run_owned_group(
    context: &RuntimeContext,
    group: OwnerGroup,
    ownership: &mut RuntimeOwnership,
    stop: &StopHandle,
) -> Result<(), RuntimeError> {
    let subsystem = group.subsystem();
    let role = match group {
        OwnerGroup::Platform => crate::platform::LiveProviderRole::Platform,
        OwnerGroup::Jobs => crate::platform::LiveProviderRole::Jobs,
        OwnerGroup::Health => crate::platform::LiveProviderRole::Health,
        OwnerGroup::Events => crate::platform::LiveProviderRole::Events,
    };
    if let Ok(mut owners) = context.owners.lock() {
        owners.extend(ownership.contexts());
    }
    let started = match subsystem {
        SubsystemName::Platform => platform::start(context).await,
        SubsystemName::Jobs => jobs::start(context).await,
        SubsystemName::Health => health::start(context).await,
        SubsystemName::Events => events::start(context).await,
    };
    let handle = match started {
        Ok(handle) => handle,
        Err(error) => {
            if let Ok(mut owners) = context.owners.lock() {
                owners.remove(&group);
            }
            // Work that failed to start because its lease went first stands by.
            return Err(ownership.renewal_failure_now().unwrap_or(error));
        }
    };
    if group == OwnerGroup::Platform {
        context.serve_platform_routes(Some(context.take_http_router()?));
    }
    let mut handles = vec![handle];
    // A built-in provider's bootstrap failure fails the group, never a
    // dispatch-time fallback.
    let bootstrap_failure = tokio::select! {
        biased;
        lost = ownership.wait_for_renewal_failure() => {
            Some((Err(lost), RuntimeStopCause::OwnershipLost))
        }
        () = stop.stopped() => Some((Ok(()), RuntimeStopCause::Signal)),
        result = bootstrap_live_provider(context, role) => {
            result.err().map(|error| (Err(error), RuntimeStopCause::SubsystemFailed))
        }
    };
    let (primary, cause) = match bootstrap_failure {
        Some(failure) => failure,
        None => {
            context.groups.set(group, GroupState::Running);
            let server = std::pin::pin!(std::future::pending::<Result<(), ServerError>>());
            let signal = std::pin::pin!(stop.stopped());
            let (primary, _, cause) = wait_for_runtime_event(
                server,
                &mut handles,
                signal,
                ownership.wait_for_renewal_failure(),
                &context.groups.components,
                &[component_label(subsystem)],
            )
            .await;
            (primary, cause)
        }
    };
    tracing::info!(%subsystem, ?cause, "runtime subsystem stopping");
    if cause == RuntimeStopCause::OwnershipLost {
        context.groups.set(group, GroupState::StandingBy);
    }
    if group == OwnerGroup::Platform {
        context.serve_platform_routes(None);
        if let Ok(mut store) = context.authorization_store.lock() {
            *store = None;
        }
    }
    // Work whose lease is gone stops at once; otherwise it stops cooperatively.
    let stopped = if cause == RuntimeStopCause::OwnershipLost {
        abort_subsystems(&mut handles).await
    } else {
        for handle in &handles {
            handle.stop.stop();
        }
        join_subsystems(&mut handles, SUBSYSTEM_SHUTDOWN_TIMEOUT).await
    };
    // The provider's detached ingress can still hold clients, so it closes
    // before the lease is released.
    let provider = tokio::time::timeout(
        SUBSYSTEM_SHUTDOWN_TIMEOUT,
        context.live_providers.shutdown_role(role),
    )
    .await
    .unwrap_or_else(|_| {
        Err(RuntimeError::Platform(
            "built-in provider transport shutdown timed out".into(),
        ))
    });
    if let Ok(mut owners) = context.owners.lock() {
        owners.remove(&group);
    }
    preserve_run_primary(primary, preserve_run_primary(stopped, provider))
}

fn preserve_run_primary(
    primary: Result<(), RuntimeError>,
    shutdown: Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    match (primary, shutdown) {
        (Err(primary), Err(secondary)) => {
            tracing::error!(error = %secondary, "runtime shutdown also failed");
            Err(primary)
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), result) => result,
    }
}

/// Bound on one built-in live provider's ordinary native bootstrap.
const BUILTIN_LIVE_PROVIDER_CONNECT_TIMEOUT_MS: u64 = 35_000;

/// Connects the built-in live provider for one role and delivers it to the
/// subsystem that serves its live routes.
///
/// Providers authenticate through native Callout and signed own-context, without
/// HTTP bootstrap. In a process that also runs the platform, a provider is
/// prepared through the platform's store once the platform has started here;
/// while another server owns the platform, it trusts the configured Auth origin,
/// as a split role does.
async fn bootstrap_live_provider(
    context: &RuntimeContext,
    role: crate::platform::LiveProviderRole,
) -> Result<(), RuntimeError> {
    let nats = context
        .config
        .resolve_nats_runtime_with(
            context
                .nats_override
                .as_ref()
                .and_then(|value| value.runtime_servers.as_deref()),
        )
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
    if role != crate::platform::LiveProviderRole::Platform
        && context.mode.subsystems().contains(&SubsystemName::Platform)
    {
        context.groups.settled(OwnerGroup::Platform).await;
    }
    let store = context
        .authorization_store
        .lock()
        .ok()
        .and_then(|store| store.clone());
    let store = store.as_ref();
    // The configured broker CA also anchors the Auth origin in a split
    // deployment; the installation's generated CA issues both certificates.
    let root_certificates: Vec<_> = nats.ca_certificate.iter().cloned().collect();
    let mut trust = match store {
        Some(store) => {
            let store = store.clone();
            crate::platform::auth::blocking(move || store.runtime_authorization_trust()).await?
        }
        None => {
            let origin = context.config.public_origin();
            trellis_rs::auth::discover_native_oauth(&origin, &root_certificates)
                .await
                .map_err(|error| {
                    RuntimeError::Platform(format!("discover configured Auth origin: {error}"))
                })?
                .trust
        }
    };
    trust.root_certificates = root_certificates;
    trust.nats_server_name = nats.tls_server_name.clone();
    let identity_seed = crate::platform::load_live_provider_seed(&context.config, role)?;
    let deployment = if let Some(store) = store {
        Some(
            crate::platform::prepare_builtin_live_provider(
                role,
                store,
                &context.trellis_nats,
                &identity_seed,
            )
            .await?,
        )
    } else {
        None
    };
    let client = crate::platform::connect_builtin_live_provider(
        role,
        &identity_seed,
        crate::platform::BuiltinLiveProviderConnectOptions {
            nats_servers: nats.server_urls(),
            trust: trust.clone(),
            timeout_ms: BUILTIN_LIVE_PROVIDER_CONNECT_TIMEOUT_MS,
        },
    )
    .await?;
    if let Some(deployment) = deployment {
        let authority = client
            .authorization_context()
            .map_err(|error| RuntimeError::Platform(error.to_string()))?
            .ok_or_else(|| {
                RuntimeError::Platform("built-in provider has no installed authority".into())
            })?;
        if authority.context.unsigned.principal_id != deployment.principal_id
            || authority.context.unsigned.binding
                != (trellis_protocol::SessionBinding::Service {
                    participant_id: deployment.participant_id,
                    deployment_id: deployment.deployment_id,
                    instance_id: deployment.instance_id,
                })
        {
            return Err(RuntimeError::Platform(
                "installed built-in authority does not match provisioning".into(),
            ));
        }
    }
    context.live_providers.install(
        role,
        trellis_rs::__internal::LiveProviderOwner::from_connected_client(Arc::new(client)),
    );
    Ok(())
}

async fn wait_for_runtime_event<F, S, R>(
    mut server: Pin<&mut F>,
    handles: &mut Vec<SubsystemHandle>,
    mut signal: Pin<&mut S>,
    renewal: R,
    readiness: &crate::telemetry::snapshots::ComponentReadiness,
    component_names: &[&'static str],
) -> (Result<(), RuntimeError>, bool, RuntimeStopCause)
where
    F: Future<Output = Result<(), ServerError>>,
    S: Future<Output = ()>,
    R: Future<Output = RuntimeError>,
{
    tokio::pin!(renewal);
    tokio::select! {
        biased;
        renewal_error = &mut renewal => (
            Err(renewal_error),
            false,
            RuntimeStopCause::OwnershipLost,
        ),
        () = signal.as_mut() => (Ok(()), false, RuntimeStopCause::Signal),
        server_result = server.as_mut() => (
            server_result.map_err(RuntimeError::from),
            true,
            RuntimeStopCause::HttpFinished,
        ),
        (index, task_result) = wait_for_subsystem(handles), if !handles.is_empty() => {
            let failed = handles.swap_remove(index);
            // The failing component is no longer ready; the supervisor records
            // the transition before reporting the failure.
            let label = component_label(failed.name);
            if component_names.contains(&label) {
                readiness.set(label, 0.0);
            }
            let result = match task_result {
                Ok(Ok(())) => Err(RuntimeError::SubsystemExited { subsystem: failed.name }),
                Ok(Err(error)) => Err(error),
                Err(source) => Err(RuntimeError::SubsystemTask {
                    subsystem: failed.name,
                    source,
                }),
            };
            (result, false, RuntimeStopCause::SubsystemFailed)
        }
    }
}

async fn wait_for_subsystem(
    handles: &mut [SubsystemHandle],
) -> (
    usize,
    Result<Result<(), RuntimeError>, tokio::task::JoinError>,
) {
    let waits = handles
        .iter_mut()
        .enumerate()
        .map(|(index, handle)| Box::pin(async move { (index, (&mut handle.join).await) }))
        .collect::<Vec<_>>();
    futures_util::future::select_all(waits).await.0
}

async fn finish_http_shutdown<F>(
    mut server: Pin<&mut F>,
    server_finished: bool,
    timeout: Duration,
) -> Result<(), RuntimeError>
where
    F: Future<Output = Result<(), ServerError>>,
{
    if server_finished {
        return Ok(());
    }
    match tokio::time::timeout(timeout, server.as_mut()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(RuntimeError::Server(error)),
        Err(_) => Err(RuntimeError::HttpShutdownTimeout),
    }
}

async fn abort_subsystems(handles: &mut [SubsystemHandle]) -> Result<(), RuntimeError> {
    for handle in handles.iter() {
        handle.join.abort();
    }

    let mut first_error = None;
    for handle in handles {
        let subsystem = handle.name;
        match (&mut handle.join).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                if first_error.is_some() {
                    tracing::error!(error = %error, "additional runtime subsystem shutdown failed");
                } else {
                    first_error = Some(error);
                }
            }
            Err(source) if source.is_cancelled() => {}
            Err(source) => {
                let error = RuntimeError::SubsystemTask { subsystem, source };
                if first_error.is_some() {
                    tracing::error!(error = %error, "additional runtime subsystem shutdown failed");
                } else {
                    first_error = Some(error);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn join_subsystems(
    handles: &mut [SubsystemHandle],
    timeout: Duration,
) -> Result<(), RuntimeError> {
    let mut first_error = None;
    let deadline = tokio::time::Instant::now() + timeout;
    for handle in handles {
        let subsystem = handle.name;
        tracing::info!(?subsystem, "joining runtime subsystem during shutdown");
        match tokio::time::timeout_at(deadline, &mut handle.join).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                if first_error.is_some() {
                    tracing::error!(error = %error, "additional runtime subsystem shutdown failed");
                } else {
                    first_error = Some(error);
                }
            }
            Ok(Err(source)) => {
                let error = RuntimeError::SubsystemTask { subsystem, source };
                if first_error.is_some() {
                    tracing::error!(error = %error, "additional runtime subsystem shutdown failed");
                } else {
                    first_error = Some(error);
                }
            }
            Err(_) => {
                handle.join.abort();
                let _ = (&mut handle.join).await;
                let error = RuntimeError::SubsystemShutdownTimeout { subsystem };
                if first_error.is_some() {
                    tracing::error!(error = %error, "additional runtime subsystem shutdown failed");
                } else {
                    first_error = Some(error);
                }
            }
        }
        tracing::info!(?subsystem, "runtime subsystem shutdown join finished");
    }

    first_error.map_or(Ok(()), Err)
}

fn component_label(name: SubsystemName) -> &'static str {
    match name {
        SubsystemName::Platform => "platform",
        SubsystemName::Jobs => "jobs",
        SubsystemName::Events => "events",
        SubsystemName::Health => "health",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn refuses_plaintext_for_a_remote_public_origin() {
        // A remote origin may be served as plaintext only behind a trusted
        // HTTPS proxy; there is no insecure-origin escape hatch.
        for http in [
            "public_origin = \"http://trellis-demo.lan:3000\"",
            "public_origin = \"http://trellis-demo.lan:3000\"\n[http.transport]\n\
            mode = \"proxy\"\ntrusted_proxies = [\"10.0.0.0/8\"]",
            "public_origin = \"http://localhost:3000\"\nbind_address = \"0.0.0.0\"",
        ] {
            let config =
                RuntimeConfig::from_toml_str(&format!("[http]\n{http}\n")).expect("parse config");
            let error = run_with_stop(
                RuntimeOptions {
                    mode: RuntimeMode::All,
                    config,
                    reset_admin: false,
                    nats_override: None,
                },
                None,
            )
            .await
            .expect_err("startup must refuse plaintext the transport cannot protect");
            assert!(
                matches!(
                    error,
                    RuntimeError::Config(crate::ConfigError::InvalidHttpTransport { .. })
                ),
                "unexpected startup error for {http}: {error}"
            );
        }
    }

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn owner_stop_handle_ends_the_runtime_without_a_signal() {
        let handle = crate::shutdown::StopHandle::new();
        let stopper = handle.clone();
        let waiter = tokio::spawn(shutdown_or_stop(Some(handle)));
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        stopper.stop();
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("owner stop is observed")
            .expect("waiter task joined");
    }

    #[tokio::test]
    async fn shutdown_signal_is_observed_outside_http_future() {
        let mut server = Box::pin(std::future::pending::<Result<(), ServerError>>());
        let mut handles = Vec::new();

        let readiness = crate::telemetry::snapshots::ComponentReadiness::default();
        let signal = std::future::ready(());
        tokio::pin!(signal);
        let (result, server_finished, cause) = wait_for_runtime_event(
            server.as_mut(),
            &mut handles,
            signal.as_mut(),
            std::future::pending(),
            &readiness,
            &[],
        )
        .await;

        assert!(result.is_ok());
        assert!(!server_finished);
        assert_eq!(cause, RuntimeStopCause::Signal);
    }

    #[tokio::test]
    async fn ownership_loss_wins_when_runtime_events_are_simultaneously_ready() {
        let mut server = Box::pin(std::future::ready(Ok(())));
        let mut handles = vec![SubsystemHandle {
            name: SubsystemName::Jobs,
            stop: StopHandle::new(),
            join: AbortOnDropHandle::new(tokio::spawn(async { Ok(()) })),
        }];

        let readiness = crate::telemetry::snapshots::ComponentReadiness::default();
        let signal = std::future::ready(());
        tokio::pin!(signal);
        let (result, server_finished, cause) = wait_for_runtime_event(
            server.as_mut(),
            &mut handles,
            signal.as_mut(),
            std::future::ready(RuntimeError::OwnerRenewalRoundTimeout {
                owner_id: "owner".to_owned(),
            }),
            &readiness,
            &["jobs"],
        )
        .await;

        assert!(matches!(
            result,
            Err(RuntimeError::OwnerRenewalRoundTimeout { .. })
        ));
        assert!(!server_finished);
        assert_eq!(cause, RuntimeStopCause::OwnershipLost);
        assert_eq!(handles.len(), 1);
        handles[0].join.abort();
        let _ = (&mut handles[0].join).await;
    }

    #[tokio::test]
    async fn pending_http_drain_is_cancelled_at_its_bound() {
        let server = std::pin::pin!(std::future::pending::<Result<(), ServerError>>());

        let error = finish_http_shutdown(server, false, Duration::from_millis(20))
            .await
            .expect_err("pending HTTP drain must time out");

        assert!(matches!(error, RuntimeError::HttpShutdownTimeout));
    }

    #[tokio::test]
    async fn pending_subsystem_is_aborted_at_the_shared_bound() {
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(Arc::clone(&dropped));
        let join = tokio::spawn(async move {
            let _marker = marker;
            std::future::pending::<Result<(), RuntimeError>>().await
        });

        let error = join_subsystems(
            &mut [SubsystemHandle {
                name: SubsystemName::Jobs,
                stop: StopHandle::new(),
                join: AbortOnDropHandle::new(join),
            }],
            Duration::from_millis(20),
        )
        .await
        .expect_err("pending subsystem must time out");

        assert!(matches!(
            error,
            RuntimeError::SubsystemShutdownTimeout {
                subsystem: SubsystemName::Jobs
            }
        ));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn ownership_loss_aborts_a_subsystem_that_would_not_stop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(Arc::clone(&dropped));
        let join = tokio::spawn(async move {
            let _marker = marker;
            std::future::pending::<Result<(), RuntimeError>>().await
        });

        tokio::time::timeout(
            Duration::from_millis(100),
            abort_subsystems(&mut [SubsystemHandle {
                name: SubsystemName::Jobs,
                stop: StopHandle::new(),
                join: AbortOnDropHandle::new(join),
            }]),
        )
        .await
        .expect("ownership loss must abort the subsystem immediately")
        .expect("an aborted subsystem is not a shutdown failure");
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn signal_shutdown_remains_cooperative() {
        let stop = StopHandle::new();
        let task_stop = stop.clone();
        let completed = Arc::new(AtomicBool::new(false));
        let task_completed = Arc::clone(&completed);
        let join = tokio::spawn(async move {
            task_stop.stopped().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            task_completed.store(true, Ordering::SeqCst);
            Ok(())
        });
        stop.stop();

        join_subsystems(
            &mut [SubsystemHandle {
                name: SubsystemName::Jobs,
                stop,
                join: AbortOnDropHandle::new(join),
            }],
            Duration::from_millis(50),
        )
        .await
        .expect("signal shutdown should allow cooperative completion");

        assert!(completed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn final_nats_flush_is_bounded() {
        let error = bounded_flush(
            std::future::pending::<Result<(), std::io::Error>>(),
            Duration::from_millis(20),
        )
        .await
        .expect_err("pending flush must time out");

        assert!(matches!(error, RuntimeError::NatsFlushTimeout));
    }

    #[test]
    fn shutdown_preserves_original_primary_error() {
        let primary = preserve_run_primary(
            Err(RuntimeError::OwnerRenewalRoundTimeout {
                owner_id: "owner-1".to_owned(),
            }),
            Err(RuntimeError::HttpShutdownTimeout),
        );
        let result = preserve_primary(
            primary,
            Err(RuntimeError::OwnerRenewalShutdownTimeout {
                owner_id: "owner-1".to_owned(),
            }),
            Err(RuntimeError::NatsFlushTimeout),
        );

        assert!(matches!(
            result,
            Err(RuntimeError::OwnerRenewalRoundTimeout { .. })
        ));
    }
}
