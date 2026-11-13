//! A Rust caller admitted before its provider existed reaches it once added.

use std::time::Duration;

use trellis_test_fixture::apps::trellis_test_fixture_caller::{
    Client as CallerClient, Request as CallerRequest,
};
use trellis_test_fixture::participants::trellis_test_fixture_consumer::Participant as ConsumerParticipant;
use trellis_test_fixture::participants::trellis_test_fixture_provider::{
    Participant as ProviderParticipant, Provider,
};
use trellis_test_fixture::types::Value;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_caller_follows_an_operation_on_a_provider_added_after_it_connected() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    // A consumer is added first, which brings the API into the catalog with
    // no provider serving it.
    runtime
        .register_service::<ConsumerParticipant>("consumer")
        .await
        .unwrap();
    let caller_identity = runtime
        .register_client::<CallerRequest>("caller")
        .await
        .unwrap();
    let caller = CallerClient::from_generated(
        trellis_rs::generated::Client::connect_user(caller_identity.connect_options())
            .await
            .unwrap(),
    );

    // The provider joins only after the caller's routes were derived.
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .unwrap();
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .unwrap();
    {
        let mut provider = Provider::new(&mut service);
        let mut api = provider.trellis_test_fixture_echo_v1();
        api.register_echo(|_context, input| async move { Ok(input) });
        api.register_silent(|_context, input, operation| async move {
            operation.complete(input).await?;
            Ok(())
        });
    }
    let task = tokio::spawn(async move { service.run().await });

    let api = caller.trellis_test_fixture_echo_v1();
    let input = Value {
        value: "after the caller".to_owned(),
        extra: Default::default(),
    };
    // Starting uses the API's shared route once the provider serves it.
    let operation = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match api.silent().start(&input).await {
                Ok(operation) => break operation,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("the provider begins serving");
    // Following it uses the route bound to that provider, which the caller
    // gains on Auth's notice, well before its scheduled renewal.
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match operation.wait().await {
                Ok(completed) => break completed,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("the caller reaches the provider added after it connected");
    assert_eq!(completed.output.unwrap().value, input.value);

    task.abort();
    let _ = task.await;
    runtime.shutdown().await.unwrap();
}
