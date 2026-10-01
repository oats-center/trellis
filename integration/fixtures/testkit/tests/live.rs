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
use trellis_test_fixture::participants::trellis_test_fixture_caller::{
    Client as CallerClient, Participant as CallerParticipant,
};
use trellis_test_fixture::participants::trellis_test_fixture_provider::{
    Participant as ProviderParticipant, Provider,
};
use trellis_test_fixture::participants::trellis_test_fixture_restricted_caller::Participant as RestrictedCallerParticipant;
use trellis_test_fixture::participants::trellis_test_fixture_unsupported_device::Participant as UnsupportedDeviceParticipant;
use trellis_test_fixture::types::Value;
use trellis_testkit::{TrellisTestErrorKind, TrellisTestRuntime};

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
        extra: Default::default(),
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
    use trellis_testkit::{NatsSource, TestTimeouts};

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
    use trellis_testkit::WorkdirRetention;

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

/// A caller-selected invocation id replays one execution and opens the same
/// durable operation through generated control; unavailable optional calls deny.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operation_chosen_invocation_id_replays_and_preserves_availability() {
    use trellis_rs::client::CallError;
    use trellis_rs::generated::OptionalAction;

    const OPTIONAL_ACTIONS: &[OptionalAction] = &[OptionalAction::operation(
        trellis_test_fixture::apis::trellis_test_fixture_echo_v1::API_ID,
        "Silent",
    )];

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider");
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .expect("connect provider");
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    Provider::new(&mut service)
        .trellis_test_fixture_echo_v1()
        .register_silent(move |_context, input, operation| {
            let calls = Arc::clone(&handler_calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                operation.complete(input).await?;
                Ok(())
            }
        });
    let task = tokio::spawn(async move { service.run().await });
    let identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");
    let caller = CallerClient::connect(identity.connect_options())
        .await
        .expect("connect caller");
    let api = caller.trellis_test_fixture_echo_v1();
    let invocation_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let input = value("chosen invocation");
    let operation = api
        .silent()
        .start_with_invocation_id(invocation_id, &input)
        .await
        .expect("start Silent with chosen id");
    assert_eq!(operation.id(), invocation_id);
    let completed = tokio::time::timeout(Duration::from_secs(20), operation.wait())
        .await
        .expect("Silent completes before timeout")
        .expect("wait for Silent");
    assert_eq!(completed.output.expect("Silent output").value, input.value);
    let replay = api
        .silent()
        .start_with_invocation_id(invocation_id, &input)
        .await
        .expect("replay Silent");
    assert_eq!(replay.id(), invocation_id);
    assert_eq!(
        replay
            .get()
            .await
            .expect("read replay")
            .output
            .expect("replayed output")
            .value,
        input.value
    );
    assert_eq!(
        api.silent()
            .control(invocation_id)
            .expect("open chosen id")
            .get()
            .await
            .expect("read recovered operation")
            .output
            .expect("recovered output")
            .value,
        input.value
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let identity = runtime
        .register_client::<RestrictedCallerParticipant>("restricted")
        .await
        .expect("register restricted caller");
    let restricted = trellis_rs::generated::Client::connect_user(identity.connect_options())
        .await
        .expect("connect restricted caller")
        .with_optional_actions(OPTIONAL_ACTIONS);
    let restricted =
        trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client::from_generated(
            restricted,
        );
    assert!(matches!(
        restricted.silent().start(&input).await,
        Err(CallError::AuthorizationUnavailable(_))
    ));
    assert!(matches!(
        restricted
            .silent()
            .start_with_invocation_id(invocation_id, &input)
            .await,
        Err(CallError::AuthorizationUnavailable(_))
    ));
    assert!(matches!(
        restricted
            .silent()
            .start_cancelled_with_invocation_id(invocation_id, &input)
            .await,
        Err(CallError::AuthorizationUnavailable(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    task.abort();
    let _ = task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// Cancellation-first admission freezes input/creator and cannot be undone by
/// normal replay, concurrent admission, or a recovered cleanup execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_admission_is_atomic_and_replayable() {
    use trellis_rs::client::OperationState;
    use trellis_rs::generated::{Client as GeneratedClient, ParticipantDescriptor};
    use trellis_rs::service::OperationCancellationReason;
    use trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client as EchoClient;
    use trellis_test_fixture::participants::trellis_test_fixture_agent_caller::{
        Client as AgentClient, Participant as AgentParticipant,
    };

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider");
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .expect("connect provider");
    let business = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let retry_failures = Arc::new(AtomicUsize::new(0));
    let cleanup = Arc::new(tokio::sync::Semaphore::new(0));
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    {
        let mut provider = Provider::new(&mut service);
        provider
            .trellis_test_fixture_echo_v1()
            .register_echo(|context, _input| async move {
                let caller = context
                    .request()
                    .caller
                    .as_ref()
                    .expect("authenticated caller");
                Ok(value(&format!(
                    "{}|{}",
                    caller.principal_id, caller.participant_id
                )))
            });
        provider.trellis_test_fixture_echo_v1().register_silent({
            let business = Arc::clone(&business);
            let executions = Arc::clone(&executions);
            let retry_failures = Arc::clone(&retry_failures);
            let cleanup = Arc::clone(&cleanup);
            move |context, input, operation| {
                let business = Arc::clone(&business);
                let executions = Arc::clone(&executions);
                let retry_failures = Arc::clone(&retry_failures);
                let cleanup = Arc::clone(&cleanup);
                let entered = entered.clone();
                async move {
                    executions.fetch_add(1, Ordering::SeqCst);
                    let cancellation = operation.cancellation();
                    let initial_reason = cancellation.reason();
                    if initial_reason.is_none() {
                        if operation.started().await.is_ok() {
                            business.fetch_add(1, Ordering::SeqCst);
                        } else {
                            // Cancellation can win between admission and the
                            // fenced started write. Join cleanup, not new work.
                            assert_eq!(
                                cancellation.cancelled().await,
                                OperationCancellationReason::Requested
                            );
                        }
                    }
                    entered
                        .send((input.value.clone(), initial_reason, context.resuming))
                        .unwrap();
                    if input.value == "complete" {
                        operation.complete(input).await?;
                        return Ok(());
                    }
                    assert_eq!(
                        cancellation.cancelled().await,
                        OperationCancellationReason::Requested
                    );
                    if input.value == "retry" && retry_failures.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        return Err(ServerError::Nats("cleanup needs recovery".to_owned()));
                    }
                    cleanup.acquire().await.unwrap().forget();
                    Ok(())
                }
            }
        });
    }
    let task = tokio::spawn(async move { service.run().await });
    let identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");
    let caller = CallerClient::connect(identity.connect_options())
        .await
        .expect("connect caller");
    let identity = runtime
        .register_client::<CallerParticipant>("another-name")
        .await
        .expect("register same principal");
    let same_creator = CallerClient::connect(identity.connect_options())
        .await
        .expect("connect same creator");
    let identity = runtime
        .register_client::<AgentParticipant>("agent")
        .await
        .expect("register different participant");
    let agent = AgentClient::connect(identity.connect_options())
        .await
        .expect("connect agent");
    let api = caller.trellis_test_fixture_echo_v1();
    let who = api
        .echo(&value("identity"))
        .await
        .expect("read caller identity")
        .value;
    let same_who = same_creator
        .trellis_test_fixture_echo_v1()
        .echo(&value("identity"))
        .await
        .expect("read same caller identity")
        .value;
    let agent_who = agent
        .trellis_test_fixture_echo_v1()
        .echo(&value("identity"))
        .await
        .expect("read agent identity")
        .value;
    assert_eq!(who, same_who, "connection names are not creator identities");
    assert_eq!(
        who.split_once('|').unwrap().0,
        agent_who.split_once('|').unwrap().0
    );
    assert_eq!(who.split_once('|').unwrap().1, CallerParticipant::ID);
    assert_eq!(agent_who.split_once('|').unwrap().1, AgentParticipant::ID);

    let id = "01J00000000000000000000010";
    let input = value("cancel-first");
    let operation = api
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .expect("cancel unknown reserved id");
    let entry = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .expect("cleanup entered")
        .unwrap();
    assert_eq!(
        entry,
        (
            input.value.clone(),
            Some(OperationCancellationReason::Requested),
            false
        )
    );
    assert_eq!(operation.id(), id);
    assert_eq!(business.load(Ordering::SeqCst), 0);
    let same_api = same_creator.trellis_test_fixture_echo_v1();
    let replay = same_api
        .silent()
        .start_with_invocation_id(id, &input)
        .await
        .expect("late normal start replays");
    assert_eq!(replay.id(), id);
    let pending = operation.get().await.expect("pending cleanup");
    assert_eq!(pending.state, OperationState::Pending);
    let cancelled_retry = api
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .expect("nonterminal cancelled admission replay");
    assert_eq!(cancelled_retry.id(), id);
    assert_eq!(
        cancelled_retry.get().await.unwrap().revision,
        pending.revision
    );
    assert!(api
        .silent()
        .start_cancelled_with_invocation_id(id, &value("changed"))
        .await
        .is_err());
    assert!(agent
        .trellis_test_fixture_echo_v1()
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .is_err());
    assert_eq!(
        operation.get().await.expect("unchanged record").revision,
        pending.revision
    );
    cleanup.add_permits(1);
    let terminal = tokio::time::timeout(Duration::from_secs(20), operation.wait())
        .await
        .expect("cleanup finishes")
        .expect("Cancelled result");
    assert_eq!(terminal.state, OperationState::Cancelled);
    let retry = api
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .expect("durable cancelled replay");
    assert_eq!(retry.get().await.unwrap().revision, terminal.revision);
    let normal = api
        .silent()
        .start_with_invocation_id(id, &input)
        .await
        .expect("terminal normal replay");
    assert_eq!(normal.get().await.unwrap().state, OperationState::Cancelled);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(business.load(Ordering::SeqCst), 0);

    // Invoke/Cancel authority is absent for this actual installed participant.
    let identity = runtime
        .register_client::<RestrictedCallerParticipant>("restricted")
        .await
        .expect("register restricted");
    let restricted = GeneratedClient::connect_user(identity.connect_options())
        .await
        .expect("connect restricted");
    let restricted_api = EchoClient::from_generated(restricted);
    let denied_id = "01J00000000000000000000011";
    assert!(restricted_api
        .silent()
        .start_cancelled_with_invocation_id(denied_id, &value("denied"))
        .await
        .is_err());
    assert!(api
        .silent()
        .control(denied_id)
        .unwrap()
        .get()
        .await
        .is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 1);

    // Reusing the denied ID as an authorized, different participant proves that
    // denial did not reserve it (a persisted restricted creator would conflict).
    let after_denial = api
        .silent()
        .start_cancelled_with_invocation_id(denied_id, &value("denied"))
        .await
        .expect("denial left invocation absent");
    let entry = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        entry,
        (
            "denied".to_owned(),
            Some(OperationCancellationReason::Requested),
            false
        )
    );
    cleanup.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), after_denial.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::Cancelled
    );
    assert_eq!(executions.load(Ordering::SeqCst), 2);

    // Normal admission wins deterministically, then cancelled admission joins it.
    let id = "01J00000000000000000000012";
    let input = value("normal-first");
    let normal = api
        .silent()
        .start_with_invocation_id(id, &input)
        .await
        .expect("normal admission");
    let entry = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .expect("business entered")
        .unwrap();
    assert_eq!(entry, (input.value.clone(), None, false));
    let cancelled = api
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .expect("cancel admitted operation");
    assert_eq!(normal.id(), cancelled.id());
    cleanup.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), cancelled.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::Cancelled
    );
    assert_eq!(business.load(Ordering::SeqCst), 1);

    // Concurrent creates must converge to one cleanup and one durable identity.
    let id = "01J00000000000000000000013";
    let input = value("race");
    let silent = api.silent();
    let (normal, cancelled) = tokio::join!(
        silent.start_with_invocation_id(id, &input),
        silent.start_cancelled_with_invocation_id(id, &input)
    );
    let normal = normal.expect("racing normal admission");
    let cancelled = cancelled.expect("racing cancelled admission");
    assert_eq!(normal.id(), cancelled.id());
    let entry = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .expect("one racing handler")
        .unwrap();
    assert_eq!(entry.0, "race");
    assert!(!entry.2);
    cleanup.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), cancelled.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::Cancelled
    );
    assert_eq!(executions.load(Ordering::SeqCst), 4);
    assert!(business.load(Ordering::SeqCst) <= 2);

    // Completed terminals replay unchanged, and ordinary uncancelled work works.
    let id = "01J00000000000000000000014";
    let input = value("complete");
    let completed = api
        .silent()
        .start_with_invocation_id(id, &input)
        .await
        .expect("ordinary start");
    let terminal = tokio::time::timeout(Duration::from_secs(20), completed.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.state, OperationState::Completed);
    let replay = api
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .expect("completed replay");
    assert_eq!(replay.get().await.unwrap().revision, terminal.revision);
    assert_eq!(
        replay.get().await.unwrap().output.unwrap().value,
        input.value
    );
    entries.recv().await.expect("completed handler entry");

    // Failed cancellation-first cleanup recovers with Requested still seeded.
    let id = "01J00000000000000000000015";
    let input = value("retry");
    let retry = api
        .silent()
        .start_cancelled_with_invocation_id(id, &input)
        .await
        .expect("cancel-first with failed cleanup");
    let entry = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        entry,
        (
            input.value.clone(),
            Some(OperationCancellationReason::Requested),
            false
        )
    );
    let before = business.load(Ordering::SeqCst);
    let entry = tokio::time::timeout(Duration::from_secs(55), entries.recv())
        .await
        .expect("ordinary owner recovery")
        .unwrap();
    assert_eq!(
        entry,
        (
            input.value.clone(),
            Some(OperationCancellationReason::Requested),
            true
        )
    );
    assert_eq!(retry.get().await.unwrap().state, OperationState::Pending);
    assert_eq!(business.load(Ordering::SeqCst), before);
    cleanup.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), retry.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        OperationState::Cancelled
    );
    assert_eq!(executions.load(Ordering::SeqCst), 7);
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// Failed cancellation cleanup remains nonterminal; recovery enters cleanup
/// already cancelled and retries without repeating business work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_cleanup_failure_recovers_through_generated_client() {
    use trellis_rs::client::OperationState;
    use trellis_rs::service::OperationCancellationReason;

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider");
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .expect("connect provider");
    let attempts = Arc::new(AtomicUsize::new(0));
    let business_calls = Arc::new(AtomicUsize::new(0));
    let release_cleanup = Arc::new(tokio::sync::Notify::new());
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let (failed, mut failures) = tokio::sync::mpsc::unbounded_channel();
    Provider::new(&mut service)
        .trellis_test_fixture_echo_v1()
        .register_silent({
            let attempts = Arc::clone(&attempts);
            let business_calls = Arc::clone(&business_calls);
            let release_cleanup = Arc::clone(&release_cleanup);
            move |context, _input, operation| {
                let attempts = Arc::clone(&attempts);
                let business_calls = Arc::clone(&business_calls);
                let release_cleanup = Arc::clone(&release_cleanup);
                let entered = entered.clone();
                let failed = failed.clone();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    let cancellation = operation.cancellation();
                    let initial_reason = cancellation.reason();
                    if initial_reason.is_none() {
                        business_calls.fetch_add(1, Ordering::SeqCst);
                        operation.started().await?;
                    }
                    entered
                        .send((attempt, context.resuming, initial_reason))
                        .unwrap();
                    assert_eq!(
                        cancellation.cancelled().await,
                        OperationCancellationReason::Requested
                    );
                    if attempt < 2 {
                        failed.send(attempt).unwrap();
                        return Err(ServerError::Nats("owned child cleanup failed".to_owned()));
                    }
                    release_cleanup.notified().await;
                    Ok(())
                }
            }
        });
    let task = tokio::spawn(async move { service.run().await });
    let identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");
    let caller = CallerClient::connect(identity.connect_options())
        .await
        .expect("connect caller");
    let api = caller.trellis_test_fixture_echo_v1();
    let operation = api
        .silent()
        .start(&value("cleanup"))
        .await
        .expect("start Silent");
    let entry = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .expect("initial handler enters")
        .expect("initial entry");
    assert_eq!(entry, (0, false, None));
    let cancel = operation.cancel();
    tokio::pin!(cancel);
    tokio::select! {
        failure = failures.recv() => assert_eq!(failure, Some(0)),
        result = &mut cancel => panic!("cancel completed before cleanup failed: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(20)) => panic!("handler did not observe Requested"),
    }
    // Each failed execution must remain recoverable through ordinary lease expiry.
    // A bounded entry wait, not a sleep, synchronizes with each recovered handler.
    for attempt in 1..=2 {
        let entry = tokio::select! {
            entry = entries.recv() => entry.expect("recovered entry"),
            result = &mut cancel => panic!("cancel completed after failed cleanup: {result:?}"),
            _ = tokio::time::sleep(Duration::from_secs(55)) => panic!("cleanup was not recovered"),
        };
        assert_eq!(
            entry,
            (attempt, true, Some(OperationCancellationReason::Requested))
        );
        let snapshot = operation.get().await.expect("read pending cleanup");
        assert_eq!(snapshot.state, OperationState::Running);
        assert!(snapshot.output.is_none());
    }
    assert_eq!(failures.recv().await, Some(1));
    assert_eq!(business_calls.load(Ordering::SeqCst), 1);
    release_cleanup.notify_one();
    let terminal = tokio::time::timeout(Duration::from_secs(20), &mut cancel)
        .await
        .expect("successful cleanup finalizes")
        .expect("cancel succeeds");
    assert_eq!(terminal.state, OperationState::Cancelled);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    let durable = operation.get().await.expect("read durable cancellation");
    assert_eq!(durable.state, OperationState::Cancelled);
    assert_eq!(durable.revision, terminal.revision);
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// Cancellation-first uploads recover cleanup without requiring staged bytes
/// after the original provider runtime has stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_upload_recovers_after_provider_replacement() {
    use trellis_rs::client::{CallError, OperationState, TrellisClientError};
    use trellis_rs::service::{OperationCancellationReason, ServiceConnectOptions};

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider once");
    let business_calls = Arc::new(AtomicUsize::new(0));
    let upload_calls = Arc::new(AtomicUsize::new(0));
    let cleanup_attempts = Arc::new(AtomicUsize::new(0));
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();

    // Dropping this provider's own runtime stops its spawned recovery and
    // execution tasks too, rather than leaving a survivor in the caller runtime.
    let first_provider = {
        let url = identity.trellis_url().to_owned();
        let seed = identity.seed().to_owned();
        let name = identity.name().to_owned();
        let entered = entered.clone();
        let business_calls = Arc::clone(&business_calls);
        let upload_calls = Arc::clone(&upload_calls);
        let cleanup_attempts = Arc::clone(&cleanup_attempts);
        std::thread::spawn(move || {
            let provider_runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("create first provider runtime");
            provider_runtime.block_on(async move {
                let mut service = ProviderParticipant::connect(
                    ServiceConnectOptions::new(&url, &seed).with_name(&name),
                )
                .await
                .expect("connect first provider");
                Provider::new(&mut service)
                    .trellis_test_fixture_echo_v1()
                    .register_echo(|_context, input| async move { Ok(input) });
                Provider::new(&mut service)
                    .trellis_test_fixture_echo_v1()
                    .register_upload(move |context, input, operation| {
                        let entered = entered.clone();
                        let business_calls = Arc::clone(&business_calls);
                        let upload_calls = Arc::clone(&upload_calls);
                        let cleanup_attempts = Arc::clone(&cleanup_attempts);
                        async move {
                            let reason = operation.cancellation().reason();
                            if reason.is_none() {
                                business_calls.fetch_add(1, Ordering::SeqCst);
                                operation.started().await?;
                            }
                            let upload = operation.upload().await?;
                            if upload.is_some() {
                                upload_calls.fetch_add(1, Ordering::SeqCst);
                            }
                            cleanup_attempts.fetch_add(1, Ordering::SeqCst);
                            entered
                                .send((context.resuming, reason, input.value, upload.is_none()))
                                .unwrap();
                            Err(ServerError::Nats("upload cleanup failed".to_owned()))
                        }
                    });
                let mut task = tokio::spawn(async move { service.run().await });
                tokio::select! {
                    result = &mut task => panic!("first provider stopped unexpectedly: {result:?}"),
                    _ = stopped => {}
                }
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            });
        })
    };

    let caller_identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");
    let caller = CallerClient::connect(caller_identity.connect_options())
        .await
        .expect("connect caller");
    let api = caller.trellis_test_fixture_echo_v1();
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut readiness = tokio::time::interval(Duration::from_millis(100));
        loop {
            readiness.tick().await;
            match api.echo(&value("provider-ready")).await {
                Ok(reply) => {
                    assert_eq!(reply.value, "provider-ready");
                    break;
                }
                Err(CallError::Transport(error)) if error.to_string().contains("no responders") => {
                }
                Err(error) => panic!("provider readiness RPC failed: {error:?}"),
            }
        }
    })
    .await
    .expect("first provider serves its generated RPC");
    let input = value("cancelled-upload");
    let operation = api
        .upload()
        .start_cancelled_with_invocation_id("01J00000000000000000000016", &input)
        .await
        .expect("admit cancellation-first upload");
    let first = tokio::time::timeout(Duration::from_secs(20), entries.recv())
        .await
        .expect("first cleanup enters")
        .unwrap();
    assert_eq!(
        first,
        (
            false,
            Some(OperationCancellationReason::Requested),
            input.value.clone(),
            true,
        )
    );
    assert!(matches!(
        operation.upload(b"must not upload").await,
        Err(TrellisClientError::OperationProtocol(message))
            if message == "operation does not have an accepted transfer session"
    ));
    stop.send(()).expect("stop first provider");
    tokio::task::spawn_blocking(move || first_provider.join().unwrap())
        .await
        .expect("join stopped first provider runtime");
    assert_eq!(cleanup_attempts.load(Ordering::SeqCst), 1);

    // Reconnect the same registration, not a fresh deployment or resource set.
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .expect("connect replacement provider using original identity");
    let release_cleanup = Arc::new(tokio::sync::Semaphore::new(0));
    Provider::new(&mut service)
        .trellis_test_fixture_echo_v1()
        .register_upload({
            let business_calls = Arc::clone(&business_calls);
            let upload_calls = Arc::clone(&upload_calls);
            let cleanup_attempts = Arc::clone(&cleanup_attempts);
            let release_cleanup = Arc::clone(&release_cleanup);
            move |context, input, operation| {
                let entered = entered.clone();
                let business_calls = Arc::clone(&business_calls);
                let upload_calls = Arc::clone(&upload_calls);
                let cleanup_attempts = Arc::clone(&cleanup_attempts);
                let release_cleanup = Arc::clone(&release_cleanup);
                async move {
                    let reason = operation.cancellation().reason();
                    if reason.is_none() {
                        business_calls.fetch_add(1, Ordering::SeqCst);
                        operation.started().await?;
                    }
                    let upload = operation.upload().await?;
                    if upload.is_some() {
                        upload_calls.fetch_add(1, Ordering::SeqCst);
                    }
                    cleanup_attempts.fetch_add(1, Ordering::SeqCst);
                    entered
                        .send((context.resuming, reason, input.value, upload.is_none()))
                        .unwrap();
                    release_cleanup.acquire().await.unwrap().forget();
                    Ok(())
                }
            }
        });
    let task = tokio::spawn(async move { service.run().await });
    let recovered = tokio::time::timeout(Duration::from_secs(55), entries.recv())
        .await
        .expect("replacement automatically recovers cleanup after ordinary lease expiry")
        .unwrap();
    assert_eq!(
        recovered,
        (
            true,
            Some(OperationCancellationReason::Requested),
            input.value,
            true,
        )
    );
    assert_eq!(
        operation.get().await.unwrap().state,
        OperationState::Pending
    );
    release_cleanup.add_permits(1);
    let terminal = tokio::time::timeout(Duration::from_secs(20), operation.wait())
        .await
        .expect("replacement cleanup finishes")
        .expect("original caller observes terminal cancellation");
    assert_eq!(terminal.state, OperationState::Cancelled);
    assert_eq!(cleanup_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(business_calls.load(Ordering::SeqCst), 0);
    assert_eq!(upload_calls.load(Ordering::SeqCst), 0);
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}

