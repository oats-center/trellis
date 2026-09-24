//! Builder, out-of-process startup state machine, and runtime ownership.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use trellis_rs::generated::ParticipantDescriptor;

use crate::admin::{bootstrap_first_admin, AdminSession, ADMIN_USERNAME};
use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use crate::identity::{InstalledParticipant, TestClientIdentity, TestServiceIdentity};
use crate::process::{run_captured, OutputTail, ProcessSupervisor};
use crate::sandbox::{ensure_supported_platform, PortLease, Sandbox, WorkdirRetention};

/// Maximum number of startup attempts, sharing one overall deadline.
const MAX_STARTUP_ATTEMPTS: usize = 3;

/// Where the managed NATS executable comes from.
#[derive(Clone, Debug)]
pub enum NatsSource {
    /// Use an explicit NATS executable path.
    Path(PathBuf),
    /// Resolve `nats-server` from the caller's `PATH`.
    PathLookup,
    /// Ask the server to download and verify its pinned NATS release.
    DownloadPinned,
}

/// Startup, request, and shutdown budgets for a runtime.
#[derive(Clone, Copy, Debug)]
pub struct TestTimeouts {
    /// Overall deadline for `start`.
    pub startup: Duration,
    /// Deadline for one public install/registration operation.
    pub request: Duration,
    /// Overall deadline for `shutdown`.
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

/// Consuming builder for [`TrellisTestRuntime`].
#[derive(Clone)]
pub struct TrellisTestRuntimeBuilder {
    cli_binary: Option<PathBuf>,
    server_binary: Option<PathBuf>,
    nats: Option<NatsSource>,
    workdir_parent: Option<PathBuf>,
    retention: WorkdirRetention,
    timeouts: TestTimeouts,
    timeouts_explicit: bool,
}

impl Default for TrellisTestRuntimeBuilder {
    fn default() -> Self {
        Self {
            cli_binary: None,
            server_binary: None,
            nats: None,
            workdir_parent: None,
            retention: WorkdirRetention::OnFailure,
            timeouts: TestTimeouts::default(),
            timeouts_explicit: false,
        }
    }
}

impl TrellisTestRuntimeBuilder {
    /// Uses an explicit `trellis` CLI executable.
    #[must_use]
    pub fn cli_binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.cli_binary = Some(path.into());
        self
    }

    /// Uses an explicit `trellis-server` executable.
    #[must_use]
    pub fn server_binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.server_binary = Some(path.into());
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

