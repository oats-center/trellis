//! Live coverage for the `trellis-server` managed and configured NATS modes.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use async_nats::jetstream::{self, stream};
use serde_json::Value;
use trellis_local_nats::{
    LocalNats, LocalNatsPorts, NatsBinarySource, NatsOutput, NatsServerBinary,
};
use ulid::Ulid;

/// Configures a spawned child to receive `SIGTERM` when this process exits.
///
/// Linux-only; the CLI/server test owns its children directly and needs no
/// external test crate for this tiny guard.
#[cfg(target_os = "linux")]
fn terminate_on_parent_exit(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    let parent_pid = std::process::id();
    // SAFETY: only async-signal-safe libc calls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid as libc::pid_t {
                return Err(std::io::Error::other("parent exited before exec"));
            }
            Ok(())
        });
    }
}

/// No parent-death signal is available off Linux.
#[cfg(not(target_os = "linux"))]
fn terminate_on_parent_exit(_command: &mut Command) {}

/// Deliberately bogus NATS URLs baked into the bundle so managed mode's endpoint
/// override is observable: nothing listens on these, yet the report must be valid.
const BOGUS_NATS_URL: &str = "nats://nats.invalid:4222";
const BOGUS_WS_URL: &str = "ws://browser.invalid:8080";
/// The first managed run downloads the pinned nats-server binary into an empty
/// cache, so startup allows 600s for that initial acquisition.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(600);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("trellis-rs crate should live under crates/trellis")
}

fn cli_command() -> Command {
    let mut command = if let Some(binary) = std::env::var_os("TRELLIS_TEST_CLI_BIN") {
        Command::new(binary)
    } else {
        let mut command = Command::new("cargo");
        command.args([
            "run",
            "--quiet",
            "--manifest-path",
            repo_root()
                .join("Cargo.toml")
                .to_str()
                .expect("UTF-8 Cargo path"),
            "-p",
            "trellis-cli",
            "--bin",
            "trellis",
            "--",
        ]);
        command
    };
    command.current_dir(repo_root()).stdin(Stdio::null());
    command
}

fn server_command(workdir: &Path) -> Command {
    let mut command = if let Some(binary) = std::env::var_os("TRELLIS_TEST_SERVER_BIN") {
        Command::new(binary)
    } else {
        let mut command = Command::new("cargo");
        command.args([
            "run",
            "--quiet",
            "--manifest-path",
            repo_root()
                .join("Cargo.toml")
                .to_str()
                .expect("UTF-8 Cargo path"),
            "-p",
            "trellis-server",
            "--bin",
            "trellis-server",
            "--",
        ]);
        command
    };
    command
        .current_dir(repo_root())
        .stdin(Stdio::null())
        .env("HOME", workdir.join("home"))
        .env("XDG_CONFIG_HOME", workdir.join("config"))
        .env("XDG_DATA_HOME", workdir.join("data"))
        .env("XDG_STATE_HOME", workdir.join("state"))
        .env("XDG_CACHE_HOME", workdir.join("cache"))
        .env("XDG_RUNTIME_DIR", workdir.join("runtime"));
    command
}

/// Removes the per-case workdir (bundle, logs, and the managed-NATS binary cache).
struct WorkdirGuard(PathBuf);

impl WorkdirGuard {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("trellis-cli-live-{}", Ulid::new()));
        fs::create_dir_all(&path).expect("create CLI test workdir");
        Self(path)
    }
}

impl Drop for WorkdirGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // Keep the evidence for post-mortem inspection on failure.
            eprintln!(
                "CLI live case workdir kept for inspection: {}",
                self.0.display()
            );
            return;
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Owns a spawned `trellis-server` child and, in the external-override phase, the
/// test-owned managed nats-server. Drop always reaps the CLI child (owned `Child`,
/// `try_wait`-based) and any nats-server the CLI left behind (pid-file identity,
/// no `/proc` dependency), then the test-owned nats guard stops itself.
struct ChildGuard {
    child: Option<Child>,
    nats: Option<LocalNats>,
    label: &'static str,
}