/// T11: generated distinct `progress`/`update` schemas deliver a live preview
/// while the durable snapshot keeps only the declared progress status.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t20_operation_live_update_uses_declared_update_schema() {
    use trellis_rs::client::OperationEvent;
    use trellis_test_fixture::apis::trellis_test_fixture_echo_v1::operations::{
        UpdateOnlyContinueSignal, WorkContinueSignal,
    };
    use trellis_test_fixture::types::{Preview, Status};

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider");
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .expect("connect provider");
    {
        let mut provider = Provider::new(&mut service);
        provider.trellis_test_fixture_echo_v1().register_work(
            |_context, input, operation| async move {
                operation
                    .progress(Status {
                        stage: "transcribing".to_owned(),
                        extra: Default::default(),
                    })
                    .await?;
                let mut signals = operation.signals().await?;
                let release_preview = signals
                    .next()
                    .await
                    .transpose()?
                    .ok_or_else(|| ServerError::Nats("signal stream ended".to_owned()))?;
                operation
                    .emit_update(Preview {
                        text: "partial preview".to_owned(),
                        extra: Default::default(),
                    })
                    .await?;
                operation
                    .acknowledge_signal(release_preview.signal_sequence)
                    .await?;
                // Stay nonterminal until the observer has read the preview.
                let release_completion = signals
                    .next()
                    .await
                    .transpose()?
                    .ok_or_else(|| ServerError::Nats("signal stream ended".to_owned()))?;
                operation
                    .acknowledge_signal(release_completion.signal_sequence)
                    .await?;
                operation.complete(input).await?;
                Ok(())
            },
        );
        provider
            .trellis_test_fixture_echo_v1()
            .register_update_only(|_context, input, operation| async move {
                let mut signals = operation.signals().await?;
                let release_preview = signals
                    .next()
                    .await
                    .transpose()?
                    .ok_or_else(|| ServerError::Nats("signal stream ended".to_owned()))?;
                operation
                    .emit_update(Preview {
                        text: "update-only preview".to_owned(),
                        extra: Default::default(),
                    })
                    .await?;
                operation
                    .acknowledge_signal(release_preview.signal_sequence)
                    .await?;
                let release_completion = signals
                    .next()
                    .await
                    .transpose()?
                    .ok_or_else(|| ServerError::Nats("signal stream ended".to_owned()))?;
                operation
                    .acknowledge_signal(release_completion.signal_sequence)
                    .await?;
                operation.complete(input).await?;
                Ok(())
            });
        provider.trellis_test_fixture_echo_v1().register_silent(
            |_context, input, operation| async move {
                operation.complete(input).await?;
                Ok(())
            },
        );
    }
    let task = tokio::spawn(async move { service.run().await });

    let caller_identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");
    let caller = CallerClient::connect(caller_identity.connect_options())
        .await
        .expect("connect caller");
    let api = caller.trellis_test_fixture_echo_v1();

    // Work: the preview is live-only; durable progress keeps the status schema.
    let operation = api.work().start(&value("go")).await.expect("start Work");
    let mut events = operation
        .live_with_updates()
        .await
        .expect("watch Work updates");
    // Consume the initial durable snapshot before releasing the preview.
    let _ = tokio::time::timeout(Duration::from_secs(20), events.next())
        .await
        .expect("Work initial snapshot before timeout")
        .expect("Work stream yields an item")
        .expect("Work initial snapshot decodes");
    operation
        .signal::<WorkContinueSignal>(&value("preview"))
        .await
        .expect("release Work preview");
    let mut previews = Vec::new();
    let terminal = loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.next())
            .await
            .expect("Work event before timeout")
            .expect("Work stream yields an item")
            .expect("Work event decodes");
        match event {
            OperationEvent::Update { update } => {
                previews.push(update.update.text);
                operation
                    .signal::<WorkContinueSignal>(&value("complete"))
                    .await
                    .expect("release Work completion");
            }
            terminal @ (OperationEvent::Completed { .. }
            | OperationEvent::Failed { .. }
            | OperationEvent::Cancelled { .. }) => break terminal,
            _ => {}
        }
    };
    assert_eq!(previews, vec!["partial preview".to_owned()]);
    let OperationEvent::Completed { snapshot } = terminal else {
        panic!("Work must complete: {terminal:?}");
    };
    assert_eq!(
        snapshot.output.as_ref().map(|output| output.value.as_str()),
        Some("go")
    );
    assert_eq!(
        snapshot
            .progress
            .as_ref()
            .map(|progress| progress.stage.as_str()),
        Some("transcribing")
    );
    let durable = operation.get().await.expect("durable Work snapshot");
    assert_eq!(
        durable
            .progress
            .as_ref()
            .map(|progress| progress.stage.as_str()),
        Some("transcribing"),
        "the durable snapshot never carries the transient preview"
    );

    // UpdateOnly: an explicit update schema with no durable progress schema.
    let only = api
        .update_only()
        .start(&value("only"))
        .await
        .expect("start UpdateOnly");
    let mut events = only
        .live_with_updates()
        .await
        .expect("watch UpdateOnly updates");
    let _ = tokio::time::timeout(Duration::from_secs(20), events.next())
        .await
        .expect("UpdateOnly initial snapshot before timeout")
        .expect("UpdateOnly stream yields an item")
        .expect("UpdateOnly initial snapshot decodes");
    only.signal::<UpdateOnlyContinueSignal>(&value("preview"))
        .await
        .expect("release UpdateOnly preview");
    let mut only_previews = Vec::new();
    let only_snapshot = loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.next())
            .await
            .expect("UpdateOnly event before timeout")
            .expect("UpdateOnly stream yields an item")
            .expect("UpdateOnly event decodes");
        match event {
            OperationEvent::Update { update } => {
                only_previews.push(update.update.text);
                only.signal::<UpdateOnlyContinueSignal>(&value("complete"))
                    .await
                    .expect("release UpdateOnly completion");
            }
            OperationEvent::Completed { snapshot } => break snapshot,
            OperationEvent::Failed { snapshot } | OperationEvent::Cancelled { snapshot } => {
                panic!("UpdateOnly terminated: {snapshot:?}")
            }
            _ => {}
        }
    };
    assert_eq!(only_previews, vec!["update-only preview".to_owned()]);
    assert!(only_snapshot.progress.is_none());
    assert_eq!(
        only_snapshot
            .output
            .as_ref()
            .map(|output| output.value.as_str()),
        Some("only")
    );

    // Silent: neither schema, durable terminal result only.
    let silent = api
        .silent()
        .start(&value("silent"))
        .await
        .expect("start Silent");
    let silent_snapshot = silent.wait().await.expect("wait Silent");
    assert!(silent_snapshot.progress.is_none());
    assert_eq!(
        silent_snapshot
            .output
            .as_ref()
            .map(|output| output.value.as_str()),
        Some("silent")
    );

    drop(api);
    drop(caller);
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.expect("shutdown runtime");
}