    /// Starts an isolated runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when inputs are invalid, a prerequisite is missing, or
    /// startup fails before the runtime is authenticated.
    pub async fn start(self) -> Result<TrellisTestRuntime, TrellisTestError> {
        ensure_supported_platform()?;
        let cli = resolve_binary(self.cli_binary, "TRELLIS_TEST_CLI_BIN", "Trellis CLI")?;
        let server = resolve_binary(
            self.server_binary,
            "TRELLIS_TEST_SERVER_BIN",
            "Trellis server",
        )?;
        let nats = resolve_nats_source(self.nats);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let parent = self
            .workdir_parent
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let timeouts = if self.timeouts_explicit {
            self.timeouts
        } else if matches!(nats, NatsSource::DownloadPinned) {
            TestTimeouts {
                startup: Duration::from_secs(600),
                ..self.timeouts
            }
        } else {
            self.timeouts
        };

        // Validate immutable inputs once, before any startup attempt.
        let mut validation = Sandbox::create(&parent, WorkdirRetention::Never)?;
        let validated = validate_versions(&cli, &server, &validation, &path, timeouts).await;
        let _ = validation.cleanup();
        validated?;

        let password = generate_password();
        let deadline = Instant::now() + timeouts.startup;
        for _attempt in 0..MAX_STARTUP_ATTEMPTS {
            match Self::start_once(
                &cli,
                &server,
                &nats,
                &parent,
                &path,
                self.retention,
                timeouts,
                &password,
                deadline,
            )
            .await
            {
                Ok(runtime) => return Ok(runtime),
                Err((error, retryable)) => {
                    if retryable && Instant::now() < deadline {
                        continue;
                    }
                    return Err(error);
                }
            }
        }
        Err(TrellisTestError::new(
            TrellisTestErrorKind::PortConflict,
            TrellisTestStage::PortAllocation,
            "startup exhausted its port-conflict retries",
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_once(
        cli: &Path,
        server: &Path,
        nats: &NatsSource,
        parent: &Path,
        path: &OsString,
        retention: WorkdirRetention,
        timeouts: TestTimeouts,
        password: &str,
        deadline: Instant,
    ) -> Result<TrellisTestRuntime, (TrellisTestError, bool)> {
        let mut sandbox = Sandbox::create(parent, retention).map_err(|e| (e, false))?;
        let lease = PortLease::reserve().map_err(|e| (e, false))?;
        let ports = lease.ports().map_err(|e| (e, false))?;
        let public_origin = format!("http://127.0.0.1:{}", ports.http);
        let nats_url = format!("nats://127.0.0.1:{}", ports.nats);
        let websocket_url = format!("ws://127.0.0.1:{}", ports.websocket);

        let config_path = generate_bundle(cli, &sandbox, path, ports, &public_origin, timeouts)
            .map_err(|e| {
                sandbox.mark_failed();
                (e, false)
            })?;
        edit_config_toml(&config_path, ports.http).map_err(|e| {
            sandbox.mark_failed();
            (e, false)
        })?;

        let mut command = build_server_command(server, &config_path, ports, nats);
        lease.release_for_spawn();
        let supervisor = ProcessSupervisor::new();
        let (pid, stdout, stderr) = supervisor
            .spawn(&mut command, &sandbox, path, "Trellis server")
            .map_err(|e| {
                sandbox.mark_failed();
                (e, false)
            })?;

        // Wait for the first-administrator bootstrap URL from real server output.
        let token = match wait_for_bootstrap_url(&stdout, &supervisor, pid, deadline).await {
            Ok(token) => token,
            Err(error) => {
                let (error, retryable) =
                    classify_startup(&error, &stdout, &stderr, ports, &mut sandbox);
                return Err((error, retryable));
            }
        };
        if let Err(error) = bootstrap_first_admin(
            &public_origin,
            &token,
            ADMIN_USERNAME,
            password,
            request_ms(&timeouts),
        )
        .await
        {
            sandbox.mark_failed();
            return Err((error, false));
        }
        let admin = match AdminSession::connect(
            &public_origin,
            ADMIN_USERNAME,
            password,
            request_ms(&timeouts),
        )
        .await
        {
            Ok(admin) => admin,
            Err(error) => {
                sandbox.mark_failed();
                return Err((error, false));
            }
        };
        let workdir = sandbox.root().to_path_buf();
        let mut runtime = TrellisTestRuntime {
            workdir,
            trellis_url: public_origin,
            nats_url,
            websocket_url,
            admin: Some(admin),
            supervisor: Some(supervisor),
            sandbox: Some(sandbox),
            password: password.to_owned(),
            names: Vec::new(),
            participants: Vec::new(),
            revisions: Vec::new(),
            timeouts,
            stopped: false,
        };
        // Confirm a real authenticated boundary before returning.
        if let Err(error) = runtime.verify_admin().await {
            runtime.mark_failed();
            let _ = runtime.shutdown().await;
            return Err((error, false));
        }
        Ok(runtime)
    }
}

/// A running Trellis runtime owned by the calling test.
pub struct TrellisTestRuntime {
    workdir: PathBuf,
    trellis_url: String,
    nats_url: String,
    websocket_url: String,
    admin: Option<AdminSession>,
    supervisor: Option<ProcessSupervisor>,
    sandbox: Option<Sandbox>,
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

    /// URL of the runtime's managed NATS server.
    #[must_use]
    pub fn nats_url(&self) -> &str {
        &self.nats_url
    }

    /// WebSocket URL of the runtime's managed NATS server.
    #[must_use]
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    /// Sandbox work directory.
    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
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

    async fn verify_admin(&self) -> Result<(), TrellisTestError> {
        self.admin()?.verify().await
    }

    fn mark_failed(&mut self) {
        if let Some(sandbox) = self.sandbox.as_mut() {
            sandbox.mark_failed();
        }
    }

    /// Installs `P` into the runtime, idempotently for an identical digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the participant cannot be installed or conflicts
    /// with a previously installed digest.
    pub async fn install_participant<P: ParticipantDescriptor>(
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
        let revision = self.admin()?.install_participant::<P>().await?;
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
    ///
    /// Returns an error when `P` is not a service or provisioning fails.
    pub async fn register_service<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestServiceIdentity, TrellisTestError> {
        if P::KIND != trellis_rs::generated::ParticipantKind::Service {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::UnsupportedParticipantKind,
                TrellisTestStage::Validation,
                format!("{} is not a service participant", P::ID),
            ));
        }
        self.reserve_name(name)?;
        let admin = self.admin()?;
        admin.install_participant::<P>().await?;
        let deployment_id = admin.create_deployment(name).await?;
        admin.apply_participant::<P>(&deployment_id).await?;
        let (instance_id, seed) = admin.provision_service_instance(&deployment_id).await?;
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

    /// Registers `P` as an app/agent caller and completes a participant-bound login.
    ///
    /// # Errors
    ///
    /// Returns an error when `P` is not an app/agent or login fails.
    pub async fn register_client<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        use trellis_rs::generated::ParticipantKind;
        if !matches!(P::KIND, ParticipantKind::App | ParticipantKind::Agent) {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::UnsupportedParticipantKind,
                TrellisTestStage::Validation,
                format!("{} is not an app or agent participant", P::ID),
            ));
        }
        self.reserve_name(name)?;
        self.admin()?.install_participant::<P>().await?;
        let session = crate::admin::login_client(
            &self.trellis_url,
            P::ID,
            ADMIN_USERNAME,
            &self.password,
            request_ms(&self.timeouts),
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

    /// Stops the runtime idempotently and cleans the sandbox per policy.
    ///
    /// # Errors
    ///
    /// Returns an error when owned infrastructure cannot be stopped.
    pub async fn shutdown(&mut self) -> Result<(), TrellisTestError> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        self.admin = None;
        let mut failure = None;
        if let Some(supervisor) = self.supervisor.take() {
            let failures = supervisor.stop(self.timeouts.shutdown);
            if let Some(first) = failures.into_iter().next() {
                failure = Some(first);
            }
        }
        if let Some(mut sandbox) = self.sandbox.take() {
            if let Some(error) = sandbox.cleanup() {
                failure = Some(match failure {
                    Some(primary) => primary.with_cleanup(error),
                    None => error,
                });
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for TrellisTestRuntime {
    fn drop(&mut self) {
        // Best effort: the supervisor thread performs cleanup when it is dropped.
        self.admin = None;
        let _ = self.supervisor.take();
        let _ = self.sandbox.take();
    }
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

fn resolve_binary(
    explicit: Option<PathBuf>,
    env_var: &str,
    role: &str,
) -> Result<PathBuf, TrellisTestError> {
    let path = explicit.or_else(|| {
        std::env::var_os(env_var)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let Some(path) = path else {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::MissingBinary,
            TrellisTestStage::Validation,
            format!("no {role} executable: set the builder path or {env_var}"),
        ));
    };
    let canonical = std::fs::canonicalize(&path).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::InvalidBinary,
            TrellisTestStage::Validation,
            format!(
                "resolving the {role} executable {}: {error}",
                path.display()
            ),
        )
    })?;
    let metadata = std::fs::metadata(&canonical).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::InvalidBinary,
            TrellisTestStage::Validation,
            format!("reading the {role} executable: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::InvalidBinary,
            TrellisTestStage::Validation,
            format!("the {role} executable is not a regular file"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::InvalidBinary,
                TrellisTestStage::Validation,
                format!("the {role} executable is not executable"),
            ));
        }
    }
    Ok(canonical)
}

fn resolve_nats_source(explicit: Option<NatsSource>) -> NatsSource {
    if let Some(source) = explicit {
        return source;
    }
    match std::env::var_os("TRELLIS_TEST_NATS_BIN") {
        Some(value) if !value.is_empty() => NatsSource::Path(PathBuf::from(value)),
        _ => NatsSource::PathLookup,
    }
}

fn generate_bundle(
    cli: &Path,
    sandbox: &Sandbox,
    path: &OsString,
    ports: crate::sandbox::PortSet,
    public_origin: &str,
    timeouts: TestTimeouts,
) -> Result<PathBuf, TrellisTestError> {
    let out = sandbox.config_dir();
    let mut command = Command::new(cli);
    command
        .arg("--format")
        .arg("json")
        .arg("init")
        .arg("config")
        .arg("--out")
        .arg(&out)
        .arg("--trellis-port")
        .arg(ports.http.to_string())
        .arg("--nats-port")
        .arg(ports.nats.to_string())
        .arg("--nats-monitor-port")
        .arg(ports.monitor.to_string())
        .arg("--nats-ws-port")
        .arg(ports.websocket.to_string())
        .arg("--nats-server-url")
        .arg(format!("nats://127.0.0.1:{}", ports.nats))
        .arg("--nats-websocket-url")
        .arg(format!("ws://127.0.0.1:{}", ports.websocket))
        .arg("--public-origin")
        .arg(public_origin);
    let (ok, stdout, stderr) = run_captured(&mut command, sandbox, path, timeouts.request)
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::ConfigGeneration,
                format!("generating the bootstrap bundle: {error}"),
            )
        })?;
    if !ok {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::ConfigGeneration,
            format!("`trellis init config` failed: {}", stderr.trim()),
        ));
    }
    let parsed: Value = serde_json::from_str(&stdout).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::ConfigGeneration,
            format!("decoding `trellis init config` output: {error}"),
        )
    })?;
    let config = parsed
        .get("trellisConfig")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::ConfigGeneration,
                "`trellis init config` did not report a trellisConfig path",
            )
        })?;
    let config_path = PathBuf::from(config);
    if !config_path.starts_with(sandbox.root()) {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::ConfigGeneration,
            "the generated config path escaped the sandbox",
        ));
    }
    Ok(config_path)
}

