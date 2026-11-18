//! A stopped provider is unavailable, not a malformed live offer.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use trellis_rs::client::{CallError, ServiceUnavailableError, TrellisClientError};
use trellis_test_fixture::apps::trellis_test_fixture_caller::{
    Client as CallerClient, Request as CallerRequest,
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
        .register_client::<CallerRequest>("caller")
        .await
        .unwrap();
    let caller = CallerClient::from_generated(
        trellis_rs::generated::Client::connect_user(caller_identity.connect_options())
            .await
            .unwrap(),
    );
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

    // Ending the run latches the provider's stop at once, but its intake
    // retires on its own task by draining its subscriptions, and a request the
    // broker delivered before that drain is still answered rather than left
    // to time out. So wait, within a bound, for the provider to stop serving.
    // Each call that finds no provider first waits up to five seconds for one.
    let rpc = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match api.echo(&input).await {
                Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(error) => return error,
            }
        }
    })
    .await
    .expect("the stopped provider kept answering");
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
    // A call made while the provider is away waits for it to come back, as
    // when a provider reconnects: the provider stays away for a moment after
    // the call is sent, then connects and subscribes its routes.
    let (echo, watch, task) = tokio::join!(api.echo(&input), operation.live(), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
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
        tokio::spawn(async move { service.run().await })
    });
    assert_eq!(
        echo.expect("returned provider answers Echo").value,
        input.value
    );
    drop(watch.expect("returned provider opens the observation"));
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

