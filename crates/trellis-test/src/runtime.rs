//! Builder, out-of-process startup state machine, and runtime ownership.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use trellis_rs::generated::ParticipantDescriptor;

use crate::admin::{bootstrap_first_admin, AdminSession, ADMIN_USERNAME};
use crate::error::{TrellisTestError, TrellisTestErrorKind, TrellisTestStage};
use crate::identity::{InstalledParticipant, TestClientIdentity, TestServiceIdentity};
use crate::process::{run_captured, LineObserver, OutputTail, ProcessSupervisor};
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

/// A NATS source resolved once to a concrete selection.
#[derive(Clone, Debug)]
enum NatsExecutable {
    /// A concrete executable passed with `--local-nats=<path>`.
    Path(PathBuf),
    /// The server's pinned download path, selected with `--nats-download`.
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
        let timeouts = if self.timeouts_explicit {
            self.timeouts
        } else if matches!(&self.nats, Some(NatsSource::DownloadPinned)) {
            TestTimeouts {
                startup: Duration::from_secs(600),
                ..self.timeouts
            }
        } else {
            self.timeouts
        };
        validate_timeouts(&timeouts)?;

        let cli = resolve_binary(self.cli_binary, "TRELLIS_TEST_CLI_BIN", "Trellis CLI")?;
        let server = resolve_binary(
            self.server_binary,
            "TRELLIS_TEST_SERVER_BIN",
            "Trellis server",
        )?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let nats = resolve_nats(self.nats, &path)?;
        let parent = self
            .workdir_parent
            .clone()
            .unwrap_or_else(std::env::temp_dir);

        // One absolute startup deadline governs version checks and every attempt.
        let deadline = Instant::now() + timeouts.startup;
        let mut state =
            StartupState::new(ProcessSupervisor::new(timeouts.shutdown), timeouts.shutdown);

        // Validate immutable inputs once, before any startup attempt. The
        // validation sandbox lives under the same cancellation owner as the
        // attempt sandboxes, so an aborted start still removes it.
        state.set_sandbox(Sandbox::create(&parent, WorkdirRetention::Never)?);
        let validated = validate_versions(
            state.supervisor(),
            &cli,
            &server,
            state.sandbox(),
            &path,
            deadline,
        )
        .await;
        let validation_cleanup = state.sandbox_mut().cleanup();
        validated?;
        if let Some(error) = validation_cleanup {
            return Err(error);
        }

