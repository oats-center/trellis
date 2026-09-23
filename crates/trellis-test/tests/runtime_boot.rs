//! A test runtime owns a real control plane, answers readiness, and stops cleanly.

use trellis_test::{TrellisTestRuntime, TrellisTestRuntimeOptions};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starts_and_stops_a_real_runtime() {
    let mut runtime = TrellisTestRuntime::start(TrellisTestRuntimeOptions::default())
        .await
        .expect("start a real runtime");

    let response = reqwest::Client::new()
        .get(format!("{}/readyz", runtime.trellis_url()))
        .send()
        .await
        .expect("readiness request");
    assert!(
        response.status().is_success(),
        "runtime should be ready, got {}",
        response.status()
    );
    assert!(runtime.nats_url().starts_with("nats://127.0.0.1:"));
    assert!(runtime.workdir().exists());

    runtime.stop().await.expect("stop the runtime");
}
