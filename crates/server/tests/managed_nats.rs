//! A server that runs its own NATS tells clients where to reach it.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// Clients on other machines must be given the address `init` chose for
/// them, never the loopback address the server itself uses for its broker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_nats_is_advertised_at_its_configured_address() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let server = env!("CARGO_BIN_EXE_trellis-server");
    let reserved: Vec<_> = (0..5)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect();
    let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
    let config_dir = root.join("installation");
    let generated = tokio::process::Command::new(server)
        .args(["init", "--dev", "--out"])
        .arg(&config_dir)
        .args(["--trellis-port", &ports[0].to_string()])
        .args(["--console-port", &ports[4].to_string()])
        .args(["--nats-port", &ports[1].to_string()])
        .args(["--nats-monitor-port", &ports[2].to_string()])
        .args(["--nats-ws-port", &ports[3].to_string()])
        .output()
        .await
        .unwrap();
    assert!(
        generated.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let mut running = tokio::process::Command::new(server)
        .arg("--config")
        .arg(config_dir.join("config.toml"))
        .arg("--nats-download")
        .arg("--local-nats-cache")
        .arg(workspace.join("target/trellis-test-cache"))
        .arg("--local-nats-ports")
        .arg(format!("{},{},{}", ports[1], ports[2], ports[3]))
        .arg("--isolated-process-group")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let group = running.id().unwrap() as i32;

    let issuer = format!("https://localhost:{}", ports[0]);
    let roots = [config_dir.join("tls/ca.crt")];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let discovered = loop {
        match trellis_rs::auth::discover_native_oauth(&issuer, &roots).await {
            Ok(discovered) => break discovered,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the server never served sign-in: {error}"
                );
                assert!(
                    running.try_wait().unwrap().is_none(),
                    "the server exited before serving sign-in"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    assert_eq!(
        discovered.nats_servers,
        vec![format!("tls://localhost:{}", ports[1])]
    );

    // SAFETY: signals the process group this test started and owns.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    tokio::time::timeout(Duration::from_secs(30), running.wait())
        .await
        .unwrap()
        .unwrap();
    drop(reserved);
}
