//! A real `nats-server` with JetStream for Events integration tests.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub async fn start_nats() -> (Server, tempfile::TempDir, async_nats::Client) {
    // Held until the broker's listener is up; see ReservedPort.
    let reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
    let port = reservation.port();
    let directory = tempfile::tempdir().expect("temporary NATS state");
    let server = Server(
        Command::new(nats_server())
            .args(["-a", "127.0.0.1", "-p", &port.to_string(), "-js"])
            .arg("-sd")
            .arg(directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn nats-server"),
    );
    let url = format!("nats://127.0.0.1:{port}");
    for _ in 0..100 {
        if let Ok(client) = async_nats::connect(&url).await {
            return (server, directory, client);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("NATS did not start");
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
