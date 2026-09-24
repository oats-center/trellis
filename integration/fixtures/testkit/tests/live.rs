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
