//! Live job updates from a generated worker reach generated subscribers.
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::{Update, UpdateDetail, Value};
use trellis_rs::jobs::{JobProcessError, JobState, JobSubmitOutcome};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_streams_typed_updates_to_job_and_queue_subscribers_until_the_job_ends() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("job updates")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    // The worker waits until both subscriptions are open: updates are live
    // only, so earlier ones would not reach them.
    let subscribed = Arc::new(tokio::sync::Semaphore::new(0));
    let mut provider = Provider::new(&mut service);
    provider
        .register_work({
            let subscribed = Arc::clone(&subscribed);
            move |job| {
                let subscribed = Arc::clone(&subscribed);
                async move {
                    if job.payload().value == "streamed" {
                        let _ = subscribed.acquire_many(2).await.unwrap();
                        for count in 1..=3 {
                            job.emit_update(Update {
                                value: format!("step {count}"),
                                nested: UpdateDetail {
                                    count: runtime_trellis::Int64(count),
                                    payload: vec![count as u8].into(),
                                    extra: Default::default(),
                                },
                                extra: Default::default(),
                            })
                            .await
                            .map_err(|error| JobProcessError::Failed(error.to_string()))?;
                        }
                    }
                    Ok(job.payload().clone())
                }
            }
        })
        .await
        .unwrap();

    // Any task can create and observe jobs through the cloneable handle.
    let jobs = provider.jobs_client();
    let (job, by_ref, by_id) = tokio::spawn(async move {
        let job = jobs
            .create_work(Value {
                value: "streamed".into(),
                extra: Default::default(),
            })
            .await
            .unwrap();
        let by_ref = job.updates().await.unwrap();
        let by_id = jobs.work_updates(&job.identity().id).await.unwrap();
        (job, by_ref, by_id)
    })
    .await
    .unwrap();
    subscribed.add_permits(2);
    for updates in [by_ref, by_id] {
        // Each stream ends by itself once the job completes.
        let received = tokio::time::timeout(Duration::from_secs(30), updates.collect::<Vec<_>>())
            .await
            .unwrap()
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            received
                .iter()
                .map(|update| (
                    update.job_id.as_str(),
                    update.sequence,
                    update.update.value.as_str(),
                    update.update.nested.count.0,
                ))
                .collect::<Vec<_>>(),
            [
                (job.identity().id.as_str(), 1, "step 1", 1),
                (job.identity().id.as_str(), 2, "step 2", 2),
                (job.identity().id.as_str(), 3, "step 3", 3),
            ]
        );
    }
    assert_eq!(job.wait().await.unwrap().state, JobState::Completed);

    // Policy-aware submission accepts an unkeyed job and runs it.
    let JobSubmitOutcome::Accepted { job_ref, key } = provider
        .submit_work(Value {
            value: "submitted".into(),
            extra: Default::default(),
        })
        .await
        .unwrap()
    else {
        panic!("an unkeyed job is always accepted");
    };
    assert_eq!(key, None);
    let finished = tokio::time::timeout(Duration::from_secs(30), job_ref.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(finished.state, JobState::Completed);
    assert_eq!(finished.result.unwrap().value, "submitted");
    drop(service);
    runtime.shutdown().await.unwrap();
}
