//! Live acceptance tests for the external Rust testkit.
//!
//! These tests are explicitly selected by the fixture's `live` target and run
//! against real `trellis`/`trellis-server` executables. They never start
//! infrastructure during compilation or documentation generation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use trellis_rs::client::EventSubscribeOptions;
use trellis_rs::service::ServerError;
use trellis_test::{TrellisTestErrorKind, TrellisTestRuntime};
use trellis_test_fixture::participants::trellis_test_fixture_caller::{
    Client as CallerClient, Participant as CallerParticipant,
};
use trellis_test_fixture::participants::trellis_test_fixture_provider::{
    Participant as ProviderParticipant, Provider,
};
use trellis_test_fixture::participants::trellis_test_fixture_restricted_caller::Participant as RestrictedCallerParticipant;
use trellis_test_fixture::participants::trellis_test_fixture_unsupported_device::Participant as UnsupportedDeviceParticipant;
use trellis_test_fixture::types::Value;

/// A running provider service plus the count of executed Echo handlers.
struct ProviderFixture {
    task: tokio::task::JoinHandle<Result<(), trellis_rs::service::ServiceRuntimeError>>,
    calls: Arc<AtomicUsize>,
}

/// Registers and runs a Provider whose Echo handler publishes `Observed`.
async fn start_provider(runtime: &mut TrellisTestRuntime, name: &str) -> ProviderFixture {
    let identity = runtime
        .register_service::<ProviderParticipant>(name)
        .await
        .expect("register provider");
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .expect("connect provider");
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let publisher = Provider::new(&mut service).client();
        let handler_calls = Arc::clone(&calls);
        let mut provider = Provider::new(&mut service);
        provider
            .trellis_test_fixture_echo_v1()
            .register_echo(move |_context, input| {
                let publisher = publisher.clone();
                let handler_calls = Arc::clone(&handler_calls);
                async move {
                    handler_calls.fetch_add(1, Ordering::SeqCst);
                    publisher
                        .trellis_test_fixture_echo_v1()
                        .publish_observed(&input)
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    Ok(input)
                }
            });
    }
    let task = tokio::spawn(async move { service.run().await });
    ProviderFixture { task, calls }
}

fn value(text: &str) -> Value {
    Value {
        value: text.to_owned(),
    }
}

/// Parses the loopback port out of an endpoint URL such as `nats://127.0.0.1:4222`.
fn endpoint_port(url: &str) -> u16 {
    url.rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("endpoint URL has no port: {url}"))
}

/// Polls until nothing accepts a loopback TCP connection on `port`.
async fn wait_until_no_listener(port: u16, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Runs a child test process with an explicit populated profile, without
/// mutating this process's environment.
async fn run_profile_child(child_name: &str, label: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current test executable");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let profile = std::env::temp_dir().join(format!(
        "trellis-test-profile-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(profile.join("trellis")).expect("create profile");
    std::fs::write(
        profile.join("trellis").join("admin-session.json"),
        b"sentinel-admin-session",
    )
    .expect("write profile sentinel");
    let status = tokio::process::Command::new(exe)
        .args(["--exact", child_name, "--ignored", "--nocapture"])
        .env("TRELLIS_TEST_PROFILE_HOME", &profile)
        .status()
        .await
        .expect("run child test process");
    assert!(status.success(), "the profile child process must succeed");
    profile
}

/// T04: a real runtime bootstraps, authenticates, registers a Provider and
/// Caller, performs the typed RPC, and receives the real typed event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_real_rpc_and_event_between_provider_and_caller() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let provider = start_provider(&mut runtime, "provider").await;
    let caller_identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");

    let caller = CallerClient::connect(caller_identity.connect_options())
        .await
        .expect("connect caller");
    let api = caller.trellis_test_fixture_echo_v1();
    let mut events = api
        .subscribe_observed(EventSubscribeOptions::ephemeral())
        .await
        .expect("subscribe to Observed");

    let output = api.echo(&value("hello")).await.expect("call Echo");
    assert_eq!(output.value, "hello");

    let observed = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("event arrives before the timeout")
        .expect("event stream yields an item")
        .expect("event decodes");
    assert_eq!(observed.value, "hello");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    drop(events);
    drop(api);
    drop(caller);
    provider.task.abort();
    let _ = provider.task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T06: two providers registered under different names keep distinct
/// deployment and instance identities.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t06_two_providers_have_distinct_deployments() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let first = runtime
        .register_service::<ProviderParticipant>("provider-a")
        .await
        .expect("register first provider");
    let second = runtime
        .register_service::<ProviderParticipant>("provider-b")
        .await
        .expect("register second provider");

    assert_ne!(first.deployment_id(), second.deployment_id());
    assert_ne!(first.instance_id(), second.instance_id());
    assert_eq!(first.participant_id(), second.participant_id());

    runtime.shutdown().await.expect("shutdown runtime");
}

