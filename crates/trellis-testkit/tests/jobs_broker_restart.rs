//! A running service rides out its broker restarting, as every restart of a
//! `trellis-server --dev` installation makes it do.
use std::sync::Arc;
use std::time::Duration;

use runtime_trellis::apis::runtime_trellis_events_v1::events::Alpha;
use runtime_trellis::apis::runtime_trellis_events_v1::Client as EventsClient;
use runtime_trellis::apps::runtime_trellis_caller::{Client as CallerClient, Request};
use runtime_trellis::participants::runtime_trellis_event_service_both::Participant as EventParticipant;
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::{Sample, Value};
use tokio::sync::Semaphore;
use trellis_rs::client::OperationState;
use trellis_rs::jobs::{JobProcessError, JobState};
use trellis_rs::service::{ServerError, ServiceEventListenOptions};
use trellis_testkit::TrellisTestRuntime;

fn value(value: &str) -> Value {
    Value {
        value: value.into(),
        extra: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_service_keeps_serving_after_its_broker_restarts() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("restarted broker")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let mut provider = Provider::new(&mut service);
    provider
        .register_work_with_concurrency(4, move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send(job.payload().value.clone());
                Ok::<_, JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    // The first run stays in its handler until the broker has restarted.
    let (held, mut holding) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    provider
        .register_restart_work({
            let release = Arc::clone(&release);
            move |job| {
                let held = held.clone();
                let release = Arc::clone(&release);
                async move {
                    let _ = held.send(());
                    drop(release.acquire().await.unwrap());
                    Ok::<_, JobProcessError<String>>(job.payload().clone())
                }
            }
        })
        .await
        .unwrap();
    let jobs = provider.jobs_client();
    provider
        .runtime_trellis_runtime_v1()
        .register_work(|_context, input, operation| async move {
            operation.complete(input).await?;
            Ok::<_, ServerError>(())
        });
    let mut run = tokio::spawn(service.run());
    let caller = runtime.register_client::<Request>("caller").await.unwrap();

    jobs.create_work(value("before")).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(30), runs.recv())
            .await
            .unwrap()
            .unwrap(),
        "before"
    );

    let in_flight = jobs.create_restart_work(value("held")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), holding.recv())
        .await
        .unwrap()
        .unwrap();

    runtime.restart_with_broker().await.unwrap();
    release.add_permits(Semaphore::MAX_PERMITS);

    let recovered = async {
        // Submissions fail until the service has its new connection.
        while jobs.create_work(value("after")).await.is_err() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(runs.recv().await.unwrap(), "after");

        let caller = CallerClient::from_generated(
            trellis_rs::generated::Client::connect_user(caller.connect_options())
                .await
                .unwrap(),
        );
        let api = caller.runtime_trellis_runtime_v1();
        let work = api.work();
        let operation = work.start(&value("served")).await.unwrap();
        let finished = operation.wait().await.unwrap();
        assert_eq!(finished.state, OperationState::Completed);
        assert_eq!(finished.output.unwrap().value, "served");

        assert_eq!(in_flight.wait().await.unwrap().state, JobState::Completed);
    };
    tokio::select! {
        result = &mut run => panic!("the service stopped across the restart: {result:?}"),
        finished = tokio::time::timeout(Duration::from_secs(90), recovered) => finished.unwrap(),
    }

    run.abort();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_durable_event_consumer_keeps_delivering_after_its_broker_restarts() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<EventParticipant>("restarted events")
        .await
        .unwrap();
    let service = EventParticipant::connect(identity.connect_options())
        .await
        .unwrap();
    let (seen, mut delivered) = tokio::sync::mpsc::unbounded_channel();
    let _consumer = service
        .listen_event::<Alpha, _, _>(
            move |event, _| {
                let _ = seen.send(event.value);
                async { Ok::<_, ServerError>(()) }
            },
            ServiceEventListenOptions::default(),
        )
        .await
        .unwrap();
    let publisher = EventsClient::from_generated(service.generated_client());
    let publish = |value: &'static str| {
        let publisher = &publisher;
        async move {
            let sample = Sample {
                site: "restart".into(),
                value: value.into(),
                extra: Default::default(),
            };
            publisher.publish_alpha(&sample).await
        }
    };
    let mut run = tokio::spawn(service.run());

    publish("before").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(30), delivered.recv())
            .await
            .unwrap()
            .unwrap(),
        "before"
    );

    runtime.restart_with_broker().await.unwrap();

    let recovered = async {
        while publish("after").await.is_err() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        while delivered.recv().await.unwrap() != "after" {}
    };
    tokio::select! {
        result = &mut run => panic!("the service stopped across the restart: {result:?}"),
        finished = tokio::time::timeout(Duration::from_secs(90), recovered) => finished.unwrap(),
    }

    run.abort();
    runtime.shutdown().await.unwrap();
}