fn edit_config_toml(config_path: &Path, http_port: u16) -> Result<(), TrellisTestError> {
    let text = std::fs::read_to_string(config_path).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::ConfigGeneration,
            format!("reading the generated config: {error}"),
        )
    })?;
    let mut document = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::ConfigGeneration,
            format!("parsing the generated config: {error}"),
        )
    })?;
    if !document.contains_key("http") {
        document["http"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let http = &mut document["http"];
    http["bind_address"] = toml_edit::value("127.0.0.1");
    http["port"] = toml_edit::value(i64::from(http_port));
    http["rate_limit_max"] = toml_edit::value(0i64);
    std::fs::write(config_path, document.to_string()).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::Bootstrap,
            TrellisTestStage::ConfigGeneration,
            format!("writing the sandbox config: {error}"),
        )
    })
}

fn build_server_command(
    server: &Path,
    config_path: &Path,
    ports: crate::sandbox::PortSet,
    nats: &NatsSource,
) -> Command {
    let mut command = Command::new(server);
    command
        .arg("--config")
        .arg(config_path)
        .arg("all")
        .arg(format!(
            "--local-nats-ports={},{},{}",
            ports.nats, ports.monitor, ports.websocket
        ));
    match nats {
        NatsSource::PathLookup => {
            command.arg("--local-nats");
        }
        NatsSource::Path(path) => {
            command.arg(format!("--local-nats={}", path.display()));
        }
        NatsSource::DownloadPinned => {
            command.arg("--nats-download");
        }
    }
    command
}

