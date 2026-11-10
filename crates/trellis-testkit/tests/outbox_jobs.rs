//! A job stored in an outbox runs once, even when its dispatch is repeated.
use std::time::Duration;

use runtime_trellis::participants::runtime_trellis_provider::types::KeyedValue;
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

async fn jobs_admin(
    runtime: &mut TrellisTestRuntime,
) -> trellis_runtime_apis::apis::trellis_jobs_v1::Client {
    let admin = runtime
        .register_client::<runtime_trellis::apps::runtime_trellis_jobs_admin::Request>("admin")
        .await
        .unwrap();
    trellis_runtime_apis::apis::trellis_jobs_v1::Client::from_generated(
        trellis_rs::generated::Client::connect_user(admin.connect_options())
            .await
            .unwrap(),
    )
}

async fn next_run(runs: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(30), runs.recv())
        .await
        .expect("the job ran")
        .unwrap()
}

fn keyed(key: &str, value: &str) -> KeyedValue {
    KeyedValue {
        key: key.into(),
        value: value.into(),
        extra: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatching_a_finished_keyed_job_again_leaves_its_key_free() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("keyed outbox")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let mut provider = Provider::new(&mut service);
    // This queue refuses work while its key is taken, so one leftover
    // reservation would refuse every later job.
    provider
        .register_rejecting_work(move |job| {
            let ran = ran.clone();
            async move {
                let _ = ran.send(job.payload().value.clone());
                Ok::<_, trellis_rs::jobs::JobProcessError<String>>(job.payload().clone())
            }
        })
        .await
        .unwrap();
    let jobs = provider.jobs_client();
    let dispatcher = jobs.outbox_dispatcher();
    let admin = jobs_admin(&mut runtime).await;

    let prepared = jobs
        .prepare_rejecting_work(keyed("field", "first"))
        .unwrap();
    let first = dispatcher.dispatch_job(prepared.clone()).await.unwrap();
    let OutboxJobOutcome::Accepted { job, .. } = first else {
        panic!("the first dispatch was not accepted: {first:?}");
    };
    assert_eq!(next_run(&mut runs).await, "first");
    let lookup = serde_json::from_value(serde_json::json!({
        "service": job.service,
        "type": job.job_type,
        "key": "field",
    }))
    .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(key) = admin.get_key(&lookup).await {
                let key = serde_json::to_value(&key).unwrap();
                if key["active"] == serde_json::json!([]) && key["queued"] == serde_json::json!([])
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the finished job freed its key");

    // The dispatcher crashed before marking its record, and sends it again.
    let again = dispatcher.dispatch_job(prepared.clone()).await.unwrap();
    assert!(
        matches!(&again, OutboxJobOutcome::Accepted { job, .. } if job.id == prepared.job_id()),
        "{again:?}"
    );

    jobs.create_rejecting_work(keyed("field", "next"))
        .await
        .unwrap();
    assert_eq!(next_run(&mut runs).await, "next");
    drop(service);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_outbox_job_can_be_found_as_skipped_by_its_prepared_id() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("refused outbox")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let (started, mut holding) = tokio::sync::mpsc::unbounded_channel();
    let release = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let mut provider = Provider::new(&mut service);
    provider
        .register_rejecting_work({
            let release = std::sync::Arc::clone(&release);
            move |job| {
                let started = started.clone();
                let release = std::sync::Arc::clone(&release);
                async move {
                    let _ = started.send(());
                    drop(release.acquire().await.unwrap());
                    Ok::<_, trellis_rs::jobs::JobProcessError<String>>(job.payload().clone())
                }
            }
        })
        .await
        .unwrap();
    let jobs = provider.jobs_client();
    let admin = jobs_admin(&mut runtime).await;

    jobs.create_rejecting_work(keyed("field", "holder"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), holding.recv())
        .await
        .unwrap()
        .unwrap();

    let prepared = jobs
        .prepare_rejecting_work(keyed("field", "refused"))
        .unwrap();
    let outcome = jobs
        .outbox_dispatcher()
        .dispatch_job(prepared.clone())
        .await
        .unwrap();
    assert!(
        matches!(outcome, OutboxJobOutcome::Rejected { .. }),
        "{outcome:?}"
    );

    let request = serde_json::from_value(serde_json::json!({})).unwrap();
    let id = prepared.job_id();
    let found = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(listed) = admin.query(&request).await {
                let listed = serde_json::to_value(&listed).unwrap();
                let job = listed["items"]
                    .as_array()
                    .and_then(|items| items.iter().find(|job| job["id"] == id).cloned());
                if let Some(job) = job {
                    break job;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the refused job is listed");
    assert_eq!(found["state"], "skipped");

    release.add_permits(tokio::sync::Semaphore::MAX_PERMITS);
    drop(service);
    runtime.shutdown().await.unwrap();
}
