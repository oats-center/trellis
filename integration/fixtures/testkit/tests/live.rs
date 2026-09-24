//! Live acceptance tests for the external Rust testkit.
//!
//! These tests are explicitly selected by the fixture's `live` target and run
//! against real `trellis`/`trellis-server` executables. They never start
//! infrastructure during compilation or documentation generation.

use std::time::Duration;

use futures_util::StreamExt;
use trellis_rs::client::EventSubscribeOptions;
use trellis_rs::service::ServerError;
use trellis_test::TrellisTestRuntime;
use trellis_test_fixture::participants::trellis_test_fixture_caller::{
    Client as CallerClient, Participant as CallerParticipant,
};
use trellis_test_fixture::participants::trellis_test_fixture_provider::{
    Participant as ProviderParticipant, Provider,
};
use trellis_test_fixture::types::Value;

/// T04: a real runtime bootstraps, authenticates, registers a Provider and
/// Caller, performs the typed RPC, and receives the real typed event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t04_real_rpc_and_event_between_provider_and_caller() {
    let mut runtime = TrellisTestRuntime::builder()
        .start()
        .await
        .expect("start runtime");

    let provider_identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .expect("register provider");
    let caller_identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .expect("register caller");

    let mut service = ProviderParticipant::connect(provider_identity.connect_options())
        .await
        .expect("connect provider");
    {
        let publisher = Provider::new(&mut service).client();
        let mut provider = Provider::new(&mut service);
        provider
            .trellis_test_fixture_echo_v1()
            .register_echo(move |_context, input| {
                let publisher = publisher.clone();
                async move {
                    publisher
                        .trellis_test_fixture_echo_v1()
                        .publish_observed(&input)
                        .await
                        .map_err(|error| ServerError::Nats(error.to_string()))?;
                    Ok(input)
                }
            });
    }
    let service_task = tokio::spawn(async move { service.run().await });

    let caller = CallerClient::connect(caller_identity.connect_options())
        .await
        .expect("connect caller");
    let api = caller.trellis_test_fixture_echo_v1();
    let mut events = api
        .subscribe_observed(EventSubscribeOptions::ephemeral())
        .await
        .expect("subscribe to Observed");

    let output = api
        .echo(&Value {
            value: "hello".to_owned(),
        })
        .await
        .expect("call Echo");
    assert_eq!(output.value, "hello");

    let observed = tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("event arrives before the timeout")
        .expect("event stream yields an item")
        .expect("event decodes");
    assert_eq!(observed.value, "hello");

    drop(events);
    drop(api);
    drop(caller);
    service_task.abort();
    runtime.shutdown().await.expect("shutdown runtime");
}
