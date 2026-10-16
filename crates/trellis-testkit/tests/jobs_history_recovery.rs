//! Captured TypeScript JobRefs and workers follow ordinary transport replacement.
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typescript_job_history_uses_the_successor_transport() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args([
            "--experimental-transform-types",
            "integration/fixtures/runtime/job_history_recovery.ts",
            runtime.oauth_origin(),
            runtime.admin_username(),
            runtime.admin_password(),
        ])
        .arg(runtime.workdir().join("history-recovery"))
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(90), command.output())
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
