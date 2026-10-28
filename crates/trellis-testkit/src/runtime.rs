//! Production runtime hosting, isolated bootstrap, and managed broker ownership.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::distribution::{Distribution, NativeSource};
use crate::process::Process;
use std::process::{Command, Stdio};
use trellis_rs::generated::{AppRequestDescriptor, ParticipantDescriptor};

use crate::admin::{AdminSession, ADMIN_USERNAME};
use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use crate::identity::{
    TestClientIdentity, TestRegisteredNativeClient, TestServiceIdentity, TestUserIdentity,
};
use crate::sandbox::{ensure_supported_platform, PortLease, Sandbox, WorkdirRetention};

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

/// Consuming builder for production Trellis and NATS processes.
#[derive(Clone)]
pub struct TrellisTestRuntimeBuilder {
    nats: Option<NatsSource>,
    native: Option<NativeSource>,
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
            native: None,
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
    /// Select production CLI and server paths, or verified release acquisition.
    #[must_use]
    pub fn native(mut self, source: NativeSource) -> Self {
        self.native = Some(source);
        self
    }
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
    /// Replaces the additional allowed browser origins.
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

    /// Starts isolated production executables, acquiring pinned releases when needed.
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
        let parent = self
            .workdir_parent
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let sandbox = Sandbox::create(&parent, self.retention)?;
        let workdir = sandbox.root().to_owned();
        match tokio::time::timeout(
            timeouts.startup,
            start_once(
                sandbox,
                source,
                self.native,
                timeouts,
                username,
                &password,
                &self.extra_origins,
            ),
        )
        .await
        {
            Ok(result) => result.map_err(|error| error.with_workdir(workdir)),
            Err(_) => Err(TrellisTestError::new(
                TrellisTestErrorKind::Timeout,
                TrellisTestStage::RuntimeStart,
                "startup exceeded its deadline",
            )
            .with_workdir(workdir)),
        }
    }
}

/// Also owns partially initialized resources when startup or shutdown is cancelled.
struct Infrastructure {
    sandbox: Option<Sandbox>,
    nats: Option<Process>,
    port_ownership: Option<[UdpSocket; 5]>,
    server: Option<Process>,
    starting: bool,
}

