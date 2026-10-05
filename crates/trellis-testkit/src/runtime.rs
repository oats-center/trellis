//! Production runtime hosting, isolated bootstrap, and managed broker ownership.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio_util::task::AbortOnDropHandle;
use trellis_local_nats::{LocalNats, LocalNatsError, LocalNatsPorts, NatsBinarySource, NatsOutput};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime::shutdown::StopHandle;
use trellis_runtime::{
    NatsEndpointOverride, RuntimeConfig, RuntimeError, RuntimeMode, RuntimeOptions,
    RuntimePathDefaults,
};

use crate::admin::{AdminSession, ADMIN_USERNAME};
use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use crate::identity::{InstalledParticipant, TestClientIdentity, TestServiceIdentity};
use crate::sandbox::{ensure_supported_platform, PortLease, Sandbox, WorkdirRetention};

const MAX_STARTUP_ATTEMPTS: usize = 3;

/// Where the real managed NATS executable comes from.
#[derive(Clone, Debug)]
pub enum NatsSource {
    /// Use an explicit NATS executable path.
    Path(PathBuf),
    /// Resolve `nats-server` from the caller's `PATH`.
    PathLookup,
    /// Download and verify the production pinned NATS release.
    DownloadPinned,
}

/// Startup, request, and shutdown budgets for a runtime.
#[derive(Clone, Copy, Debug)]
pub struct TestTimeouts {
    /// Overall deadline for `start`.
    pub startup: Duration,
    /// Deadline for one public install/registration operation.
    pub request: Duration,
    /// Overall deadline for cooperative runtime shutdown.
    pub shutdown: Duration,
}

impl Default for TestTimeouts {
    fn default() -> Self {
        Self {
            startup: Duration::from_secs(120),
            request: Duration::from_secs(30),
            shutdown: Duration::from_secs(30),
        }
    }
}

/// Consuming builder for a linked production runtime and real managed NATS.
#[derive(Clone)]
pub struct TrellisTestRuntimeBuilder {
    nats: Option<NatsSource>,
    workdir_parent: Option<PathBuf>,
    retention: WorkdirRetention,
    timeouts: TestTimeouts,
    timeouts_explicit: bool,
    extra_origins: Vec<String>,
    admin_username: Option<String>,
    admin_password: Option<String>,
}

impl Default for TrellisTestRuntimeBuilder {
    fn default() -> Self {
        Self {
            nats: None,
            workdir_parent: None,
            retention: WorkdirRetention::OnFailure,
            timeouts: TestTimeouts::default(),
            timeouts_explicit: false,
            extra_origins: Vec::new(),
            admin_username: None,
            admin_password: None,
        }
    }
}

impl TrellisTestRuntimeBuilder {
    /// Selects where the managed NATS executable comes from.
    #[must_use]
    pub fn nats(mut self, source: NatsSource) -> Self {
        self.nats = Some(source);
        self
    }
    /// Sets the parent directory for the sandbox.
    #[must_use]
    pub fn workdir_parent(mut self, path: impl Into<PathBuf>) -> Self {
        self.workdir_parent = Some(path.into());
        self
    }
    /// Sets the work-directory retention policy.
    #[must_use]
    pub fn retention(mut self, policy: WorkdirRetention) -> Self {
        self.retention = policy;
        self
    }
    /// Overrides the default timeouts.
    #[must_use]
    pub fn timeouts(mut self, timeouts: TestTimeouts) -> Self {
        self.timeouts = timeouts;
        self.timeouts_explicit = true;
        self
    }
    /// Replaces the additional allowed HTTP origins and insecure-origin allow-list.
    #[must_use]
    pub fn extra_origins(mut self, origins: Vec<String>) -> Self {
        self.extra_origins = origins;
        self
    }
    /// Adds one additional allowed HTTP origin.
    #[must_use]
    pub fn extra_origin(mut self, origin: impl Into<String>) -> Self {
        self.extra_origins.push(origin.into());
        self
    }
    /// Pins the sandbox administrator's local username; defaults to `trellis-testkit-admin`.
    #[must_use]
    pub fn admin_username(mut self, username: impl Into<String>) -> Self {
        self.admin_username = Some(username.into());
        self
    }
    /// Pins the sandbox-only administrator password; defaults to a fresh random value.
    #[must_use]
    pub fn admin_password(mut self, password: impl Into<String>) -> Self {
        self.admin_password = Some(password.into());
        self
    }

