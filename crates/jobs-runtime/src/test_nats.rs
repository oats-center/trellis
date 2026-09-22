use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub struct TestNats {
    _directory: TempDir,
    server: Child,
    pub client: async_nats::Client,
}

impl TestNats {
    pub async fn start() -> Self {
        let directory = tempfile::tempdir().expect("temporary NATS directory should be created");
        let port = free_port();
        let mut server = Command::new(nats_server())
            .args(["-js", "-p", &port.to_string(), "-sd"])
            .arg(directory.path().join("data"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("nats-server should start");
        let url = format!("nats://127.0.0.1:{port}");
        let started = Instant::now();
        let client = loop {
            if let Ok(client) = async_nats::connect(&url).await {
                break client;
            }
            assert!(
                server
                    .try_wait()
                    .expect("NATS process should be observable")
                    .is_none(),
                "nats-server exited before accepting connections"
            );
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "nats-server did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        Self {
            _directory: directory,
            server,
            client,
        }
    }
}

fn nats_server() -> PathBuf {
    if let Some(path) = std::env::var_os("NATS_SERVER_BIN") {
        return path.into();
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("nats-server"))
            .find(|candidate| candidate.is_file())
    }) {
        return path;
    }
    let cache = std::env::var_os("TRELLIS_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/trellis")))
        .expect("HOME, TRELLIS_CACHE_DIR, or NATS_SERVER_BIN is required");
    std::fs::read_dir(cache)
        .expect("read Trellis cache")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("nats-server-v") && !name.ends_with(".gz"))
        })
        .max()
        .expect("install nats-server or populate the Trellis cache")
}

impl Drop for TestNats {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("ephemeral port should bind")
        .local_addr()
        .expect("ephemeral port should have an address")
        .port()
}