async fn validate_versions(
    cli: &Path,
    server: &Path,
    sandbox: &Sandbox,
    path: &OsString,
    timeouts: TestTimeouts,
) -> Result<(), TrellisTestError> {
    let deadline = Duration::from_secs(10).min(timeouts.startup);
    let mut cli_command = Command::new(cli);
    cli_command.arg("--format").arg("json").arg("version");
    let (ok, stdout, stderr) = run_captured(&mut cli_command, sandbox, path, deadline)?;
    if !ok {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::VersionMismatch,
            TrellisTestStage::VersionCheck,
            format!("`trellis version` failed: {}", stderr.trim()),
        ));
    }
    let parsed: Value = serde_json::from_str(&stdout).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::VersionMismatch,
            TrellisTestStage::VersionCheck,
            format!("decoding the CLI version: {error}"),
        )
    })?;
    let cli_version = parsed
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            TrellisTestError::new(
                TrellisTestErrorKind::VersionMismatch,
                TrellisTestStage::VersionCheck,
                "the CLI version output carried no version",
            )
        })?;

    let mut server_command = Command::new(server);
    server_command.arg("--version");
    let (ok, stdout, stderr) = run_captured(&mut server_command, sandbox, path, deadline)?;
    if !ok {
        return Err(TrellisTestError::new(
            TrellisTestErrorKind::VersionMismatch,
            TrellisTestStage::VersionCheck,
            format!("`trellis-server --version` failed: {}", stderr.trim()),
        ));
    }
    let server_version = stdout
        .split_whitespace()
        .last()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TrellisTestError::new(
                TrellisTestErrorKind::VersionMismatch,
                TrellisTestStage::VersionCheck,
                "the server version output was empty",
            )
        })?;

    let expected = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
        TrellisTestError::new(
            TrellisTestErrorKind::VersionMismatch,
            TrellisTestStage::VersionCheck,
            format!("parsing the testkit version: {error}"),
        )
    })?;
    for (role, actual) in [("CLI", cli_version), ("server", server_version)] {
        let actual = semver::Version::parse(actual).map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::VersionMismatch,
                TrellisTestStage::VersionCheck,
                format!("parsing the {role} version '{actual}': {error}"),
            )
        })?;
        if versions_differ(&expected, &actual) {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::VersionMismatch,
                TrellisTestStage::VersionCheck,
                format!(
                    "the {role} version {actual} does not match the testkit version {expected}"
                ),
            ));
        }
    }
    Ok(())
}