impl ChildGuard {
    fn spawn(command: &mut Command, label: &'static str) -> Self {
        terminate_on_parent_exit(command);
        let child = command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn {label}: {error}"));
        Self {
            child: Some(child),
            nats: None,
            label,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child is still owned")
    }

    /// Keeps the test-owned nats guard in this struct: it stays reapable in `Drop`
    /// even after `wait_for_exit` reaped the CLI child.
    fn set_nats(&mut self, server: LocalNats) {
        self.nats = Some(server);
    }

    fn signal_term(&mut self) {
        let pid = self.child_mut().id();
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .expect("run kill -TERM");
        assert!(status.success(), "kill -TERM failed for pid {pid}");
    }

    /// Waits up to `timeout` for the child to exit; does not clear the owned child,
    /// so `Drop` can still reap or force-kill it afterwards.
    fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.child_mut().try_wait().expect("poll child exit") {
                return Some(status);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        None
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Never signal a possibly-reaped child: its pid may have been recycled.
            // Signal only when `try_wait` says the process is still running.
            let running = child.try_wait().ok().flatten().is_none();
            if running {
                let _ = Command::new("kill")
                    .args(["-TERM", &child.id().to_string()])
                    .status();
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut exited = false;
                while Instant::now() < deadline {
                    if let Ok(Some(_)) = child.try_wait() {
                        exited = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if !exited {
                    let _ = child.kill();
                    let _ = child.wait();
                    eprintln!("{} did not exit after SIGTERM; forced SIGKILL", self.label);
                }
            }
        }
        // The test-owned nats guard stops itself on drop (owned Child, try_wait-based,
        // ownership-safe pid removal); it is kept in this struct so `wait_for_exit`
        // (CLI child only) never clears it.
        self.nats = None;
    }
}

fn port_accepts(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// Liveness probe used only for post-shutdown assertions (never for cleanup signals).
fn pid_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn log_tail(path: &Path) -> String {
    fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .rev()
                .take(20)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|_| "<unreadable>".to_string())
}

/// Polls `condition` until true, failing fast when the CLI child exits first.
async fn wait_until<F>(mut condition: F, child: &mut ChildGuard, stderr_log: &Path, what: &str)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        if child
            .child_mut()
            .try_wait()
            .expect("poll child exit")
            .is_some()
        {
            panic!(
                "trellis-server exited before {what}\nstderr tail:\n{}",
                log_tail(stderr_log)
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "timed out waiting for {what}\nstderr tail:\n{}",
        log_tail(stderr_log)
    );
}

#[tokio::test]
async fn cli_server_managed_nats() {
    let workdir = WorkdirGuard::new();
    let bundle = workdir.0.join("bundle");
    let effective_root = workdir.0.join("effective");
    // Empty private XDG roots: explicit download acquires pinned NATS on first run.
    let cache_dir = effective_root.join("cache/nats");
    fs::create_dir_all(&cache_dir).expect("create managed-NATS cache dir");
    fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700))
        .expect("make managed-NATS cache dir private");
    let reservations = [(); 4].map(|_| TcpListener::bind(("127.0.0.1", 0)).expect("reserve port"));
    let [runtime_port, nats_port, monitor_port, websocket_port] = reservations
        .each_ref()
        .map(|socket| socket.local_addr().expect("port address").port());
    let managed_ports = [nats_port, monitor_port, websocket_port];
    drop(reservations);
    let ports_arg = format!("{nats_port},{monitor_port},{websocket_port}");

