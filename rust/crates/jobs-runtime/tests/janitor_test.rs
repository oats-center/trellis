use serde_json::json;
use trellis_jobs_runtime::plan_expired_events;
use trellis_rs::jobs::{Job, JobContext, JobEventType, JobState};

fn sample_job(id: &str, state: JobState, deadline: Option<&str>) -> Job {
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
        tries: 2,
        max_tries: 5,
        last_error: None,
        error_detail: None,
        deadline: deadline.map(ToString::to_string),
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
fn plan_expired_events_selects_only_overdue_non_terminal_jobs() {
    let jobs = vec![
        (
            "documents.document-process.overdue-active".to_string(),
            sample_job(
                "overdue-active",
                JobState::Active,
                Some("2026-03-28T12:00:00.000Z"),
            ),
        ),
        (
            "documents.document-process.future-active".to_string(),
            sample_job(
                "future-active",
                JobState::Active,
                Some("2026-03-28T12:02:00.000Z"),
            ),
        ),
        (
            "documents.document-process.overdue-completed".to_string(),
            sample_job(
                "overdue-completed",
                JobState::Completed,
                Some("2026-03-28T12:00:00.000Z"),
            ),
        ),
        (
            "documents.document-process.no-deadline".to_string(),
            sample_job("no-deadline", JobState::Pending, None),
        ),
    ];

    let planned = plan_expired_events(&jobs, "2026-03-28T12:01:00.000Z", "job exceeded deadline");

    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].key, "documents.document-process.overdue-active");
    assert_eq!(
        planned[0].subject,
        "trellis.jobs.documents.document-process.overdue-active.expired"
    );
    assert_eq!(planned[0].event.event_type, JobEventType::Expired);
    assert_eq!(planned[0].event.state, JobState::Expired);
    assert_eq!(planned[0].event.previous_state, Some(JobState::Active));
    assert_eq!(planned[0].event.tries, 2);
    assert_eq!(
        planned[0].event.error.as_deref(),
        Some("job exceeded deadline")
    );
}

#[test]
fn plan_expired_events_selects_pending_and_retry_when_overdue() {
    let jobs = vec![
        (
            "documents.document-process.overdue-pending".to_string(),
            sample_job(
                "overdue-pending",
                JobState::Pending,
                Some("2026-03-28T12:00:00.000Z"),
            ),
        ),
        (
            "documents.document-process.overdue-retry".to_string(),
            sample_job(
                "overdue-retry",
                JobState::Retry,
                Some("2026-03-28T12:00:00.000Z"),
            ),
        ),
    ];

    let planned = plan_expired_events(&jobs, "2026-03-28T12:01:00.000Z", "job exceeded deadline");

    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].event.event_type, JobEventType::Expired);
    assert_eq!(planned[1].event.event_type, JobEventType::Expired);
}
