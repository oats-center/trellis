//! Constructors for Jobs lifecycle events.
//!
//! `EventMeta` borrows the common job identity, context, and timestamp fields
//! used by each constructor. Constructors that represent a current attempt take
//! its attempt count separately.

use serde_json::Value;

use crate::jobs::types::{
    JobAdminAction, JobConcurrency, JobContext, JobErrorDetail, JobEvent, JobEventType,
    JobLogEntry, JobProgress, JobQueuePolicy, JobState, JobWaitEdge,
};

/// Borrowed fields shared by every Jobs lifecycle event.
#[derive(Debug, Clone, Copy)]
pub struct EventMeta<'a> {
    /// Service that owns the job.
    pub service: &'a str,
    /// Service-local job type.
    pub job_type: &'a str,
    /// Stable job identifier.
    pub job_id: &'a str,
    /// Correlation and trace context inherited by the job.
    pub context: &'a JobContext,
    /// RFC 3339 event timestamp.
    pub timestamp: &'a str,
}

fn admin_action(reason: Option<&str>) -> Option<JobAdminAction> {
    reason.map(|reason| JobAdminAction {
        reason: Some(reason.to_string()),
    })
}

fn base_event(
    meta: EventMeta<'_>,
    event_type: JobEventType,
    state: JobState,
    previous_state: Option<JobState>,
    tries: u64,
) -> JobEvent {
    JobEvent {
        job_id: meta.job_id.to_string(),
        context: meta.context.clone(),
        service: meta.service.to_string(),
        job_type: meta.job_type.to_string(),
        event_type,
        state,
        previous_state,
        tries,
        max_tries: None,
        error: None,
        error_detail: None,
        progress: None,
        logs: None,
        payload: None,
        result: None,
        deadline: None,
        concurrency: None,
        queue_policy: None,
        trigger: None,
        lineage: None,
        wait_edge: None,
        admin_action: None,
        timestamp: meta.timestamp.to_string(),
    }
}

/// Construct a `created` lifecycle event.
pub fn created(
    meta: EventMeta<'_>,
    payload: Value,
    max_tries: u64,
    deadline: Option<&str>,
) -> JobEvent {
    let mut event = base_event(meta, JobEventType::Created, JobState::Pending, None, 0);
    event.payload = Some(payload);
    event.max_tries = Some(max_tries);
    event.deadline = deadline.map(ToString::to_string);
    event
}

/// Construct a `created` lifecycle event with keyed concurrency policy metadata.
pub fn created_with_policy(
    meta: EventMeta<'_>,
    payload: Value,
    max_tries: u64,
    deadline: Option<&str>,
    concurrency: Option<JobConcurrency>,
    queue_policy: Option<JobQueuePolicy>,
) -> JobEvent {
    let mut event = created(meta, payload, max_tries, deadline);
    event.concurrency = concurrency;
    event.queue_policy = queue_policy;
    event
}

/// Construct a `started` lifecycle event.
pub fn started(meta: EventMeta<'_>, tries: u64, previous_state: JobState) -> JobEvent {
    base_event(
        meta,
        JobEventType::Started,
        JobState::Active,
        Some(previous_state),
        tries,
    )
}

/// Construct a `started` lifecycle event with active key ownership metadata.
pub fn started_with_concurrency(
    meta: EventMeta<'_>,
    tries: u64,
    previous_state: JobState,
    concurrency: JobConcurrency,
) -> JobEvent {
    let mut event = started(meta, tries, previous_state);
    event.concurrency = Some(concurrency);
    event
}

/// Construct a `retry` lifecycle event.
pub fn retry(
    meta: EventMeta<'_>,
    tries: u64,
    previous_state: JobState,
    error: Option<&str>,
) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Retry,
        JobState::Retry,
        Some(previous_state),
        tries,
    );
    event.error = error.map(ToString::to_string);
    event.error_detail =
        error.map(|message| JobErrorDetail::from_message(meta.service, meta.job_type, message));
    event
}

