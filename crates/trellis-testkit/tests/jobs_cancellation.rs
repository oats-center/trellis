//! Public generated JobRef cancellation on provisioned keyed and ordinary queues.
use runtime_trellis::participants::runtime_trellis_operation_provider::types::KeyedValue;
use runtime_trellis::participants::{
    runtime_trellis_operation_provider, runtime_trellis_ordinary_jobs,
};
use runtime_trellis::types::Value;
use trellis_rs::jobs::JobState;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancels_pending_jobs_without_provisioning_unneeded_key_coordination() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let ordinary = runtime
        .register_service::<runtime_trellis_ordinary_jobs::Participant>("ordinary cancellation")
        .await
        .unwrap();
    let mut service =
        runtime_trellis_ordinary_jobs::Participant::connect(ordinary.connect_options())
            .await
            .unwrap();
    let job = runtime_trellis_ordinary_jobs::Provider::new(&mut service)
        .submit_work(Value {
            value: "cancel ordinary".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(15), job.cancel())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(job.get().await.unwrap().state, JobState::Cancelled);
    drop(service);

    let keyed = runtime
        .register_service::<runtime_trellis_operation_provider::Participant>("keyed cancellation")
        .await
        .unwrap();
    let mut service =
        runtime_trellis_operation_provider::Participant::connect(keyed.connect_options())
            .await
            .unwrap();
    let job = runtime_trellis_operation_provider::Provider::new(&mut service)
        .submit_keyed_work(KeyedValue {
            key: "one".into(),
            value: "cancel keyed".into(),
            extra: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(15), job.cancel())
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Cancelled
    );
    drop(service);
    runtime.shutdown().await.unwrap();
}