/// T08: a caller without Echo permission is denied by the real server and the
/// provider handler never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t08_restricted_caller_is_denied() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let provider = start_provider(&mut runtime, "provider").await;
    let restricted = runtime
        .register_client::<RestrictedCallerParticipant>("restricted")
        .await
        .expect("register restricted caller");

    let client = trellis_rs::generated::Client::connect_user(restricted.connect_options())
        .await
        .expect("connect restricted caller");
    let api =
        trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client::from_generated(client);
    let result = api.echo(&value("denied")).await;
    assert!(result.is_err(), "restricted caller must be denied");
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "the provider handler must not run for a denied call"
    );

    provider.task.abort();
    let _ = provider.task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T10: unsupported participant kinds fail before provisioning.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t10_unsupported_participant_kind_is_rejected() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let error = runtime
        .register_service::<UnsupportedDeviceParticipant>("device")
        .await
        .expect_err("device must be rejected as a service");
    assert_eq!(
        error.kind(),
        TrellisTestErrorKind::UnsupportedParticipantKind
    );
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T09: duplicate registration names fail deterministically.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t09_duplicate_names_are_rejected() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider");
    let error = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect_err("duplicate name must be rejected");
    assert_eq!(error.kind(), TrellisTestErrorKind::DuplicateName);
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T14: shutdown releases the real HTTP and managed-NATS listeners, is
/// idempotent, and refuses further registration with `RuntimeStopped`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t14_shutdown_is_idempotent() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let endpoints = [
        runtime.trellis_url().to_owned(),
        runtime.nats_url().to_owned(),
        runtime.websocket_url().to_owned(),
        runtime.monitor_url().to_owned(),
    ];
    runtime.shutdown().await.expect("first shutdown");
    runtime.shutdown().await.expect("second shutdown");
    for url in &endpoints {
        let port = endpoint_port(url);
        assert!(
            wait_until_no_listener(port, Duration::from_secs(15)).await,
            "endpoint {url} must stop listening after shutdown"
        );
    }
    let error = runtime
        .install_participant::<ProviderParticipant>()
        .await
        .expect_err("installing after shutdown must fail");
    assert_eq!(
        error.kind(),
        TrellisTestErrorKind::RuntimeStopped,
        "a stopped runtime must report RuntimeStopped, not a cached success"
    );
}