    /// Starts an isolated production runtime without a Trellis CLI or server executable.
    ///
    /// # Errors
    /// Returns invalid-input, bootstrap, broker, runtime, or authentication failures.
    pub async fn start(self) -> Result<TrellisTestRuntime, TrellisTestError> {
        ensure_supported_platform()?;
        let source =
            self.nats
                .clone()
                .unwrap_or_else(|| match std::env::var_os("TRELLIS_TEST_NATS_BIN") {
                    Some(path) if !path.is_empty() => NatsSource::Path(path.into()),
                    _ => NatsSource::DownloadPinned,
                });
        let timeouts = if !self.timeouts_explicit && matches!(source, NatsSource::DownloadPinned) {
            TestTimeouts {
                startup: Duration::from_secs(600),
                ..self.timeouts
            }
        } else {
            self.timeouts
        };
        validate_timeouts(&timeouts)?;
        for origin in &self.extra_origins {
            validate_origin(origin)?;
        }
        let username = self.admin_username.as_deref().unwrap_or(ADMIN_USERNAME);
        let password = self
            .admin_password
            .clone()
            .unwrap_or_else(generate_password);
        validate_admin_credentials(username, &password)?;
        let source = match source {
            NatsSource::Path(path) => NatsBinarySource::Path(path),
            NatsSource::PathLookup => NatsBinarySource::PathLookup,
            NatsSource::DownloadPinned => NatsBinarySource::DownloadPinned,
        };
        let parent = self
            .workdir_parent
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let deadline = Instant::now() + timeouts.startup;
        for attempt in 1..=MAX_STARTUP_ATTEMPTS {
            let sandbox = Sandbox::create(&parent, self.retention)?;
            let workdir = sandbox.root().to_owned();
            match tokio::time::timeout_at(
                deadline.into(),
                start_once(
                    sandbox,
                    source.clone(),
                    timeouts,
                    username,
                    &password,
                    &self.extra_origins,
                ),
            )
            .await
            {
                Ok(Ok(runtime)) => return Ok(runtime),
                Ok(Err((error, retryable))) => {
                    if retryable && attempt < MAX_STARTUP_ATTEMPTS && Instant::now() < deadline {
                        continue;
                    }
                    return Err(error.with_workdir(workdir));
                }
                Err(_) => {
                    return Err(TrellisTestError::new(
                        TrellisTestErrorKind::Timeout,
                        TrellisTestStage::RuntimeStart,
                        "startup exceeded its deadline",
                    )
                    .with_workdir(workdir))
                }
            }
        }
        unreachable!("every startup attempt returns or retries")
    }
}

/// Also owns partially initialized resources when startup or shutdown is cancelled.
struct Infrastructure {
    sandbox: Option<Sandbox>,
    nats: Option<LocalNats>,
    stop: Option<StopHandle>,
    task: Option<AbortOnDropHandle<Result<(), RuntimeError>>>,
    starting: bool,
}

impl Infrastructure {
    fn mark_failed(&mut self) {
        if let Some(sandbox) = self.sandbox.as_mut() {
            sandbox.mark_failed();
        }
    }

