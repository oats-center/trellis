//! Owns a real Trellis runtime: generated bundle, managed NATS, control plane, and cleanup.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use trellis_local_nats::{LocalNats, LocalNatsPorts, NatsBinarySource, NatsOutput};
use trellis_runtime::shutdown::StopHandle;
use trellis_runtime::{
    run_with_stop, NatsEndpointOverride, RuntimeConfig, RuntimeMode, RuntimeOptions,
};

use crate::config::generate_bundle;
use crate::error::TrellisTestError;
use crate::ports::reserve_local_port;

/// Default local administrator username for a test runtime.
pub const DEFAULT_ADMIN_USERNAME: &str = "admin";

/// Readiness and shutdown budgets for a test runtime.
#[derive(Clone, Debug)]
pub struct TrellisTestTimeouts {
    /// How long the control plane may take to answer `/readyz`.
    pub startup_ms: u64,
    /// Default budget for [`TrellisTestRuntime::wait_for`].
    pub wait_for_ms: u64,
    /// How long a cooperative stop may take before the harness gives up.
    pub shutdown_ms: u64,
}

impl Default for TrellisTestTimeouts {
    fn default() -> Self {
        Self {
            startup_ms: 60_000,
            wait_for_ms: 10_000,
            shutdown_ms: 10_000,
        }
    }
}

/// Options for [`TrellisTestRuntime::start`].
#[derive(Clone, Debug)]
pub struct TrellisTestRuntimeOptions {
    /// Keep the work directory after the runtime stops, for evidence.
    pub keep_workdir: bool,
    /// Default deployment name helpers act on.
    pub deployment: String,
    /// Local administrator username seeded into the fresh platform.
    pub admin_username: String,
    /// Local administrator password seeded into the fresh platform.
    pub admin_password: String,
    /// Where the managed `nats-server` binary comes from.
    pub nats_binary: NatsBinarySource,
    /// Override the binary cache directory for pinned NATS downloads.
    pub nats_cache_dir: Option<PathBuf>,
    /// Additional origins the runtime accepts, for callers served from elsewhere.
    ///
    /// A browser app serving from its own development origin (for example
    /// `http://localhost:5174`) needs its origin here to complete a portal login.
    pub extra_origins: Vec<String>,
    /// Readiness and shutdown budgets.
    pub timeouts: TrellisTestTimeouts,
}

impl Default for TrellisTestRuntimeOptions {
    fn default() -> Self {
        Self {
            keep_workdir: false,
            deployment: "test".to_owned(),
            admin_username: DEFAULT_ADMIN_USERNAME.to_owned(),
            admin_password: format!("trellis-test-{}", ulid::Ulid::new()),
            nats_binary: NatsBinarySource::DownloadPinned,
            nats_cache_dir: None,
            extra_origins: Vec::new(),
            timeouts: TrellisTestTimeouts::default(),
        }
    }
}

/// A running Trellis runtime owned by the calling test.
pub struct TrellisTestRuntime {
    workdir: PathBuf,
    keep_workdir: bool,
    temp: Option<TempDir>,
    trellis_url: String,
    nats_url: String,
    admin_username: String,
    admin_password: String,
    deployment: String,
    wait_for_ms: u64,
    shutdown_ms: u64,
    stop: StopHandle,
    task: Option<tokio::task::JoinHandle<Result<(), trellis_runtime::RuntimeError>>>,
    nats: Option<LocalNats>,
    outcome: Arc<Mutex<Option<String>>>,
}