/// T07: eight independent runtimes run concurrently in one Rust test process,
/// remain live together behind a barrier, and never cross nonce-bearing typed
/// calls or events. No caller-specified ports.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn t07_eight_concurrent_runtimes_are_isolated() {
    use tokio::sync::Barrier;

    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for index in 0..8u32 {
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            let mut runtime = TrellisTestRuntime::builder()
                .start()
                .await
                .expect("start runtime");
            let provider = start_provider(&mut runtime, "provider").await;
            let caller_identity = runtime
                .register_client::<CallerParticipant>("caller")
                .await
                .expect("register caller");
            let caller = CallerClient::connect(caller_identity.connect_options())
                .await
                .expect("connect caller");
            let api = caller.trellis_test_fixture_echo_v1();
            let mut events = api
                .subscribe_observed(EventSubscribeOptions::ephemeral())
                .await
                .expect("subscribe to Observed");

            // Every runtime is fully live and idle until all eight arrive here.
            barrier.wait().await;

            let nonce = format!("runtime-nonce-{index}");
            let output = api.echo(&value(&nonce)).await.expect("call Echo");
            assert_eq!(output.value, nonce, "RPC must return this runtime's nonce");
            let observed = tokio::time::timeout(Duration::from_secs(15), events.next())
                .await
                .expect("event arrives before the timeout")
                .expect("event stream yields an item")
                .expect("event decodes");
            assert_eq!(
                observed.value, nonce,
                "event must carry this runtime's nonce"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

            // Stay live until every runtime has completed its own traffic.
            barrier.wait().await;

            drop(events);
            drop(api);
            drop(caller);
            provider.task.abort();
            let _ = provider.task.await;
            let endpoints = (
                runtime.trellis_url().to_owned(),
                runtime.nats_url().to_owned(),
                runtime.websocket_url().to_owned(),
                runtime.monitor_url().to_owned(),
                runtime.workdir().to_path_buf(),
            );
            runtime.shutdown().await.expect("shutdown runtime");
            endpoints
        }));
    }

    let mut http = std::collections::HashSet::new();
    let mut nats = std::collections::HashSet::new();
    let mut websocket = std::collections::HashSet::new();
    let mut monitor = std::collections::HashSet::new();
    let mut workdirs = std::collections::HashSet::new();
    for handle in handles {
        let (h, n, w, m, dir) = tokio::time::timeout(Duration::from_secs(300), handle)
            .await
            .expect("runtime task timed out")
            .expect("join runtime task");
        http.insert(h);
        nats.insert(n);
        websocket.insert(w);
        monitor.insert(m);
        workdirs.insert(dir);
    }
    assert_eq!(http.len(), 8, "HTTP endpoints must be distinct");
    assert_eq!(nats.len(), 8, "NATS endpoints must be distinct");
    assert_eq!(websocket.len(), 8, "WebSocket endpoints must be distinct");
    assert_eq!(monitor.len(), 8, "NATS monitor endpoints must be distinct");
    assert_eq!(workdirs.len(), 8, "sandboxes must be distinct");
}

/// T21: `complete_session` binds a non-administrator session without writing a
/// populated default profile. The work runs in a child process with explicit
/// environment, so this test never mutates the supervising process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t21_complete_session_does_not_write_the_default_store() {
    let profile = run_profile_child("profile_child_registers_caller", "t21").await;
    let sentinel = profile.join("trellis").join("admin-session.json");
    assert_eq!(
        std::fs::read(&sentinel).expect("read profile sentinel"),
        b"sentinel-admin-session",
        "complete_session must not write or truncate the default admin session store"
    );
    let _ = std::fs::remove_dir_all(&profile);
}

/// T20: a pre-populated parent profile sentinel is byte-for-byte unchanged after
/// concurrent startup, login, and shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t20_parent_profile_is_untouched() {
    let profile = run_profile_child("profile_child_registers_caller", "t20").await;
    let sentinel = profile.join("trellis").join("admin-session.json");
    assert_eq!(
        std::fs::read(&sentinel).expect("read profile sentinel"),
        b"sentinel-admin-session",
        "the populated profile sentinel must be unchanged"
    );
    let _ = std::fs::remove_dir_all(&profile);
}

/// Started only by the T20/T21 profile tests as a child process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invoked as a child process by the profile tests"]
async fn profile_child_registers_caller() {
    let profile = std::env::var_os("TRELLIS_TEST_PROFILE_HOME").expect("profile home");
    std::env::set_var("HOME", &profile);
    std::env::set_var("XDG_CONFIG_HOME", &profile);
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime in the profile child");
    let identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller in the profile child");
    assert!(!identity.login_session_id().is_empty());
    runtime
        .shutdown()
        .await
        .expect("shutdown runtime in the profile child");
}

