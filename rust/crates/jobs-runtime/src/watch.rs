//! `Jobs.Watch` feed implementation.

use futures_util::{stream, Stream, StreamExt};
use trellis_rs::jobs::types::{JobEvent, JobState, JobTriggerKind};
use trellis_rs::jobs::{JobsRuntime, JobsRuntimeMessageStream};
use trellis_rs::service::{Router, ServerError};
use trellis_runtime_apis::apis::trellis_jobs_v1::feeds::Watch;
use trellis_runtime_apis::types::{
    Bytes, JobsWatchFrame as JobsWatchEvent, JobsWatchRequest as JobsWatchInput,
    JobsWatchRequestquery as JobsWatchInputQuery,
};

const JOBS_EVENTS_SUBJECT_WILDCARD: &str = "trellis.jobs.>";

/// Register the `Jobs.Watch` feed on a built-in Jobs router.
pub fn register_jobs_watch_feed(
    router: &mut Router,
    jobs_runtime: JobsRuntime,
    jobs_stream: String,
) {
    router.register_feed::<Watch, _, _>(move |_ctx, input| {
        watch_jobs(input, jobs_runtime.clone(), jobs_stream.clone())
    });
}

fn watch_jobs(
    input: JobsWatchInput,
    jobs_runtime: JobsRuntime,
    jobs_stream: String,
) -> impl Stream<Item = Result<JobsWatchEvent, ServerError>> + Send + 'static {
    let filter_subject = input
        .job_id
        .as_ref()
        .map(|job_id| format!("trellis.jobs.*.*.{}.>", job_id.0))
        .unwrap_or_else(|| JOBS_EVENTS_SUBJECT_WILDCARD.to_string());
    stream::unfold(
        WatchState::Init {
            jobs_runtime,
            jobs_stream,
            filter_subject,
            input,
        },
        next_watch_frame,
    )
}

enum WatchState {
    Init {
        jobs_runtime: JobsRuntime,
        jobs_stream: String,
        filter_subject: String,
        input: JobsWatchInput,
    },
    Open {
        messages: JobsRuntimeMessageStream,
        input: JobsWatchInput,
    },
    Done,
}

async fn next_watch_frame(
    state: WatchState,
) -> Option<(Result<JobsWatchEvent, ServerError>, WatchState)> {
    match state {
        WatchState::Init {
            jobs_runtime,
            jobs_stream,
            filter_subject,
            input,
        } => {
            let messages = match jobs_runtime
                .watch_messages(&jobs_stream, &filter_subject)
                .await
            {
                Ok(messages) => messages,
                Err(error) => {
                    return Some((
                        Err(ServerError::Nats(format!(
                            "failed to start Jobs.Watch: {error}"
                        ))),
                        WatchState::Done,
                    ));
                }
            };
            Some((
                Ok(ready_frame(now_timestamp_string())),
                WatchState::Open { messages, input },
            ))
        }
        WatchState::Open {
            mut messages,
            input,
        } => loop {
            let message = match messages.next().await {
                Some(Ok(message)) => message,
                Some(Err(error)) => {
                    return Some((
                        Err(ServerError::Nats(format!("Jobs.Watch failed: {error}"))),
                        WatchState::Done,
                    ));
                }
                None => return None,
            };
            let event = serde_json::from_slice::<JobEvent>(message.payload()).ok();
            let _ = message.ack().await;
            let Some(event) = event else {
                continue;
            };
            if let Some(frame) = watch_frame_for_event(&input, &event) {
                return Some((Ok(frame), WatchState::Open { messages, input }));
            }
        },
        WatchState::Done => None,
    }
}

fn ready_frame(timestamp: String) -> JobsWatchEvent {
    watch_frame(serde_json::json!({ "kind": "ready", "timestamp": timestamp }))
}

fn watch_frame_for_event(input: &JobsWatchInput, event: &JobEvent) -> Option<JobsWatchEvent> {
    if input.job_id.as_ref().map(|id| id.0.as_str()) == Some(event.job_id.as_str()) {
        return Some(watch_frame(serde_json::json!({
            "kind": "jobInspectChanged",
            "id": event.job_id,
            "timestamp": event.timestamp,
        })));
    }

    if input.job_id.is_some() {
        return None;
    }
    let reason = match input
        .query
        .as_ref()
        .map(|query| query_invalidation_reason(query, event))
    {
        Some(QueryInvalidation::No) => return None,
        Some(QueryInvalidation::Unknown) => "unknownMatch",
        Some(QueryInvalidation::Matched) | None => "matchedJobChanged",
    };
    Some(watch_frame(serde_json::json!({
        "kind": "queryInvalidated",
        "reason": reason,
        "timestamp": event.timestamp,
    })))
}

