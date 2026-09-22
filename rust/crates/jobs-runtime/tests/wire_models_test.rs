use serde_json::json;
use trellis_rs::jobs::{Job, JobContext, JobState};

#[test]
fn job_model_serializes_expected_wire_keys() {
    let job = Job {
        id: "job-1".to_string(),
        context: context(),
        service: "documents".to_string(),
        job_type: "document-process".to_string(),
        state: JobState::Pending,
        payload: json!({ "documentId": "doc-1" }),
        result: None,
        created_at: "2026-03-28T12:00:00.000Z".to_string(),
        updated_at: "2026-03-28T12:00:00.000Z".to_string(),
        started_at: None,
        completed_at: None,
        tries: 0,
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
    };
    let job_json = serde_json::to_value(job).expect("serialize job");
    assert_eq!(job_json.get("type"), Some(&json!("document-process")));
    assert_eq!(
        job_json.pointer("/context/requestId"),
        Some(&json!("request-job-1"))
    );
    assert_eq!(job_json.get("maxTries"), Some(&json!(5)));
    assert!(job_json.get("job_type").is_none());
}

fn context() -> JobContext {
    JobContext {
        request_id: "request-job-1".to_string(),
        trace_id: "0123456789abcdef0123456789abcdef".to_string(),
        traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
        tracestate: None,
    }
}