/// T02: missing or explicitly invalid Trellis binaries fail before startup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t02_missing_or_invalid_binaries_fail() {
    let result = TrellisTestRuntime::builder()
        .cli_binary("/nonexistent/trellis")
        .server_binary("/nonexistent/trellis-server")
        .start()
        .await;
    let error = match result {
        Ok(_) => panic!("missing binaries must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), TrellisTestErrorKind::InvalidBinary);
}

/// T05: an AgentCaller completes its own participant-bound session and calls the
/// provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t05_agent_caller_calls_the_provider() {
    use trellis_test_fixture::participants::trellis_test_fixture_agent_caller::Client as AgentClient;

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let provider = start_provider(&mut runtime, "provider").await;
    let agent_identity = runtime
        .register_client::<trellis_test_fixture::participants::trellis_test_fixture_agent_caller::Participant>(
            "agent",
        )
        .await
        .expect("register agent caller");

    let agent = AgentClient::connect(agent_identity.connect_options())
        .await
        .expect("connect agent caller");
    let output = agent
        .trellis_test_fixture_echo_v1()
        .echo(&value("from-agent"))
        .await
        .expect("call Echo");
    assert_eq!(output.value, "from-agent");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    provider.task.abort();
    let _ = provider.task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T23: a sandbox parent path containing spaces works end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t23_sandbox_path_with_spaces_works() {
    let parent = std::env::temp_dir().join(format!("trellis test {}", std::process::id()));
    std::fs::create_dir_all(&parent).expect("create spaced parent");
    let mut runtime = TrellisTestRuntime::builder()
        .workdir_parent(&parent)
        .start()
        .await
        .expect("start runtime under a spaced path");
    assert!(runtime.workdir().to_string_lossy().contains(' '));
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T15: dropping a started runtime without explicit shutdown stops the real
/// server and its managed NATS descendant, releasing every exposed listener,
/// while an unrelated live listener survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t15_dropping_a_runtime_cleans_up() {
    // Hold an unrelated loopback listener open for the whole test.
    let unrelated =
        std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind an unrelated listener");
    let unrelated_port = unrelated
        .local_addr()
        .expect("unrelated listener address")
        .port();

    let endpoints = {
        let runtime = TrellisTestRuntime::builder()
            .start()
            .await
            .expect("start runtime");
        [
            runtime.trellis_url().to_owned(),
            runtime.nats_url().to_owned(),
            runtime.websocket_url().to_owned(),
            runtime.monitor_url().to_owned(),
        ]
    };

    for url in &endpoints {
        let port = endpoint_port(url);
        assert_ne!(port, unrelated_port);
        assert!(
            wait_until_no_listener(port, Duration::from_secs(30)).await,
            "dropped runtime endpoint {url} must stop listening"
        );
    }
    // The unrelated listener must remain bound and accept a fresh connection.
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", unrelated_port)).is_ok(),
        "an unrelated live listener must survive runtime cleanup"
    );
    drop(unrelated);
}

/// T11: a missing real NATS executable fails startup cleanly instead of hiding
/// a skip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t11_missing_nats_fails_cleanly() {
    use trellis_test::{NatsSource, TestTimeouts};

    let result = TrellisTestRuntime::builder()
        .nats(NatsSource::Path("/nonexistent/nats-server".into()))
        .timeouts(TestTimeouts {
            startup: Duration::from_secs(45),
            ..TestTimeouts::default()
        })
        .start()
        .await;
    let error = match result {
        Ok(_) => panic!("a missing NATS executable must fail"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error.kind(),
            TrellisTestErrorKind::InvalidBinary
                | TrellisTestErrorKind::MissingBinary
                | TrellisTestErrorKind::ProcessExited
                | TrellisTestErrorKind::Bootstrap
                | TrellisTestErrorKind::Timeout
        ),
        "an explicitly invalid NATS path must fail without falling back, got {:?}",
        error.kind()
    );
}

