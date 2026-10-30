//! Persisted TypeScript activation is checked through ordinary authenticated enrollment.
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_typescript_activation_does_not_create_another_review() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args([
            "--experimental-transform-types",
            "integration/fixtures/runtime/activation_recheck.ts",
            runtime.trellis_url(),
            runtime.admin_username(),
            runtime.admin_password(),
        ])
        .env("NODE_EXTRA_CA_CERTS", &runtime.trust_roots()[0])
        .arg(runtime.workdir().join("activation-recheck"))
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    runtime.shutdown().await.unwrap();
}
