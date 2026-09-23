//! The harness completes the real local login as the seeded administrator and creates a
//! deployment through the admin RPCs.

use trellis_test::{TrellisTestRuntime, TrellisTestRuntimeOptions};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_login_creates_a_deployment() {
    let mut runtime = TrellisTestRuntime::start(TrellisTestRuntimeOptions::default())
        .await
        .expect("start a real runtime");

    let admin = runtime.connect_admin().await.expect("admin login");
    let deployment = admin
        .create_deployment("harness")
        .await
        .expect("create deployment");
    assert!(
        deployment.starts_with("dep_"),
        "deployment id should be a deployment id, got {deployment}"
    );

    runtime.stop().await.expect("stop the runtime");
}
