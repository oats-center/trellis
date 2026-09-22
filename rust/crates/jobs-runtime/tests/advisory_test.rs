use serde_json::json;
use trellis_jobs_runtime::{map_dead_event_from_advisory_job, MaxDeliveriesAdvisory};
use trellis_rs::jobs::{Job, JobContext, JobEventType, JobState};

fn sample_job(id: &str, state: JobState, tries: u64) -> Job {
    Job {
        id: id.to_string(),
        context: context(id),
        service: "documents".to_string(),
        job_type: "document-process".to_string(),
        state,
        payload: json!({ "documentId": id }),
        result: None,
        created_at: "2026-03-28T11:00:00.000Z".to_string(),
        updated_at: "2026-03-28T11:00:00.000Z".to_string(),
        started_at: None,
        completed_at: None,
        tries,
        max_tries: 5,
        last_error: None,
        error_detail: None,
        deadline: None,
        progress: None,
        logs: None,
        concurrency: None,
        queue_policy: None,
        trigger: None,
        lineage: None,
        waiting_on: None,
    }
}

fn context(id: &str) -> JobContext {
    JobContext {
        request_id: format!("request-{id}"),
        trace_id: "0123456789abcdef0123456789abcdef".to_string(),
        traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
        tracestate: None,
    }
}

#[test]
fn max_deliveries_advisory_parses_supported_spellings() {
    let expected = MaxDeliveriesAdvisory {
        advisory_type: "io.nats.jetstream.advisory.v1.max_deliver".to_owned(),
        id: "advisory-1".to_owned(),
        stream: "JOBS_WORK".to_string(),
        consumer: "documents-document-process".to_string(),
        stream_seq: 41,
        deliveries: 3,
        timestamp: "2026-03-28T12:05:00.000Z".to_string(),
    };

    assert!(serde_json::from_value::<MaxDeliveriesAdvisory>(json!({
        "stream": "JOBS_WORK",
        "consumer": "documents-document-process",
        "timestamp": "2026-03-28T12:05:00.000Z"
    }))
    .is_err());

    for raw in [
        json!({
            "type": "io.nats.jetstream.advisory.v1.max_deliver",
            "id": "advisory-1",
            "stream": "JOBS_WORK",
            "consumer": "documents-document-process",
            "stream_seq": 41,
            "deliveries": 3,
            "timestamp": "2026-03-28T12:05:00.000Z"
        }),
        json!({
            "type": "io.nats.jetstream.advisory.v1.max_deliver",
            "id": "advisory-1",
            "stream": "JOBS_WORK",
            "consumer": "documents-document-process",
            "streamSeq": 41,
            "deliveries": 3,
            "timestamp": "2026-03-28T12:05:00.000Z"
        }),
        json!({
            "type": "io.nats.jetstream.advisory.v1.max_deliver",
            "id": "advisory-1",
            "stream": "JOBS_WORK",
            "consumer": "documents-document-process",
            "stream_seq": 41,
            "num_deliveries": 3,
            "timestamp": "2026-03-28T12:05:00.000Z"
        }),
        json!({
            "type": "io.nats.jetstream.advisory.v1.max_deliver",
            "id": "advisory-1",
            "stream": "JOBS_WORK",
            "consumer": "documents-document-process",
            "streamSeq": 41,
            "num_deliveries": 3,
            "timestamp": "2026-03-28T12:05:00.000Z"
        }),
    ] {
        let advisory =
            serde_json::from_value::<MaxDeliveriesAdvisory>(raw).expect("advisory should parse");

        assert_eq!(advisory, expected);
    }
}

#[test]
fn map_dead_event_from_advisory_job_uses_current_state_and_max_tries() {
    let advisory = MaxDeliveriesAdvisory {
        advisory_type: "io.nats.jetstream.advisory.v1.max_deliver".to_owned(),
        id: "advisory-1".to_owned(),
        stream: "JOBS_WORK".to_string(),
        consumer: "documents-document-process".to_string(),
        stream_seq: 41,
        deliveries: 3,
        timestamp: "2026-03-28T12:05:00.000Z".to_string(),
    };
    let work = sample_job("job-1", JobState::Pending, 0);
    let current = sample_job("job-1", JobState::Active, 2);

    let mapped = map_dead_event_from_advisory_job(Some(&current), &work, &advisory)
        .expect("active job should map to dead event");

    assert_eq!(
        mapped.subject,
        "trellis.jobs.documents.document-process.job-1.dead"
    );
    assert_eq!(mapped.event.event_type, JobEventType::Dead);
    assert_eq!(mapped.event.state, JobState::Dead);
    assert_eq!(mapped.event.previous_state, Some(JobState::Active));
    assert_eq!(mapped.event.tries, 3);
    assert_eq!(mapped.event.timestamp, "2026-03-28T12:05:00.000Z");
    assert_eq!(
        mapped.event.error.as_deref(),
        Some("max deliveries exceeded: stream=JOBS_WORK consumer=documents-document-process deliveries=3"),
    );
}

#[test]
fn map_dead_event_without_projection_does_not_invent_previous_work_state() {
    let advisory = MaxDeliveriesAdvisory {
        advisory_type: "io.nats.jetstream.advisory.v1.max_deliver".to_owned(),
        id: "advisory-1".to_owned(),
        stream: "JOBS_WORK".to_string(),
        consumer: "documents-document-process".to_string(),
        stream_seq: 41,
        deliveries: 3,
        timestamp: "2026-03-28T12:05:00.000Z".to_string(),
    };
    let work = sample_job("job-1", JobState::Active, 99);

    let mapped = map_dead_event_from_advisory_job(None, &work, &advisory)
        .expect("valid advisory should map to a dead event");

    assert_eq!(mapped.event.previous_state, None);
    assert_eq!(mapped.event.tries, 3);
}

#[test]
fn map_dead_event_from_advisory_job_skips_terminal_current_job() {
    let advisory = MaxDeliveriesAdvisory {
        advisory_type: "io.nats.jetstream.advisory.v1.max_deliver".to_owned(),
        id: "advisory-1".to_owned(),
        stream: "JOBS_WORK".to_string(),
        consumer: "documents-document-process".to_string(),
        stream_seq: 41,
        deliveries: 3,
        timestamp: "2026-03-28T12:05:00.000Z".to_string(),
    };
    let work = sample_job("job-1", JobState::Pending, 0);
    let current = sample_job("job-1", JobState::Completed, 2);

    let mapped = map_dead_event_from_advisory_job(Some(&current), &work, &advisory);
    assert_eq!(mapped, None);
}
