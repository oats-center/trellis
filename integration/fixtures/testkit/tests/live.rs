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
    let api = trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client::from_generated(client);
    let result = api.echo(&value("denied")).await;
    assert!(result.is_err(), "restricted caller must be denied");
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        0,
        "the provider handler must not run for a denied call"
    );

    provider.task.abort();
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
    assert_eq!(error.kind(), TrellisTestErrorKind::UnsupportedParticipantKind);
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

/// T14: shutdown is idempotent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t14_shutdown_is_idempotent() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    runtime.shutdown().await.expect("first shutdown");
    runtime.shutdown().await.expect("second shutdown");
    assert!(runtime.install_participant::<ProviderParticipant>().await.is_err());
}

/// T07: eight independent runtimes start concurrently in one test process with
/// no caller-specified ports and keep distinct loopback endpoints.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn t07_eight_concurrent_runtimes_are_isolated() {
    let mut handles = Vec::new();
    for _ in 0..8 {
        handles.push(tokio::spawn(async {
            let mut runtime = TrellisTestRuntime::builder()
                .start()
                .await
                .expect("start runtime");
            let endpoints = (
                runtime.trellis_url().to_owned(),
                runtime.nats_url().to_owned(),
                runtime.websocket_url().to_owned(),
            );
            runtime.shutdown().await.expect("shutdown runtime");
            endpoints
        }));
    }
    let mut http = std::collections::HashSet::new();
    let mut nats = std::collections::HashSet::new();
    let mut websocket = std::collections::HashSet::new();
    for handle in handles {
        let (h, n, w) = handle.await.expect("join runtime task");
        http.insert(h);
        nats.insert(n);
        websocket.insert(w);
    }
    assert_eq!(http.len(), 8);
    assert_eq!(nats.len(), 8);
    assert_eq!(websocket.len(), 8);
}

/// T21: `complete_session` binds a non-administrator session and never writes
/// the default session store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t21_complete_session_does_not_write_the_default_store() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let config_home = std::env::temp_dir().join(format!("trellis-test-xdg-{unique}"));
    std::fs::create_dir_all(&config_home).expect("create config home");
    let sentinel = config_home.join("sentinel.txt");
    std::fs::write(&sentinel, b"untouched").expect("write sentinel");
    // SAFETY: the fixture has no other test that reads or writes the store.
    std::env::set_var("XDG_CONFIG_HOME", &config_home);

    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");
    let identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");
    assert!(!identity.login_session_id().is_empty());

    let store = config_home.join("trellis").join("admin-session.json");
    assert!(
        !store.exists(),
        "complete_session must not write the default admin session store"
    );
    assert_eq!(
        std::fs::read(&sentinel).expect("read sentinel"),
        b"untouched",
        "the preexisting profile sentinel must be byte-for-byte unchanged"
    );

    runtime.shutdown().await.expect("shutdown runtime");
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

/// T15: dropping a started runtime stops its real infrastructure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t15_dropping_a_runtime_cleans_up() {
    let url = {
        let runtime = TrellisTestRuntime::builder()
            .start()
            .await
            .expect("start runtime");
        runtime.trellis_url().to_owned()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .no_proxy()
        .build()
        .expect("build client");
    let mut stopped = false;
    for _ in 0..40 {
        match client.get(format!("{url}/readyz")).send().await {
            Ok(response) if response.status().is_success() => {}
            _ => {
                stopped = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(stopped, "the dropped runtime's server should stop");
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
    assert!(matches!(
        error.kind(),
        TrellisTestErrorKind::ProcessExited
            | TrellisTestErrorKind::Bootstrap
            | TrellisTestErrorKind::Timeout
    ));
}