impl TrellisTestRuntime {
    /// Starts a fresh runtime in a temporary work directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the bundle cannot be generated, the managed NATS server cannot be
    /// started, or the control plane does not become ready before `startup_ms`.
    pub async fn start(options: TrellisTestRuntimeOptions) -> Result<Self, TrellisTestError> {
        let temp = tempfile::Builder::new()
            .prefix("trellis-test-")
            .tempdir()?;
        let workdir = temp.path().to_path_buf();

        // The pinned NATS binary lives in a test-only cache shared by every run, so a suite
        // downloads it once and never writes into the user's configuration or data directories.
        let cache_dir = options
            .nats_cache_dir
            .clone()
            .unwrap_or_else(default_nats_cache_dir);
        create_private_dir(&cache_dir)?;

        let mut http = reserve_local_port()?;
        let mut nats_port = reserve_local_port()?;
        let mut monitor = reserve_local_port()?;
        let mut websocket = reserve_local_port()?;
        let bundle = generate_bundle(
            &workdir,
            "Trellis Test",
            &http,
            &nats_port,
            &monitor,
            &websocket,
            &options.extra_origins,
        )?;
        // The managed NATS server and the runtime stores both write beneath the work directory,
        // so create every root before either starts.
        for dir in [
            &bundle.nats_state,
            &bundle.nats_cache,
            &workdir.join("data"),
            &workdir.join("run"),
            &workdir.join("logs"),
        ] {
            create_private_dir(dir)?;
        }

        // Hand the reserved NATS listeners to the managed server.
        nats_port.release_for_bind();
        monitor.release_for_bind();
        websocket.release_for_bind();
        let nats = LocalNats::builder()
            .binary(options.nats_binary.clone())
            .source(bundle.nats_source.clone())
            .state(bundle.nats_state.clone())
            .ports(LocalNatsPorts {
                nats: nats_port.port(),
                monitor: monitor.port(),
                websocket: websocket.port(),
            })
            .cache_dir(cache_dir.clone())
            .pid_file(bundle.nats_pid.clone())
            .output(NatsOutput::Log {
                path: bundle.nats_log.clone(),
                mirror: false,
            })
            .start()
            .map_err(|error| TrellisTestError::Nats(error.to_string()))?;

        let trellis_url = format!("http://localhost:{}", http.port());
        let (config, _defaults) = RuntimeConfig::load_from_path_with_defaults(
            &bundle.config_path,
            trellis_runtime::RuntimePathDefaults {
                data: workdir.join("data"),
                state: workdir.join("state"),
                cache: workdir.join("cache"),
                runtime: workdir.join("run"),
                logs: workdir.join("logs"),
            },
        )
        .map_err(|error| TrellisTestError::Config(error.to_string()))?;

        // Seed the local administrator before the control plane starts: the same effect as
        // `trellis-server bootstrap-admin`, so no one-time setup flow is created and the harness
        // can complete a real login later.
        trellis_runtime::platform::seed_admin_credentials(
            &config,
            &options.admin_username,
            &options.admin_password,
        )
        .await
        .map_err(|error| TrellisTestError::Runtime(format!("seeding the administrator: {error}")))?;

        let nats_override = NatsEndpointOverride {
            servers: nats.nats_url().to_string(),
            websocket: Some(nats.websocket_url().to_string()),
        };
        let nats_url = nats.nats_url().to_string();
        let stop = StopHandle::new();
        let runtime_stop = stop.clone();
        let outcome = Arc::new(Mutex::new(None));
        let task_outcome = Arc::clone(&outcome);
        http.release_for_bind();
        let task = tokio::spawn(async move {
            let result = run_with_stop(
                RuntimeOptions {
                    mode: RuntimeMode::All,
                    config,
                    reset_admin: false,
                    nats_override: Some(nats_override),
                },
                Some(runtime_stop),
            )
            .await;
            let message = match &result {
                Ok(()) => "control plane stopped".to_owned(),
                Err(error) => format!("control plane failed: {error}"),
            };
            if let Ok(mut outcome) = task_outcome.lock() {
                *outcome = Some(message);
            }
            result
        });

        let mut runtime = Self {
            workdir,
            keep_workdir: options.keep_workdir,
            temp: Some(temp),
            trellis_url,
            nats_url,
            admin_username: options.admin_username,
            admin_password: options.admin_password,
            deployment: options.deployment,
            wait_for_ms: options.timeouts.wait_for_ms,
            shutdown_ms: options.timeouts.shutdown_ms,
            stop,
            task: Some(task),
            nats: Some(nats),
            outcome,
        };
        if let Err(error) = runtime.wait_for_ready(options.timeouts.startup_ms).await {
            if runtime.keep_workdir {
                if let Some(temp) = runtime.temp.take() {
                    let _ = temp.keep();
                }
                eprintln!(
                    "trellis-test kept workdir at {}",
                    runtime.workdir().display()
                );
            }
            return Err(error);
        }
        Ok(runtime)
    }