    async fn shutdown(&mut self, timeout: Duration) -> Result<(), TrellisTestError> {
        if let Some(stop) = &self.stop {
            stop.stop();
        }
        let mut failure = None;
        if let Some(task) = self.task.as_mut() {
            match tokio::time::timeout(timeout, &mut *task).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    failure = Some(runtime_error(error, TrellisTestStage::Shutdown))
                }
                Ok(Err(error)) => {
                    failure = Some(TrellisTestError::new(
                        TrellisTestErrorKind::Runtime,
                        TrellisTestStage::Shutdown,
                        format!("runtime task failed: {error}"),
                    ))
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    failure = Some(request_timeout(
                        "runtime shutdown",
                        TrellisTestStage::Shutdown,
                    ));
                }
            }
        }
        self.task = None;
        self.stop = None;
        if failure.is_some() {
            self.mark_failed();
        }
        // Both guards stay in the blocking worker if the shutdown future is cancelled.
        let nats = self.nats.take();
        let sandbox = self.sandbox.take();
        let cleanup = tokio::task::spawn_blocking(move || cleanup_broker(nats, sandbox)).await;
        match cleanup {
            Ok(Err(error)) => failure = Some(merge_failure(failure, error)),
            Err(error) => {
                failure = Some(merge_failure(
                    failure,
                    TrellisTestError::new(
                        TrellisTestErrorKind::Cleanup,
                        TrellisTestStage::Shutdown,
                        format!("broker cleanup task failed: {error}"),
                    ),
                ))
            }
            Ok(Ok(())) => {}
        }
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for Infrastructure {
    fn drop(&mut self) {
        if self.starting || std::thread::panicking() {
            self.mark_failed();
        }
        if let Some(stop) = self.stop.take() {
            stop.stop();
        }
        self.task = None; // abort-on-drop, including cancellation during startup
        let nats = self.nats.take();
        let sandbox = self.sandbox.take();
        if nats.is_some() || sandbox.is_some() {
            let _ = std::thread::Builder::new()
                .name("trellis-testkit-cleanup".into())
                .spawn(move || {
                    let _ = cleanup_broker(nats, sandbox);
                });
        }
    }
}

fn cleanup_broker(
    mut nats: Option<LocalNats>,
    mut sandbox: Option<Sandbox>,
) -> Result<(), TrellisTestError> {
    let mut failure = nats
        .as_mut()
        .and_then(|nats| nats.stop().err())
        .map(|error| nats_error(error, TrellisTestStage::Shutdown));
    drop(nats);
    if let Some(sandbox) = sandbox.as_mut() {
        if failure.is_some() {
            sandbox.mark_failed();
        }
        if let Some(error) = sandbox.cleanup() {
            failure = Some(merge_failure(failure, error));
        }
    }
    failure.map_or(Ok(()), Err)
}

