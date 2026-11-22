use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, stream};
use serde_json::Value;
use trellis_events_runtime::{
    start_events_projector, ConsumerBinding, ConsumerBindingResolver, EventVerifier, EventsQuery,
    EventsRuntime, EventsStore,
};
use trellis_rs::__internal::FromProviderNats as _;
use trellis_rs::service::EventVerificationFailure;

mod common;
use common::start_nats;

const HELD: &str = "held-event";

struct NoBindings;

impl ConsumerBindingResolver for NoBindings {
    fn by_resource<'a>(
        &'a self,
        _resource_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }

    fn by_consumer<'a>(
        &'a self,
        _stream: &'a str,
        _consumer_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }

    fn all(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ConsumerBinding>, String>> + Send + '_>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// Verification of the held event stays temporarily unavailable until
/// released; every other event gets a final verdict at once.
struct Verification {
    released: Arc<AtomicBool>,
    held_attempts: Arc<AtomicUsize>,
}

fn verifier() -> (EventVerifier, Verification) {
    let released = Arc::new(AtomicBool::new(false));
    let held_attempts = Arc::new(AtomicUsize::new(0));
    let state = Verification {
        released: Arc::clone(&released),
        held_attempts: Arc::clone(&held_attempts),
    };
    let verifier: EventVerifier = Arc::new(move |input| {
        let released = Arc::clone(&released);
        let held_attempts = Arc::clone(&held_attempts);
        Box::pin(async move {
            let id = input
                .headers
                .as_ref()
                .and_then(|headers| headers.get("Nats-Msg-Id"))
                .map(|value| value.as_str().to_owned())
                .unwrap_or_default();
            if id == HELD {
                held_attempts.fetch_add(1, Ordering::SeqCst);
                if !released.load(Ordering::SeqCst) {
                    return Err(EventVerificationFailure::Retryable(
                        "authorization is unavailable".into(),
                    ));
                }
            }
            Err(EventVerificationFailure::Rejected(
                "signature mismatch".into(),
            ))
        })
    });
    (verifier, state)
}

async fn create_event_stream(
    context: &jetstream::Context,
    max_messages: i64,
) -> jetstream::stream::Stream {
    context
        .create_stream(stream::Config {
            name: "trellis".to_owned(),
            subjects: vec!["events.>".to_owned()],
            storage: stream::StorageType::Memory,
            max_messages,
            ..Default::default()
        })
        .await
        .expect("create event stream")
}

async fn publish(context: &jetstream::Context, id: &str) {
    let mut headers = async_nats::HeaderMap::new();
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    for (name, value) in [
        ("Nats-Msg-Id", id),
        ("Trellis-Event-Time", now.as_str()),
        ("Trellis-Event-Descriptor", "descriptor"),
        ("session-key", "session-key"),
        ("authorization-context", "context-digest"),
        ("proof", "proof"),
    ] {
        headers.insert(name, value);
    }
    context
        .publish_with_headers("events.v1.test.Changed", headers, "{}".into())
        .await
        .expect("publish event")
        .await
        .expect("broker stores event");
}

async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn projected(store: &EventsStore, id: &str) -> Option<Value> {
    store
        .inspect_event(Some(id), None)
        .expect("inspect event")
        .map(|inspected| inspected["event"]["row"].clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redelivered_event_fills_its_place_in_history() {
    let (_server, _nats_directory, client) = start_nats().await;
    let context = jetstream::new(client.clone());
    create_event_stream(&context, -1).await;
    let projection_directory = tempfile::tempdir().expect("temporary projection");
    let store = EventsStore::open(
        projection_directory.path().join("events.sqlite"),
        std::time::Duration::from_secs(5),
    )
    .expect("open projection");
    let runtime = EventsRuntime::from_nats(client);
    let query = EventsQuery::new(store.clone(), runtime.clone(), Arc::new(NoBindings));
    let (verifier, verification) = verifier();
    let projector = start_events_projector(runtime, store.clone(), verifier)
        .await
        .expect("start projector");

    publish(&context, HELD).await;
    publish(&context, "later-1").await;
    publish(&context, "later-2").await;
    wait_until("later events to project past the held one", || {
        verification.held_attempts.load(Ordering::SeqCst) > 0
            && projected(&store, "later-2").is_some()
    })
    .await;
    assert!(projected(&store, HELD).is_none());
    let waiting = query.diagnostics().await.expect("diagnostics");
    assert_eq!(waiting["gapDetected"], false);
    assert_eq!(waiting["catchingUp"], true);

    verification.released.store(true, Ordering::SeqCst);
    wait_until("the held event to be redelivered", || {
        projected(&store, HELD).is_some()
    })
    .await;
    // The redelivered event's sequence settles a moment after its row lands.
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let diagnostics = query.diagnostics().await.expect("diagnostics");
            if diagnostics["catchingUp"] == false {
                break diagnostics;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("projection catches up after redelivery");
    assert_eq!(settled["gapDetected"], false);
    assert_eq!(settled["completeSince"], Value::Null);
    projector.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_discarded_before_projection_is_reported_missing() {
    let (_server, _nats_directory, client) = start_nats().await;
    let context = jetstream::new(client.clone());
    // The broker keeps two events; a third discards the oldest.
    let mut stream = create_event_stream(&context, 2).await;
    let projection_directory = tempfile::tempdir().expect("temporary projection");
    let path = projection_directory.path().join("events.sqlite");
    let store =
        EventsStore::open(&path, std::time::Duration::from_secs(5)).expect("open projection");
    let runtime = EventsRuntime::from_nats(client);
    let query = EventsQuery::new(store.clone(), runtime.clone(), Arc::new(NoBindings));
    let (verifier, verification) = verifier();
    let projector = start_events_projector(runtime, store.clone(), verifier)
        .await
        .expect("start projector");

    publish(&context, HELD).await;
    publish(&context, "kept").await;
    wait_until("the kept event to project past the held one", || {
        verification.held_attempts.load(Ordering::SeqCst) > 0 && projected(&store, "kept").is_some()
    })
    .await;
    let waiting = query.diagnostics().await.expect("diagnostics");
    assert_eq!(waiting["gapDetected"], false);
    assert_eq!(waiting["catchingUp"], true);

    publish(&context, "latest").await;
    assert_eq!(
        stream
            .info()
            .await
            .expect("stream info")
            .state
            .first_sequence,
        2,
        "the broker discarded the held event"
    );
    let reported = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let diagnostics = query.diagnostics().await.expect("diagnostics");
            if diagnostics["gapDetected"] == true && diagnostics["catchingUp"] == false {
                break diagnostics;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the projector records the discarded event as missing");
    let kept = projected(&store, "kept").expect("kept event");
    assert_eq!(reported["completeSince"], kept["eventTime"]);
    projector.stop().await;
    drop(store);

    let reopened =
        EventsStore::open(&path, std::time::Duration::from_secs(5)).expect("reopen projection");
    assert_eq!(
        reopened.diagnostics().expect("diagnostics")["gapDetected"],
        true
    );
}