fn watch_frame(value: serde_json::Value) -> JobsWatchEvent {
    JobsWatchEvent(Bytes(value.to_string().into_bytes()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryInvalidation {
    No,
    Matched,
    Unknown,
}

fn query_invalidation_reason(query: &JobsWatchInputQuery, event: &JobEvent) -> QueryInvalidation {
    if query
        .service
        .as_ref()
        .is_some_and(|service| service.0 != event.service)
        || query
            .r#type
            .as_ref()
            .is_some_and(|job_type| job_type.0 != event.job_type)
        || query.state.as_ref().is_some_and(|states| {
            !states
                .iter()
                .any(|state| wire_token(state) == job_state_token(event.state))
        })
        || query.trigger.as_ref().is_some_and(|trigger| {
            event.trigger.as_ref().is_some_and(|event_trigger| {
                trigger.as_str() != trigger_kind_token(event_trigger.kind)
            })
        })
    {
        return QueryInvalidation::No;
    }

    if query.queue_key.as_ref().is_some_and(|queue_key| {
        event
            .concurrency
            .as_ref()
            .map(|concurrency| queue_key != &concurrency.key)
            .unwrap_or(false)
    }) {
        return QueryInvalidation::No;
    }

    if query
        .search
        .as_deref()
        .is_some_and(|search| !search.trim().is_empty())
        || query.runtime_band.is_some()
        || (query.queue_key.is_some() && event.concurrency.is_none())
        || (query.trigger.is_some() && event.trigger.is_none())
    {
        QueryInvalidation::Unknown
    } else {
        QueryInvalidation::Matched
    }
}

fn now_timestamp_string() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn job_state_token(state: JobState) -> &'static str {
    match state {
        JobState::Pending => "pending",
        JobState::Active => "active",
        JobState::Retry => "retry",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
        JobState::Expired => "expired",
        JobState::Skipped => "skipped",
        JobState::Stale => "stale",
        JobState::Dead => "dead",
        JobState::Dismissed => "dismissed",
    }
}

fn trigger_kind_token(kind: JobTriggerKind) -> &'static str {
    match kind {
        JobTriggerKind::Schedule => "schedule",
        JobTriggerKind::Operation => "operation",
        JobTriggerKind::Rpc => "rpc",
        JobTriggerKind::Event => "event",
        JobTriggerKind::ManualReplay => "manualReplay",
        JobTriggerKind::ServiceCode => "serviceCode",
        JobTriggerKind::ParentJob => "parentJob",
    }
}

fn wire_token(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_rs::generated::Codec as _;
    use trellis_rs::jobs::events::{created, EventMeta};
    use trellis_rs::jobs::types::{JobContext, JobState};

    use super::*;

    fn meta<'a>(context: &'a JobContext) -> EventMeta<'a> {
        EventMeta {
            service: "documents",
            job_type: "document-process",
            job_id: "job-1",
            context,
            timestamp: "2026-03-28T12:00:00.000Z",
        }
    }

    #[test]
    fn unfiltered_watch_invalidates_for_created_job() {
        let event = created(meta(&context()), json!({ "documentId": "doc-1" }), 3, None);
        let input = JobsWatchInput::decode(json!({ "includeInitial": false })).unwrap();

        let frame = watch_frame_for_event(&input, &event).expect("unfiltered watch must refresh");
        let JobsWatchEvent(Bytes(payload)) = frame;
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["kind"], "queryInvalidated");
        assert_eq!(payload["reason"], "matchedJobChanged");
    }

    #[test]
    fn query_invalidation_matches_scalar_filters() {
        let event = created(meta(&context()), json!({ "documentId": "doc-1" }), 3, None);
        let query = JobsWatchInputQuery::decode(json!({
            "limit": "50",
            "service": "documents",
            "state": ["pending"],
            "type": "document-process"
        }))
        .unwrap();

        assert_eq!(
            query_invalidation_reason(&query, &event),
            QueryInvalidation::Matched
        );
    }

    #[test]
    fn query_invalidation_rejects_scalar_mismatches() {
        let mut event = created(meta(&context()), json!({ "documentId": "doc-1" }), 3, None);
        event.state = JobState::Failed;
        let query = JobsWatchInputQuery::decode(json!({
            "limit": "50",
            "service": "documents",
            "state": ["pending"]
        }))
        .unwrap();

        assert_eq!(
            query_invalidation_reason(&query, &event),
            QueryInvalidation::No
        );
    }

    #[test]
    fn query_invalidation_is_unknown_for_text_search() {
        let event = created(meta(&context()), json!({ "documentId": "doc-1" }), 3, None);
        let query = JobsWatchInputQuery::decode(json!({
            "limit": "50",
            "search": "doc-1",
            "service": "documents"
        }))
        .unwrap();

        assert_eq!(
            query_invalidation_reason(&query, &event),
            QueryInvalidation::Unknown
        );
    }

    #[test]
    fn query_invalidation_is_unknown_for_missing_trigger_data() {
        let event = created(meta(&context()), json!({ "documentId": "doc-1" }), 3, None);
        let query = JobsWatchInputQuery::decode(json!({
            "limit": "50",
            "service": "documents",
            "trigger": "schedule"
        }))
        .unwrap();

        assert_eq!(
            query_invalidation_reason(&query, &event),
            QueryInvalidation::Unknown
        );
    }

    fn context() -> JobContext {
        JobContext {
            request_id: "request-job-1".to_string(),
            trace_id: "0123456789abcdef0123456789abcdef".to_string(),
            traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
            tracestate: None,
        }
    }
}