        let password = generate_password();
        for attempt in 1..=MAX_STARTUP_ATTEMPTS {
            let sandbox = Sandbox::create(&parent, self.retention)?;
            state.set_sandbox(sandbox);
            match start_once(
                &mut state, &cli, &server, &nats, &path, timeouts, &password, deadline,
            )
            .await
            {
                Ok(runtime) => return Ok(runtime),
                Err((error, retryable)) => {
                    let error = state.clean_failed_attempt(error, deadline).await;
                    if retryable && attempt < MAX_STARTUP_ATTEMPTS && Instant::now() < deadline {
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
}

/// Everything still owned while `start` is in flight.
///
/// If the startup future is cancelled, dropping this guard signals the
/// supervisor to stop and performs the retention decision on its own thread
/// without blocking the caller.
struct StartupState {
    supervisor: Option<ProcessSupervisor>,
    sandbox: Option<Sandbox>,
    shutdown_timeout: Duration,
    armed: bool,
}

impl StartupState {
    fn new(supervisor: ProcessSupervisor, shutdown_timeout: Duration) -> Self {
        Self {
            supervisor: Some(supervisor),
            sandbox: None,
            shutdown_timeout,
            armed: true,
        }
    }

    fn supervisor(&self) -> &ProcessSupervisor {
        self.supervisor.as_ref().expect("startup supervisor")
    }

    fn sandbox(&self) -> &Sandbox {
        self.sandbox.as_ref().expect("startup sandbox")
    }

    fn sandbox_mut(&mut self) -> &mut Sandbox {
        self.sandbox.as_mut().expect("startup sandbox")
    }

    fn set_sandbox(&mut self, sandbox: Sandbox) {
        self.sandbox = Some(sandbox);
    }

    fn disarm(&mut self) -> (ProcessSupervisor, Sandbox) {
        self.armed = false;
        (
            self.supervisor.take().expect("startup supervisor"),
            self.sandbox.take().expect("startup sandbox"),
        )
    }

    /// Stops and retires a failed attempt's processes and sandbox.
    async fn clean_failed_attempt(
        &mut self,
        mut error: TrellisTestError,
        deadline: Instant,
    ) -> TrellisTestError {
        self.sandbox_mut().mark_failed();
        let (mut cleanup, timed_out) = self.supervisor().stop(deadline).await;
        if timed_out {
            // The supervisor is still finishing process cleanup. Hand it and the
            // sandbox to detached cleanup and install a fresh supervisor so a
            // retry is not blocked by the timed-out one.
            let supervisor = self.supervisor.take().expect("startup supervisor");
            let sandbox = self.sandbox.take().expect("startup sandbox");
            spawn_detached_cleanup(supervisor, sandbox);
            self.supervisor = Some(ProcessSupervisor::new(self.shutdown_timeout));
        } else if let Some(cleanup_error) = self.sandbox_mut().cleanup() {
            cleanup.push(cleanup_error);
        }
        if let Some(first) = cleanup.into_iter().next() {
            error = error.with_cleanup(first);
        }
        error
    }
}

impl Drop for StartupState {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let sandbox = self.sandbox.take();
        let supervisor = self.supervisor.take();
        match (sandbox, supervisor) {
            (Some(mut sandbox), Some(supervisor)) => {
                sandbox.mark_failed();
                spawn_detached_cleanup(supervisor, sandbox);
            }
            (Some(mut sandbox), None) => {
                sandbox.mark_failed();
                let _ = sandbox.cleanup();
            }
            (None, Some(supervisor)) => supervisor.request_stop(),
            (None, None) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_once(
    state: &mut StartupState,
    cli: &Path,
    server: &Path,
    nats: &NatsExecutable,
    path: &OsString,
    timeouts: TestTimeouts,
    password: &str,
    deadline: Instant,
) -> Result<TrellisTestRuntime, (TrellisTestError, bool)> {
    let lease = PortLease::reserve().map_err(|e| (e, false))?;
    let ports = lease.ports().map_err(|e| (e, false))?;
    let public_origin = format!("http://127.0.0.1:{}", ports.http);
    let nats_url = format!("nats://127.0.0.1:{}", ports.nats);
    let websocket_url = format!("ws://127.0.0.1:{}", ports.websocket);
    let monitor_url = format!("http://127.0.0.1:{}", ports.monitor);

    let config_path = generate_bundle(state, cli, path, ports, &public_origin, timeouts, deadline)
        .await
        .map_err(|e| {
            state.sandbox_mut().mark_failed();
            (e, false)
        })?;
    edit_config_toml(&config_path, ports.http).map_err(|e| {
        state.sandbox_mut().mark_failed();
        (e, false)
    })?;

    let mut command = build_server_command(server, &config_path, ports, nats);
    lease.release_for_spawn();

    // Capture the bootstrap URL from complete lines on either stream, before the
    // rolling tail can discard it.
    let captured_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let observer = bootstrap_observer(Arc::clone(&captured_token), &public_origin);

    let spawned = state
        .supervisor()
        .spawn(
            &mut command,
            state.sandbox(),
            path,
            "Trellis server",
            Some(observer),
        )
        .map_err(|e| {
            state.sandbox_mut().mark_failed();
            (e, false)
        })?;
    let pid = spawned.pid;
    // Redact the administrator password from the diagnostic tail as it is
    // captured, before the bounded tail can truncate it.
    spawned.stdout.add_secret(password.as_bytes());
    spawned.stderr.add_secret(password.as_bytes());

    let token = match wait_for_bootstrap(&captured_token, state.supervisor(), pid, deadline).await {
        Ok(token) => token,
        Err(error) => {
            let (error, retryable) = classify_startup(
                &error,
                &spawned.stdout,
                &spawned.stderr,
                ports,
                state.sandbox_mut(),
                &[password],
            );
            return Err((error, retryable));
        }
    };
    // The bootstrap token is discovered from the captured output; register it so
    // already-captured bytes and every later occurrence are redacted too.
    spawned.stdout.add_secret(token.as_bytes());
    spawned.stderr.add_secret(token.as_bytes());
    if let Err(error) = wait_for_readyz(&public_origin, state.supervisor(), pid, deadline).await {
        let (error, retryable) = classify_startup(
            &error,
            &spawned.stdout,
            &spawned.stderr,
            ports,
            state.sandbox_mut(),
            &[password, &token],
        );
        return Err((error, retryable));
    }

    let request_deadline = deadline.min(Instant::now() + timeouts.request);
    if let Err(error) = bootstrap_first_admin(
        &public_origin,
        &token,
        ADMIN_USERNAME,
        password,
        remaining_ms(request_deadline),
    )
    .await
    {
        state.sandbox_mut().mark_failed();
        return Err((error, false));
    }
    // The remaining startup deadline bounds the authenticated session too, so
    // `start` has one real upper bound until it returns a usable runtime.
    let admin = match tokio::time::timeout_at(
        deadline.into(),
        AdminSession::connect(&public_origin, ADMIN_USERNAME, password),
    )
    .await
    {
        Ok(Ok(admin)) => admin,
        Ok(Err(error)) => {
            state.sandbox_mut().mark_failed();
            return Err((error, false));
        }
        Err(_) => {
            state.sandbox_mut().mark_failed();
            return Err((
                request_timeout(
                    "connecting the administrator session",
                    TrellisTestStage::AdministratorLogin,
                ),
                false,
            ));
        }
    };
    let workdir = state.sandbox().root().to_path_buf();
    let (supervisor, sandbox) = state.disarm();
    let mut runtime = TrellisTestRuntime {
        workdir,
        trellis_url: public_origin,
        nats_url,
        websocket_url,
        monitor_url,
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
    // Confirm a real authenticated boundary before returning, within the same
    // startup deadline.
    match tokio::time::timeout_at(deadline.into(), runtime.verify_admin()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            runtime.mark_failed();
            let _ = runtime.shutdown().await;
            return Err((error, false));
        }
        Err(_) => {
            runtime.mark_failed();
            let _ = runtime.shutdown().await;
            return Err((
                request_timeout(
                    "verifying the administrator session",
                    TrellisTestStage::AdministratorLogin,
                ),
                false,
            ));
        }
    }
    Ok(runtime)
}

/// A running Trellis runtime owned by the calling test.
pub struct TrellisTestRuntime {
    workdir: PathBuf,
    trellis_url: String,
    nats_url: String,
    websocket_url: String,
    monitor_url: String,
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

    /// HTTP monitoring URL of the runtime's managed NATS server.
    #[must_use]
    pub fn monitor_url(&self) -> &str {
        &self.monitor_url
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

    /// Confirms the runtime is still running before any public mutation.
    fn ensure_running(&self) -> Result<(), TrellisTestError> {
        if self.stopped {
            return Err(self.stopped_error());
        }
        self.admin().map(|_| ())
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
        self.ensure_running()?;
        let deadline = Instant::now() + self.timeouts.request;
        match tokio::time::timeout_at(deadline.into(), self.install_participant_inner::<P>()).await
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
    ///
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
        let deadline = Instant::now() + self.timeouts.request;
        match tokio::time::timeout_at(deadline.into(), self.register_service_inner::<P>(name)).await
        {
            Ok(result) => {
                if result.is_err() {
                    self.mark_failed();
                }
                result
            }
            Err(_) => {
                self.mark_failed();
                Err(request_timeout(
                    "registering a service",
                    TrellisTestStage::ServiceProvisioning,
                ))
            }
        }
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
        self.ensure_running()?;
        if !matches!(P::KIND, ParticipantKind::App | ParticipantKind::Agent) {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::UnsupportedParticipantKind,
                TrellisTestStage::Validation,
                format!("{} is not an app or agent participant", P::ID),
            ));
        }
        self.reserve_name(name)?;
        let deadline = Instant::now() + self.timeouts.request;
        match tokio::time::timeout_at(deadline.into(), self.register_client_inner::<P>(name)).await
        {
            Ok(result) => {
                if result.is_err() {
                    self.mark_failed();
                }
                result
            }
            Err(_) => {
                self.mark_failed();
                Err(request_timeout(
                    "registering a client",
                    TrellisTestStage::ClientLogin,
                ))
            }
        }
    }

    async fn register_client_inner<P: ParticipantDescriptor>(
        &mut self,
        name: &str,
    ) -> Result<TestClientIdentity, TrellisTestError> {
        self.install_participant_inner::<P>().await?;
        let admin = self.admin()?;
        let session = crate::admin::login_client(
            &self.trellis_url,
            P::ID,
            ADMIN_USERNAME,
            &self.password,
            admin,
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
        let deadline = Instant::now() + self.timeouts.shutdown;
        let mut failure: Option<TrellisTestError> = None;
        if let Some(supervisor) = self.supervisor.take() {
            let (failures, timed_out) = supervisor.stop(deadline).await;
            for cleanup in failures {
                failure = Some(merge_failure(failure, cleanup));
            }
            if timed_out {
                // Processes may still be finishing. Hand the supervisor and the
                // sandbox to detached cleanup so retention is applied only after
                // the owned processes are actually gone, and return the timeout
                // promptly.
                match self.sandbox.take() {
                    Some(mut sandbox) => {
                        sandbox.mark_failed();
                        spawn_detached_cleanup(supervisor, sandbox);
                    }
                    None => supervisor.request_stop(),
                }
                return Err(failure.unwrap_or_else(|| {
                    TrellisTestError::new(
                        TrellisTestErrorKind::Timeout,
                        TrellisTestStage::Shutdown,
                        "the process supervisor did not finish cleanup within the shutdown deadline",
                    )
                }));
            }
        }
        if let Some(mut sandbox) = self.sandbox.take() {
            if let Some(cleanup) = sandbox.cleanup() {
                failure = Some(merge_failure(failure, cleanup));
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
        self.admin = None;
        if std::thread::panicking() {
            self.mark_failed();
        }
        match (self.supervisor.take(), self.sandbox.take()) {
            (Some(supervisor), Some(sandbox)) => spawn_detached_cleanup(supervisor, sandbox),
            (Some(supervisor), None) => supervisor.request_stop(),
            (None, Some(mut sandbox)) => {
                let _ = sandbox.cleanup();
            }
            (None, None) => {}
        }
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
        format!("{operation} exceeded the request deadline"),
    )
}

/// Hands process and sandbox cleanup to a dedicated thread so `Drop` and
/// cancellation never block the caller's executor.
fn spawn_detached_cleanup(supervisor: ProcessSupervisor, mut sandbox: Sandbox) {
    let _ = std::thread::Builder::new()
        .name("trellis-test-cleanup".to_owned())
        .spawn(move || {
            supervisor.join();
            let _ = sandbox.cleanup();
        });
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

fn request_ms(timeouts: &TestTimeouts) -> u64 {
    u64::try_from(timeouts.request.as_millis()).unwrap_or(u64::MAX)
}

fn remaining_ms(deadline: Instant) -> u64 {
    u64::try_from(
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
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
    canonical_executable(&path, role)
}

/// Canonicalizes and validates an executable path without lossy conversion.
fn canonical_executable(path: &Path, role: &str) -> Result<PathBuf, TrellisTestError> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
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

/// Resolves the NATS selection once, before any attempt, using the snapshotted PATH.
fn resolve_nats(
    explicit: Option<NatsSource>,
    path: &OsString,
) -> Result<NatsExecutable, TrellisTestError> {
    let source = explicit.unwrap_or_else(|| match std::env::var_os("TRELLIS_TEST_NATS_BIN") {
        Some(value) if !value.is_empty() => NatsSource::Path(PathBuf::from(value)),
        _ => NatsSource::PathLookup,
    });
    match source {
        NatsSource::DownloadPinned => Ok(NatsExecutable::DownloadPinned),
        NatsSource::Path(path) => Ok(NatsExecutable::Path(canonical_executable(&path, "NATS")?)),
        NatsSource::PathLookup => {
            let resolved = find_on_path("nats-server", path).ok_or_else(|| {
                TrellisTestError::new(
                    TrellisTestErrorKind::MissingBinary,
                    TrellisTestStage::Validation,
                    "no nats-server executable on PATH: set the builder nats source or TRELLIS_TEST_NATS_BIN",
                )
            })?;
            Ok(NatsExecutable::Path(resolved))
        }
    }
}

/// Finds a plain executable filename on a snapshotted `PATH`.
fn find_on_path(program: &str, path: &OsString) -> Option<PathBuf> {
    for directory in std::env::split_paths(path) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(program);
        if std::fs::metadata(&candidate)
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            if let Ok(canonical) = canonical_executable(&candidate, program) {
                return Some(canonical);
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn generate_bundle(
    state: &StartupState,
    cli: &Path,
    path: &OsString,
    ports: crate::sandbox::PortSet,
    public_origin: &str,
    timeouts: TestTimeouts,
    deadline: Instant,
) -> Result<PathBuf, TrellisTestError> {
    // Reject non-UTF-8 sandbox paths before generating any configuration.
    crate::sandbox::require_utf8_path(state.sandbox().root(), "sandbox")?;
    let out = state.sandbox().config_dir();
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
    let command_deadline = deadline.min(Instant::now() + timeouts.request);
    let (ok, stdout, stderr) = run_captured(
        state.supervisor(),
        &mut command,
        state.sandbox(),
        path,
        command_deadline,
    )
    .await
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
    if !config_path.starts_with(state.sandbox().root()) {
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
    nats: &NatsExecutable,
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
        NatsExecutable::DownloadPinned => {
            command.arg("--nats-download");
        }
        NatsExecutable::Path(path) => {
            // Build the argument as native OS text so non-UTF-8 paths survive.
            let mut argument = OsString::from("--local-nats=");
            argument.push(path.as_os_str());
            command.arg(argument);
        }
    }
    command
}

async fn validate_versions(
    supervisor: &ProcessSupervisor,
    cli: &Path,
    server: &Path,
    sandbox: &Sandbox,
    path: &OsString,
    startup_deadline: Instant,
) -> Result<(), TrellisTestError> {
    let cli_deadline = startup_deadline.min(Instant::now() + Duration::from_secs(10));
    let mut cli_command = Command::new(cli);
    cli_command.arg("--format").arg("json").arg("version");
    let (ok, stdout, stderr) =
        run_captured(supervisor, &mut cli_command, sandbox, path, cli_deadline).await?;
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

    let server_deadline = startup_deadline.min(Instant::now() + Duration::from_secs(10));
    let mut server_command = Command::new(server);
    server_command.arg("--version");
    let (ok, stdout, stderr) = run_captured(
        supervisor,
        &mut server_command,
        sandbox,
        path,
        server_deadline,
    )
    .await?;
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

/// Builds a line observer that durably captures the first valid bootstrap token.
fn bootstrap_observer(capture: Arc<Mutex<Option<String>>>, origin: &str) -> LineObserver {
    let origin = origin.to_owned();
    Arc::new(move |line: &str| {
        let Some(url) = extract_bootstrap_url(line) else {
            return;
        };
        let Some(token) = validated_bootstrap_token(&url, &origin) else {
            return;
        };
        if let Ok(mut slot) = capture.lock() {
            if slot.is_none() {
                *slot = Some(token);
            }
        }
    })
}

async fn wait_for_bootstrap(
    capture: &Arc<Mutex<Option<String>>>,
    supervisor: &ProcessSupervisor,
    pid: u32,
    deadline: Instant,
) -> Result<String, TrellisTestError> {
    loop {
        if let Some(token) = capture.lock().ok().and_then(|slot| slot.clone()) {
            return Ok(token);
        }
        if let Some(code) = supervisor.exit_code(pid).await {
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

fn extract_bootstrap_url(line: &str) -> Option<String> {
    json_bootstrap_url(line).or_else(|| fallback_bootstrap_url(line))
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

/// Validates that a bootstrap URL has the exact owned origin and a nonempty token.
fn validated_bootstrap_token(url: &str, origin: &str) -> Option<String> {
    let candidate = url::Url::parse(url).ok()?;
    let owned = url::Url::parse(origin).ok()?;
    if candidate.scheme() != owned.scheme()
        || candidate.host_str() != owned.host_str()
        || candidate.port_or_known_default() != owned.port_or_known_default()
    {
        return None;
    }
    candidate
        .query_pairs()
        .find(|(key, _)| key == "adminAccountToken")
        .map(|(_, value)| value.into_owned())
        .filter(|value| !value.is_empty())
}

async fn wait_for_readyz(
    trellis_url: &str,
    supervisor: &ProcessSupervisor,
    pid: u32,
    deadline: Instant,
) -> Result<(), TrellisTestError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            TrellisTestError::new(
                TrellisTestErrorKind::Bootstrap,
                TrellisTestStage::ServerStart,
                format!("building the readiness HTTP client: {error}"),
            )
        })?;
    let url = format!("{}/readyz", trellis_url.trim_end_matches('/'));
    loop {
        if supervisor.exit_code(pid).await.is_some() {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::ProcessExited,
                TrellisTestStage::ServerStart,
                "the Trellis server exited before it became ready",
            ));
        }
        if let Ok(response) = client.get(&url).send().await {
            if response.status().is_success() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(TrellisTestError::new(
                TrellisTestErrorKind::Timeout,
                TrellisTestStage::ServerStart,
                "timed out waiting for the readiness endpoint",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn classify_startup(
    error: &TrellisTestError,
    stdout: &OutputTail,
    stderr: &OutputTail,
    ports: crate::sandbox::PortSet,
    sandbox: &mut Sandbox,
    secrets: &[&str],
) -> (TrellisTestError, bool) {
    sandbox.mark_failed();
    let stdout_tail = crate::error::redact_secrets(&stdout.text(), secrets);
    let stderr_tail = crate::error::redact_secrets(&stderr.text(), secrets);
    let diagnostics = format!("{stdout_tail}\n{stderr_tail}");
    // Surface the failed runtime's output so a startup failure is diagnosable from
    // the test log; the output is already redacted.
    eprintln!("trellis-test: server startup output:\n{diagnostics}");
    if crate::sandbox::is_port_conflict(&diagnostics, &ports) {
        let conflict = TrellisTestError::new(
            TrellisTestErrorKind::PortConflict,
            TrellisTestStage::PortAllocation,
            "a selected loopback port was already bound",
        )
        .with_workdir(sandbox.root())
        .with_output(stdout_tail, stderr_tail);
        return (conflict, true);
    }
    let with_output = TrellisTestError::new(error.kind(), error.stage(), error.message())
        .with_workdir(sandbox.root())
        .with_output(stdout_tail, stderr_tail);
    (with_output, false)
}

#[cfg(test)]
mod tests {
    use super::{extract_bootstrap_url, validated_bootstrap_token, versions_differ};
    use semver::Version;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("parse version")
    }

    #[test]
    fn version_comparison_ignores_build_metadata_only() {
        let base = version("0.100.0");
        assert!(!versions_differ(&base, &version("0.100.0+build.5")));
        assert!(versions_differ(&base, &version("0.100.1")));
        assert!(versions_differ(&base, &version("0.101.0")));
        assert!(versions_differ(&base, &version("0.100.0-rc.1")));
        assert!(versions_differ(
            &version("0.100.0-rc.1"),
            &version("0.100.0-rc.2")
        ));
    }

    #[test]
    fn bootstrap_url_is_extracted_from_json_and_fallback_lines() {
        assert_eq!(
            extract_bootstrap_url(
                r#"{"adminAccountUrl":"http://127.0.0.1:1/x?adminAccountToken=t"}"#
            )
            .as_deref(),
            Some("http://127.0.0.1:1/x?adminAccountToken=t")
        );
        assert_eq!(
            extract_bootstrap_url(
                "prefix TRELLIS_ADMIN_BOOTSTRAP_URL=http://127.0.0.1:1/x?adminAccountToken=t trailing"
            )
            .as_deref(),
            Some("http://127.0.0.1:1/x?adminAccountToken=t")
        );
        assert_eq!(extract_bootstrap_url("no url here"), None);
    }

    #[test]
    fn bootstrap_token_requires_the_exact_owned_origin() {
        let origin = "http://127.0.0.1:53001";
        let good = "http://127.0.0.1:53001/console?adminAccountToken=secret-token";
        assert_eq!(
            validated_bootstrap_token(good, origin).as_deref(),
            Some("secret-token")
        );
        // A different port, host, or scheme is not the owned origin.
        assert!(
            validated_bootstrap_token("http://127.0.0.1:60000/x?adminAccountToken=t", origin)
                .is_none()
        );
        assert!(
            validated_bootstrap_token("http://evil.test:53001/x?adminAccountToken=t", origin)
                .is_none()
        );
        // A missing token is rejected.
        assert!(validated_bootstrap_token("http://127.0.0.1:53001/x", origin).is_none());
    }
}