    // 1. `trellis init config` renders the bundle the managed server expects, with
    //    deliberately bogus NATS URLs so the managed endpoint override is observable.
    let init_output = cli_command()
        .args([
            "--format",
            "json",
            "init",
            "config",
            "--out",
            bundle.to_str().expect("UTF-8 bundle path"),
            "--trellis-port",
            &runtime_port.to_string(),
            "--nats-port",
            &nats_port.to_string(),
            "--nats-monitor-port",
            &monitor_port.to_string(),
            "--nats-ws-port",
            &websocket_port.to_string(),
            "--nats-server-url",
            BOGUS_NATS_URL,
            "--nats-websocket-url",
            BOGUS_WS_URL,
        ])
        .output()
        .expect("run trellis init config");
    assert!(
        init_output.status.success(),
        "trellis init config failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );
    let init: Value = serde_json::from_slice(&init_output.stdout)
        .expect("trellis init config should emit a JSON report");
    assert_eq!(init["generated"], true);
    assert_eq!(init["out"], bundle.display().to_string());
    let config_path = bundle.join("config.toml");
    assert!(
        config_path.is_file(),
        "bundle trellis config was not generated"
    );
    assert!(
        bundle.join("nats/nats.conf").is_file(),
        "bundle nats.conf was not generated"
    );
    assert!(
        bundle.join("nats/jwt.conf").is_file(),
        "bundle jwt.conf was not generated"
    );
    let mut config_toml = fs::read_to_string(&config_path).expect("read bundle config");
    assert!(
        config_toml.contains(BOGUS_NATS_URL),
        "bundle must configure the bogus NATS URL so the managed override is observable"
    );
    assert!(
        config_toml.contains(BOGUS_WS_URL),
        "bundle must configure the bogus websocket URL so the managed override is observable"
    );
    config_toml.push_str(&format!(
        "\n[paths]\ndata = {data:?}\nstate = {state:?}\ncache = {cache:?}\nruntime = {runtime:?}\nlogs = {logs:?}\n",
        data = effective_root.join("data").display().to_string(),
        state = effective_root.join("state").display().to_string(),
        cache = effective_root.join("cache").display().to_string(),
        runtime = effective_root.join("runtime").display().to_string(),
        logs = effective_root.join("logs").display().to_string(),
    ));
    fs::write(&config_path, &config_toml).expect("write configured path roots");
    let authored_nats = fs::read(bundle.join("nats/nats.conf")).expect("read authored nats.conf");
    let authored_jwt = fs::read(bundle.join("nats/jwt.conf")).expect("read authored jwt.conf");

    // 2. Managed-mode run: the first run downloads the binary, creates the stores the
    //    preflight report requires, and a clean SIGTERM shutdown stops the managed
    //    nats-server.
    let pid_file = effective_root.join("runtime/nats-server.pid");
    let run_stdout = workdir.0.join("cli-run.stdout.log");
    let run_stderr = workdir.0.join("cli-run.stderr.log");
    let mut run_command = server_command(&workdir.0);
    let mut child = ChildGuard::spawn(
        run_command
            .args([
                "all",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "--nats-download",
                "--local-nats-ports",
                &ports_arg,
            ])
            .stdout(Stdio::from(
                fs::File::create(&run_stdout).expect("create run stdout log"),
            ))
            .stderr(Stdio::from(
                fs::File::create(&run_stderr).expect("create run stderr log"),
            )),
        "trellis-server",
    );
    wait_until(
        || {
            fs::read_to_string(&pid_file)
                .ok()
                .and_then(|content| content.trim().parse::<i32>().ok())
                .is_some()
        },
        &mut child,
        &run_stderr,
        "the managed nats-server pid file",
    )
    .await;
    let managed_pid = fs::read_to_string(&pid_file)
        .expect("read pid file")
        .trim()
        .parse::<i32>()
        .expect("parse pid");
    wait_until(
        || managed_ports.iter().all(|port| port_accepts(*port)),
        &mut child,
        &run_stderr,
        "all managed ports to accept connections",
    )
    .await;
    wait_until(
        || port_accepts(runtime_port),
        &mut child,
        &run_stderr,
        "the runtime HTTP listener",
    )
    .await;
    child.signal_term();
    let exit = child.wait_for_exit(SHUTDOWN_TIMEOUT);
    assert!(
        exit.is_some(),
        "trellis-server did not exit after SIGTERM\nstderr tail:\n{}",
        log_tail(&run_stderr)
    );
    assert!(
        exit.unwrap().success(),
        "trellis-server exited with failure\nstdout tail:\n{}\nstderr tail:\n{}",
        log_tail(&run_stdout),
        log_tail(&run_stderr)
    );
    assert!(
        !pid_file.exists(),
        "managed nats-server pid file was not removed on shutdown"
    );
    assert!(
        !pid_alive(managed_pid),
        "managed nats-server (pid {managed_pid}) is still alive after shutdown"
    );
    assert!(
        !managed_ports.iter().any(|port| port_accepts(*port)),
        "managed ports still accept connections after shutdown"
    );
    let effective_nats = fs::read_to_string(effective_root.join("state/nats/nats.conf"))
        .expect("read effective managed nats.conf");
    assert!(
        !effective_nats.contains("0.0.0.0"),
        "the managed local effective config must bind loopback only: {effective_nats}"
    );
    for (label, port) in [
        ("listen", nats_port),
        ("http", monitor_port),
        ("listen", websocket_port),
    ] {
        assert!(
            effective_nats.contains(&format!("{label}: 127.0.0.1:{port}")),
            "effective managed config must bind {label} on loopback port {port}: {effective_nats}"
        );
    }
    let nats_log = effective_root.join("logs/nats-server.log");
    assert!(nats_log.is_file());
    assert!(
        fs::metadata(&nats_log)
            .expect("stat managed NATS log")
            .len()
            > 0,
        "managed NATS stdout and stderr must be captured in its dedicated log"
    );
    assert!(effective_root.join("state/nats/nats.conf").is_file());
    for database in [
        "platform.sqlite",
        "jobs.sqlite",
        "health.sqlite",
        "events.sqlite",
    ] {
        assert!(
            effective_root.join("data").join(database).is_file(),
            "omitted {database} path must use the configured data root"
        );
        assert!(
            !bundle.join(database).exists(),
            "mutable {database} must not land under the config root"
        );
    }
    assert!(
        effective_root.join("state/event-context.digest").is_file(),
        "omitted event context state must use the configured state root"
    );
    assert_eq!(
        fs::read(bundle.join("nats/nats.conf")).expect("reread authored nats.conf"),
        authored_nats,
        "managed startup must not rewrite authored nats.conf"
    );
    assert_eq!(
        fs::read(bundle.join("nats/jwt.conf")).expect("reread authored jwt.conf"),
        authored_jwt,
        "managed startup must not rewrite authored jwt.conf"
    );

    // 3. Managed-mode `check` after the first run: valid preflight report (proving
    //    the managed endpoint override — the bundle points at invalid hosts, yet
    //    the checks connect to the managed server), JSON-only stdout, exit
    //    0, and the check's own server fully stopped.
    let check_stdout = workdir.0.join("cli-check.stdout.log");
    let check_stderr = workdir.0.join("cli-check.stderr.log");
    let mut check_command = server_command(&workdir.0);
    let mut check = ChildGuard::spawn(
        check_command
            .args([
                "check",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "--nats-download",
                "--local-nats-ports",
                &ports_arg,
                "all",
            ])
            .stdout(Stdio::from(
                fs::File::create(&check_stdout).expect("create check stdout log"),
            ))
            .stderr(Stdio::from(
                fs::File::create(&check_stderr).expect("create check stderr log"),
            )),
        "trellis-server check",
    );
    let check_exit = check.wait_for_exit(STARTUP_TIMEOUT);
    assert!(
        check_exit.is_some(),
        "trellis-server check did not exit\nstderr tail:\n{}",
        log_tail(&check_stderr)
    );
    let check_stdout_text = fs::read_to_string(&check_stdout).expect("read check stdout log");
    assert!(
        check_exit.unwrap().success(),
        "trellis-server check failed with a preflight report:\n{check_stdout_text}\nstderr tail:\n{}",
        log_tail(&check_stderr)
    );
    let report: Value = serde_json::from_str(&check_stdout_text)
        .expect("trellis-server check should emit a JSON report on stdout");
    assert_eq!(
        report["valid"], true,
        "preflight report expected valid: {report}"
    );
    assert_eq!(report["mode"], "all");
    let checks = report["checks"].as_array().expect("checks is an array");
    assert!(
        checks.iter().any(|check| {
            check["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("nats.") && check["status"] == "ok")
        }),
        "preflight report should include an ok NATS check: {report}"
    );
    assert!(
        !pid_file.exists(),
        "check left a managed nats-server pid file behind"
    );
    assert!(
        !port_accepts(nats_port),
        "managed nats-server is still accepting connections after check"
    );

    // 3b. An occupied selected port is the owned PortInUse startup error; startup
    //     must never silently change ports. The test owns the occupying listener and
    //     releases it before the external phase reuses the port.
    let occupied = TcpListener::bind(("127.0.0.1", nats_port)).expect("occupy selected port");
    let occupied_stderr = workdir.0.join("cli-occupied.stderr.log");
    let mut occupied_command = server_command(&workdir.0);
    let mut occupied_child = ChildGuard::spawn(
        occupied_command
            .args([
                "all",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "--nats-download",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(&occupied_stderr).expect("create occupied stderr log"),
            )),
        "trellis-server occupied port startup",
    );
    let occupied_exit = occupied_child
        .wait_for_exit(SHUTDOWN_TIMEOUT)
        .expect("occupied selected port must fail startup");
    assert!(
        !occupied_exit.success(),
        "occupied selected port must fail startup instead of changing ports"
    );
    assert!(
        fs::read_to_string(&occupied_stderr)
            .expect("read occupied stderr log")
            .contains(&format!("port {nats_port} is already in use")),
        "startup must report the occupied selected port\nstderr tail:\n{}",
        log_tail(&occupied_stderr)
    );
    drop(occupied);

    // 3c. A managed bundle without an explicit nats.conf listener set is a
    //     configuration error, never a silent fallback to default ports.
    let broken_bundle = workdir.0.join("broken-bundle");
    let broken_init = cli_command()
        .args([
            "init",
            "config",
            "--out",
            broken_bundle.to_str().expect("UTF-8 broken bundle path"),
            "--trellis-port",
            &runtime_port.to_string(),
            "--nats-port",
            &nats_port.to_string(),
            "--nats-monitor-port",
            &monitor_port.to_string(),
            "--nats-ws-port",
            &websocket_port.to_string(),
        ])
        .output()
        .expect("run trellis init config for broken bundle");
    assert!(
        broken_init.status.success(),
        "trellis init config failed: {}",
        String::from_utf8_lossy(&broken_init.stderr)
    );
    fs::remove_file(broken_bundle.join("nats/nats.conf")).expect("remove authored nats.conf");
    let cached_binary =
        NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, Some(&cache_dir))
            .expect("cached nats-server binary");
    let broken_stderr = workdir.0.join("cli-broken-nats.stderr.log");
    let broken_config = broken_bundle.join("config.toml");
    let local_nats = format!("--local-nats={}", cached_binary.display());
    let mut broken_command = server_command(&workdir.0);
    let mut broken = ChildGuard::spawn(
        broken_command
            .args([
                "check",
                "--config",
                broken_config.to_str().expect("UTF-8 broken config path"),
                local_nats.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(&broken_stderr).expect("create broken stderr log"),
            )),
        "trellis-server missing nats.conf startup",
    );
    let broken_exit = broken
        .wait_for_exit(SHUTDOWN_TIMEOUT)
        .expect("a managed bundle without nats.conf must fail startup");
    assert!(
        !broken_exit.success(),
        "a managed bundle without nats.conf must not fall back to default ports"
    );
    assert!(
        fs::read_to_string(&broken_stderr)
            .expect("read broken stderr log")
            .contains("failed to read managed NATS config"),
        "startup must report the unreadable managed NATS config\nstderr tail:\n{}",
        log_tail(&broken_stderr)
    );

    // 4. Plain startup uses configured external NATS and never owns that process.
    let binary = NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, Some(&cache_dir))
        .expect("cached nats-server binary");
    let external_pid_file = workdir.0.join("external-nats-server.pid");
    let external = LocalNats::builder()
        .binary(NatsBinarySource::Path(binary))
        .source(bundle.join("nats"))
        .state(workdir.0.join("external-nats-state"))
        .ports(LocalNatsPorts {
            nats: nats_port,
            monitor: monitor_port,
            websocket: websocket_port,
        })
        .pid_file(&external_pid_file)
        .output(NatsOutput::Log {
            path: workdir.0.join("external-nats-server.log"),
            mirror: false,
        })
        .start()
        .expect("spawn test-owned managed nats-server");
    let nats = async_nats::ConnectOptions::new()
        .credentials_file(bundle.join("nats/creds/trellis-auth.creds"))
        .await
        .expect("load Trellis NATS credentials")
        .connect(external.nats_url())
        .await
        .expect("connect to external NATS");
    let jetstream = jetstream::new(nats);
    let external_config = config_toml
        .replace(BOGUS_NATS_URL, &format!("nats://127.0.0.1:{nats_port}"))
        .replace(BOGUS_WS_URL, &format!("ws://localhost:{websocket_port}"));
    fs::write(&config_path, external_config).expect("write external NATS config");