async fn start_once(
    sandbox: Sandbox,
    source: NatsBinarySource,
    timeouts: TestTimeouts,
    username: &str,
    password: &str,
    extra_origins: &[String],
) -> Result<TrellisTestRuntime, (TrellisTestError, bool)> {
    let mut infrastructure = Infrastructure {
        sandbox: Some(sandbox),
        nats: None,
        stop: None,
        task: None,
        starting: true,
    };
    let workdir = infrastructure
        .sandbox
        .as_ref()
        .expect("owned sandbox")
        .root()
        .to_owned();
    let lease = PortLease::reserve().map_err(|error| (error, false))?;
    let ports = lease.ports().map_err(|error| (error, false))?;
    let public_origin = format!("http://127.0.0.1:{}", ports.http);
    let config_dir = infrastructure
        .sandbox
        .as_ref()
        .expect("owned sandbox")
        .config_dir();
    crate::sandbox::require_utf8_path(&workdir, "sandbox").map_err(|error| (error, false))?;
    let mut options = trellis_bootstrap::TrellisBootstrapOptions::new(&config_dir);
    options.runtime.trellis_port = ports.http;
    options.runtime.nats_server_url = format!("nats://127.0.0.1:{}", ports.nats);
    options.runtime.nats_websocket_url = format!("ws://127.0.0.1:{}", ports.websocket);
    options.runtime.public_origin = public_origin.clone();
    options.runtime.extra_origins = extra_origins.to_vec();
    options.nats.nats_port = ports.nats;
    options.nats.monitor_port = ports.monitor;
    options.nats.websocket_port = ports.websocket;
    trellis_bootstrap::generate_trellis_bootstrap(&options).map_err(|error| {
        (
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::ConfigGeneration,
                error.to_string(),
            ),
            false,
        )
    })?;
    let config_path = config_dir.join("config.toml");
    let (mut config, _) = RuntimeConfig::load_from_path_with_defaults(
        &config_path,
        RuntimePathDefaults {
            data: workdir.join("data"),
            state: workdir.join("state"),
            cache: workdir.join("cache"),
            runtime: workdir.join("runtime"),
            logs: workdir.join("logs"),
        },
    )
    .map_err(|error| {
        (
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::ConfigGeneration,
                error.to_string(),
            ),
            false,
        )
    })?;
    let http = config
        .http
        .as_mut()
        .expect("bootstrap generates HTTP configuration");
    http.bind_address = Some(std::net::Ipv4Addr::LOCALHOST.into());
    http.port = Some(ports.http);
    http.rate_limit_max = Some(0);
    let effective = toml::to_string_pretty(&config).map_err(|error| {
        (
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::ConfigGeneration,
                error.to_string(),
            ),
            false,
        )
    })?;
    std::fs::write(&config_path, effective).map_err(|error| (error.into(), false))?;
    trellis_runtime::platform::seed_admin_credentials(&config, username, password)
        .await
        .map_err(|error| {
            (
                runtime_error(error, TrellisTestStage::AdministratorBootstrap),
                false,
            )
        })?;

    // The blocking production broker API owns the complete startup guard. Cancelling
    // its waiter cannot delete the sandbox while broker startup is still using it.
    infrastructure = tokio::task::spawn_blocking(move || {
        let root = infrastructure
            .sandbox
            .as_ref()
            .expect("owned sandbox")
            .root();
        let builder = LocalNats::builder()
            .binary(source)
            .source(config_dir.join("nats"))
            .state(root.join("state/nats"))
            .pid_file(root.join("runtime/nats-server.pid"))
            .output(NatsOutput::Log {
                path: root.join("logs/nats-server.log"),
                mirror: false,
            })
            .ports(LocalNatsPorts {
                nats: ports.nats,
                monitor: ports.monitor,
                websocket: ports.websocket,
            });
        lease.release_for_spawn();
        match builder.start() {
            Ok(nats) => {
                infrastructure.nats = Some(nats);
                Ok(infrastructure)
            }
            Err(error) => {
                let retryable = matches!(&error, LocalNatsError::PortInUse { port } if [ports.nats, ports.monitor, ports.websocket].contains(port));
                Err((nats_error(error, TrellisTestStage::RuntimeStart), retryable))
            },
        }
    })
    .await
    .map_err(|error| {
        (
            TrellisTestError::new(
                TrellisTestErrorKind::Runtime,
                TrellisTestStage::RuntimeStart,
                format!("broker startup task failed: {error}"),
            ),
            false,
        )
    })??;
    let nats = infrastructure.nats.as_ref().expect("started broker");
    let nats_url = nats.nats_url().to_owned();
    let websocket_url = nats.websocket_url().to_owned();
    let stop = StopHandle::new();
    infrastructure.stop = Some(stop.clone());
    infrastructure.task = Some(AbortOnDropHandle::new(tokio::spawn(
        trellis_runtime::run_with_stop(
            RuntimeOptions {
                mode: RuntimeMode::All,
                config,
                reset_admin: false,
                nats_override: Some(NatsEndpointOverride {
                    runtime_servers: nats_url.clone(),
                    advertised_native: Some(vec![nats_url.clone()]),
                    advertised_websocket: Some(vec![websocket_url.clone()]),
                }),
            },
            Some(stop),
        ),
    )));
    if let Err(error) = wait_for_readyz(&public_origin, &mut infrastructure).await {
        let retryable = matches!(&error, StartupError::Runtime(RuntimeError::Server(trellis_runtime::ServerError::Bind { addr, .. })) if addr.port() == ports.http);
        let mut error = match error {
            StartupError::Runtime(error) => runtime_error(error, TrellisTestStage::RuntimeStart),
            StartupError::Test(error) => error,
        };
        infrastructure.mark_failed();
        if let Err(cleanup) = infrastructure.shutdown(timeouts.shutdown).await {
            error = error.with_cleanup(cleanup);
        }
        return Err((error, retryable));
    }
    let admin = AdminSession::connect(&public_origin, username, password)
        .await
        .map_err(|error| (error, false))?;
    admin.verify().await.map_err(|error| (error, false))?;
    infrastructure.starting = false;
    Ok(TrellisTestRuntime {
        workdir,
        trellis_url: public_origin,
        nats_url,
        websocket_url,
        monitor_url: format!("http://127.0.0.1:{}", ports.monitor),
        admin: Some(admin),
        infrastructure,
        username: username.to_owned(),
        password: password.to_owned(),
        names: Vec::new(),
        participants: Vec::new(),
        revisions: Vec::new(),
        timeouts,
        stopped: false,
    })
}

