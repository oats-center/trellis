#![cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use tokio::task::JoinHandle;
use ulid::Ulid;

const HTTP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const SUBSYSTEM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const OWNERSHIP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const NATS_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

use crate::leases::{LeaseError, LeaseKey};
use crate::ownership::{OwnerContext, OwnerGroup, RuntimeOwnership};
use crate::platform::auth::verifier::RuntimeAuthVerifier;
use crate::resources::{stream_is_compatible, ExpectedRuntimeResources};
use crate::shutdown::StopHandle;
use crate::storage::{RuntimeStores, StoreError};
use crate::{
    events, health, jobs, platform, RuntimeConfig, RuntimeMode, ServerError, SubsystemName,
};

/// Replacement for configured NATS endpoints used by managed `trellis-server` startup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsEndpointOverride {
    /// Replacement native NATS server URL, used for the runtime connection and the
    /// advertised native client endpoint.
    pub servers: String,
    /// Replacement advertised websocket endpoint. `None` keeps the configured value
    /// (external `--nats` deployments); managed mode sets it to the local websocket.
    pub websocket: Option<String>,
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
    let trellis_nats = match check_nats_connection(&nats.servers, &nats.trellis_creds_path).await {
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
                if status.history() == 1
                    && status.max_age() == Duration::from_millis(leases.ttl_ms)
                    && (leases.replicas == 0
                        || status.info.config.num_replicas == usize::from(leases.replicas)) =>
            {
                report.push(
                    "nats.kv.leases",
                    RuntimeCheckStatus::Ok,
                    "lease bucket exists with compatible history, TTL, and replicas",
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
    if expected_resources.requires(SubsystemName::Platform) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| RuntimeError::Platform(error.to_string()))?
            .as_secs();
        let now = i64::try_from(now)
            .map_err(|_| RuntimeError::Platform("current time exceeds i64 seconds".to_owned()))?;
        let authorization = config.resolve_authorization()?;
        let trust = match crate::platform::auth::context::trust::VerifiedTrustMaterial::load(
            authorization,
            now,
        ) {
            Ok(trust) => {
                report.push(
                    "authorization.issuer_file",
                    RuntimeCheckStatus::Ok,
                    "configured issuer signing material is valid",
                );
                trust
            }
            Err(error) => {
                let detail = error.to_string();
                let status = if detail.contains("expired") {
                    RuntimeCheckStatus::Expired
                } else {
                    RuntimeCheckStatus::Error
                };
                report.push("authorization.issuer_file", status, detail);
                return Ok(report);
            }
        };
        if stores.platform()?.exists() {
            let auth_store = crate::platform::auth::SqliteAuthorizationStore::open_read_only(
                stores.platform()?,
            )?;
            match auth_store
                .get_issuer_key(
                    trust.issuer.key_id.clone(),
                    now.checked_mul(1_000).ok_or_else(|| {
                        RuntimeError::Platform("issuer check time overflow".to_owned())
                    })?,
                )
                .await
                .map_err(|error| RuntimeError::Platform(error.to_string()))?
            {
                Some(key) if key == trust.issuer => report.push(
                    "authorization.issuer_record",
                    RuntimeCheckStatus::Ok,
                    "configured public issuer key is retained and active",
                ),
                Some(_) => report.push(
                    "authorization.issuer_record",
                    RuntimeCheckStatus::Incompatible,
                    "configured issuer is retired, revoked, or has different public material",
                ),
                None => report.push(
                    "authorization.issuer_record",
                    RuntimeCheckStatus::Missing,
                    "configured issuer has not been installed",
                ),
            }
        }
        let user_jwt_ttl_ms = crate::platform::auth_callout::resolve_user_jwt_ttl_ms(
            config
                .platform
                .as_ref()
                .and_then(|platform| platform.ttl_ms.as_ref())
                .and_then(|ttl| ttl.nats_jwt),
        )
        .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        let connection_max_age =
            crate::platform::auth_callout::connection_presence_max_age(user_jwt_ttl_ms)
                .map_err(|error| RuntimeError::Platform(error.to_string()))?;
        match crate::platform::auth::NatsAuthEphemeralRepository::check(
            trellis_nats.clone(),
            connection_max_age,
        )
        .await
        {
            Ok(()) => report.push(
                "nats.auth_kv",
                RuntimeCheckStatus::Ok,
                "all required auth KV buckets exist with compatible limits",
            ),
            Err(error) => {
                let detail = error.to_string();
                let status = if detail.contains("missing") {
                    RuntimeCheckStatus::Missing
                } else {
                    RuntimeCheckStatus::Incompatible
                };
                report.push("nats.auth_kv", status, detail);
            }
        }
        match crate::platform::auth::context::AuthorizationContextRegistry::check(
            trellis_nats.clone(),
            authorization,
        )
        .await
        {
            Ok(()) => report.push(
                "authorization.context_registry",
                RuntimeCheckStatus::Ok,
                "context and revocation registry settings match configuration",
            ),
            Err(error) => {
                let detail = error.to_string();
                let status = if detail.contains("does not exist") || detail.contains("not found") {
                    RuntimeCheckStatus::Missing
                } else {
                    RuntimeCheckStatus::Incompatible
                };
                report.push("authorization.context_registry", status, detail);
            }
        }
        for (name, credentials) in [
            ("nats.auth", &nats.auth_creds_path),
            ("nats.system", &nats.system_creds_path),
        ] {
            match check_nats_connection(&nats.servers, credentials).await {
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

async fn check_nats_connection(
    servers: &str,
    credentials: &std::path::Path,
) -> Result<async_nats::Client, RuntimeError> {
    let options = async_nats::ConnectOptions::new()
        .credentials_file(credentials)
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    let client = tokio::time::timeout(Duration::from_secs(10), options.connect(servers))
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
    #[error("runtime owner lease {key:?} for {subsystem} is already held; owner {owner_id} cannot start: {source}")]
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
#[derive(Debug)]
pub(crate) struct RuntimeContext {
    /// Loaded and validated runtime configuration.
    pub(crate) config: RuntimeConfig,
    /// Runtime mode selected for this process.
    pub(crate) mode: RuntimeMode,
    /// Whether startup must rotate the pending first-administrator bootstrap flow.
    pub(crate) reset_admin: bool,
    /// Optional replacement for the configured NATS endpoints (managed local NATS).
    pub(crate) nats_override: Option<NatsEndpointOverride>,
    /// SQLite stores opened for the selected subsystems.
    pub(crate) stores: RuntimeStores,
    /// Trellis-account runtime NATS client shared by built-in subsystems.
    pub(crate) trellis_nats: async_nats::Client,
    /// Fixed owner contexts for selected runtime subsystems.
    owners: BTreeMap<OwnerGroup, OwnerContext>,
    http_router: std::sync::Mutex<axum::Router>,
    /// Runtime-local Auth verifier installed by the platform subsystem once the
    /// validator cache is ready; absent in platform-less modes (fail closed).
    pub(crate) platform_verifier: Arc<tokio::sync::OnceCell<RuntimeAuthVerifier>>,
    /// Per-role delivery slots for built-in live provider owners.
    pub(crate) live_providers: crate::platform::LiveProviderSlots,
}

impl RuntimeContext {
    pub(crate) fn owner(&self, group: OwnerGroup) -> Result<OwnerContext, RuntimeError> {
        self.owners
            .get(&group)
            .cloned()
            .ok_or(RuntimeError::OwnerContextMissing {
                subsystem: group.subsystem(),
            })
    }

    /// Resolve the public origin used for provider bootstrap and advertised URLs.
    pub(crate) fn public_origin(&self) -> String {
        self.config
            .http
            .as_ref()
            .and_then(|http| http.public_origin.clone())
            .unwrap_or_else(|| format!("http://localhost:{}", self.config.http_port()))
    }

    /// Return whether the configured non-loopback HTTP origin is allowed.
    pub(crate) fn allows_insecure_origin(&self, origin: &str) -> bool {
        self.config
            .http
            .as_ref()
            .is_some_and(|http| http.allows_insecure_origin(origin))
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
}

/// Handle for a started subsystem scaffold.
#[derive(Debug)]
pub(crate) struct SubsystemHandle {
    /// Started subsystem name.
    pub(crate) name: SubsystemName,
    /// Cooperative stop request handle for the subsystem task.
    pub(crate) stop: StopHandle,
    /// Join handle for the subsystem task.
    pub(crate) join: JoinHandle<Result<(), RuntimeError>>,
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
pub async fn run_with_stop(
    options: RuntimeOptions,
    stop: Option<crate::shutdown::StopHandle>,
) -> Result<(), RuntimeError> {
    let config = options.config;
    config.validate_for_mode(options.mode)?;
    let nats = config
        .resolve_nats_runtime_with(options.nats_override.as_ref().map(|o| o.servers.as_str()))?;
    let leases = config.resolve_leases()?;
    let trellis_nats = async_nats::ConnectOptions::new()
        .credentials_file(&nats.trellis_creds_path)
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?
        .connect(&nats.servers)
        .await
        .map_err(|error| RuntimeError::Nats(error.to_string()))?;
    let owner_id = format!(
        "{}-{}",
        config.instance_name.as_deref().unwrap_or("trellis-runtime"),
        Ulid::new()
    );
    let mut ownership =
        RuntimeOwnership::acquire(trellis_nats.clone(), &leases, owner_id, options.mode).await?;
    let result = run_owned(
        config,
        options.mode,
        options.reset_admin,
        options.nats_override,
        trellis_nats.clone(),
        &mut ownership,
        stop,
    )
    .await;
    let release_result = ownership.shutdown().await;
    let flush_result = bounded_flush(trellis_nats.flush(), NATS_FLUSH_TIMEOUT).await;
    preserve_primary(result, release_result, flush_result)
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

async fn run_owned(
    config: RuntimeConfig,
    mode: RuntimeMode,
    reset_admin: bool,
    nats_override: Option<NatsEndpointOverride>,
    trellis_nats: async_nats::Client,
    ownership: &mut RuntimeOwnership,
    stop: Option<crate::shutdown::StopHandle>,
) -> Result<(), RuntimeError> {
    let storage = if nats_override.is_some() {
        tracing::warn!("using in-memory NATS JetStream storage; this mode is not production ready");
        async_nats::jetstream::stream::StorageType::Memory
    } else {
        async_nats::jetstream::stream::StorageType::File
    };
    ExpectedRuntimeResources::for_mode(mode, &config, storage)
        .converge_streams(trellis_nats.clone())
        .await?;
    let stores = RuntimeStores::from_config(&config, mode)?;
    stores.migrate_all()?;
    let context = RuntimeContext {
        config,
        mode,
        reset_admin,
        nats_override,
        stores,
        trellis_nats,
        owners: ownership.contexts(),
        http_router: std::sync::Mutex::new(axum::Router::new()),
        platform_verifier: Arc::new(tokio::sync::OnceCell::new()),
        live_providers: crate::platform::LiveProviderSlots::new(),
    };
    let mut handles = start_subsystems(&context).await?;
    let root_stop = StopHandle::new();
    // Only components selected by the runtime mode report ready; others are
    // absent rather than failed. Readiness is written here from the real
    // lifecycle: ready once the bootstrap listener serves and the built-in
    // live providers have authenticated, not-ready as soon as the runtime
    // begins stopping.
    let component_readiness =
        std::sync::Arc::new(crate::telemetry::snapshots::ComponentReadiness::default());
    let component_names: Vec<&'static str> = handles
        .iter()
        .map(|handle| component_label(handle.name))
        .collect();
    let _component_sampler = crate::telemetry::snapshots::spawn_component_sampler(
        component_names.clone(),
        std::sync::Arc::clone(&component_readiness),
        root_stop.clone(),
    );
    let server_stop = root_stop.clone();
    let http_router = context.take_http_router()?;
    // Readiness stays false until built-in live providers have finished their
    // bootstrap, so callers never begin work against half-started routes.
    let http_ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Register the OS shutdown signal before the HTTP listener becomes
    // observable. A host can connect to the bound port immediately, so a
    // SIGTERM delivered during startup must already be observed cooperatively
    // instead of terminating the process with its runtime lease held.
    let shutdown = shutdown_or_stop(stop);
    tokio::pin!(shutdown);
    std::future::poll_fn(|cx| {
        let _ = std::future::Future::poll(shutdown.as_mut(), cx);
        std::task::Poll::Ready(())
    })
    .await;
    let listener = match crate::bind_http_listener(&context.config).await {
        Ok(listener) => listener,
        Err(error) => {
            root_stop.stop();
            if let Err(cleanup) = stop_subsystems(handles).await {
                tracing::error!(error = %cleanup, "runtime startup cleanup also failed");
            }
            return Err(RuntimeError::from(error));
        }
    };
    let mut server = Box::pin(crate::serve_http_listener(
        listener,
        context.mode,
        http_router,
        std::sync::Arc::clone(&http_ready),
        async move { server_stop.stopped().await },
    ));
    // The bootstrap listener serves while the built-in live providers
    // authenticate through the ordinary native bootstrap. A provider failure
    // or an early listener exit is a startup failure, never a dispatch-time
    // fallback.
    // The readiness endpoint answers as soon as this listener serves, so a
    // restart can deliver the host shutdown signal while built-in live
    // providers are still bootstrapping. Observe that signal from the first
    // serve poll so the stop stays cooperative and runtime ownership is
    // released instead of the process being terminated with its lease held.
    let mut signalled_during_bootstrap = false;
    let bootstrap_result: Result<(), RuntimeError> = tokio::select! {
        result = bootstrap_live_providers(&context) => result,
        result = server.as_mut() => result.map_err(RuntimeError::from),
        () = shutdown.as_mut() => {
            signalled_during_bootstrap = true;
            Ok(())
        }
    };
    if signalled_during_bootstrap {
        http_ready.store(false, std::sync::atomic::Ordering::SeqCst);
        for name in &component_names {
            component_readiness.set(name, 0.0);
        }
        root_stop.stop();
        let shutdown_result = finish_shutdown(
            server,
            false,
            handles,
            RuntimeStopCause::Signal,
            HTTP_SHUTDOWN_TIMEOUT,
            SUBSYSTEM_SHUTDOWN_TIMEOUT,
        )
        .await;
        return preserve_run_primary(Ok(()), shutdown_result);
    }
    if let Err(error) = bootstrap_result {
        http_ready.store(false, std::sync::atomic::Ordering::SeqCst);
        root_stop.stop();
        if let Err(cleanup) = stop_subsystems(handles).await {
            tracing::error!(error = %cleanup, "runtime startup cleanup also failed");
        }
        return Err(error);
    }
    for name in &component_names {
        component_readiness.set(name, 1.0);
    }
    http_ready.store(true, std::sync::atomic::Ordering::SeqCst);
    let (primary, server_finished, cause) = wait_for_runtime_event(
        server.as_mut(),
        &mut handles,
        shutdown.as_mut(),
        ownership.wait_for_renewal_failure(),
        &component_readiness,
        &component_names,
    )
    .await;
    // From this point the runtime is stopping, so selected components stop
    // being ready regardless of how the stop was triggered.
    http_ready.store(false, std::sync::atomic::Ordering::SeqCst);
    for name in &component_names {
        component_readiness.set(name, 0.0);
    }

    root_stop.stop();
    let shutdown = finish_shutdown(
        server,
        server_finished,
        handles,
        cause,
        HTTP_SHUTDOWN_TIMEOUT,
        SUBSYSTEM_SHUTDOWN_TIMEOUT,
    )
    .await;
    preserve_run_primary(primary, shutdown)
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
const BUILTIN_LIVE_PROVIDER_CONNECT_TIMEOUT_MS: u64 = 30_000;

/// Connect every built-in live provider role selected by the runtime mode.
///
/// This runs after the bootstrap HTTP listener begins serving so a built-in
/// provider authenticates through the ordinary native service bootstrap and
/// Auth Callout. Each connected owner is delivered to the subsystem that
/// serves its live routes; a role whose subsystem is selected but whose owner
/// cannot be connected fails startup before any live router serves traffic.
async fn bootstrap_live_providers(context: &RuntimeContext) -> Result<(), RuntimeError> {
    let trellis_url = context.public_origin();
    let allow_insecure_origin = context.allows_insecure_origin(&trellis_url);
    for role in crate::platform::LiveProviderRole::roles_for_mode(context.mode) {
        let identity_seed = crate::platform::load_live_provider_seed(&context.config, role)?;
        let client = crate::platform::connect_builtin_live_provider(
            role,
            &identity_seed,
            crate::platform::BuiltinLiveProviderConnectOptions {
                trellis_url: &trellis_url,
                timeout_ms: BUILTIN_LIVE_PROVIDER_CONNECT_TIMEOUT_MS,
                allow_insecure_origin,
            },
        )
        .await?;
        context.live_providers.install(
            role,
            trellis_rs::service::LiveProviderOwner::from_connected_client(Arc::new(client)),
        );
    }
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

async fn stop_subsystems(mut handles: Vec<SubsystemHandle>) -> Result<(), RuntimeError> {
    for handle in &handles {
        handle.stop.stop();
    }
    join_subsystems(&mut handles, SUBSYSTEM_SHUTDOWN_TIMEOUT).await
}

async fn finish_shutdown<F>(
    server: Pin<Box<F>>,
    server_finished: bool,
    mut handles: Vec<SubsystemHandle>,
    cause: RuntimeStopCause,
    http_timeout: Duration,
    subsystem_timeout: Duration,
) -> Result<(), RuntimeError>
where
    F: Future<Output = Result<(), ServerError>>,
{
    for handle in &handles {
        handle.stop.stop();
    }

    let http_shutdown = finish_http_shutdown(server, server_finished, http_timeout);
    let subsystem_shutdown = async {
        if cause == RuntimeStopCause::OwnershipLost {
            abort_subsystems(&mut handles).await
        } else {
            join_subsystems(&mut handles, subsystem_timeout).await
        }
    };
    let (http_result, subsystem_result) = tokio::join!(http_shutdown, subsystem_shutdown);

    let mut first_error = http_result.err();
    if let Err(error) = subsystem_result {
        if first_error.is_some() {
            tracing::error!(error = %error, "runtime subsystem shutdown also failed");
        } else {
            first_error = Some(error);
        }
    }

    first_error.map_or(Ok(()), Err)
}

async fn finish_http_shutdown<F>(
    mut server: Pin<Box<F>>,
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

async fn start_subsystems(context: &RuntimeContext) -> Result<Vec<SubsystemHandle>, RuntimeError> {
    let mut handles = Vec::new();
    for subsystem in context.mode.subsystems() {
        let result = match subsystem {
            SubsystemName::Platform => platform::start(context).await,
            SubsystemName::Jobs => jobs::start(context).await,
            SubsystemName::Health => health::start(context).await,
            SubsystemName::Events => events::start(context).await,
        };
        match result {
            Ok(handle) => handles.push(handle),
            Err(primary) => {
                handles.reverse();
                if let Err(cleanup) = stop_subsystems(handles).await {
                    tracing::error!(error = %cleanup, "started subsystem cleanup also failed");
                }
                return Err(primary);
            }
        }
    }
    Ok(handles)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn runtime_options_carries_nats_endpoint_override() {
        let options = RuntimeOptions {
            mode: RuntimeMode::All,
            config: RuntimeConfig::from_toml_str("").expect("empty config"),
            reset_admin: false,
            nats_override: Some(NatsEndpointOverride {
                servers: "nats://127.0.0.1:4222".to_string(),
                websocket: Some("ws://127.0.0.1:8080".to_string()),
            }),
        };
        assert_eq!(
            options.nats_override.as_ref().map(|o| o.servers.as_str()),
            Some("nats://127.0.0.1:4222")
        );
        assert_eq!(options.clone(), options);

        let plain = RuntimeOptions {
            nats_override: None,
            ..options.clone()
        };
        assert_eq!(plain.nats_override, None);
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
            join: tokio::spawn(async { Ok(()) }),
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
    async fn subsystem_stop_is_signaled_before_pending_http_drain_is_awaited() {
        let stop = StopHandle::new();
        let task_stop = stop.clone();
        let notified = Arc::new(AtomicBool::new(false));
        let task_notified = Arc::clone(&notified);
        let (sent, received) = tokio::sync::oneshot::channel();
        let join = tokio::spawn(async move {
            task_stop.stopped().await;
            task_notified.store(true, Ordering::SeqCst);
            sent.send(()).expect("HTTP test receiver remains open");
            std::future::pending().await
        });
        let server = Box::pin(async move {
            received.await.expect("subsystem reports stop notification");
            std::future::pending::<Result<(), ServerError>>().await
        });

        let error = finish_shutdown(
            server,
            false,
            vec![SubsystemHandle {
                name: SubsystemName::Jobs,
                stop,
                join,
            }],
            RuntimeStopCause::Signal,
            Duration::from_millis(20),
            Duration::from_millis(20),
        )
        .await
        .expect_err("pending HTTP drain must time out");

        assert!(notified.load(Ordering::SeqCst));
        assert!(matches!(error, RuntimeError::HttpShutdownTimeout));
    }

    #[tokio::test]
    async fn pending_http_drain_is_cancelled_at_its_bound() {
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(Arc::clone(&dropped));
        let server = Box::pin(async move {
            let _marker = marker;
            std::future::pending::<Result<(), ServerError>>().await
        });

        let error = finish_shutdown(
            server,
            false,
            Vec::new(),
            RuntimeStopCause::Signal,
            Duration::from_millis(20),
            Duration::from_millis(20),
        )
        .await
        .expect_err("pending HTTP drain must time out");

        assert!(matches!(error, RuntimeError::HttpShutdownTimeout));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn pending_subsystem_is_aborted_at_the_shared_bound() {
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(Arc::clone(&dropped));
        let join = tokio::spawn(async move {
            let _marker = marker;
            std::future::pending::<Result<(), RuntimeError>>().await
        });

        let error = finish_shutdown(
            Box::pin(std::future::ready(Ok(()))),
            true,
            vec![SubsystemHandle {
                name: SubsystemName::Jobs,
                stop: StopHandle::new(),
                join,
            }],
            RuntimeStopCause::Signal,
            Duration::from_millis(20),
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
    async fn ownership_loss_aborts_subsystem_without_waiting_for_http_drain() {
        let stop = StopHandle::new();
        let observed_stop = stop.clone();
        let task_stop = stop.clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = DropMarker(Arc::clone(&dropped));
        let join = tokio::spawn(async move {
            let _marker = marker;
            task_stop.stopped().await;
            std::future::pending::<Result<(), RuntimeError>>().await
        });
        let shutdown = tokio::spawn(finish_shutdown(
            Box::pin(std::future::pending::<Result<(), ServerError>>()),
            false,
            vec![SubsystemHandle {
                name: SubsystemName::Jobs,
                stop,
                join,
            }],
            RuntimeStopCause::OwnershipLost,
            Duration::from_secs(5),
            Duration::from_secs(5),
        ));

        tokio::time::timeout(Duration::from_millis(100), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ownership loss must abort the subsystem immediately");

        assert!(observed_stop.is_stopped());
        assert!(
            !shutdown.is_finished(),
            "HTTP drain remains independently bounded"
        );
        shutdown.abort();
        let _ = shutdown.await;
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

        finish_shutdown(
            Box::pin(std::future::ready(Ok(()))),
            true,
            vec![SubsystemHandle {
                name: SubsystemName::Jobs,
                stop,
                join,
            }],
            RuntimeStopCause::Signal,
            Duration::from_millis(50),
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
