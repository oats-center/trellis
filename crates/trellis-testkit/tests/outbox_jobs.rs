//! A job stored in an outbox runs once, even when its dispatch is repeated.
use std::time::Duration;

use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use trellis_rs::client::{
    dispatch_outbox_once, MemoryOutboxStore, OutboxDispatchResult, OutboxDispatcher, OutboxStore,
};
use trellis_rs::jobs::OutboxJobOutcome;
use trellis_testkit::TrellisTestRuntime;

fn value(value: &str) -> Value {
    Value {
        value: value.into(),
        extra: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outboxed_job_runs_once_when_a_crashed_dispatcher_submits_it_again() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("outbox jobs")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let mut provider = Provider::new(&mut service);
    // One job at a time, in delivery order.
    provider
        .register_work_with_concurrency(1, move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send((job.id().to_owned(), job.payload().value.clone()));
                Ok::<_, trellis_rs::jobs::JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    let jobs = provider.jobs_client();

    // The application stores the job inside its own transaction.
    let prepared = jobs.prepare_work(value("outboxed")).unwrap();
    let mut outbox = MemoryOutboxStore::new();
    outbox.enqueue("order-1-refresh", &prepared).await.unwrap();

    let dispatcher = jobs.outbox_dispatcher();
    let first = dispatch_outbox_once(&mut outbox, &dispatcher)
        .await
        .unwrap();
    let OutboxDispatchResult::JobDispatched {
        id,
        outcome: OutboxJobOutcome::Accepted { job, key: None },
    } = &first
    else {
        panic!("the outboxed job was not accepted: {first:?}");
    };
    assert_eq!(
        (id.as_str(), job.id.as_str()),
        ("order-1-refresh", prepared.job_id())
    );
    assert!(dispatch_outbox_once(&mut outbox, &dispatcher)
        .await
        .is_ok_and(|result| result == OutboxDispatchResult::Empty));
    // A dispatcher that crashed before marking the record submits it again.
    let again = dispatcher.dispatch_job(prepared.clone()).await.unwrap();
    assert!(
        matches!(&again, OutboxJobOutcome::Accepted { job, .. } if job.id == prepared.job_id()),
        "{again:?}"
    );

    // A job created afterwards runs next: a duplicate would have run before it.
    let later = jobs.create_work(value("later")).await.unwrap();
    let mut order = Vec::new();
    while order.len() < 2 {
        order.push(
            tokio::time::timeout(Duration::from_secs(30), runs.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(
        order,
        [
            (prepared.job_id().to_owned(), "outboxed".to_owned()),
            (later.identity().id.clone(), "later".to_owned()),
        ]
    );
    drop(service);
    runtime.shutdown().await.unwrap();
}