impl Infrastructure {
    fn mark_failed(&mut self) {
        if let Some(sandbox) = self.sandbox.as_mut() {
            sandbox.mark_failed();
        }
    }
    async fn shutdown(&mut self, timeout: Duration) -> Result<(), TrellisTestError> {
        if self.starting {
            self.mark_failed();
        }
        let deadline = Instant::now() + timeout;
        let mut failure = None;
        if let Some(server) = self.server.as_mut() {
            if let Err(error) = server
                .stop(deadline.saturating_duration_since(Instant::now()))
                .await
            {
                failure = Some(error);
            }
        }
        self.server = None;
        if let Some(nats) = self.nats.as_mut() {
            if let Err(error) = nats
                .stop(deadline.saturating_duration_since(Instant::now()))
                .await
            {
                failure = Some(merge_failure(failure, error));
            }
        }
        self.nats = None;
        if failure.is_some() {
            self.mark_failed();
        }
        if let Some(sandbox) = self.sandbox.as_mut() {
            if let Some(error) = sandbox.cleanup() {
                failure = Some(merge_failure(failure, error));
            }
        }
        self.sandbox = None;
        self.port_ownership = None;
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for Infrastructure {
    fn drop(&mut self) {
        if self.starting || std::thread::panicking() {
            self.mark_failed();
        }
        // Reap both children before any sandbox cleanup, including cancelled startup.
        self.server = None;
        self.nats = None;
        if let Some(sandbox) = self.sandbox.as_mut() {
            let _ = sandbox.cleanup();
        }
    }
}

async fn start_once(
    sandbox: Sandbox,
    source: NatsSource,
    native: Option<NativeSource>,
    timeouts: TestTimeouts,
    username: &str,
    password: &str,
    extra_origins: &[String],
) -> Result<TrellisTestRuntime, TrellisTestError> {
    let mut infrastructure = Infrastructure {
        sandbox: Some(sandbox),
        nats: None,
        port_ownership: None,
        server: None,
        starting: true,
    };
    let result = async {
        let workdir = infrastructure
            .sandbox
            .as_ref()
            .expect("owned sandbox")
            .root()
            .to_owned();
        let distribution = crate::distribution::resolve(native, &source).await?;
        let lease = PortLease::reserve()?;
        let ports = lease.ports()?;
        // Live tests run on the generated installation TLS that ordinary
        // initialization produces; clients trust only this runtime's CA.
        let public_origin = format!("https://127.0.0.1:{}", ports.http);
        let oauth_origin = format!("https://127.0.0.1:{}", ports.oauth);
        let nats_url = format!("tls://127.0.0.1:{}", ports.nats);
        let websocket_url = format!("wss://127.0.0.1:{}", ports.websocket);
        let config_dir = infrastructure
            .sandbox
            .as_ref()
            .expect("owned sandbox")
            .config_dir();
        let config_path = config_dir.join("config.toml");
        std::fs::create_dir_all(workdir.join("logs"))?;
        let mut command = Command::new(&distribution.server);
        command
            .args(["init", "--out"])
            .arg(&config_dir)
            .args([
                "--trellis-port",
                &ports.http.to_string(),
                "--nats-port",
                &ports.nats.to_string(),
                "--nats-monitor-port",
                &ports.monitor.to_string(),
                "--nats-ws-port",
                &ports.websocket.to_string(),
                "--nats-server-url",
                &nats_url,
                "--nats-websocket-url",
                &websocket_url,
                "--public-origin",
                &public_origin,
                "--oauth-origin",
                &oauth_origin,
                "--oauth-port",
                &ports.oauth.to_string(),
                "--bind-address",
                "127.0.0.1",
                "--nats-data-dir",
            ])
            .arg(workdir.join("state/nats/data"));
        for origin in extra_origins {
            command.arg("--extra-origin").arg(origin);
        }
        crate::process::run(
            &mut command,
            &workdir.join("logs/bootstrap.log"),
            None,
            TrellisTestStage::ConfigGeneration,
        )
        .await?;
        let mut config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)
            .map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Bootstrap,
                    TrellisTestStage::ConfigGeneration,
                    error.to_string(),
                )
            })?;
        let mut paths = toml::map::Map::new();
        for name in ["data", "state", "cache", "runtime", "logs"] {
            let path = workdir.join(name);
            std::fs::create_dir_all(&path)?;
            paths.insert(
                name.to_owned(),
                crate::sandbox::require_utf8_path(&path, name)?
                    .to_owned()
                    .into(),
            );
        }
        config
            .as_table_mut()
            .ok_or_else(|| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Bootstrap,
                    TrellisTestStage::ConfigGeneration,
                    "generated configuration is not a TOML table",
                )
            })?
            .insert("paths".into(), toml::Value::Table(paths));
        std::fs::write(
            &config_path,
            toml::to_string_pretty(&config).map_err(|error| {
                TrellisTestError::new(
                    TrellisTestErrorKind::Bootstrap,
                    TrellisTestStage::ConfigGeneration,
                    error.to_string(),
                )
            })?,
        )?;
        crate::process::run(
            Command::new(&distribution.server)
                .args(["init", "admin", "--config"])
                .arg(&config_path)
                .args(["--username", username, "--password-stdin"]),
            &workdir.join("logs/bootstrap.log"),
            Some(password),
            TrellisTestStage::AdministratorBootstrap,
        )
        .await?;
        infrastructure.port_ownership = Some(lease.release_for_spawn());
        infrastructure.nats = Some(Process::spawn(
            Command::new(&distribution.nats)
                .arg("-c")
                .arg(config_dir.join("nats/nats.conf"))
                .stdin(Stdio::null()),
            &workdir.join("logs/nats-server.log"),
        )?);
        let broker_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            infrastructure
                .nats
                .as_mut()
                .expect("owned broker")
                .check_startup()?;
            let mut ready = true;
            for port in [ports.nats, ports.monitor, ports.websocket] {
                ready &= tokio::time::timeout(Duration::from_millis(100), async {
                    let socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
                    if port == ports.nats {
                        // Another concurrent pre-exec child can still hold a
                        // reservation listener. TCP connect alone therefore
                        // cannot prove that the production broker is ready.
                        let mut info = [0; 5];
                        Ok::<_, std::io::Error>(
                            socket.peek(&mut info).await? == info.len() && info == *b"INFO ",
                        )
                    } else {
                        Ok(true)
                    }
                })
                .await
                .is_ok_and(|result| result.unwrap_or(false));
            }
            if ready {
                break;
            }
            if Instant::now() >= broker_deadline {
                return Err(request_timeout(
                    "broker readiness",
                    TrellisTestStage::RuntimeStart,
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        infrastructure.server = Some(Process::spawn(
            Command::new(&distribution.server)
                .arg("--config")
                .arg(&config_path)
                .stdin(Stdio::null()),
            &workdir.join("logs/trellis-server.log"),
        )?);
        let trust_roots = vec![config_dir.join("tls/ca.crt")];
        wait_for_readyz(&public_origin, &trust_roots, &mut infrastructure).await?;
        let admin = AdminSession::connect(
            &oauth_origin,
            &trust_roots,
            &workdir.join("credentials/admin"),
            username,
            password,
        )
        .await?;
        let admin_principal_id = admin.verify().await?;
        Ok((
            workdir,
            distribution,
            config_path,
            public_origin,
            oauth_origin,
            trust_roots,
            nats_url,
            websocket_url,
            ports.monitor,
            admin,
            admin_principal_id,
        ))
    }
    .await;
    match result {
        Ok((
            workdir,
            distribution,
            config,
            trellis_url,
            oauth_origin,
            trust_roots,
            nats_url,
            websocket_url,
            monitor,
            admin,
            admin_principal_id,
        )) => {
            infrastructure.starting = false;
            Ok(TrellisTestRuntime {
                config,
                distribution,
                workdir,
                trellis_url,
                oauth_origin,
                trust_roots,
                nats_url,
                websocket_url,
                monitor_url: format!("http://127.0.0.1:{monitor}"),
                admin: Some(admin),
                admin_principal_id,
                infrastructure,
                username: username.to_owned(),
                password: password.to_owned(),
                names: Vec::new(),
                prepared_client_capabilities: Default::default(),
                users: Default::default(),
                native_applications: Default::default(),
                timeouts,
                stopped: false,
            })
        }
        Err(mut error) => {
            infrastructure.mark_failed();
            if let Err(cleanup) = infrastructure.shutdown(timeouts.shutdown).await {
                error = error.with_cleanup(cleanup);
            }
            Err(error)
        }
    }
}

/// Builds an HTTP client that trusts only `trust_roots` when any are given.
pub(crate) fn https_client_builder(
    trust_roots: &[PathBuf],
) -> Result<reqwest::ClientBuilder, std::io::Error> {
    let builder = reqwest::Client::builder();
    if trust_roots.is_empty() {
        return Ok(builder);
    }
    let mut certificates = Vec::new();
    for root in trust_roots {
        certificates.extend(
            reqwest::Certificate::from_pem_bundle(&std::fs::read(root)?)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
        );
    }
    Ok(builder.tls_certs_only(certificates))
}

async fn wait_for_readyz(
    origin: &str,
    trust_roots: &[PathBuf],
    infrastructure: &mut Infrastructure,
) -> Result<(), TrellisTestError> {
    let client = https_client_builder(trust_roots)
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Runtime,
                TrellisTestStage::RuntimeStart,
                error.to_string(),
            )
        })?
        .timeout(Duration::from_secs(1))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Runtime,
                TrellisTestStage::RuntimeStart,
                error.to_string(),
            )
        })?;
    loop {
        for process in [&mut infrastructure.nats, &mut infrastructure.server] {
            process
                .as_mut()
                .ok_or_else(|| {
                    TrellisTestError::new(
                        TrellisTestErrorKind::Runtime,
                        TrellisTestStage::RuntimeStart,
                        "missing production process",
                    )
                })?
                .check_startup()?;
        }
        let process_id = u64::from(
            infrastructure
                .server
                .as_ref()
                .expect("checked server owner")
                .id(),
        );
        match client.get(format!("{origin}/readyz")).send().await {
            Ok(response) if response.status() == reqwest::StatusCode::OK => {
                if response
                    .json::<serde_json::Value>()
                    .await
                    .is_ok_and(|metadata| metadata["processId"].as_u64() == Some(process_id))
                {
                    return Ok(());
                }
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A running production Trellis runtime owned by the calling test.
pub struct TrellisTestRuntime {
    config: PathBuf,
    distribution: Distribution,
    workdir: PathBuf,
    trellis_url: String,
    oauth_origin: String,
    trust_roots: Vec<PathBuf>,
    nats_url: String,
    websocket_url: String,
    monitor_url: String,
    admin: Option<AdminSession>,
    admin_principal_id: String,
    infrastructure: Infrastructure,
    username: String,
    password: String,
    names: Vec<String>,
    prepared_client_capabilities: std::collections::BTreeSet<(String, String)>,
    users: std::collections::BTreeMap<String, (TestUserIdentity, String)>,
    native_applications: std::collections::BTreeMap<String, String>,
    timeouts: TestTimeouts,
    stopped: bool,
}

impl TrellisTestRuntime {
    /// Restart the ordinary runtime against its existing persistent data and broker.
    /// Existing callers must reconnect; no credentials or platform state are reseeded.
    ///
    /// # Errors
    /// Returns shutdown, startup, readiness, or administrator reconnect failures.
    pub async fn restart(&mut self) -> Result<(), TrellisTestError> {
        if self.stopped {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::RuntimeStopped,
                TrellisTestStage::RuntimeStart,
                "the test runtime has already stopped",
            ));
        }
        self.infrastructure.starting = true;
        self.admin = None;
        if let Some(server) = self.infrastructure.server.as_mut() {
            server.stop(self.timeouts.shutdown).await?;
        }
        self.infrastructure.server = Some(Process::spawn(
            Command::new(&self.distribution.server)
                .arg("--config")
                .arg(&self.config)
                .stdin(Stdio::null()),
            &self.workdir.join("logs/trellis-server.log"),
        )?);
        tokio::time::timeout(
            self.timeouts.startup,
            wait_for_readyz(
                &self.trellis_url,
                &self.trust_roots,
                &mut self.infrastructure,
            ),
        )
        .await
        .map_err(|_| request_timeout("runtime restart", TrellisTestStage::RuntimeStart))??;
        self.admin = Some(
            AdminSession::connect(
                &self.oauth_origin,
                &self.trust_roots,
                &self.workdir.join("credentials/admin"),
                &self.username,
                &self.password,
            )
            .await?,
        );
        self.admin_principal_id = self
            .admin
            .as_ref()
            .expect("reconnected administrator")
            .verify()
            .await?;
        self.infrastructure.starting = false;
        Ok(())
    }

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
    /// Isolated OAuth issuer and portal origin, distinct from the application listener.
    #[must_use]
    pub fn oauth_origin(&self) -> &str {
        &self.oauth_origin
    }
    /// PEM files of this runtime's generated installation CA.
    ///
    /// Clients of this runtime must trust them explicitly, exactly as a real
    /// deployment's users do; they are never installed into system trust.
    #[must_use]
    pub fn trust_roots(&self) -> Vec<PathBuf> {
        self.trust_roots.clone()
    }

    /// HTTP client builder trusting only this runtime's installation CA.
    ///
    /// # Panics
    /// Panics when the generated CA certificate cannot be read.
    pub fn https_client_builder(&self) -> reqwest::ClientBuilder {
        https_client_builder(&self.trust_roots).expect("read generated installation CA")
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

    /// Log in to the ordinary portal and explicitly approve a pending native
    /// device flow. The caller retains its own SDK custody and token exchange.
    /// Only available consent items are approved; unavailable required items fail.
    pub async fn approve_device_login(
        &mut self,
        client_id: &str,
        user_code: &str,
    ) -> Result<(), TrellisTestError> {
        self.ensure_running()?;
        let result = tokio::time::timeout(
            self.timeouts.request,
            crate::admin::approve_device(
                &self.oauth_origin,
                &self.trust_roots,
                client_id,
                user_code,
                &self.username,
                &self.password,
            ),
        )
        .await;
        match result {
            Ok(result) => {
                if result.is_err() {
                    self.mark_failed();
                }
                result
            }
            Err(_) => {
                self.mark_failed();
                Err(request_timeout(
                    "approving native device login",
                    TrellisTestStage::ClientLogin,
                ))
            }
        }
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
            .server
            .as_ref()
            .map(Process::running)
            .transpose()?
            != Some(true)
        {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Runtime,
                TrellisTestStage::RuntimeStart,
                "the production server process has exited",
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

    /// Registers `P` as a service in its own deployment and provisions an instance.
    /// Reviews implemented and consumed APIs before deployment creation, including
    /// optional consumption, so consumers may be registered before providers.
    ///
    /// # Errors
    /// Returns an error when `P` is not a service or provisioning fails.
    pub async fn register_service<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestServiceIdentity, TrellisTestError> {
        self.register::<P>(name, false).await
    }

    /// Registers `P` like [`Self::register_service`] when another deployment
    /// already serves an API `P` implements. Every deployment serving an API
    /// shares its requests at random, so Auth requires this explicit
    /// acknowledgement; use it only to test that situation. Replicas of one
    /// service are instances of one deployment instead.
    ///
    /// # Errors
    /// Returns an error when `P` is not a service or provisioning fails.
    pub async fn register_service_sharing_apis<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestServiceIdentity, TrellisTestError> {
        self.register::<P>(name, true).await
    }

    async fn register<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
        acknowledge_shared_apis: bool,
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
            self.register_service_inner::<P>(name, acknowledge_shared_apis),
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
        acknowledge_shared_apis: bool,
    ) -> Result<TestServiceIdentity, TrellisTestError> {
        let admin = self.admin()?;
        admin.accept_apis::<P>().await?;
        let deployment_id = admin
            .create_deployment::<P>(name, acknowledge_shared_apis)
            .await?;
        let (instance_id, seed) = admin.provision_service_instance(&deployment_id).await?;
        Ok(TestServiceIdentity {
            name: name.to_owned(),
            participant_id: P::ID.to_owned(),
            deployment_id,
            instance_id,
            seed,
            trellis_url: self.trellis_url.clone(),
            nats_servers: admin.nats_servers.clone(),
            trust: admin.trust.clone(),
            timeout_ms: request_ms(&self.timeouts),
        })
    }

    /// Registers a native OAuth client and approves the generated application's request.
    ///
    /// # Errors
    /// Returns an error for browser requests, unavailable authority, or failed login.
    pub async fn register_client<P: AppRequestDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        let result = tokio::time::timeout(self.timeouts.request, async {
            let app = self.register_native_application::<P>(name).await?;
            let capabilities: Vec<_> = P::REQUIRED_CAPABILITIES
                .iter()
                .chain(P::OPTIONAL_CAPABILITIES)
                .filter(|capability| {
                    !self
                        .prepared_client_capabilities
                        .contains(&(self.admin_principal_id.clone(), (**capability).to_owned()))
                })
                .copied()
                .collect();
            if !capabilities.is_empty() {
                self.admin()?
                    .prepare_client_capabilities(&self.admin_principal_id, &capabilities)
                    .await?;
                self.prepared_client_capabilities.extend(
                    capabilities
                        .into_iter()
                        .map(|capability| (self.admin_principal_id.clone(), capability.to_owned())),
                );
            }
            self.login_native_application(&app, &self.username, &self.password)
                .await
        })
        .await
        .unwrap_or_else(|_| {
            Err(request_timeout(
                "registering a client",
                TrellisTestStage::ClientLogin,
            ))
        });
        if result.is_err() {
            self.mark_failed();
        }
        result
    }

    /// Create an ordinary account through Auth, retaining its random password privately.
    ///
    /// # Errors
    /// Returns an error when the runtime is stopped or authenticated creation fails.
    pub async fn create_user(&mut self, name: &str) -> Result<TestUserIdentity, TrellisTestError> {
        self.ensure_running()?;
        let username = format!(
            "user-{}",
            ulid::Ulid::new().to_string().to_ascii_lowercase()
        );
        let password = generate_password();
        let user = self
            .admin()?
            .create_user(&username, name, &password)
            .await?;
        self.users
            .insert(user.principal_id.clone(), (user.clone(), password));
        Ok(user)
    }

    /// Explicitly grant capabilities to this runtime's ordinary user; login never regrants them.
    ///
    /// # Errors
    /// Rejects foreign handles, stopped runtimes, and failed administrative grants.
    pub async fn grant_user_capabilities(
        &mut self,
        user: &TestUserIdentity,
        capability_ids: &[&str],
    ) -> Result<(), TrellisTestError> {
        self.ensure_running()?;
        if !self
            .users
            .get(&user.principal_id)
            .is_some_and(|(owned, _)| owned == user)
        {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::Validation,
                "test user does not belong to this runtime",
            ));
        }
        self.admin()?
            .prepare_client_capabilities(&user.principal_id, capability_ids)
            .await?;
        self.prepared_client_capabilities.extend(
            capability_ids
                .iter()
                .map(|capability| (user.principal_id.clone(), (*capability).to_owned())),
        );
        Ok(())
    }

    /// Register one native application without logging in or granting user capabilities.
    ///
    /// # Errors
    /// Rejects non-native requests, duplicate names, stopped runtimes, and failed registration.
    pub async fn register_native_application<P: AppRequestDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestRegisteredNativeClient<P>, TrellisTestError> {
        self.ensure_running()?;
        if P::KIND != trellis_rs::generated::AppKind::Native {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::UnsupportedParticipantKind,
                TrellisTestStage::Validation,
                format!("{} is not a native application request", P::ID),
            ));
        }
        self.reserve_name(name)?;
        let client_id = self.admin()?.register_native_client(name).await?;
        self.native_applications
            .insert(client_id.clone(), P::ID.to_owned());
        Ok(TestRegisteredNativeClient {
            name: name.to_owned(),
            client_id,
            request: std::marker::PhantomData,
        })
    }

    /// Complete independent Device Code and Portal consent for a user on the same registered app.
    ///
    /// # Errors
    /// Rejects foreign handles and real authentication/consent failures, without admin fallback.
    pub async fn login_native_application_as<P: AppRequestDescriptor>(
        &self,
        app: &TestRegisteredNativeClient<P>,
        user: &TestUserIdentity,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        self.ensure_running()?;
        let Some((owned, password)) = self
            .users
            .get(&user.principal_id)
            .filter(|(owned, _)| owned == user)
        else {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::Validation,
                "test user does not belong to this runtime",
            ));
        };
        self.login_native_application(app, &owned.username, password)
            .await
    }

    async fn login_native_application<P: AppRequestDescriptor>(
        &self,
        app: &TestRegisteredNativeClient<P>,
        username: &str,
        password: &str,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        if self
            .native_applications
            .get(&app.client_id)
            .map(String::as_str)
            != Some(P::ID)
        {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Authentication,
                TrellisTestStage::Validation,
                "native application does not belong to this runtime",
            ));
        }
        let (session, nats_servers) = tokio::time::timeout(
            self.timeouts.request,
            crate::admin::login_client(
                &self.oauth_origin,
                &self.trust_roots,
                &self
                    .workdir
                    .join("credentials")
                    .join(&app.client_id)
                    .join(ulid::Ulid::new().to_string()),
                &app.client_id,
                &serde_json::json!([
                    {
                        "type":"trellis_capabilities",
                        "required":P::REQUIRED_CAPABILITIES,
                        "optional":P::OPTIONAL_CAPABILITIES
                    }
                ]),
                username,
                password,
            ),
        )
        .await
        .map_err(|_| {
            request_timeout(
                "logging in a native application",
                TrellisTestStage::ClientLogin,
            )
        })??;
        Ok(TestClientIdentity {
            name: app.name.clone(),
            client_id: app.client_id.clone(),
            session,
            nats_servers,
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
    if username.is_empty()
        || username.chars().any(char::is_control)
        || password.chars().count() < 8
        || password.contains(['\r', '\n'])
    {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::InvalidConfiguration,
            TrellisTestStage::Validation,
            "administrator username must be nonempty without controls and password \
            at least 8 characters without line breaks",
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
