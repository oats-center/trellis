//! A stopped provider is unavailable, not a malformed live offer.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use trellis_rs::client::{CallError, ServiceUnavailableError, TrellisClientError};
use trellis_test_fixture::participants::trellis_test_fixture_caller::{
    Client as CallerClient, Participant as CallerParticipant,
};
use trellis_test_fixture::participants::trellis_test_fixture_provider::{
    Participant as ProviderParticipant, Provider,
};
use trellis_test_fixture::types::Value;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopped_provider_is_unavailable_and_same_operation_can_be_observed_after_return() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<ProviderParticipant>("provider")
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .unwrap();
    let handler_calls = Arc::clone(&calls);
    {
        let mut provider = Provider::new(&mut service);
        let mut api = provider.trellis_test_fixture_echo_v1();
        api.register_echo(|_context, input| async move { Ok(input) });
        api.register_silent(move |_context, input, operation| {
            let calls = Arc::clone(&handler_calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                operation.complete(input).await?;
                Ok(())
            }
        });
    }
    let task = tokio::spawn(async move { service.run().await });
    let caller_identity = runtime
        .register_client::<CallerParticipant>("caller")
        .await
        .unwrap();
    let caller = CallerClient::connect(caller_identity.connect_options())
        .await
        .unwrap();
    let api = caller.trellis_test_fixture_echo_v1();
    let input = Value {
        value: "one durable execution".to_owned(),
        extra: Default::default(),
    };
    assert_eq!(api.echo(&input).await.unwrap().value, input.value);
    let operation = api.silent().start(&input).await.unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(20), operation.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.output.unwrap().value, input.value);

    // Drop the ordinary service run future and its owned subscriptions, exactly
    // as a service process stopping does. Infrastructure and caller stay alive.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    let rpc = api.echo(&input).await.unwrap_err();
    let wait = operation.wait().await.unwrap_err();
    let watch = match operation.live().await {
        Ok(_) => panic!("stopped provider must not open an observation"),
        Err(error) => error,
    };
    eprintln!("stopped provider rpc: {rpc}; wait: {wait}; watch: {watch}");
    assert!(matches!(
        rpc,
        CallError::ServiceUnavailable(ServiceUnavailableError)
    ));
    assert!(matches!(
        wait,
        TrellisClientError::ServiceUnavailable(ServiceUnavailableError)
    ));
    assert!(matches!(
        watch,
        TrellisClientError::ServiceUnavailable(ServiceUnavailableError)
    ));
    for (path, error) in [
        ("rpc", rpc.to_string()),
        ("wait", wait.to_string()),
        ("watch", watch.to_string()),
    ] {
        assert!(
            error.contains("requested Trellis service is unavailable"),
            "{path}: {error}"
        );
    }

    let mut service = ProviderParticipant::connect(identity.connect_options())
        .await
        .unwrap();
    let handler_calls = Arc::clone(&calls);
    {
        let mut provider = Provider::new(&mut service);
        let mut api = provider.trellis_test_fixture_echo_v1();
        api.register_echo(|_context, input| async move { Ok(input) });
        api.register_silent(move |_context, input, operation| {
            let calls = Arc::clone(&handler_calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                operation.complete(input).await?;
                Ok(())
            }
        });
    }
    let task = tokio::spawn(async move { service.run().await });
    // Connection bootstrap precedes route subscription. Await real serving
    // readiness through the ordinary RPC, not an artificial server-ready hook.
    let echo = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match api.echo(&input).await {
                Err(CallError::ServiceUnavailable(_)) => tokio::task::yield_now().await,
                result => break result,
            }
        }
    })
    .await
    .expect("returned provider begins serving")
    .expect("returned provider answers Echo");
    assert_eq!(echo.value, input.value);
    let returned = tokio::time::timeout(Duration::from_secs(20), operation.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(returned.output.unwrap().value, input.value);
    assert_eq!(returned.revision, completed.revision);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "observation must not reexecute durable work"
    );
    eprintln!(
        "returned provider observed existing operation {} at revision {}; executions: {}",
        operation.id(),
        returned.revision,
        calls.load(Ordering::SeqCst)
    );
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.unwrap();
}
