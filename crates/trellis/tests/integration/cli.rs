//! Live coverage for `trellis-server` running its own NATS in development and
//! connecting to NATS servers it does not run.

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

/// A NATS URL nothing listens on, written into the installation so that
/// development mode's endpoint override is observable: the report must still
/// be valid.
const BOGUS_NATS_URL: &str = "nats://127.0.0.1:1";
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
    // The workspace build compiles both rustls backends, so choose the one the
    // runtime and CLI install.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let workdir = WorkdirGuard::new();
    let bundle = workdir.0.join("bundle");
    // Held outside the ephemeral range until the server is done; see ReservedPort.
    let reservations =
        [(); 4].map(|_| trellis_local_nats::ReservedPort::reserve().expect("reserve port"));
    let [nats_port, monitor_port, websocket_port, oauth_port] = reservations
        .each_ref()
        .map(trellis_local_nats::ReservedPort::port);
    let managed_ports = [nats_port, monitor_port, websocket_port];
    let ports_arg = format!("{nats_port},{monitor_port},{websocket_port}");

    // An empty private cache: explicit download acquires pinned NATS on first run.
    let cache_dir = workdir.0.join("cache/trellis/nats");
    fs::create_dir_all(&cache_dir).expect("create managed-NATS cache dir");
    fs::set_permissions(&cache_dir, fs::Permissions::from_mode(0o700))
        .expect("make managed-NATS cache dir private");

    // 1. The first `--dev` start creates the installation; a check stops
    //    there. The installation then moves to this test's ports, and its
    //    [nats] servers to an address nothing answers on.
    let created = server_command(&workdir.0)
        .args(["check", "--dev", "--config"])
        .arg(&bundle)
        .args(["--nats-download", "--local-nats-ports", &ports_arg])
        .output()
        .expect("create the development installation");
    let config_path = bundle.join("config.toml");
    assert!(
        config_path.is_file(),
        "the first --dev start did not create the installation: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let moved = |path: &Path, ports: &[(&str, u16)]| {
        let mut text = fs::read_to_string(path).expect("read installation file");
        for (from, to) in ports {
            text = text.replace(from, &format!(":{to}"));
        }
        fs::write(path, text).expect("write installation file");
    };
    moved(
        &config_path,
        &[
            (":4222", 1),
            (":8080", websocket_port),
            (":3000", oauth_port),
        ],
    );
    let config_text = fs::read_to_string(&config_path)
        .expect("read config")
        .replace("port = 3000", &format!("port = {oauth_port}"));
    fs::write(&config_path, config_text).expect("write config");
    moved(
        &bundle.join("nats.conf"),
        &[
            (":4222", nats_port),
            (":8222", monitor_port),
            (":8080", websocket_port),
        ],
    );
    // A bundle keeps everything the server writes in its var folder, apart
    // from the NATS download, which every installation shares.
    let effective_root = bundle.join("var");
    let seeded = server_command(&workdir.0)
        .args([
            "admin",
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "--username",
            "managed-nats-admin",
            "--password",
            "managed-nats-fixture-password",
        ])
        .output()
        .expect("seed fresh installation administrator");
    assert!(
        seeded.status.success(),
        "bootstrap administrator: {}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let authored_nats = fs::read(bundle.join("nats.conf")).expect("read authored nats.conf");

    // 2. Managed-mode run: the first run downloads the binary, creates the stores the
    //    preflight report requires, and a clean SIGTERM shutdown stops the managed
    //    nats-server.
    let pid_file = effective_root.join("run/nats-server.pid");
    let run_stdout = workdir.0.join("cli-run.stdout.log");
    let run_stderr = workdir.0.join("cli-run.stderr.log");
    let mut run_command = server_command(&workdir.0);
    let mut child = ChildGuard::spawn(
        run_command
            .args([
                "--dev",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "--nats-download",
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
        || port_accepts(oauth_port),
        &mut child,
        &run_stderr,
        "the Trellis listener",
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
    let nats_log = effective_root.join("log/nats-server.log");
    assert!(nats_log.is_file());
    assert!(
        fs::metadata(&nats_log)
            .expect("stat managed NATS log")
            .len()
            > 0,
        "managed NATS stdout and stderr must be captured in its dedicated log"
    );
    assert!(effective_root.join("nats/nats.conf").is_file());
    for database in [
        "platform.sqlite",
        "jobs.sqlite",
        "health.sqlite",
        "events.sqlite",
    ] {
        assert!(
            effective_root.join(database).is_file(),
            "omitted {database} path must use the bundle's var folder"
        );
        assert!(
            !bundle.join(database).exists(),
            "mutable {database} must not land under the config root"
        );
    }
    assert_eq!(
        fs::read(bundle.join("nats.conf")).expect("reread authored nats.conf"),
        authored_nats,
        "managed startup must not rewrite authored nats.conf"
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
                "--dev",
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "--nats-download",
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
        "trellis-server check failed with a preflight \
        report:\n{check_stdout_text}\nstderr tail:\n{}",
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
                "--dev",
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
    fs::create_dir(&broken_bundle).expect("create broken bundle");
    fs::copy(&config_path, broken_bundle.join("config.toml")).expect("copy config");
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
                "--dev",
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

    // 4. An installation without `managed` uses configured external NATS and
    //    never owns that process.
    let binary = NatsServerBinary::resolve(&NatsBinarySource::DownloadPinned, Some(&cache_dir))
        .expect("cached nats-server binary");
    let external_pid_file = workdir.0.join("external-nats-server.pid");
    // The platform database is bound to the managed run's JetStream store, so
    // the external server must own that same store directory to stay paired;
    // a fresh store would be a different environment and must not start.
    let external = LocalNats::builder()
        .binary(NatsBinarySource::Path(binary))
        .source(&bundle)
        .state(effective_root.join("nats"))
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
    let creds = server_command(&workdir.0)
        .args(["nats", "trellis", "--print", "--config"])
        .arg(&bundle)
        .output()
        .expect("print Trellis NATS credentials");
    assert!(
        creds.status.success(),
        "{}",
        String::from_utf8_lossy(&creds.stderr)
    );
    let nats = async_nats::ConnectOptions::with_credentials(
        &String::from_utf8(creds.stdout).expect("UTF-8 creds"),
    )
    .expect("load Trellis NATS credentials")
    .connect(external.nats_url())
    .await
    .expect("connect to external NATS");
    let jetstream = jetstream::new(nats);
    // Dropping `managed` hands NATS to the operator: the same installation
    // now connects to the servers listed in [nats] servers. The first is
    // down, so the runtime only starts if it moves on to the second.
    let external_config = fs::read_to_string(&config_path)
        .expect("read bundle config")
        .replace("managed = true\n", "")
        .replace(
            &format!("servers = \"{BOGUS_NATS_URL}\""),
            &format!("servers = \"nats://127.0.0.2:1,nats://localhost:{nats_port}\""),
        );
    fs::write(&config_path, external_config).expect("write external NATS config");

    for (name, subjects, max_messages_per_subject, discard_new_per_subject) in [
        ("trellis_consumer_dlq", "_trellis.consumer.dlq.>", 1, true),
        (
            "trellis_consumer_replays",
            "_trellis.consumer.replay.>",
            1,
            false,
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
                .args(["--config", config_path.to_str().expect("UTF-8 config path")])
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
            discard_new_per_subject: false,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create incompatible replay stream");
    let expansion_stderr = workdir.0.join("safe-expansion-startup.stderr.log");
    let mut expansion_command = server_command(&workdir.0);
    let mut expansion = ChildGuard::spawn(
        expansion_command
            .args(["--config", config_path.to_str().expect("UTF-8 config path")])
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
    let advertised = format!("wss://localhost:{websocket_port}");
    let mut external_child = ChildGuard::spawn(
        external_command
            .args(["--config", config_path.to_str().expect("UTF-8 config path")])
            .args(["--advertise-nats-websocket", &advertised])
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
        || port_accepts(oauth_port),
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

/// A start with no installation creates nothing and says how to make one;
/// `--dev` creates a localhost development installation over plain HTTP,
/// which every later `--dev` start then finds. A plain start refuses it,
/// since only a development installation runs its own NATS. Without
/// nats-server on PATH the managed start stops before binding any of the
/// installation's fixed default ports.
#[test]
fn only_a_dev_start_creates_and_runs_a_development_installation() {
    let workdir = WorkdirGuard::new();
    let search_path = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .and_then(|cargo| cargo.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let start = |extra: &[&str]| {
        let output = server_command(&workdir.0)
            .env("PATH", &search_path)
            .args(extra)
            .output()
            .expect("run trellis-server");
        assert!(!output.status.success(), "start without NATS succeeded");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let installation = workdir.0.join("config/trellis");

    let refused = start(&[]);
    assert!(
        refused.contains("no Trellis configuration at") && refused.contains("config example"),
        "unexpected refusal: {refused}"
    );
    assert!(!installation.exists(), "a plain start created files");

    let created = start(&["--dev", "--config", installation.to_str().unwrap()]);
    let config = fs::read_to_string(installation.join("config.toml"))
        .unwrap_or_else(|_| panic!("--dev did not create the installation: {created}"));
    assert!(
        config.contains("public_origin = \"http://localhost:3000\"")
            && config.contains("mode = \"loopback_development\""),
        "--dev did not create a plain-HTTP localhost installation:\n{config}"
    );
    for stderr in [
        created,
        start(&["--dev", "--config", installation.to_str().unwrap()]),
    ] {
        assert!(
            stderr.contains("nats-server is not on PATH"),
            "start did not reach managed NATS: {stderr}"
        );
    }
    let production = start(&["--config", installation.to_str().unwrap()]);
    assert!(
        production.contains("start it with --dev"),
        "a plain start ran a development installation: {production}"
    );
}

/// Generated TLS serves Trellis and the Console on their own hostnames from a
/// CA Trellis makes, which nothing trusts without being told, and every
/// Console and Portal user is warned about it.
#[tokio::test]
async fn server_serves_generated_installation_tls() {
    let workdir = WorkdirGuard::new();
    let bundle = workdir.0.join("tls");
    // Held outside the ephemeral range until the server is done; see ReservedPort.
    let ports = [(); 4].map(|_| trellis_local_nats::ReservedPort::reserve().unwrap());
    let [nats, monitor, websocket, oauth] =
        ports.each_ref().map(trellis_local_nats::ReservedPort::port);
    // An operator's NATS server, and an installation using it whose Console
    // has a hostname of its own on the Trellis port.
    let nats_folder = workdir.0.join("nats");
    let made = server_command(&workdir.0)
        .args(["nats", "new", "--servers", "1", "--out"])
        .arg(&nats_folder)
        .args(["--client-port", &nats.to_string()])
        .args(["--monitor-port", &monitor.to_string()])
        .args(["--websocket-port", &websocket.to_string()])
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    fs::create_dir(&bundle).unwrap();
    let config = bundle.join("config.toml");
    let quoted = |path: PathBuf| serde_json::to_string(&path.display().to_string()).unwrap();
    fs::write(
        &config,
        format!(
            "[http]\npublic_origin = \"https://localhost:{oauth}\"\nport = {oauth}\n\
            nats_websocket_upstream = \"127.0.0.1:{websocket}\"\n\n\
            [http.transport]\nmode = \"generated\"\n\n\
            [console]\norigin = \"https://127.0.0.1:{oauth}\"\n\n\
            [nats]\nservers = \"nats://127.0.0.1:{nats}\"\nsystem_credentials = {}\n\
            operator_signing_key = {}\n",
            quoted(nats_folder.join("system.creds")),
            quoted(nats_folder.join("trellis-operator.nk")),
        ),
    )
    .unwrap();
    let bootstrap = server_command(&workdir.0)
        .args(["admin", "--config"])
        .arg(&config)
        .args([
            "--username",
            "tls-admin",
            "--password",
            "tls-fixture-password",
        ])
        .output()
        .unwrap();
    assert!(
        bootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    let status = server_command(&workdir.0)
        .args(["tls", "status", "--format", "json", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    let fingerprint = status["generatedTls"]["caSha256Fingerprint"]
        .as_str()
        .expect("tls status reports the generated CA fingerprint")
        .to_owned();
    let ca = PathBuf::from(
        status["generatedTls"]["caCertificate"]
            .as_str()
            .expect("tls status names the CA"),
    );
    let broker_log = bundle.join("nats.log");
    let binary = std::env::var_os("TRELLIS_TEST_NATS_BIN").unwrap_or_else(|| "nats-server".into());
    let mut broker = ChildGuard::spawn(
        Command::new(binary)
            .arg("-c")
            .arg(nats_folder.join("nats-1.conf"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&broker_log).unwrap())),
        "NATS",
    );
    wait_until(
        || port_accepts(nats),
        &mut broker,
        &broker_log,
        "NATS listener",
    )
    .await;
    let server_log = bundle.join("server.log");
    let server_output = bundle.join("server.stdout.log");
    let mut server = ChildGuard::spawn(
        server_command(&workdir.0)
            .arg("--config")
            .arg(&config)
            .env_remove("SSL_CERT_FILE")
            .stdout(Stdio::from(fs::File::create(&server_output).unwrap()))
            .stderr(Stdio::from(fs::File::create(&server_log).unwrap())),
        "TLS Trellis server",
    );
    wait_until(
        || port_accepts(oauth),
        &mut server,
        &server_log,
        "runtime serving generated TLS",
    )
    .await;
    assert!(
        fs::read_to_string(&server_output)
            .unwrap()
            .contains(&fingerprint),
        "startup must warn operators with the generated CA fingerprint"
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .no_proxy()
        .tls_info(true)
        .pool_max_idle_per_host(0)
        .tls_certs_only([reqwest::Certificate::from_pem(&fs::read(&ca).unwrap()).unwrap()])
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let oauth_origin = format!("https://localhost:{oauth}");
    let metadata: Value = client
        .get(format!(
            "{oauth_origin}/.well-known/oauth-authorization-server"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["issuer"], oauth_origin);
    assert!(
        client
            .get(format!(
                "http://localhost:{oauth}/.well-known/oauth-authorization-server"
            ))
            .send()
            .await
            .is_err(),
        "OAuth TLS must not accept plaintext"
    );
    let untrusted = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    assert!(
        untrusted
            .get(format!(
                "{oauth_origin}/.well-known/oauth-authorization-server"
            ))
            .send()
            .await
            .is_err(),
        "the generated CA must not be trusted without explicit configuration"
    );
    // Every Console and Portal user is told the installation serves its own
    // generated certificates. The Console answers on its own hostname, and
    // the Trellis hostname sends people there.
    let redirect = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .tls_certs_only([reqwest::Certificate::from_pem(&fs::read(&ca).unwrap()).unwrap()])
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .get(format!("{oauth_origin}/console/admin"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        redirect.headers()[reqwest::header::LOCATION],
        format!("https://127.0.0.1:{oauth}/console/admin").as_str()
    );
    let console = client
        .get(format!(
            "https://127.0.0.1:{oauth}/assets/web/runtime-config.js"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(console.contains(r#""generatedTls":true"#), "{console}");
    let portal = client
        .get(format!("{oauth_origin}/device/complete"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(portal.contains("<html data-generated-tls"), "{portal}");
    // A real WSS upgrade reaches the broker through the Trellis port's
    // verified TLS. Upgrades are an HTTP/1.1 mechanism.
    let upgraded = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .tls_certs_only([reqwest::Certificate::from_pem(&fs::read(&ca).unwrap()).unwrap()])
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .get(format!("{oauth_origin}/"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(upgraded.status(), reqwest::StatusCode::SWITCHING_PROTOCOLS);

    server.signal_term();
    assert!(server.wait_for_exit(SHUTDOWN_TIMEOUT).unwrap().success());
    broker.signal_term();
    assert!(broker.wait_for_exit(SHUTDOWN_TIMEOUT).unwrap().success());
}
