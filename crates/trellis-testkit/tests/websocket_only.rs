//! A Trellis that keeps native NATS private is used entirely over its WebSocket.
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_connect_over_the_websocket_when_native_nats_is_private() {
    // Starting signs the Rust administrator in, which already needs NATS.
    let mut runtime = TrellisTestRuntime::builder()
        .nats_websocket_only()
        .start()
        .await
        .unwrap();
    let http = runtime.https_client_builder().build().unwrap();
    let metadata: serde_json::Value = http
        .get(format!(
            "{}/.well-known/oauth-authorization-server",
            runtime.oauth_origin()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        metadata["trellis"]["natsEndpoints"],
        serde_json::json!([runtime.websocket_url()])
    );

    // A load balancer's health check names the target, not the sign-in host.
    // Readiness answers it; sign-in itself still refuses that host.
    let probe = |path: &str| {
        http.get(format!("{}{path}", runtime.oauth_origin()))
            .header(reqwest::header::HOST, "load-balancer.invalid")
            .send()
    };
    assert_eq!(probe("/readyz").await.unwrap().status(), 200);
    assert_eq!(
        probe("/.well-known/oauth-authorization-server")
            .await
            .unwrap()
            .status(),
        400
    );

    runtime.create_user("WebSocket user").await.unwrap();

    // TypeScript native clients and devices discover and use the WebSocket too.
    let mut command = tokio::process::Command::new("node");
    command
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args([
            "--experimental-transform-types",
            "integration/fixtures/runtime/activation_recheck.ts",
            runtime.oauth_origin(),
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