/// A wait for an absent provider sends the request again only within the
/// call's deadline. A caller whose runtime is busy past the deadline returns a
/// timeout without sending the pending resend, so a provider that arrived in
/// the meantime never executes a call its caller already reported as timed
/// out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resend_that_wakes_after_the_deadline_is_not_sent() {
    use std::sync::{mpsc, Mutex};
    use trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client as EchoClient;
    use trellis_test_fixture::participants::trellis_test_fixture_consumer::Participant as ConsumerParticipant;

    const DEADLINE: Duration = Duration::from_secs(3);
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let consumer_identity = runtime
        .register_service::<ConsumerParticipant>("deadline consumer")
        .await
        .unwrap();
    let provider_identity = runtime
        .register_service::<ProviderParticipant>("deadline provider")
        .await
        .unwrap();
    let caller_identity = runtime
        .register_client::<CallerRequest>("deadline probe")
        .await
        .unwrap();
    let probe = CallerClient::from_generated(
        trellis_rs::generated::Client::connect_user(caller_identity.connect_options())
            .await
            .unwrap(),
    );

    // The consumer runs on its own single-threaded runtime so that blocking
    // that thread delays its resend wakeup past the deadline, as a busy
    // caller does, while the provider keeps running here.
    let (start_provider, provider_wanted) = mpsc::channel::<()>();
    let (provider_serving, serving) = mpsc::channel::<()>();
    let consumer = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let service = ConsumerParticipant::connect(
                    consumer_identity
                        .connect_options()
                        .with_timeout_ms(u64::try_from(DEADLINE.as_millis()).unwrap()),
                )
                .await
                .unwrap();
                let api = EchoClient::from_generated(service.generated_client());
                let run = tokio::spawn(async move { service.run().await });
                let late = value("late");
                let started = std::time::Instant::now();
                let (late, ()) = tokio::join!(api.echo(&late), async {
                    // With no provider the request is resent after 100, 200,
                    // 400, and 800 ms, then waits 1000 ms for its last resend
                    // at about 2.5 s. Block in the middle of that wait.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    start_provider.send(()).unwrap();
                    serving.recv().unwrap();
                    let past = started + DEADLINE + Duration::from_millis(200);
                    let now = std::time::Instant::now();
                    std::thread::sleep(past.saturating_duration_since(now));
                });
                let late = late.expect_err("the late call must not complete");
                // A later call on the same connection reaches the provider, so
                // anything the late call had sent was delivered before it.
                let after = api.echo(&value("after")).await.unwrap();
                run.abort();
                let _ = run.await;
                (late, after.value)
            })
    });

    tokio::task::spawn_blocking(move || provider_wanted.recv().unwrap())
        .await
        .unwrap();
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut service = ProviderParticipant::connect(provider_identity.connect_options())
        .await
        .unwrap();
    {
        let received = Arc::clone(&received);
        let mut provider = Provider::new(&mut service);
        let mut api = provider.trellis_test_fixture_echo_v1();
        api.register_echo(move |_context, input| {
            let received = Arc::clone(&received);
            async move {
                received.lock().unwrap().push(input.value.clone());
                Ok(input)
            }
        });
        api.register_silent(|_context, input, operation| async move {
            operation.complete(input).await?;
            Ok(())
        });
    }
    let task = tokio::spawn(async move { service.run().await });
    let probe_api = probe.trellis_test_fixture_echo_v1();
    tokio::time::timeout(Duration::from_secs(20), async {
        while probe_api.echo(&value("probe")).await.is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the provider begins serving");
    provider_serving.send(()).unwrap();

    let (late, after) = tokio::task::spawn_blocking(move || consumer.join().unwrap())
        .await
        .unwrap();
    assert!(matches!(late, CallError::Timeout), "late call: {late:?}");
    assert_eq!(after, "after");
    let received = received.lock().unwrap().clone();
    assert_eq!(
        received
            .iter()
            .filter(|value| value.as_str() != "probe")
            .collect::<Vec<_>>(),
        ["after"],
        "the provider received a request sent after its deadline"
    );
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.unwrap();
}

/// A wait for an absent provider resends a Live opening only within the
/// call's deadline. A caller whose runtime is busy past the deadline returns a
/// timeout without sending the pending resend, so a provider that arrived in
/// the meantime never reserves a session for an opening its caller already
/// reported as timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_opening_resend_that_wakes_after_the_deadline_is_not_sent() {
    use futures_util::StreamExt;
    use std::sync::mpsc;
    use trellis_test_fixture::apis::trellis_test_fixture_echo_v1::Client as EchoClient;
    use trellis_test_fixture::participants::trellis_test_fixture_consumer::Participant as ConsumerParticipant;

    const DEADLINE: Duration = Duration::from_secs(3);
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let consumer_identity = runtime
        .register_service::<ConsumerParticipant>("live deadline consumer")
        .await
        .unwrap();
    let provider_identity = runtime
        .register_service::<ProviderParticipant>("live deadline provider")
        .await
        .unwrap();
    let caller_identity = runtime
        .register_client::<CallerRequest>("live deadline probe")
        .await
        .unwrap();
    let probe = CallerClient::from_generated(
        trellis_rs::generated::Client::connect_user(caller_identity.connect_options())
            .await
            .unwrap(),
    );
    let probe_api = &probe.trellis_test_fixture_echo_v1();
    let provider_identity = &provider_identity;
    let serve = move || async move {
        let mut service = ProviderParticipant::connect(provider_identity.connect_options())
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
        tokio::time::timeout(Duration::from_secs(20), async {
            while probe_api.echo(&value("probe")).await.is_err() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the provider begins serving");
        task
    };
    let task = serve().await;

    // The consumer runs on its own single-threaded runtime so that blocking
    // that thread delays its resend wakeup past the deadline, as a busy
    // caller does, while the provider and the test keep running here.
    let (to_test, from_consumer) = mpsc::channel::<()>();
    let (to_consumer, from_test) = mpsc::channel::<()>();
    let consumer = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let service = ConsumerParticipant::connect(
                    consumer_identity
                        .connect_options()
                        .with_timeout_ms(u64::try_from(DEADLINE.as_millis()).unwrap()),
                )
                .await
                .unwrap();
                let api = EchoClient::from_generated(service.generated_client());
                let run = tokio::spawn(async move { service.run().await });
                let operation = api.silent().start(&value("watched")).await.unwrap();
                operation.wait().await.unwrap();
                // The provider stops, leaving the operation with no owner to
                // answer its observation.
                to_test.send(()).unwrap();
                from_test.recv().unwrap();
                let started = std::time::Instant::now();
                let (late, ()) = tokio::join!(operation.live(), async {
                    // With no provider the opening is resent after 100, 200,
                    // 400, and 800 ms, then waits 1000 ms for its last resend
                    // at about 2.5 s. Block in the middle of that wait.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    to_test.send(()).unwrap();
                    from_test.recv().unwrap();
                    let past = started + DEADLINE + Duration::from_millis(200);
                    let now = std::time::Instant::now();
                    std::thread::sleep(past.saturating_duration_since(now));
                });
                let late = match late {
                    Ok(_) => panic!("the late observation must not open"),
                    Err(error) => error,
                };
                // A later opening and then a read on the same connection reach
                // the broker after anything the late opening had sent.
                drop(operation.live().await.unwrap());
                operation.get().await.unwrap();
                run.abort();
                let _ = run.await;
                late
            })
    });
    let next = |receiver: mpsc::Receiver<()>| async move {
        tokio::task::spawn_blocking(move || {
            receiver.recv().unwrap();
            receiver
        })
        .await
        .unwrap()
    };

    let from_consumer = next(from_consumer).await;
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while probe_api.echo(&value("probe")).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the stopped provider kept answering");
    to_consumer.send(()).unwrap();

    let _from_consumer = next(from_consumer).await;
    let task = serve().await;
    // A broker subscriber sees each opening the provider receives. It joins
    // only once the provider serves, so the absent provider still produced
    // no-responders replies while the consumer was resending.
    let broker = async_nats::ConnectOptions::with_credentials(
        &runtime
            .nats_creds(trellis_testkit::TestNatsUser::Trellis)
            .await
            .unwrap(),
    )
    .unwrap()
    .add_root_certificates(runtime.trust_roots()[0].clone())
    .connect(runtime.nats_url())
    .await
    .unwrap();
    let mut controls = broker.subscribe("operation.v1.>").await.unwrap();
    broker.flush().await.unwrap();
    to_consumer.send(()).unwrap();

    let late = tokio::task::spawn_blocking(move || consumer.join().unwrap())
        .await
        .unwrap();
    assert!(
        matches!(late, TrellisClientError::Timeout),
        "late: {late:?}"
    );
    let openings = tokio::time::timeout(Duration::from_secs(20), async {
        let mut openings = 0;
        while let Some(message) = controls.next().await {
            let body =
                serde_json::from_slice::<serde_json::Value>(&message.payload).unwrap_or_default();
            match body.get("action").and_then(serde_json::Value::as_str) {
                Some("watch") => openings += 1,
                Some("get") => return openings,
                _ => {}
            }
        }
        panic!("the broker subscription ended");
    })
    .await
    .expect("the read after the openings reached the broker");
    assert_eq!(
        openings, 1,
        "the provider received an opening sent after its deadline"
    );
    task.abort();
    let _ = task.await;
    runtime.shutdown().await.unwrap();
}

fn value(value: &str) -> Value {
    Value {
        value: value.to_owned(),
        extra: Default::default(),
    }
}