/// Construct a `progress` lifecycle event.
pub fn progress(meta: EventMeta<'_>, tries: u64, progress: JobProgress) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Progress,
        JobState::Active,
        Some(JobState::Active),
        tries,
    );
    event.progress = Some(progress);
    event
}

/// Construct a `logged` lifecycle event.
pub fn logged(meta: EventMeta<'_>, tries: u64, logs: Vec<JobLogEntry>) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Logged,
        JobState::Active,
        Some(JobState::Active),
        tries,
    );
    event.logs = Some(logs);
    event
}

/// Construct a `waiting` lifecycle evidence event without changing job state.
pub fn waiting(meta: EventMeta<'_>, tries: u64, wait_edge: JobWaitEdge) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Waiting,
        JobState::Active,
        Some(JobState::Active),
        tries,
    );
    event.wait_edge = Some(wait_edge);
    event
}

/// Construct a `resumed` lifecycle evidence event without changing job state.
pub fn resumed(meta: EventMeta<'_>, tries: u64, wait_edge: JobWaitEdge) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Resumed,
        JobState::Active,
        Some(JobState::Active),
        tries,
    );
    event.wait_edge = Some(wait_edge);
    event
}

/// Construct a `completed` lifecycle event.
pub fn completed(meta: EventMeta<'_>, tries: u64, result: Value) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Completed,
        JobState::Completed,
        Some(JobState::Active),
        tries,
    );
    event.result = Some(result);
    event
}

/// Construct a `failed` lifecycle event.
pub fn failed(meta: EventMeta<'_>, tries: u64, previous_state: JobState, error: &str) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Failed,
        JobState::Failed,
        Some(previous_state),
        tries,
    );
    event.error = Some(error.to_string());
    event.error_detail = Some(JobErrorDetail::from_message(
        meta.service,
        meta.job_type,
        error,
    ));
    event
}

/// Construct a `cancelled` lifecycle event.
pub fn cancelled(meta: EventMeta<'_>, tries: u64, previous_state: JobState) -> JobEvent {
    cancelled_by_admin(meta, tries, previous_state, None)
}

/// Construct a `cancelled` lifecycle event with an optional admin reason.
pub fn cancelled_by_admin(
    meta: EventMeta<'_>,
    tries: u64,
    previous_state: JobState,
    reason: Option<&str>,
) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Cancelled,
        JobState::Cancelled,
        Some(previous_state),
        tries,
    );
    event.admin_action = admin_action(reason);
    event
}

/// Construct an `expired` lifecycle event.
pub fn expired(meta: EventMeta<'_>, tries: u64, previous_state: JobState, error: &str) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Expired,
        JobState::Expired,
        Some(previous_state),
        tries,
    );
    event.error = Some(error.to_string());
    event.error_detail = Some(JobErrorDetail::from_message(
        meta.service,
        meta.job_type,
        error,
    ));
    event
}

/// Construct a `skipped` terminal lifecycle event for queued work replaced by policy.
pub fn skipped(meta: EventMeta<'_>, previous_state: JobState, reason: Option<&str>) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Skipped,
        JobState::Skipped,
        Some(previous_state),
        0,
    );
    event.error = reason.map(ToString::to_string);
    event.error_detail =
        reason.map(|message| JobErrorDetail::from_message(meta.service, meta.job_type, message));
    event
}

/// Construct a `stale` terminal lifecycle event for active work that lost its key lease.
pub fn stale(
    meta: EventMeta<'_>,
    tries: u64,
    reason: Option<&str>,
    concurrency: Option<JobConcurrency>,
) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Stale,
        JobState::Stale,
        Some(JobState::Active),
        tries,
    );
    event.error = reason.map(ToString::to_string);
    event.error_detail =
        reason.map(|message| JobErrorDetail::from_message(meta.service, meta.job_type, message));
    event.concurrency = concurrency;
    event
}