/// Compares versions ignoring only SemVer build metadata.
fn versions_differ(left: &semver::Version, right: &semver::Version) -> bool {
    left.major != right.major
        || left.minor != right.minor
        || left.patch != right.patch
        || left.pre != right.pre
}

async fn wait_for_bootstrap_url(
    stdout: &OutputTail,
    supervisor: &ProcessSupervisor,
    pid: u32,
    deadline: Instant,
) -> Result<String, TrellisTestError> {
    loop {
        if let Some(url) = parse_bootstrap_url(&stdout.text()) {
            if let Some(token) = admin_token_from_url(&url) {
                return Ok(token);
            }
        }
        if let Some(code) = supervisor.exit_code(pid) {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::ProcessExited,
                TrellisTestStage::ServerStart,
                format!("the Trellis server exited before readiness (code {code})"),
            ));
        }
        if Instant::now() >= deadline {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Timeout,
                TrellisTestStage::ServerStart,
                "timed out waiting for the administrator bootstrap URL",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn parse_bootstrap_url(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        if let Some(url) = json_bootstrap_url(line) {
            return Some(url);
        }
        if let Some(url) = fallback_bootstrap_url(line) {
            return Some(url);
        }
    }
    None
}

fn json_bootstrap_url(line: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    let root = parsed
        .get("adminAccountUrl")
        .or_else(|| parsed.get("bootstrapUrl"));
    let fields = parsed.get("fields").and_then(|fields| {
        fields
            .get("adminAccountUrl")
            .or_else(|| fields.get("bootstrapUrl"))
    });
    root.or(fields)
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
}

fn fallback_bootstrap_url(line: &str) -> Option<String> {
    line.split_once("TRELLIS_ADMIN_BOOTSTRAP_URL=")
        .map(|(_, rest)| {
            rest.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .filter(|url| !url.is_empty())
}

fn admin_token_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed
        .query_pairs()
        .find(|(key, _)| key == "adminAccountToken")
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
}

fn classify_startup(
    error: &TrellisTestError,
    stdout: &OutputTail,
    stderr: &OutputTail,
    ports: crate::sandbox::PortSet,
    sandbox: &mut Sandbox,
) -> (TrellisTestError, bool) {
    sandbox.mark_failed();
    let diagnostics = format!("{}\n{}", stdout.text(), stderr.text());
    if crate::sandbox::is_port_conflict(&diagnostics, &ports) {
        let conflict = TrellisTestError::new(
            TrellisTestErrorKind::PortConflict,
            TrellisTestStage::PortAllocation,
            "a selected loopback port was already bound",
        )
        .with_output(stdout.text(), stderr.text());
        return (conflict, true);
    }
    let with_output = TrellisTestError::new(error.kind(), error.stage(), error.message())
        .with_output(stdout.text(), stderr.text());
    (with_output, false)
}
