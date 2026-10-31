//! A server that runs its own NATS tells clients where to reach it, and
//! listens for them where they connect from.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An installation `init` generated with `init_args`, running on reserved ports
/// `[trellis, nats, monitor, websocket, console]`.
struct Installation {
    running: tokio::process::Child,
    ports: Vec<u16>,
    config_dir: PathBuf,
    _reserved: Vec<trellis_local_nats::ReservedPort>,
}

impl Installation {
    async fn start(root: &Path, init_args: &[&str]) -> Self {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let server = env!("CARGO_BIN_EXE_trellis-server");
        let reserved: Vec<_> = (0..5)
            .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
            .collect();
        let ports: Vec<_> = reserved.iter().map(|reserved| reserved.port()).collect();
        let config_dir = root.join("installation");
        let generated = tokio::process::Command::new(server)
            .arg("init")
            .args(init_args)
            .arg("--out")
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
        let running = tokio::process::Command::new(server)
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
        Self {
            running,
            ports,
            config_dir,
            _reserved: reserved,
        }
    }

    /// Waits until the managed broker accepts WebSocket connections locally.
    async fn wait_for_websocket(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::net::TcpStream::connect(("127.0.0.1", self.ports[3]))
            .await
            .is_err()
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the managed broker never listened for WebSockets"
            );
            assert!(
                self.running.try_wait().unwrap().is_none(),
                "the server exited before its broker listened"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn stop(mut self) {
        let group = self.running.id().unwrap() as i32;
        // SAFETY: signals the process group this test started and owns.
        unsafe { libc::kill(-group, libc::SIGTERM) };
        tokio::time::timeout(Duration::from_secs(30), self.running.wait())
            .await
            .unwrap()
            .unwrap();
    }
}

/// Connects to the WebSocket port at a loopback address other than
/// 127.0.0.1. Linux accepts it only for a listener on every interface, so it
/// stands in for a proxy on another host.
async fn connect_from_elsewhere(port: u16) -> std::io::Result<tokio::net::TcpStream> {
    tokio::net::TcpStream::connect(("127.0.0.2", port)).await
}

/// Clients on other machines must be given the address `init` chose for
/// them, never the loopback address the server itself uses for its broker.
/// The broker's plaintext WebSocket stays private to the Trellis port that
/// relays it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_nats_is_advertised_at_its_configured_address() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let mut installation = Installation::start(directory.path(), &["--dev"]).await;

    let issuer = format!("https://localhost:{}", installation.ports[0]);
    let roots = [installation.config_dir.join("tls/ca.crt")];
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
                    installation.running.try_wait().unwrap().is_none(),
                    "the server exited before serving sign-in"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    assert_eq!(
        discovered.nats_servers,
        vec![format!("tls://localhost:{}", installation.ports[1])]
    );
    installation.wait_for_websocket().await;
    assert!(
        connect_from_elsewhere(installation.ports[3]).await.is_err(),
        "the relayed plaintext WebSocket accepted a connection from outside loopback"
    );

    installation.stop().await;
}

/// A load balancer on another host terminates HTTPS and forwards WebSocket
/// upgrades straight to the broker's plaintext listener.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_nats_websocket_behind_a_proxy_accepts_the_proxy() {
    let directory = tempfile::tempdir().unwrap();
    let mut installation = Installation::start(
        directory.path(),
        &[
            "--tls",
            "generated",
            "--trusted-proxy",
            "127.0.0.0/8",
            "--nats-websocket-url",
            "wss://trellis.example.test",
            "--nats-websocket-behind-proxy",
            "--nats-websocket-only",
        ],
    )
    .await;
    installation.wait_for_websocket().await;

    let mut proxied = connect_from_elsewhere(installation.ports[3])
        .await
        .expect("the proxy could not reach the broker's WebSocket listener");
    proxied
        .write_all(
            b"GET / HTTP/1.1\r\nHost: trellis.example.test\r\nConnection: Upgrade\r\n\
            Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
            Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = [0; 12];
    tokio::time::timeout(Duration::from_secs(10), proxied.read_exact(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&response, b"HTTP/1.1 101");

    installation.stop().await;
}