/// Construct a keyed active-job heartbeat lifecycle event.
pub fn heartbeat(meta: EventMeta<'_>, tries: u64, concurrency: Option<JobConcurrency>) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Heartbeat,
        JobState::Active,
        Some(JobState::Active),
        tries,
    );
    event.concurrency = concurrency;
    event
}

/// Construct an observability event for a stale worker completion that was ignored.
pub fn stale_completion_ignored(
    meta: EventMeta<'_>,
    tries: u64,
    reason: Option<&str>,
    concurrency: Option<JobConcurrency>,
) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::StaleCompletionIgnored,
        JobState::Active,
        Some(JobState::Active),
        tries,
    );
    event.error = reason.map(ToString::to_string);
    event.error_detail =
        reason.map(|message| JobErrorDetail::from_message(meta.service, meta.job_type, message));
    event.concurrency = concurrency;
    event
}

/// Construct a `retried` lifecycle event.
pub fn retried(
    meta: EventMeta<'_>,
    previous_state: JobState,
    payload: Option<Value>,
    max_tries: Option<u64>,
    deadline: Option<&str>,
) -> JobEvent {
    retried_by_admin(meta, previous_state, payload, max_tries, deadline, None)
}

/// Construct a `retried` lifecycle event with an optional admin reason.
pub fn retried_by_admin(
    meta: EventMeta<'_>,
    previous_state: JobState,
    payload: Option<Value>,
    max_tries: Option<u64>,
    deadline: Option<&str>,
    reason: Option<&str>,
) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Retried,
        JobState::Pending,
        Some(previous_state),
        0,
    );
    event.payload = payload;
    event.max_tries = max_tries;
    event.deadline = deadline.map(ToString::to_string);
    event.admin_action = admin_action(reason);
    event
}

/// Construct a `dead` lifecycle event.
pub fn dead(meta: EventMeta<'_>, tries: u64, previous_state: JobState, error: &str) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Dead,
        JobState::Dead,
        Some(previous_state),
        tries,
    );
    event.error = Some(error.to_string());
    event.error_detail = Some(JobErrorDetail::from_message(
        meta.service,
        meta.job_type,
        error,
    ));
    event
}

/// Construct a `dismissed` lifecycle event.
pub fn dismissed(
    meta: EventMeta<'_>,
    tries: u64,
    previous_state: JobState,
    reason: Option<&str>,
) -> JobEvent {
    let mut event = base_event(
        meta,
        JobEventType::Dismissed,
        JobState::Dismissed,
        Some(previous_state),
        tries,
    );
    event.error = reason.map(ToString::to_string);
    event.error_detail =
        reason.map(|message| JobErrorDetail::from_message(meta.service, meta.job_type, message));
    event.admin_action = admin_action(reason);
    event
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn context() -> JobContext {
        JobContext {
            request_id: "request-1".to_string(),
            trace_id: "0123456789abcdef0123456789abcdef".to_string(),
            traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
            tracestate: None,
        }
    }

    fn meta(context: &JobContext) -> EventMeta<'_> {
        EventMeta {
            service: "documents",
            job_type: "document-process",
            job_id: "job-1",
            context,
            timestamp: "2026-03-28T12:00:00Z",
        }
    }

    #[test]
    fn attempt_counts_follow_constructor_semantics() {
        let context = context();
        let zero_attempt_events = [
            created(meta(&context), json!({}), 3, None),
            created_with_policy(meta(&context), json!({}), 3, None, None, None),
            skipped(meta(&context), JobState::Pending, None),
            retried(meta(&context), JobState::Failed, None, None, None),
            retried_by_admin(meta(&context), JobState::Dead, None, None, None, None),
        ];

        assert!(zero_attempt_events.iter().all(|event| event.tries == 0));
        assert_eq!(started(meta(&context), 7, JobState::Retry).tries, 7);
    }
}
