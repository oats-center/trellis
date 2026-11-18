use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream};
use trellis_events_runtime::dead_letters::{
    dead_letter_id, DeadLetterState, OriginalEvent, ReplayEnvelope, DLQ_STREAM, DLQ_SUBJECTS,
    REPLAY_STREAM, REPLAY_SUBJECTS,
};
use trellis_events_runtime::{
    start_dead_letter_projector, start_exhaustion_advisory_loop, start_replay_dispatcher,
    ConsumerBinding, ConsumerBindingResolver, DeadLetterJournal, DeliveryReporter, EventsStore,
};
use trellis_rs::service::EventVerificationFailure;

mod common;
use common::start_nats;

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
async fn retained_replay_survives_process_boundaries() {
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
    let source_sequence = context
        .publish("events.test.Created", "payload".into())
        .await
        .expect("publish source")
        .await
        .expect("confirm source")
        .sequence;
    let raw = event_stream.get_raw_message(source_sequence).await.unwrap();
    let published_at = raw
        .time
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let original = OriginalEvent {
        stream: "EVENTS_TEST".to_owned(),
        sequence: source_sequence,
        published_at: published_at.clone(),
        event_id: None,
        subject: raw.subject.to_string(),
        payload_bytes: raw.payload.to_vec(),
        headers: BTreeMap::new(),
        api_id: None,
        event_name: None,
        context_digest: None,
        verification_status: "malformed".to_owned(),
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
    assert_eq!(envelope.original_payload_bytes, b"payload");
    // Source retention and a new journal handle must not replace original broker
    // chronology with the later replay publication's timestamp.
    event_stream.delete_message(source_sequence).await.unwrap();
    let reopened = DeadLetterJournal::open(client.clone()).await.unwrap();
    let retained = reopened.original(&replaying).await.unwrap().unwrap();
    assert_eq!(retained.published_at, published_at);
    assert_eq!(retained.sequence, source_sequence);
    assert_eq!(retained.stream, "EVENTS_TEST");
    assert_eq!(retained.payload_bytes, envelope.original_payload_bytes);
    assert!(replay.time > raw.time);

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
        .report_replay_outcome(
            &created.id,
            1,
            trellis_events_runtime::dead_letters::DeliveryOutcome::Succeeded,
            1,
            None,
        )
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

#[tokio::test]
async fn retention_purges_settled_dead_letters_and_keeps_open_ones() {
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
    let journal = DeadLetterJournal::open(client.clone())
        .await
        .expect("open journal");
    let original = |sequence: u64| OriginalEvent {
        stream: "trellis".to_owned(),
        sequence,
        published_at: "2026-01-01T00:00:00Z".to_owned(),
        event_id: None,
        subject: "events.v1.orders.Created".to_owned(),
        payload_bytes: b"payload".to_vec(),
        headers: BTreeMap::new(),
        api_id: None,
        event_name: None,
        context_digest: None,
        verification_status: "malformed".to_owned(),
    };
    let mut ids = Vec::new();
    for sequence in 1..=3 {
        let (created, _) = journal
            .create_dead("consumer".to_owned(), original(sequence), 5, None)
            .await
            .expect("create dead letter");
        ids.push(created.id);
    }
    let (resolved, dismissed, open) = (&ids[0], &ids[1], &ids[2]);
    journal
        .request_replay(resolved, 1, "replay".to_owned(), "digest".to_owned())
        .await
        .expect("request replay");
    journal
        .report_replay_outcome(
            resolved,
            1,
            trellis_events_runtime::dead_letters::DeliveryOutcome::Succeeded,
            1,
            None,
        )
        .await
        .expect("resolve");
    journal
        .dismiss(dismissed, 1, "dismiss".to_owned(), "digest".to_owned())
        .await
        .expect("dismiss");

    let store = EventsStore::open_in_memory().expect("open projection");
    let projector = start_dead_letter_projector(client.clone(), store.clone())
        .await
        .expect("project journal");
    for (id, state) in [
        (resolved, "resolved"),
        (dismissed, "dismissed"),
        (open, "dead"),
    ] {
        assert_eq!(
            store.inspect_dead_letter(id).unwrap().unwrap()["deadLetter"]["state"],
            state
        );
    }

    // Recorded-event history is kept; settled entries are past a zero retention.
    let pass = trellis_events_runtime::apply_retention(
        &store,
        &journal,
        Duration::from_secs(24 * 60 * 60),
        Duration::ZERO,
    )
    .await
    .expect("apply retention");
    assert_eq!(pass.dead_letters, 2);
    for id in [resolved, dismissed] {
        assert!(journal.latest(id).await.unwrap().is_none());
        assert!(store.inspect_dead_letter(id).unwrap().is_none());
    }
    assert!(journal.latest(open).await.unwrap().is_some());
    assert!(store.inspect_dead_letter(open).unwrap().is_some());
    assert!(matches!(
        journal
            .request_replay(dismissed, 2, "late".to_owned(), "digest".to_owned())
            .await,
        Err(trellis_events_runtime::dead_letters::JournalError::NotFound(_))
    ));
    projector.stop().await;

    // A projection rebuilt from the purged journal catches up past the gaps.
    let rebuilt = EventsStore::open_in_memory().expect("open rebuilt projection");
    let rebuilt_projector = start_dead_letter_projector(client, rebuilt.clone())
        .await
        .expect("rebuild projection");
    assert!(rebuilt.inspect_dead_letter(open).unwrap().is_some());
    assert!(rebuilt.inspect_dead_letter(resolved).unwrap().is_none());
    rebuilt_projector.stop().await;
}

/// A retention sweep that selected a dismissed entry before a replay of it was
/// accepted purges through the sequence it saw, the original among it. The
/// replay must settle as dead with a reason the operator can act on, and a
/// projection rebuilt from the purged journal must start and hold the same
/// entry as the live one.
#[tokio::test]
async fn a_replay_accepted_during_a_retention_sweep_settles_as_unreplayable() {
    let (_server, _directory, client) = start_nats().await;
    let context = jetstream::new(client.clone());
    for (name, subjects, per_subject) in [
        (DLQ_STREAM, DLQ_SUBJECTS, -1),
        (REPLAY_STREAM, REPLAY_SUBJECTS, 1),
    ] {
        context
            .create_stream(stream::Config {
                name: name.to_owned(),
                subjects: vec![subjects.to_owned()],
                storage: stream::StorageType::Memory,
                retention: stream::RetentionPolicy::Limits,
                discard: stream::DiscardPolicy::New,
                max_messages_per_subject: per_subject,
                allow_direct: true,
                ..Default::default()
            })
            .await
            .expect("create dead-letter stream");
    }
    let journal = DeadLetterJournal::open(client.clone())
        .await
        .expect("open journal");
    let (created, _) = journal
        .create_dead(
            "consumer".to_owned(),
            OriginalEvent {
                stream: "trellis".to_owned(),
                sequence: 7,
                published_at: "2026-01-01T00:00:00Z".to_owned(),
                event_id: None,
                subject: "events.v1.orders.Created".to_owned(),
                payload_bytes: b"payload".to_vec(),
                headers: BTreeMap::new(),
                api_id: None,
                event_name: None,
                context_digest: None,
                verification_status: "malformed".to_owned(),
            },
            5,
            None,
        )
        .await
        .expect("create dead letter");
    journal
        .dismiss(&created.id, 1, "dismiss".to_owned(), "digest".to_owned())
        .await
        .expect("dismiss");
    let store = EventsStore::open_in_memory().expect("open projection");
    let projector = start_dead_letter_projector(client.clone(), store.clone())
        .await
        .expect("project journal");
    assert_eq!(
        store.inspect_dead_letter(&created.id).unwrap().unwrap()["deadLetter"]["state"],
        "dismissed"
    );

    // The projection has not yet seen the replay when the sweep runs, so the
    // sweep selects the entry as dismissed and purges through that record.
    projector.stop().await;
    journal
        .request_replay(&created.id, 2, "replay".to_owned(), "digest".to_owned())
        .await
        .expect("accept replay");
    let pass = trellis_events_runtime::apply_retention(
        &store,
        &journal,
        Duration::from_secs(24 * 60 * 60),
        Duration::ZERO,
    )
    .await
    .expect("apply retention");
    assert_eq!(pass.dead_letters, 0, "the replayed entry is kept");

    let projector = start_dead_letter_projector(client.clone(), store.clone())
        .await
        .expect("resume projection");
    let dispatcher = start_replay_dispatcher(client.clone(), journal.clone())
        .await
        .expect("start replay dispatcher");
    let settled = wait_for_state(&journal, &created.id, DeadLetterState::Dead).await;
    assert_eq!(settled.generation, 1);
    assert_eq!(
        settled.last_error.as_deref(),
        Some(trellis_events_runtime::dead_letters::ORIGINAL_REMOVED_ERROR)
    );
    assert_eq!(
        context
            .get_stream(REPLAY_STREAM)
            .await
            .expect("open replay stream")
            .info()
            .await
            .expect("replay stream info")
            .state
            .messages,
        0,
        "nothing was dispatched"
    );
    let mut live = None;
    for _ in 0..100 {
        live = store
            .inspect_dead_letter(&created.id)
            .unwrap()
            .filter(|detail| detail["deadLetter"]["revision"] == settled.revision);
        if live.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let live = live.expect("the live projection reaches the settled revision");
    assert_eq!(live["deadLetter"]["state"], "dead");
    assert_eq!(
        live["deadLetter"]["lastError"],
        trellis_events_runtime::dead_letters::ORIGINAL_REMOVED_ERROR
    );
    assert!(matches!(
        journal
            .request_replay(
                &created.id,
                settled.revision,
                "again".to_owned(),
                "digest".to_owned()
            )
            .await,
        Err(trellis_events_runtime::dead_letters::JournalError::UnreplayableOriginal)
    ));
    dispatcher.stop().await;
    projector.stop().await;

    let rebuilt = EventsStore::open_in_memory().expect("open rebuilt projection");
    let rebuilt_projector = start_dead_letter_projector(client, rebuilt.clone())
        .await
        .expect("rebuild projection from the purged journal");
    assert_eq!(
        rebuilt.inspect_dead_letter(&created.id).unwrap(),
        Some(live),
        "a rebuild holds the entry the live projection does"
    );
    rebuilt_projector.stop().await;
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