enum StartupError {
    Runtime(RuntimeError),
    Test(TrellisTestError),
}

async fn wait_for_readyz(
    origin: &str,
    infrastructure: &mut Infrastructure,
) -> Result<(), StartupError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            StartupError::Test(TrellisTestError::new(
                TrellisTestErrorKind::Runtime,
                TrellisTestStage::RuntimeStart,
                error.to_string(),
            ))
        })?;
    let task = infrastructure.task.as_mut().expect("owned runtime task");
    let readiness = async {
        loop {
            if client
                .get(format!("{origin}/readyz"))
                .send()
                .await
                .is_ok_and(|response| response.status() == reqwest::StatusCode::OK)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::select! {
        biased;
        result = task => {
            infrastructure.task = None; // a completed handle must not be polled twice
            match result {
                Ok(Err(error)) => Err(StartupError::Runtime(error)),
                Ok(Ok(())) => Err(StartupError::Test(TrellisTestError::new(TrellisTestErrorKind::Runtime, TrellisTestStage::RuntimeStart, "runtime stopped before readiness"))),
                Err(error) => Err(StartupError::Test(TrellisTestError::new(TrellisTestErrorKind::Runtime, TrellisTestStage::RuntimeStart, format!("runtime task failed: {error}")))),
            }
        },
        () = readiness => Ok(()),
    }
}

fn runtime_error(error: RuntimeError, stage: TrellisTestStage) -> TrellisTestError {
    TrellisTestError::new(TrellisTestErrorKind::Runtime, stage, error.to_string())
}

fn nats_error(error: LocalNatsError, stage: TrellisTestStage) -> TrellisTestError {
    let kind = match &error {
        LocalNatsError::InvalidBinaryPath { .. } => TrellisTestErrorKind::InvalidBinary,
        LocalNatsError::MissingBinary { .. } => TrellisTestErrorKind::MissingBinary,
        LocalNatsError::PortInUse { .. } => TrellisTestErrorKind::PortConflict,
        _ => TrellisTestErrorKind::Runtime,
    };
    TrellisTestError::new(kind, stage, error.to_string())
}

/// A running production Trellis runtime owned by the calling test.
pub struct TrellisTestRuntime {
    workdir: PathBuf,
    trellis_url: String,
    nats_url: String,
    websocket_url: String,
    monitor_url: String,
    admin: Option<AdminSession>,
    infrastructure: Infrastructure,
    username: String,
    password: String,
    names: Vec<String>,
    participants: Vec<(String, String)>,
    revisions: Vec<(String, u64)>,
    timeouts: TestTimeouts,
    stopped: bool,
}

impl TrellisTestRuntime {
    /// Creates a builder.
    #[must_use]
    pub fn builder() -> TrellisTestRuntimeBuilder {
        TrellisTestRuntimeBuilder::default()
    }
    /// Base URL of the runtime.
    #[must_use]
    pub fn trellis_url(&self) -> &str {
        &self.trellis_url
    }
    /// Native URL of the real managed NATS server.
    #[must_use]
    pub fn nats_url(&self) -> &str {
        &self.nats_url
    }
    /// WebSocket URL of the managed NATS server.
    #[must_use]
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }
    /// HTTP monitoring URL of the managed NATS server.
    #[must_use]
    pub fn monitor_url(&self) -> &str {
        &self.monitor_url
    }
    /// Sandbox work directory.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }
    /// Sandbox administrator username.
    #[must_use]
    pub fn admin_username(&self) -> &str {
        &self.username
    }
    /// Sandbox-only administrator password. Never log or upload this secret.
    #[must_use]
    pub fn admin_password(&self) -> &str {
        &self.password
    }

    fn admin(&self) -> Result<&AdminSession, TrellisTestError> {
        self.admin.as_ref().ok_or_else(|| self.stopped_error())
    }
    fn stopped_error(&self) -> TrellisTestError {
        TrellisTestError::new(
            TrellisTestErrorKind::RuntimeStopped,
            TrellisTestStage::Validation,
            "the runtime has been shut down",
        )
    }
    fn ensure_running(&self) -> Result<(), TrellisTestError> {
        if self.stopped {
            return Err(self.stopped_error());
        }
        if self
            .infrastructure
            .task
            .as_ref()
            .is_none_or(AbortOnDropHandle::is_finished)
        {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Runtime,
                TrellisTestStage::RuntimeStart,
                "the production runtime task has exited",
            ));
        }
        self.admin().map(|_| ())
    }
    fn mark_failed(&mut self) {
        self.infrastructure.mark_failed();
    }
    fn reserve_name(&mut self, name: &str) -> Result<(), TrellisTestError> {
        let trimmed = name.trim();
        if trimmed.is_empty()
            || trimmed.chars().count() > 128
            || trimmed.chars().any(char::is_control)
        {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::InvalidConfiguration,
                TrellisTestStage::Validation,
                "registration names must be 1..=128 non-control characters",
            ));
        }
        if self.names.iter().any(|existing| existing == trimmed) {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::DuplicateName,
                TrellisTestStage::Validation,
                format!("name '{trimmed}' is already reserved"),
            ));
        }
        self.names.push(trimmed.to_owned());
        Ok(())
    }

    /// Installs `P`, idempotently for an identical digest.
    ///
    /// # Errors
    /// Returns an error when installation fails or conflicts with an installed digest.
    pub async fn install_participant<P: ParticipantDescriptor>(
        &mut self,
    ) -> Result<InstalledParticipant, TrellisTestError> {
        self.ensure_running()?;
        match tokio::time::timeout(self.timeouts.request, self.install_participant_inner::<P>())
            .await
        {
            Ok(result) => result,
            Err(_) => {
                self.mark_failed();
                Err(request_timeout(
                    "installing a participant",
                    TrellisTestStage::ParticipantInstallation,
                ))
            }
        }
    }
    async fn install_participant_inner<P: ParticipantDescriptor>(
        &mut self,
    ) -> Result<InstalledParticipant, TrellisTestError> {
        let evidence = P::package_evidence();
        let digest = evidence.root_digest().to_owned();
        if let Some((installed_id, installed_digest)) =
            self.participants.iter().find(|(id, _)| id == P::ID)
        {
            if installed_digest == &digest {
                return Ok(InstalledParticipant {
                    participant_id: installed_id.clone(),
                    installed_revision: self.installed_revision(P::ID),
                });
            }
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::ParticipantConflict,
                TrellisTestStage::ParticipantInstallation,
                format!(
                    "participant {} is already installed with a different digest",
                    P::ID
                ),
            ));
        }
        let revision = match self.admin()?.install_participant::<P>().await {
            Ok(revision) => revision,
            Err(error) => {
                self.mark_failed();
                return Err(error);
            }
        };
        self.participants.push((P::ID.to_owned(), digest));
        self.revisions.push((P::ID.to_owned(), revision));
        Ok(InstalledParticipant {
            participant_id: P::ID.to_owned(),
            installed_revision: revision,
        })
    }
    fn installed_revision(&self, id: &str) -> u64 {
        self.revisions
            .iter()
            .find(|(participant, _)| participant == id)
            .map(|(_, revision)| *revision)
            .unwrap_or(0)
    }

    /// Registers `P` as a service in its own deployment and provisions an instance.
    ///
    /// # Errors
    /// Returns an error when `P` is not a service or provisioning fails.
    pub async fn register_service<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestServiceIdentity, TrellisTestError> {
        self.ensure_running()?;
        if P::KIND != trellis_rs::generated::ParticipantKind::Service {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::UnsupportedParticipantKind,
                TrellisTestStage::Validation,
                format!("{} is not a service participant", P::ID),
            ));
        }
        self.reserve_name(name)?;
        let result = match tokio::time::timeout(
            self.timeouts.request,
            self.register_service_inner::<P>(name),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(request_timeout(
                "registering a service",
                TrellisTestStage::ServiceProvisioning,
            )),
        };
        if result.is_err() {
            self.mark_failed();
        }
        result
    }
    async fn register_service_inner<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestServiceIdentity, TrellisTestError> {
        self.install_participant_inner::<P>().await?;
        let admin = self.admin()?;
        let deployment_id = admin.create_deployment(name).await?;
        admin.apply_participant::<P>(&deployment_id).await?;
        let (instance_id, seed) = admin
            .provision_service_instance(&deployment_id, P::ID)
            .await?;
        Ok(TestServiceIdentity {
            name: name.to_owned(),
            participant_id: P::ID.to_owned(),
            deployment_id,
            instance_id,
            seed,
            trellis_url: self.trellis_url.clone(),
            timeout_ms: request_ms(&self.timeouts),
        })
    }

    /// Registers `P` as an app/agent caller and completes participant-bound login.
    ///
    /// # Errors
    /// Returns an error when `P` is not an app/agent or login fails.
    pub async fn register_client<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        use trellis_rs::generated::ParticipantKind;
        self.ensure_running()?;
        if !matches!(P::KIND, ParticipantKind::App | ParticipantKind::Agent) {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::UnsupportedParticipantKind,
                TrellisTestStage::Validation,
                format!("{} is not an app or agent participant", P::ID),
            ));
        }
        self.reserve_name(name)?;
        let result = match tokio::time::timeout(
            self.timeouts.request,
            self.register_client_inner::<P>(name),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(request_timeout(
                "registering a client",
                TrellisTestStage::ClientLogin,
            )),
        };
        if result.is_err() {
            self.mark_failed();
        }
        result
    }
    async fn register_client_inner<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        self.install_participant_inner::<P>().await?;
        let session = crate::admin::login_client(
            &self.trellis_url,
            P::ID,
            &self.username,
            &self.password,
            self.admin()?,
        )
        .await?;
        Ok(TestClientIdentity {
            name: name.to_owned(),
            participant_id: P::ID.to_owned(),
            login_session_id: session.login_session_id,
            session_seed: session.session_seed,
            trellis_url: self.trellis_url.clone(),
            timeout_ms: request_ms(&self.timeouts),
        })
    }

    /// Cooperatively stops the runtime, then reaps NATS and applies retention.
    ///
    /// # Errors
    /// Preserves the runtime failure as primary when broker or sandbox cleanup also fails.
    pub async fn shutdown(&mut self) -> Result<(), TrellisTestError> {
        self.stopped = true;
        self.admin = None;
        self.infrastructure.shutdown(self.timeouts.shutdown).await
    }
}

