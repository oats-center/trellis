//! The harness applies a real participant from a source directory, provisions an instance, and
//! hands back the material a caller connects with.

use std::path::Path;

use trellis_test::{TrellisTestRuntime, TrellisTestRuntimeOptions};

/// The repository's own integration fixture project (package `runtime-trellis`).
fn fixture_project() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../integration/fixtures/runtime")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn applies_a_participant_and_provisions_an_instance() {
    let mut runtime = TrellisTestRuntime::start(TrellisTestRuntimeOptions::default())
        .await
        .expect("start a real runtime");
    let admin = runtime.connect_admin().await.expect("admin login");

    let session = admin
        .register_client(
            "admin-service",
            &fixture_project(),
            Some("runtime-trellis.AdminService"),
        )
        .await
        .expect("apply the fixture participant and provision an instance");

    assert!(
        session.instance_id.starts_with("inst_"),
        "instance id should be a provisioned instance id, got {}",
        session.instance_id
    );
    assert_eq!(session.participant_id, "runtime-trellis.AdminService");
    assert!(!session.seed.is_empty(), "provisioning must return a seed");
    assert_eq!(session.trellis_url, runtime.trellis_url());

    runtime.stop().await.expect("stop the runtime");
}
