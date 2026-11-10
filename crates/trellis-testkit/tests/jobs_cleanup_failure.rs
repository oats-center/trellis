//! A job out of attempts ends when its cleanup reports a final failure.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use runtime_trellis::participants::runtime_trellis_provider::{Participant, Provider};
use runtime_trellis::types::Value;
use trellis_rs::jobs::{JobProcessError, JobState};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_cleanup_ends_a_job_instead_of_redelivering_it_forever() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let identity = runtime
        .register_service::<Participant>("cleanup failure")
        .await
        .unwrap();
    let mut service = Participant::connect(identity.connect_options())
        .await
        .unwrap();
    let entries = Arc::new(AtomicUsize::new(0));
    let mut provider = Provider::new(&mut service);
    provider
        .register_work({
            let entries = Arc::clone(&entries);
            move |job| {
                let entries = Arc::clone(&entries);
                async move {
                    entries.fetch_add(1, Ordering::SeqCst);
                    if job.is_cancelled() {
                        return Err(JobProcessError::failed("cannot undo the partial run"));
                    }
                    Err(JobProcessError::retryable("storage is busy"))
                }
            }
        })
        .await
        .unwrap();

    let job = provider
        .create_work(Value {
            value: "fails".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(30), job.wait())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(ended.state, JobState::Dead);
    assert!(
        ended
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("cannot undo the partial run")),
        "{:?}",
        ended.last_error
    );
    // Two ordinary attempts, then the one cleanup entry.
    assert_eq!(entries.load(Ordering::SeqCst), 3);
    drop(service);
    runtime.shutdown().await.unwrap();
}