    async fn wait_for_ready(&self, timeout_ms: u64) -> Result<(), TrellisTestError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .map_err(|error| TrellisTestError::Http(error.to_string()))?;
        loop {
            if let Some(message) = self.outcome() {
                return Err(TrellisTestError::Runtime(message));
            }
            if let Ok(response) = client.get(format!("{}/readyz", self.trellis_url)).send().await {
                if response.status().is_success() {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(TrellisTestError::TimedOut(format!(
                    "waiting for {}/readyz after {timeout_ms} ms",
                    self.trellis_url
                )));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn outcome(&self) -> Option<String> {
        self.outcome.lock().ok().and_then(|outcome| outcome.clone())
    }

    /// Base URL of the control plane.
    #[must_use]
    pub fn trellis_url(&self) -> &str {
        &self.trellis_url
    }

    /// URL of the runtime's NATS server.
    #[must_use]
    pub fn nats_url(&self) -> &str {
        &self.nats_url
    }

    /// Work directory holding the bundle, stores, and logs.
    #[must_use]
    pub fn workdir(&self) -> &PathBuf {
        &self.workdir
    }

    /// Local administrator username.
    #[must_use]
    pub fn admin_username(&self) -> &str {
        &self.admin_username
    }

    /// Local administrator password.
    #[must_use]
    pub fn admin_password(&self) -> &str {
        &self.admin_password
    }

    /// Default deployment name.
    #[must_use]
    pub fn deployment(&self) -> &str {
        &self.deployment
    }

    /// Path to a fresh SQLite file inside the runtime work directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created.
    pub fn temp_sqlite_path(&self, name: &str) -> Result<PathBuf, TrellisTestError> {
        let dir = self.workdir.join("sqlite");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(name))
    }

    /// In-memory SQLite URL, for tests that need no file.
    #[must_use]
    pub fn sqlite_memory_url() -> &'static str {
        ":memory:"
    }

    /// Polls `check` until it returns a truthy value.
    ///
    /// # Errors
    ///
    /// Returns a timeout error when no value arrives inside `timeout_ms`.
    pub async fn wait_for<T, F, Fut>(&self, timeout_ms: u64, mut check: F) -> Result<T, TrellisTestError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if let Some(value) = check().await {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(TrellisTestError::TimedOut(format!(
                    "waiting for a truthy value after {timeout_ms} ms"
                )));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Polls `check` with the runtime's default wait budget.
    ///
    /// # Errors
    ///
    /// Returns a timeout error when no value arrives inside the configured budget.
    pub async fn wait<T, F, Fut>(&self, check: F) -> Result<T, TrellisTestError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        self.wait_for(self.wait_for_ms, check).await
    }

    /// Recent control-plane state. In-process ownership reports the terminal state instead of a
    /// log tail; callers that need logs read `logs/` under [`Self::workdir`].
    #[must_use]
    pub fn control_plane_output(&self) -> String {
        self.outcome().unwrap_or_else(|| "control plane running".to_owned())
    }

    /// Stops the control plane, NATS, and (unless kept) the work directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the control plane does not stop inside the shutdown budget.
    pub async fn stop(&mut self) -> Result<(), TrellisTestError> {
        self.stop.stop();
        let mut failure = None;
        if let Some(task) = self.task.take() {
            match tokio::time::timeout(Duration::from_millis(self.shutdown_ms), task).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => failure = Some(TrellisTestError::Runtime(error.to_string())),
                Ok(Err(join)) => failure = Some(TrellisTestError::Runtime(join.to_string())),
                Err(_) => {
                    failure = Some(TrellisTestError::TimedOut(
                        "stopping the control plane".to_owned(),
                    ));
                }
            }
        }
        if let Some(mut nats) = self.nats.take() {
            if let Err(error) = nats.stop() {
                failure.get_or_insert(TrellisTestError::Nats(error.to_string()));
            }
        }
        if self.keep_workdir {
            if let Some(temp) = self.temp.take() {
                let _ = temp.keep();
            }
        } else if let Some(temp) = self.temp.take() {
            drop(temp);
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Test-only cache holding the pinned NATS binaries, mirroring the TypeScript harness.
fn default_nats_cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TRELLIS_TEST_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("trellis-test")
}

/// Creates a directory (and its parents) readable only by the owning user, which the managed
/// NATS binary cache and runtime stores require.
fn create_private_dir(path: &std::path::Path) -> Result<(), TrellisTestError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

impl Drop for TrellisTestRuntime {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(mut nats) = self.nats.take() {
            let _ = nats.stop();
        }
    }
}