    for (name, subjects, max_messages_per_subject, discard_new_per_subject) in [
        ("trellis_consumer_dlq", "_trellis.consumer.dlq.>", -1, false),
        (
            "trellis_consumer_replays",
            "_trellis.consumer.replay.>",
            1,
            true,
        ),
    ] {
        for stream_name in [
            "trellis",
            "trellis_consumer_dlq",
            "trellis_consumer_replays",
            "JOBS_ADVISORIES",
            "JOBS",
            "JOBS_WORK",
            "TRELLIS_HEALTH",
        ] {
            let _ = jetstream.delete_stream(stream_name).await;
        }
        jetstream
            .create_stream(stream::Config {
                name: name.to_owned(),
                subjects: vec![subjects.to_owned()],
                retention: stream::RetentionPolicy::Limits,
                storage: stream::StorageType::Memory,
                discard: stream::DiscardPolicy::New,
                max_messages: -1,
                max_messages_per_subject,
                max_bytes: -1,
                discard_new_per_subject,
                allow_direct: true,
                ..Default::default()
            })
            .await
            .expect("create incompatible evidence stream");
        jetstream
            .publish(subjects.replace('>', "record"), "preserve me".into())
            .await
            .expect("publish evidence record")
            .await
            .expect("store evidence record");
        let before = jetstream
            .get_stream(name)
            .await
            .expect("evidence stream exists")
            .info()
            .await
            .expect("read evidence stream")
            .clone();
        let failure_stderr = workdir.0.join(format!("{name}-startup.stderr.log"));
        let mut failure_command = server_command(&workdir.0);
        let mut failure = ChildGuard::spawn(
            failure_command
                .args([
                    "all",
                    "--config",
                    config_path.to_str().expect("UTF-8 config path"),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::from(
                    fs::File::create(&failure_stderr).expect("create failed startup log"),
                )),
            "trellis-server incompatible stream startup",
        );
        let status = failure
            .wait_for_exit(SHUTDOWN_TIMEOUT)
            .expect("incompatible startup must exit");
        assert!(
            !status.success(),
            "incompatible startup unexpectedly succeeded"
        );
        assert!(
            fs::read_to_string(&failure_stderr)
                .expect("read failed startup log")
                .contains(&format!("incompatible runtime stream {name}")),
            "startup must report the incompatible stream"
        );
        let after = jetstream
            .get_stream(name)
            .await
            .expect("evidence stream remains")
            .info()
            .await
            .expect("read preserved evidence stream")
            .clone();
        assert_eq!(after.config, before.config);
        assert_eq!(after.state.messages, 1);
    }

    for stream_name in [
        "trellis",
        "trellis_consumer_dlq",
        "trellis_consumer_replays",
        "JOBS_ADVISORIES",
        "JOBS",
        "JOBS_WORK",
        "TRELLIS_HEALTH",
    ] {
        let _ = jetstream.delete_stream(stream_name).await;
    }
    jetstream
        .create_stream(stream::Config {
            name: "trellis_consumer_dlq".to_owned(),
            subjects: vec!["_trellis.consumer.dlq.>".to_owned()],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::File,
            discard: stream::DiscardPolicy::New,
            max_messages: 10,
            max_messages_per_subject: -1,
            max_bytes: -1,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create finite DLQ stream");
    jetstream
        .publish("_trellis.consumer.dlq.record", "preserve me".into())
        .await
        .expect("publish DLQ record")
        .await
        .expect("store DLQ record");
    jetstream
        .create_stream(stream::Config {
            name: "trellis_consumer_replays".to_owned(),
            subjects: vec!["_trellis.consumer.replay.>".to_owned()],
            retention: stream::RetentionPolicy::Limits,
            storage: stream::StorageType::Memory,
            discard: stream::DiscardPolicy::New,
            max_messages: -1,
            max_messages_per_subject: 1,
            max_bytes: -1,
            discard_new_per_subject: true,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create incompatible replay stream");
    let expansion_stderr = workdir.0.join("safe-expansion-startup.stderr.log");
    let mut expansion_command = server_command(&workdir.0);
    let mut expansion = ChildGuard::spawn(
        expansion_command
            .args([
                "all",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(&expansion_stderr).expect("create expansion startup log"),
            )),
        "trellis-server safe stream expansion",
    );
    assert!(
        !expansion
            .wait_for_exit(SHUTDOWN_TIMEOUT)
            .expect("startup must reach incompatible replay stream")
            .success(),
        "startup unexpectedly accepted incompatible replay stream"
    );
    let expanded = jetstream
        .get_stream("trellis_consumer_dlq")
        .await
        .expect("DLQ remains")
        .info()
        .await
        .expect("read expanded DLQ")
        .clone();
    assert_eq!(expanded.config.max_messages, -1);
    assert_eq!(expanded.state.messages, 1);
    for stream_name in [
        "trellis",
        "trellis_consumer_dlq",
        "trellis_consumer_replays",
        "JOBS_ADVISORIES",
        "JOBS",
        "JOBS_WORK",
        "TRELLIS_HEALTH",
    ] {
        let _ = jetstream.delete_stream(stream_name).await;
    }
    let external_stdout = workdir.0.join("cli-external.stdout.log");
    let external_stderr = workdir.0.join("cli-external.stderr.log");
    let mut external_command = server_command(&workdir.0);
    let mut external_child = ChildGuard::spawn(
        external_command
            .args([
                "all",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
            ])
            .stdout(Stdio::from(
                fs::File::create(&external_stdout).expect("create external stdout log"),
            ))
            .stderr(Stdio::from(
                fs::File::create(&external_stderr).expect("create external stderr log"),
            )),
        "trellis-server configured external NATS",
    );
    external_child.set_nats(external);
    wait_until(
        || port_accepts(runtime_port),
        &mut external_child,
        &external_stderr,
        "the runtime HTTP listener (external mode)",
    )
    .await;
    external_child.signal_term();
    let external_exit = external_child.wait_for_exit(SHUTDOWN_TIMEOUT);
    assert!(
        external_exit.is_some(),
        "trellis-server external mode did not exit after SIGTERM\nstderr tail:\n{}",
        log_tail(&external_stderr)
    );
    assert!(
        external_exit.unwrap().success(),
        "trellis-server external mode failed\nstdout tail:\n{}\nstderr tail:\n{}",
        log_tail(&external_stdout),
        log_tail(&external_stderr)
    );
    assert!(
        port_accepts(nats_port),
        "external mode must not stop a nats-server it did not spawn"
    );
    assert!(
        external_pid_file.exists(),
        "external mode must not touch the test-owned pid file"
    );
    // Drop stops the test-owned server and removes its pid file.
}