/// T16: cancelling an in-progress `start` after the server process has actually
/// spawned does not orphan its process group or hold any of its four listeners.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t16_cancelling_start_does_not_orphan_infrastructure() {
    let parent = std::env::temp_dir().join(format!("trellis-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&parent).expect("create the cancel parent");
    let parent_for_task = parent.clone();
    let handle = tokio::spawn(async move {
        TrellisTestRuntime::builder()
            .workdir_parent(parent_for_task)
            .start()
            .await
            .map(|_| ())
    });

    // Observe a real post-spawn condition: the server process exists and its
    // command line names the four selected listeners.
    let mut observed: Option<(u32, [u16; 4])> = None;
    for _ in 0..2400 {
        if let Some(pid) = find_server_pid_under(&parent) {
            if let Some(ports) = server_ports(pid) {
                observed = Some((pid, ports));
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (pid, ports) = observed.expect("expected the server to spawn before cancelling");
    handle.abort();
    let _ = handle.await;

    // The owned process and every one of its listeners must disappear.
    assert!(
        wait_until_process_gone(pid, Duration::from_secs(30)).await,
        "server process {pid} survived cancellation"
    );
    for port in ports {
        assert!(
            wait_until_no_listener(port, Duration::from_secs(30)).await,
            "listener {port} survived cancellation"
        );
    }

    // A later runtime must start cleanly on fresh ports under the same parent.
    let mut runtime = TrellisTestRuntime::builder()
        .workdir_parent(&parent)
        .start()
        .await
        .expect("start after cancelling another start");
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T12: automatic port assignment works across independent processes. Two child
/// test processes each start a runtime while this process also starts one, with
/// no caller-specified ports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t12_automatic_ports_across_processes() {
    let exe = std::env::current_exe().expect("current test executable");
    let mut children = Vec::new();
    for _ in 0..2 {
        let child = std::process::Command::new(&exe)
            .args(["--exact", "t12_child_runtime", "--ignored", "--nocapture"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn child test process");
        children.push(child);
    }

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime in the parent process");
    runtime.shutdown().await.expect("shutdown runtime");

    for mut child in children {
        let status = child.wait().expect("wait for the child test process");
        assert!(status.success(), "a concurrent runtime process failed");
    }
}

/// Started only by `t12_automatic_ports_across_processes` as a child process.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invoked as a child process by t12"]
async fn t12_child_runtime() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime in a child process");
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T17: panic/unwind cleanup works even when the Tokio runtime is dropped. A
/// worker thread panics with a live runtime; the supervising thread observes the
/// real listeners released afterwards.
#[test]
fn t17_panic_unwind_cleanup() {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let handle = std::thread::spawn(move || {
        let tokio_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build the panicking runtime");
        tokio_rt.block_on(async move {
            let runtime = TrellisTestRuntime::builder()
                .start()
                .await
                .expect("start runtime");
            tx.send([
                runtime.trellis_url().to_owned(),
                runtime.nats_url().to_owned(),
                runtime.websocket_url().to_owned(),
                runtime.monitor_url().to_owned(),
            ])
            .expect("send endpoints before unwinding");
            panic!("intentional unwind with a live runtime");
        });
    });
    let endpoints = rx
        .recv_timeout(Duration::from_secs(180))
        .expect("endpoints before the panic");
    assert!(handle.join().is_err(), "the worker thread must unwind");
    std::panic::set_hook(previous_hook);

    let observer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build the observer runtime");
    observer.block_on(async {
        for url in &endpoints {
            let port = endpoint_port(url);
            assert!(
                wait_until_no_listener(port, Duration::from_secs(30)).await,
                "endpoint {url} must be released after a panic unwind"
            );
        }
    });
}

/// T18: force-stopping the real server returns a meaningful failure and cleanup
/// also covers its managed NATS descendant. Linux-only: the observer locates the
/// server through its own process command line, with no harness-side hook.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t18_force_stopped_server_cleans_up_nats() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let sandbox = runtime.workdir().to_path_buf();
    let nats_port = endpoint_port(runtime.nats_url());
    let pid = find_server_pid(&sandbox).expect("locate the running server process");
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("force-stop the server");
    assert!(status.success(), "the force-stop signal must be delivered");

    let error = runtime
        .install_participant::<ProviderParticipant>()
        .await
        .expect_err("a force-stopped server must produce a failure");
    assert!(
        matches!(
            error.kind(),
            TrellisTestErrorKind::AdminRpc
                | TrellisTestErrorKind::Io
                | TrellisTestErrorKind::ProcessExited
                | TrellisTestErrorKind::Timeout
                | TrellisTestErrorKind::Authentication
        ),
        "expected a transport/administration failure, got {:?}",
        error.kind()
    );

    let _ = runtime.shutdown().await;
    assert!(
        wait_until_no_listener(nats_port, Duration::from_secs(30)).await,
        "cleanup must also stop the server's managed NATS descendant"
    );
}

/// Locates the test runtime's server process from its own command line.
#[cfg(target_os = "linux")]
fn find_server_pid(sandbox: &std::path::Path) -> Option<u32> {
    let needle = sandbox.to_string_lossy().into_owned();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if cmdline.contains("trellis-server") && cmdline.contains(&needle) {
            return Some(pid);
        }
    }
    None
}

/// Locates any server process whose command line names a sandbox under `parent`.
#[cfg(target_os = "linux")]
fn find_server_pid_under(parent: &std::path::Path) -> Option<u32> {
    let needle = parent.to_string_lossy().into_owned();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if cmdline.contains("trellis-server") && cmdline.contains(&needle) {
            return Some(pid);
        }
    }
    None
}

/// Reads the server's four selected listener ports from its command line and config.
#[cfg(target_os = "linux")]
fn server_ports(pid: u32) -> Option<[u16; 4]> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let cmdline = String::from_utf8_lossy(&raw).into_owned();
    let args: Vec<&str> = cmdline.split('\0').collect();
    let nats_ports = args
        .iter()
        .find_map(|arg| arg.strip_prefix("--local-nats-ports="))?;
    let mut nats = nats_ports.split(',');
    let nats_port = nats.next()?.parse().ok()?;
    let monitor_port = nats.next()?.parse().ok()?;
    let websocket_port = nats.next()?.parse().ok()?;
    let config = args
        .iter()
        .position(|arg| *arg == "--config")
        .and_then(|index| args.get(index + 1))?;
    let http_port = http_port_from_config(std::path::Path::new(config))?;
    Some([http_port, nats_port, monitor_port, websocket_port])
}

#[cfg(target_os = "linux")]
fn http_port_from_config(config: &std::path::Path) -> Option<u16> {
    let text = std::fs::read_to_string(config).ok()?;
    let mut in_http = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_http = line == "[http]";
            continue;
        }
        if in_http {
            if let Some(value) = line.strip_prefix("port") {
                let value = value.trim().trim_start_matches('=').trim();
                if let Ok(port) = value.parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// True once the process is gone or has become an empty-cmdline zombie.
#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|cmdline| cmdline.is_empty())
        .unwrap_or(true)
}

#[cfg(target_os = "linux")]
async fn wait_until_process_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if process_gone(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// T19: all three retention policies and a surviving preexisting sibling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t19_retention_policies_and_sibling_survival() {
    use trellis_test::WorkdirRetention;

    let parent = std::env::temp_dir().join(format!("trellis-retention-{}", std::process::id()));
    std::fs::create_dir_all(&parent).expect("create the retention parent");
    let sibling = parent.join("preexisting-sibling");
    std::fs::create_dir_all(&sibling).expect("create the preexisting sibling");
    std::fs::write(sibling.join("keep.txt"), b"keep").expect("write the sibling file");

    let kept = {
        let mut runtime = TrellisTestRuntime::builder()
            .workdir_parent(&parent)
            .retention(WorkdirRetention::Always)
            .start()
            .await
            .expect("start the Always runtime");
        let dir = runtime.workdir().to_path_buf();
        runtime
            .shutdown()
            .await
            .expect("shutdown the Always runtime");
        dir
    };
    assert!(
        kept.is_dir(),
        "Always must retain the sandbox after shutdown"
    );

    let removed = {
        let mut runtime = TrellisTestRuntime::builder()
            .workdir_parent(&parent)
            .retention(WorkdirRetention::OnFailure)
            .start()
            .await
            .expect("start the OnFailure runtime");
        let dir = runtime.workdir().to_path_buf();
        runtime
            .shutdown()
            .await
            .expect("shutdown the OnFailure runtime");
        dir
    };
    assert!(!removed.exists(), "OnFailure must remove a clean sandbox");

    assert!(
        sibling.join("keep.txt").is_file(),
        "a preexisting sibling must survive every cleanup"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

/// Started only by the TypeScript mixed-runtime test as a child process.
///
/// Starts a Rust runtime, publishes its endpoint URLs, waits for a release
/// file, then shuts down so the TypeScript side can prove overlap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invoked as a child process by the TypeScript mixed-runtime test"]
async fn runtime_endpoints_child() {
    let endpoints_file =
        std::env::var("TRELLIS_TEST_CHILD_ENDPOINTS_FILE").expect("endpoints file");
    let release_file = std::env::var("TRELLIS_TEST_CHILD_RELEASE_FILE").expect("release file");
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start child runtime");
    std::fs::write(
        &endpoints_file,
        format!(
            "{}\n{}\n{}\n{}\n",
            runtime.trellis_url(),
            runtime.nats_url(),
            runtime.websocket_url(),
            runtime.monitor_url(),
        ),
    )
    .expect("write endpoints");
    while !std::path::Path::new(&release_file).exists() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    runtime.shutdown().await.expect("shutdown child runtime");
}
