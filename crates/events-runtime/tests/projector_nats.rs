use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, stream};
use bytes::Bytes;
use trellis_events_runtime::{
    start_events_projector, EventVerifier, EventsRuntime, EventsStore, VerifiedEventPublisher,
};

const EVENT_SUBJECT: &str = "events.v1.cnVudGltZS10cmVsbGlzLmV2ZW50c0B2MQ.Alpha";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_nats(max_messages: i64) -> (Server, tempfile::TempDir, async_nats::Client) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);
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
    let client = loop {
        if let Ok(client) = async_nats::connect(&url).await {
            break client;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    jetstream::new(client.clone())
        .create_stream(stream::Config {
            name: "trellis".to_owned(),
            subjects: vec!["events.v1.>".to_owned()],
            storage: stream::StorageType::Memory,
            max_messages,
            ..Default::default()
        })
        .await
        .expect("create Events stream");
    (server, directory, client)
}

async fn publish_event(client: &async_nats::Client, event_id: &str) -> u64 {
    let mut headers = async_nats::HeaderMap::new();
    for (name, value) in [
        ("Nats-Msg-Id", event_id),
        ("Trellis-Event-Time", "2026-01-01T00:00:00Z"),
        ("session-key", "session"),
        ("proof", "proof"),
        ("authorization-context", "context"),
    ] {
        headers.insert(name, value);
    }
    jetstream::new(client.clone())
        .publish_with_headers(EVENT_SUBJECT, headers, Bytes::from_static(b"{}"))
        .await
        .expect("publish event")
        .await
        .expect("confirm event")
        .sequence
}

fn verifier(delay_first: bool) -> EventVerifier {
    Arc::new(move |input| {
        Box::pin(async move {
            if delay_first && input.event_id == "event-1" {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(VerifiedEventPublisher {
                kind: "service".to_owned(),
                deployment_id: Some("deployment".to_owned()),
                instance_id: Some("instance".to_owned()),
                participant_id: "runtime-trellis.events".to_owned(),
                principal_id: "principal".to_owned(),
                connection_id: "connection".to_owned(),
                login_session_id: None,
                context_digest: "context".to_owned(),
                owner_contract_id: "runtime-trellis.events@v1".to_owned(),
                owner_event_name: "Alpha".to_owned(),
            })
        })
    })
}

async fn wait_for_projection(store: &EventsStore, sequence: u64) {
    for _ in 0..200 {
        if store
            .contains_stream_sequence(sequence)
            .expect("inspect projection")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("event sequence {sequence} was not projected");
}

#[tokio::test]
async fn delayed_verification_commits_in_source_sequence_order() {
    let (_server, _directory, client) = start_nats(-1).await;
    publish_event(&client, "event-1").await;
    let last = publish_event(&client, "event-2").await;
    let store = EventsStore::open_in_memory().expect("open projection");
    let projector = start_events_projector(
        EventsRuntime::from_nats(client),
        store.clone(),
        verifier(true),
    )
    .await
    .expect("start projector");

    wait_for_projection(&store, last).await;
    projector.stop().await;
    assert_eq!(
        store.diagnostics().expect("diagnostics")["gapDetected"],
        false
    );
}

#[tokio::test]
async fn projector_records_a_startup_retention_gap() {
    let (_server, _directory, client) = start_nats(1).await;
    publish_event(&client, "event-1").await;
    let retained = publish_event(&client, "event-2").await;
    let projection = tempfile::tempdir().expect("temporary projection");
    let path = projection.path().join("events.sqlite");
    let store = EventsStore::open(&path).expect("open projection");
    let projector = start_events_projector(
        EventsRuntime::from_nats(client),
        store.clone(),
        verifier(false),
    )
    .await
    .expect("start projector");

    wait_for_projection(&store, retained).await;
    projector.stop().await;
    drop(store);
    assert_eq!(
        EventsStore::open(path)
            .expect("reopen projection")
            .diagnostics()
            .expect("diagnostics")["gapDetected"],
        true
    );
}

#[tokio::test]
async fn projector_records_a_live_retention_gap() {
    let (_server, _directory, client) = start_nats(-1).await;
    let first = publish_event(&client, "event-1").await;
    let store = EventsStore::open_in_memory().expect("open projection");
    let runtime = EventsRuntime::from_nats(client.clone());
    let projector = start_events_projector(runtime.clone(), store.clone(), verifier(false))
        .await
        .expect("start projector");
    wait_for_projection(&store, first).await;
    projector.stop().await;

    let context = jetstream::new(client.clone());
    let mut config = context
        .get_stream("trellis")
        .await
        .expect("open stream")
        .info()
        .await
        .expect("stream info")
        .config
        .clone();
    config.max_messages = 1;
    context
        .update_stream(config)
        .await
        .expect("truncate stream");
    publish_event(&client, "event-2").await;
    let retained = publish_event(&client, "event-3").await;

    let projector = start_events_projector(runtime, store.clone(), verifier(false))
        .await
        .expect("restart projector");
    wait_for_projection(&store, retained).await;
    projector.stop().await;
    assert_eq!(
        store.diagnostics().expect("diagnostics")["gapDetected"],
        true
    );
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
