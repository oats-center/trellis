use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream, AckKind};
use futures_util::StreamExt;
use trellis_events_runtime::dead_letters::{
    dead_letter_id, DeadLetterState, DeliveryOutcome, DeliveryReport, OriginalEvent,
    ReplayEnvelope, DLQ_STREAM, DLQ_SUBJECTS, REPLAY_STREAM, REPLAY_SUBJECTS,
};
use trellis_events_runtime::{
    start_dead_letter_projector, start_exhaustion_advisory_loop, start_replay_dispatcher,
    ConsumerBinding, ConsumerBindingResolver, DeadLetterJournal, DeliveryReportError,
    DeliveryReporter, EventsStore,
};
use trellis_rs::service::EventVerificationFailure;

struct Server(Child);

struct Resolver(ConsumerBinding);

impl ConsumerBindingResolver for Resolver {
    fn by_resource<'a>(
        &'a self,
        resource_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>> {
        Box::pin(async move { Ok((self.0.resource_id == resource_id).then(|| self.0.clone())) })
    }

    fn by_consumer<'a>(
        &'a self,
        stream: &'a str,
        consumer_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ConsumerBinding>, String>> + Send + 'a>> {
        Box::pin(async move {
            Ok(
                (self.0.stream == stream && self.0.consumer_name == consumer_name)
                    .then(|| self.0.clone()),
            )
        })
    }

    fn all(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ConsumerBinding>, String>> + Send + '_>> {
        Box::pin(async { Ok(vec![self.0.clone()]) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_nats() -> (Server, tempfile::TempDir, async_nats::Client) {
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
    for _ in 0..100 {
        if let Ok(client) = async_nats::connect(&url).await {
            return (server, directory, client);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("NATS did not start");
}

#[tokio::test]
async fn truncated_stream_bounds_leave_durable_gap_evidence() {
    let (_server, _nats_directory, client) = start_nats().await;
    let context = jetstream::new(client);
    let mut stream = context
        .create_stream(stream::Config {
            name: "EVENTS_RETENTION_TEST".to_owned(),
            subjects: vec!["events.retention.>".to_owned()],
            storage: stream::StorageType::Memory,
            max_messages: 1,
            ..Default::default()
        })
        .await
        .expect("create retained event stream");
    for payload in ["old", "retained"] {
        context
            .publish("events.retention.Changed", payload.into())
            .await
            .expect("publish event")
            .await
            .expect("confirm event");
    }
    let first_sequence = stream
        .info()
        .await
        .expect("stream info")
        .state
        .first_sequence;
    assert_eq!(first_sequence, 2);

    let projection_directory = tempfile::tempdir().expect("temporary projection");
    let path = projection_directory.path().join("events.sqlite");
    EventsStore::open(&path)
        .expect("open projection")
        .record_stream_bounds(first_sequence)
        .expect("record stream bounds");
    let diagnostics = EventsStore::open(&path)
        .expect("reopen projection")
        .diagnostics()
        .expect("read diagnostics");
    assert_eq!(diagnostics["gapDetected"], true);
}

#[tokio::test]
async fn exhaustion_advisory_journals_before_ack_and_records_expired_source_gap() {
    let (_server, _directory, client) = start_nats().await;
    let context = jetstream::new(client.clone());
    context
        .create_stream(stream::Config {
            name: DLQ_STREAM.to_owned(),
            subjects: vec![DLQ_SUBJECTS.to_owned()],
            storage: stream::StorageType::Memory,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create journal stream");
    context
        .create_stream(stream::Config {
            name: "EVENTS_ADVISORY_TEST".to_owned(),
            subjects: vec!["events.advisory.>".to_owned()],
            storage: stream::StorageType::Memory,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create source stream");
    let mut capture = context
        .create_stream(stream::Config {
            name: "EVENTS_ADVISORY_CAPTURE".to_owned(),
            subjects: vec!["$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>".to_owned()],
            storage: stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create advisory capture stream");
    let source_sequence = context
        .publish("events.advisory.Created", "payload".into())
        .await
        .expect("publish source")
        .await
        .expect("confirm source")
        .sequence;
    let binding = ConsumerBinding {
        resource_id: "consumer-advisory".to_owned(),
        owner_kind: "deployment".to_owned(),
        owner_id: "deployment".to_owned(),
        participant_id: "participant".to_owned(),
        local_name: "events".to_owned(),
        stream: "EVENTS_ADVISORY_TEST".to_owned(),
        consumer_name: "consumer-advisory".to_owned(),
        filter_subjects: vec!["events.advisory.>".to_owned()],
        replay_consumer_name: "replay-advisory".to_owned(),
    };
    let resolver = Arc::new(Resolver(binding.clone()));
    let journal = DeadLetterJournal::open(client.clone())
        .await
        .expect("open journal");
    let reporter = DeliveryReporter::new(
        client.clone(),
        journal.clone(),
        resolver,
        Arc::new(|_| {
            Box::pin(async { Err(EventVerificationFailure::Rejected("unverified".to_owned())) })
        }),
    );
    let store = EventsStore::open_in_memory().expect("open projection");
    let mut advisory_loop = start_exhaustion_advisory_loop(
        client.clone(),
        "EVENTS_ADVISORY_CAPTURE",
        reporter.clone(),
        store.clone(),
    )
    .await
    .expect("start advisory loop");
    let advisory_subject =
        "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.EVENTS_ADVISORY_TEST.consumer-advisory";
    let advisory = |id: &str, sequence: u64| {
        serde_json::to_vec(&serde_json::json!({
            "type": "io.nats.jetstream.advisory.v1.max_deliver",
            "id": id,
            "stream": "EVENTS_ADVISORY_TEST",
            "consumer": "consumer-advisory",
            "stream_seq": sequence,
            "deliveries": 4,
            "timestamp": "2026-03-28T12:05:00Z"
        }))
        .expect("encode advisory")
    };
    let advisory_sequence = context
        .publish(
            advisory_subject,
            advisory("advisory-1", source_sequence).into(),
        )
        .await
        .expect("publish advisory")
        .await
        .expect("confirm advisory")
        .sequence;
    let id = dead_letter_id(&binding.resource_id, &binding.stream, source_sequence);
    wait_for_journal(&journal, &id).await;
    wait_for_advisory_ack(&mut capture, advisory_sequence).await;

    advisory_loop.stop().await;
    capture
        .delete_consumer("events-advisories")
        .await
        .expect("force advisory redelivery");
    advisory_loop = start_exhaustion_advisory_loop(
        client.clone(),
        "EVENTS_ADVISORY_CAPTURE",
        reporter,
        store.clone(),
    )
    .await
    .expect("restart advisory loop");
    wait_for_advisory_ack(&mut capture, advisory_sequence).await;
    assert_eq!(
        context
            .get_stream(DLQ_STREAM)
            .await
            .expect("open journal stream")
            .info()
            .await
            .expect("journal info")
            .state
            .messages,
        1
    );

    context
        .publish(
            advisory_subject,
            advisory("advisory-expired", source_sequence + 100).into(),
        )
        .await
        .expect("publish expired-source advisory")
        .await
        .expect("confirm expired-source advisory");
    for _ in 0..100 {
        if store.diagnostics().expect("diagnostics")["gapDetected"] == true {
            advisory_loop.stop().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("expired source did not produce operator-visible gap diagnostics");
}

#[tokio::test]
async fn fabricated_delivery_is_rejected_and_replay_survives_process_boundaries() {
    let (_server, _directory, client) = start_nats().await;
    let context = jetstream::new(client.clone());
    context
        .create_stream(stream::Config {
            name: DLQ_STREAM.to_owned(),
            subjects: vec![DLQ_SUBJECTS.to_owned()],
            storage: stream::StorageType::Memory,
            retention: stream::RetentionPolicy::Limits,
            discard: stream::DiscardPolicy::New,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create journal stream");
    context
        .create_stream(stream::Config {
            name: REPLAY_STREAM.to_owned(),
            subjects: vec![REPLAY_SUBJECTS.to_owned()],
            storage: stream::StorageType::Memory,
            retention: stream::RetentionPolicy::Limits,
            discard: stream::DiscardPolicy::New,
            max_messages_per_subject: 1,
            allow_direct: true,
            ..Default::default()
        })
        .await
        .expect("create replay stream");

    let journal = DeadLetterJournal::open(client.clone())
        .await
        .expect("open journal");
    context
        .create_stream(stream::Config {
            name: "EVENTS_TEST".to_owned(),
            subjects: vec!["events.test.>".to_owned()],
            storage: stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create event stream");
    let event_stream = context
        .get_stream("EVENTS_TEST")
        .await
        .expect("open event stream");
    let consumer = event_stream
        .create_consumer(consumer::pull::Config {
            durable_name: Some("consumer-test".to_owned()),
            filter_subject: "events.test.>".to_owned(),
            ack_policy: consumer::AckPolicy::Explicit,
            max_deliver: 4,
            max_ack_pending: 1,
            ..Default::default()
        })
        .await
        .expect("create consumer");
    let source_sequence = context
        .publish("events.test.Created", "payload".into())
        .await
        .expect("publish source")
        .await
        .expect("confirm source")
        .sequence;
    let mut messages = consumer
        .fetch()
        .max_messages(1)
        .messages()
        .await
        .expect("fetch source");
    let pending = messages
        .next()
        .await
        .expect("source delivery")
        .expect("valid source delivery");
    let delivery_proof = pending
        .message
        .reply
        .expect("acknowledgement subject")
        .to_string();
    client
        .request(delivery_proof.clone(), AckKind::Progress.into())
        .await
        .expect("broker confirms pending delivery proof");
    let binding = ConsumerBinding {
        resource_id: "consumer-resource".to_owned(),
        owner_kind: "deployment".to_owned(),
        owner_id: "deployment".to_owned(),
        participant_id: "participant".to_owned(),
        local_name: "events".to_owned(),
        stream: "EVENTS_TEST".to_owned(),
        consumer_name: "consumer-test".to_owned(),
        filter_subjects: vec!["events.test.>".to_owned()],
        replay_consumer_name: "replay-test".to_owned(),
    };
    let reporter = DeliveryReporter::new(
        client.clone(),
        journal.clone(),
        Arc::new(Resolver(binding)),
        Arc::new(|_| {
            Box::pin(async { Err(EventVerificationFailure::Rejected("not reached".to_owned())) })
        }),
    );
    let fabricated = reporter
        .report(
            "consumer-resource",
            DeliveryReport {
                resource_id: "consumer-resource".to_owned(),
                source_stream: "EVENTS_TEST".to_owned(),
                source_sequence,
                delivery_count: 4,
                delivery_proof: Some(delivery_proof),
                replay_generation: None,
                outcome: DeliveryOutcome::Exhausted,
                error: Some("fabricated".to_owned()),
            },
        )
        .await;
    assert!(matches!(
        fabricated,
        Err(DeliveryReportError::Validation(_))
    ));
    let original = OriginalEvent {
        stream: "trellis".to_owned(),
        sequence: 42,
        event_id: Some("event-42".to_owned()),
        subject: "events.v1.orders.Created".to_owned(),
        payload_bytes: br#"{"orderId":"42"}"#.to_vec(),
        headers: BTreeMap::from([("Trellis-Auth-Context".to_owned(), vec!["proof".to_owned()])]),
        api_id: Some("orders@v1".to_owned()),
        event_name: Some("Created".to_owned()),
        context_digest: Some("digest".to_owned()),
        verification_status: "verified".to_owned(),
    };
    let (created, _) = journal
        .create_dead(
            "consumer-resource".to_owned(),
            original.clone(),
            5,
            Some("boom".to_owned()),
        )
        .await
        .expect("create dead letter");
    let duplicate = journal
        .create_dead(
            "consumer-resource".to_owned(),
            original,
            5,
            Some("boom".to_owned()),
        )
        .await
        .expect("deduplicate dead letter");
    assert_eq!(duplicate.0.id, created.id);
    assert_eq!(duplicate.0.revision, 1);

    let request = journal
        .request_replay(
            &created.id,
            1,
            "request-1".to_owned(),
            "digest-1".to_owned(),
        )
        .await
        .expect("request replay");
    let dispatcher = start_replay_dispatcher(client.clone(), journal.clone())
        .await
        .expect("start replay dispatcher");
    let replaying = wait_for_state(&journal, &created.id, DeadLetterState::Replaying).await;
    assert_eq!(replaying.generation, 1);
    assert!(replaying.replay_stream_sequence.is_some());
    let duplicate_request = journal
        .request_replay(
            &created.id,
            1,
            "request-1".to_owned(),
            "digest-1".to_owned(),
        )
        .await
        .expect("resolve duplicate request");
    assert_eq!(duplicate_request.0, request.0);

    let replay_stream = context
        .get_stream(REPLAY_STREAM)
        .await
        .expect("open replay stream");
    let replay = replay_stream
        .get_raw_message(replaying.replay_stream_sequence.expect("replay sequence"))
        .await
        .expect("read replay record");
    let envelope: ReplayEnvelope = serde_json::from_slice(&replay.payload).expect("decode replay");
    assert_eq!(envelope.dead_letter_id, created.id);
    assert_eq!(envelope.original_payload_bytes, br#"{"orderId":"42"}"#);

    let store = EventsStore::open_in_memory().expect("open projection");
    let projector = start_dead_letter_projector(client, store.clone())
        .await
        .expect("catch up journal projection");
    assert_eq!(
        store
            .inspect_dead_letter(&created.id)
            .expect("inspect projected replay")
            .expect("projected replay")["deadLetter"]["state"],
        "replaying"
    );
    journal
        .report_replay_outcome(&created.id, 1, true, 1, None)
        .await
        .expect("resolve replay");
    for _ in 0..100 {
        if store
            .inspect_dead_letter(&created.id)
            .expect("inspect resolved replay")
            .is_some_and(|detail| detail["deadLetter"]["state"] == "resolved")
        {
            projector.stop().await;
            dispatcher.stop().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("resolved transition was not projected");
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

async fn wait_for_state(
    journal: &DeadLetterJournal,
    id: &str,
    expected: DeadLetterState,
) -> trellis_events_runtime::dead_letters::DeadLetterTransition {
    for _ in 0..100 {
        let current = journal
            .latest(id)
            .await
            .expect("read latest transition")
            .expect("dead letter exists")
            .0;
        if current.state == expected {
            return current;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("dead letter did not reach {expected:?}");
}

async fn wait_for_journal(journal: &DeadLetterJournal, id: &str) {
    for _ in 0..100 {
        if journal.latest(id).await.expect("read journal").is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("dead letter was not journaled");
}

async fn wait_for_advisory_ack(capture: &mut stream::Stream, sequence: u64) {
    for _ in 0..100 {
        let mut consumer = capture
            .get_consumer::<consumer::pull::Config>("events-advisories")
            .await
            .expect("open advisory consumer");
        if consumer
            .info()
            .await
            .expect("advisory consumer info")
            .ack_floor
            .stream_sequence
            >= sequence
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("advisory was not acknowledged");
}