impl Drop for TrellisTestRuntime {
    fn drop(&mut self) {
        self.admin = None;
    }
}

fn merge_failure(primary: Option<TrellisTestError>, extra: TrellisTestError) -> TrellisTestError {
    match primary {
        Some(primary) => primary.with_cleanup(extra),
        None => extra,
    }
}
fn request_timeout(operation: &str, stage: TrellisTestStage) -> TrellisTestError {
    TrellisTestError::new(
        TrellisTestErrorKind::Timeout,
        stage,
        format!("{operation} exceeded its deadline"),
    )
}
fn validate_timeouts(timeouts: &TestTimeouts) -> Result<(), TrellisTestError> {
    for (name, value) in [
        ("startup", timeouts.startup),
        ("request", timeouts.request),
        ("shutdown", timeouts.shutdown),
    ] {
        if value.is_zero() {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::InvalidConfiguration,
                TrellisTestStage::Validation,
                format!("the {name} timeout must be greater than zero"),
            ));
        }
    }
    Ok(())
}
fn validate_admin_credentials(username: &str, password: &str) -> Result<(), TrellisTestError> {
    if username.is_empty() || username.chars().any(char::is_control) || password.chars().count() < 8
    {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::InvalidConfiguration,
            TrellisTestStage::Validation,
            "administrator username must be nonempty without controls and password at least 8 characters",
        ));
    }
    Ok(())
}
fn validate_origin(origin: &str) -> Result<(), TrellisTestError> {
    let invalid = || {
        TrellisTestError::new(
            TrellisTestErrorKind::InvalidConfiguration,
            TrellisTestStage::Validation,
            format!("invalid extra origin '{origin}': expected a bare HTTP(S) origin"),
        )
    };
    let parsed = url::Url::parse(origin).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(invalid());
    }
    Ok(())
}
fn request_ms(timeouts: &TestTimeouts) -> u64 {
    u64::try_from(timeouts.request.as_millis()).unwrap_or(u64::MAX)
}
fn generate_password() -> String {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating-system randomness");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
