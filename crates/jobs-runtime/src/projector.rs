use std::time::Instant;

use futures_util::{FutureExt, StreamExt};
use rusqlite::Connection;
use serde_json::Value;
use trellis_rs::jobs::reduce_job_event;
use trellis_rs::jobs::types::{Job, JobEvent, JobEventType};
use trellis_rs::jobs::{JobsRuntime, JobsRuntimeMessage};
use trellis_rs::service::ServerError;

use crate::storage::{
    apply_job_metadata_patch_on_connection, get_job_from_connection,
    project_timeline_event_on_connection, project_wait_edge_on_connection,
    upsert_error_projection_on_connection, upsert_job_lineage_on_connection,
    upsert_job_on_connection, JobConcurrencyMetadata, JobProjectionMetadataPatch,
    JobQueuePolicyMetadata, SqliteJobsStore, SqliteJobsStoreError,
};

const JOBS_EVENTS_SUBJECT_WILDCARD: &str = "trellis.jobs.>";
const PROJECTOR_CONSUMER_NAME: &str = "jobs-projector";
const PROJECTOR_BATCH_SIZE: usize = 100;

#[derive(Clone)]
struct ProjectorEvent {
    event: JobEvent,
    raw_event: Value,
    stream_sequence: u64,
}

pub struct JobsProjectorHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl JobsProjectorHandle {
    pub async fn stop(self) {
        let Some(task) = self.task else {
            return;
        };
        task.abort();
        let _ = task.await;
    }

    #[doc(hidden)]
    pub fn discard_completed(&mut self) {
        self.task = None;
    }

    pub async fn wait(&mut self) -> Result<(), ServerError> {
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(ServerError::Nats(format!(
                "projector loop task failed: {error}"
            ))),
        }
    }
}

pub async fn start_jobs_projector(
    jobs_runtime: JobsRuntime,
    store: SqliteJobsStore,
    jobs_stream: String,
) -> Result<JobsProjectorHandle, ServerError> {
    let consumer_name = projector_consumer_name(&store.projection_id().map_err(|error| {
        ServerError::Nats(format!(
            "failed to resolve Jobs projection identity: {error}"
        ))
    })?);
    let mut messages = jobs_runtime
        .filtered_messages(&jobs_stream, &consumer_name, JOBS_EVENTS_SUBJECT_WILDCARD)
        .await
        .map_err(|error| {
            ServerError::Nats(format!(
                "failed to start jobs projector consumer '{consumer_name}' message stream: {error}"
            ))
        })?;
    tracing::info!(
        stream = %jobs_stream,
        consumer = %consumer_name,
        filter = JOBS_EVENTS_SUBJECT_WILDCARD,
        batch_size = PROJECTOR_BATCH_SIZE,
        "started jobs projector consumer"
    );

    let task = tokio::spawn(async move {
        tracing::debug!(
            stream = %jobs_stream,
            consumer = %consumer_name,
            "jobs projector loop running"
        );
        while let Some(message) = messages.next().await {
            let mut batch = Vec::new();
            collect_projector_message(message, &consumer_name, &jobs_stream, &mut batch).await?;
            while batch.len() < PROJECTOR_BATCH_SIZE {
                let Some(message) = messages.next().now_or_never() else {
                    break;
                };
                let Some(message) = message else {
                    break;
                };
                collect_projector_message(message, &consumer_name, &jobs_stream, &mut batch)
                    .await?;
            }
            if batch.is_empty() {
                continue;
            }

            let started = Instant::now();
            let events = batch
                .iter()
                .map(|(_, event)| event.clone())
                .collect::<Vec<_>>();
            let batch_store = store.clone();
            let projected = tokio::task::spawn_blocking(move || {
                let projected = project_job_events_with_payloads(&batch_store, &events)?;
                let horizon = events
                    .iter()
                    .map(|event| event.stream_sequence)
                    .max()
                    .unwrap_or_default();
                batch_store.advance_projected_sequence(horizon)?;
                Ok::<_, SqliteJobsStoreError>(projected)
            })
            .await
            .map_err(|error| ServerError::Nats(format!("jobs projector task failed: {error}")))?
            .map_err(|error| {
                ServerError::Nats(format!("jobs projector failed to project batch: {error}"))
            })?;
            let projected_count = projected.iter().filter(|job| job.is_some()).count();
            tracing::debug!(
                stream = %jobs_stream,
                consumer = %consumer_name,
                events = batch.len(),
                projected = projected_count,
                elapsed_ms = started.elapsed().as_millis(),
                "projected jobs event batch"
            );

            let ack_count = batch.len();
            for (message, _) in batch {
                let _ = message.ack().await;
            }
            tracing::debug!(
                stream = %jobs_stream,
                consumer = %consumer_name,
                events = ack_count,
                "acked jobs projector batch"
            );
        }
        tracing::info!(
            stream = %jobs_stream,
            consumer = %consumer_name,
            "jobs projector loop ended"
        );
        Ok(())
    });

    Ok(JobsProjectorHandle { task: Some(task) })
}

async fn collect_projector_message(
    message: Result<JobsRuntimeMessage, String>,
    consumer_name: &str,
    jobs_stream: &str,
    batch: &mut Vec<(JobsRuntimeMessage, ProjectorEvent)>,
) -> Result<(), ServerError> {
    let message = message.map_err(|error| {
        ServerError::Nats(format!(
            "jobs projector failed to pull from consumer '{consumer_name}' on stream '{jobs_stream}': {error}"
        ))
    })?;
    let Some(event) = parse_projector_message(&message) else {
        tracing::debug!(
            subject = %message.subject(),
            consumer = consumer_name,
            "acking non-projectable jobs message"
        );
        let _ = message.ack().await;
        return Ok(());
    };
    batch.push((message, event));
    Ok(())
}

fn parse_projector_message(message: &JobsRuntimeMessage) -> Option<ProjectorEvent> {
    let stream_sequence = message.stream_sequence().ok()?;
    let mut raw_event = serde_json::from_slice::<Value>(message.payload()).ok()?;
    if let Value::Object(fields) = &mut raw_event {
        fields.insert(
            "_trellisSubject".to_string(),
            Value::String(message.subject().to_string()),
        );
    }
    let event = serde_json::from_value::<JobEvent>(raw_event.clone()).ok()?;
    Some(ProjectorEvent {
        event,
        raw_event,
        stream_sequence,
    })
}

/// Exact durable consumer name for one Jobs projection identity.
///
/// The runtime telemetry sampler uses this instead of enumerating every
/// historical `jobs-projector-*` consumer.
pub fn projector_consumer_name(projection_id: &str) -> String {
    format!("{PROJECTOR_CONSUMER_NAME}-{projection_id}")
}

#[cfg(test)]
fn project_job_event(
    store: &SqliteJobsStore,
    event: &JobEvent,
) -> Result<Option<Job>, SqliteJobsStoreError> {
    let raw_event =
        serde_json::to_value(event).map_err(|error| SqliteJobsStoreError::EncodeJson {
            model: "job event",
            details: error.to_string(),
        })?;
    project_job_event_with_payload(store, event, &raw_event)
}

#[cfg(test)]
fn project_job_event_with_payload(
    store: &SqliteJobsStore,
    event: &JobEvent,
    raw_event: &Value,
) -> Result<Option<Job>, SqliteJobsStoreError> {
    store.with_write_transaction(|connection| {
        project_job_event_with_payload_on_connection(connection, event, raw_event)
    })
}

fn project_job_events_with_payloads(
    store: &SqliteJobsStore,
    events: &[ProjectorEvent],
) -> Result<Vec<Option<Job>>, SqliteJobsStoreError> {
    store.with_write_transaction(|connection| {
        events
            .iter()
            .map(|event| {
                project_job_event_with_payload_on_connection(
                    connection,
                    &event.event,
                    &event.raw_event,
                )
            })
            .collect()
    })
}

fn project_job_event_with_payload_on_connection(
    connection: &Connection,
    event: &JobEvent,
    raw_event: &Value,
) -> Result<Option<Job>, SqliteJobsStoreError> {
    let current =
        get_job_from_connection(connection, &event.service, &event.job_type, &event.job_id)?;
    let next = reduce_job_event(current.as_ref(), event).or_else(|| job_from_terminal_event(event));
    let projected = next
        .as_ref()
        .zip(current.as_ref())
        .map(|(next, current)| next != current);
    let reason = if projected == Some(false) {
        if current
            .as_ref()
            .is_some_and(|job| trellis_rs::jobs::is_terminal(job.state))
        {
            Some("terminal-precedence")
        } else {
            Some("illegal-transition")
        }
    } else if next.is_none() {
        Some("illegal-transition")
    } else {
        None
    };
    project_timeline_event_on_connection(
        connection,
        event,
        raw_event,
        projected.or(Some(next.is_some())),
        reason,
    )?;
    project_wait_edge_on_connection(connection, event)?;
    let fallback_detail = event.error.as_deref().map(|message| {
        trellis_rs::jobs::types::JobErrorDetail::from_message(
            &event.service,
            &event.job_type,
            message,
        )
    });
    if let Some(detail) = event.error_detail.as_ref().or(fallback_detail.as_ref()) {
        upsert_error_projection_on_connection(
            connection,
            &event.service,
            &event.job_type,
            event.state,
            &event.timestamp,
            detail,
        )?;
    }
    let Some(next) = next else {
        return Ok(None);
    };
    upsert_job_on_connection(connection, &next)?;
    upsert_job_lineage_on_connection(connection, &next)?;
    let metadata = metadata_patch_from_event_payload(raw_event);
    apply_job_metadata_patch_on_connection(
        connection,
        &event.service,
        &event.job_type,
        &event.job_id,
        &event.timestamp,
        &metadata,
    )?;
    Ok(Some(next))
}

fn metadata_patch_from_event_payload(raw_event: &Value) -> JobProjectionMetadataPatch {
    JobProjectionMetadataPatch {
        concurrency: raw_event
            .get("concurrency")
            .and_then(concurrency_metadata_from_value),
        queue_policy: raw_event
            .get("queuePolicy")
            .and_then(queue_policy_metadata_from_value),
    }
}

fn job_from_terminal_event(event: &JobEvent) -> Option<Job> {
    if !trellis_rs::jobs::is_terminal(event.state) {
        return None;
    }

    let mut job = Job {
        id: event.job_id.clone(),
        context: event.context.clone(),
        service: event.service.clone(),
        job_type: event.job_type.clone(),
        state: event.state,
        payload: event.payload.clone().unwrap_or(Value::Null),
        result: None,
        created_at: event.timestamp.clone(),
        updated_at: event.timestamp.clone(),
        started_at: None,
        completed_at: Some(event.timestamp.clone()),
        tries: event.tries,
        max_tries: event.max_tries.unwrap_or(event.tries.max(1)),
        last_error: event.error.clone(),
        error_detail: event.error_detail.clone().or_else(|| {
            event.error.as_deref().map(|message| {
                trellis_rs::jobs::types::JobErrorDetail::from_message(
                    &event.service,
                    &event.job_type,
                    message,
                )
            })
        }),
        deadline: event.deadline.clone(),
        progress: event.progress.clone(),
        logs: event.logs.clone(),
        concurrency: event.concurrency.clone(),
        queue_policy: event.queue_policy.clone(),
        trigger: event.trigger.clone(),
        lineage: event.lineage.clone(),
        waiting_on: None,
    };

    if event.event_type == JobEventType::Completed {
        job.result = event.result.clone();
        job.last_error = None;
        job.error_detail = None;
    }

    Some(job)
}

fn concurrency_metadata_from_value(value: &Value) -> Option<JobConcurrencyMetadata> {
    let key = value.get("key")?.as_str()?.to_string();
    let key_hash = value.get("keyHash")?.as_str()?.to_string();
    Some(JobConcurrencyMetadata {
        key,
        key_hash,
        instance_id: optional_string(value, "instanceId"),
        heartbeat_at: optional_string(value, "heartbeatAt"),
        lease_expires_at: optional_string(value, "leaseExpiresAt"),
        stale_takeover_count: value.get("staleTakeoverCount").and_then(Value::as_u64),
    })
}

fn queue_policy_metadata_from_value(value: &Value) -> Option<JobQueuePolicyMetadata> {
    let outcome = value.get("outcome")?.as_str()?.to_string();
    Some(JobQueuePolicyMetadata {
        outcome,
        reason: optional_string(value, "reason"),
        existing_job_id: optional_string(value, "existingJobId"),
        replaced_job_id: optional_string(value, "replacedJobId"),
    })
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_rs::jobs::events::{
        cancelled_by_admin, created, failed, resumed, started_with_concurrency, waiting, EventMeta,
    };

    fn meta<'a>(context: &'a JobContext, job_id: &'a str, timestamp: &'a str) -> EventMeta<'a> {
        EventMeta {
            service: "documents",
            job_type: "document-process",
            job_id,
            context,
            timestamp,
        }
    }
    use trellis_rs::jobs::types::{
        JobConcurrency, JobContext, JobLineage, JobState, JobTrigger, JobTriggerKind, JobWaitEdge,
        JobWaitTarget, JobWaitTargetKind,
    };

    use super::*;

    #[test]
    fn projector_consumer_name_includes_projection_identity() {
        assert_eq!(
            projector_consumer_name("0123456789abcdef"),
            "jobs-projector-0123456789abcdef"
        );
    }

    #[test]
    fn project_job_event_upserts_sql_projection() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let event = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );

        let projected = project_job_event(&store, &event)
            .expect("projection should succeed")
            .expect("event should reduce");

        assert_eq!(projected.state, JobState::Pending);
        let stored = store
            .get_job("documents", "document-process", "job-1")
            .expect("get should succeed")
            .expect("job should be stored");
        assert_eq!(stored.id, "job-1");
        assert_eq!(stored.payload, json!({ "documentId": "doc-1" }));
    }

    #[test]
    fn project_terminal_event_without_created_keeps_evidence_visible() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let event = failed(
            meta(&context(), "old-job", "2026-03-28T12:05:00.000Z"),
            1,
            JobState::Active,
            "boom",
        );

        let projected = project_job_event(&store, &event)
            .expect("projection should succeed")
            .expect("terminal evidence should reduce");

        assert_eq!(projected.id, "old-job");
        assert_eq!(projected.state, JobState::Failed);
        assert_eq!(projected.payload, Value::Null);
        assert_eq!(projected.last_error.as_deref(), Some("boom"));
        let stored = store
            .get_job_by_global_id("old-job")
            .expect("lookup should succeed")
            .expect("synthetic terminal job should be stored");
        assert_eq!(stored.state, JobState::Failed);
    }

    #[test]
    fn project_job_event_projects_keyed_concurrency_metadata() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let event = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        let mut raw_event = serde_json::to_value(&event).expect("event should encode");
        raw_event["concurrency"] = json!({
            "key": "tenant-1:document:doc-1",
            "keyHash": "hash-1",
            "instanceId": "worker-1",
            "heartbeatAt": "2026-03-28T12:00:30.000Z",
            "leaseExpiresAt": "2026-03-28T12:02:30.000Z",
            "staleTakeoverCount": 2
        });

        project_job_event_with_payload(&store, &event, &raw_event)
            .expect("projection should succeed");

        let metadata = store
            .get_job_metadata("documents", "document-process", "job-1")
            .expect("metadata get should succeed")
            .expect("metadata should exist");
        let concurrency = metadata.concurrency.expect("concurrency should project");
        assert_eq!(concurrency.key, "tenant-1:document:doc-1");
        assert_eq!(concurrency.key_hash, "hash-1");
        assert_eq!(concurrency.instance_id.as_deref(), Some("worker-1"));
        assert_eq!(concurrency.stale_takeover_count, Some(2));
    }

    #[test]
    fn project_job_event_projects_trigger_and_lineage() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut event = created(
            meta(&context(), "child-job", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        event.trigger = Some(JobTrigger {
            kind: JobTriggerKind::ParentJob,
            id: None,
            subject: None,
            operation_id: None,
            parent_job_id: Some("parent-job".to_string()),
            trace_id: Some("0123456789abcdef0123456789abcdef".to_string()),
            request_id: Some("request-job-1".to_string()),
        });
        event.lineage = Some(JobLineage {
            parent_job_id: Some("parent-job".to_string()),
            root_job_id: Some("root-job".to_string()),
            operation_id: None,
            related_keys: None,
        });

        project_job_event(&store, &event).expect("projection should succeed");

        let projected = store
            .get_job_lineage_by_global_id("child-job")
            .expect("lineage should read");
        assert_eq!(
            projected.trigger.map(|trigger| trigger.kind),
            Some(JobTriggerKind::ParentJob)
        );
        assert_eq!(
            projected.lineage.and_then(|lineage| lineage.parent_job_id),
            Some("parent-job".to_string())
        );
    }

    #[test]
    fn project_started_event_projects_active_key_instance_id() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let created = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        project_job_event(&store, &created).expect("created projection should succeed");
        let started = started_with_concurrency(
            meta(&context(), "job-1", "2026-03-28T12:01:00.000Z"),
            1,
            JobState::Pending,
            JobConcurrency {
                key: "tenant-1:document:doc-1".to_string(),
                key_hash: "hash-1".to_string(),
                instance_id: Some("worker-1".to_string()),
                slot_token: Some("slot-1".to_string()),
                heartbeat_at: Some("2026-03-28T12:01:00.000Z".to_string()),
                lease_expires_at: Some("2026-03-28T12:03:00.000Z".to_string()),
                stale_takeover_count: Some(0),
            },
        );

        project_job_event(&store, &started).expect("started projection should succeed");

        let key = store
            .get_projected_key("documents", "document-process", "tenant-1:document:doc-1")
            .expect("key query should succeed")
            .expect("key should exist");
        assert_eq!(key.active.len(), 1);
        assert_eq!(key.active[0].job_id, "job-1");
        assert_eq!(key.active[0].instance_id.as_deref(), Some("worker-1"));
    }

    #[test]
    fn project_job_event_records_timeline_in_sequence_order() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let created = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        let started = started_with_concurrency(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            1,
            JobState::Pending,
            JobConcurrency {
                key: "tenant-1:document:doc-1".to_string(),
                key_hash: "hash-1".to_string(),
                instance_id: Some("worker-1".to_string()),
                slot_token: Some("slot-1".to_string()),
                heartbeat_at: None,
                lease_expires_at: None,
                stale_takeover_count: None,
            },
        );

        project_job_event(&store, &created).expect("created projection should succeed");
        project_job_event(&store, &started).expect("started projection should succeed");

        let timeline = store
            .list_timeline_events("job-1", 10)
            .expect("timeline should list");
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].sequence, 1);
        assert_eq!(timeline[0].event_type, "created");
        assert_eq!(timeline[1].sequence, 2);
        assert_eq!(timeline[1].event_type, "started");
        assert_eq!(timeline[1].worker_instance_id.as_deref(), Some("worker-1"));
    }

    #[test]
    fn project_job_event_projects_and_clears_current_waits() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let created = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        let started = started_with_concurrency(
            meta(&context(), "job-1", "2026-03-28T12:01:00.000Z"),
            1,
            JobState::Pending,
            JobConcurrency {
                key: "tenant-1:document:doc-1".to_string(),
                key_hash: "hash-1".to_string(),
                instance_id: Some("worker-1".to_string()),
                slot_token: Some("slot-1".to_string()),
                heartbeat_at: None,
                lease_expires_at: None,
                stale_takeover_count: None,
            },
        );
        let wait_edge = JobWaitEdge {
            id: "wait-1".to_string(),
            target: JobWaitTarget {
                kind: JobWaitTargetKind::Job,
                id: Some("child-job".to_string()),
                operation_id: None,
                label: None,
                service: Some("documents".to_string()),
                target_type: Some("document-process".to_string()),
                system: None,
                operation: None,
                key: None,
            },
            started_at: "2026-03-28T12:02:00.000Z".to_string(),
            label: None,
        };

        project_job_event(&store, &created).expect("created projection should succeed");
        project_job_event(&store, &started).expect("started projection should succeed");
        project_job_event(
            &store,
            &waiting(
                meta(&context(), "job-1", "2026-03-28T12:02:00.000Z"),
                1,
                wait_edge.clone(),
            ),
        )
        .expect("waiting projection should succeed");

        let waits = store
            .list_current_waits("job-1")
            .expect("waits should list");
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0].wait_edge.id, "wait-1");
        let timeline = store
            .list_timeline_events("job-1", 10)
            .expect("timeline should list");
        assert_eq!(timeline[2].event_type, "waiting");
        assert!(timeline[2].raw_event_json.contains("waitEdge"));

        project_job_event(
            &store,
            &resumed(
                meta(&context(), "job-1", "2026-03-28T12:03:00.000Z"),
                1,
                wait_edge,
            ),
        )
        .expect("resumed projection should succeed");
        assert!(store
            .list_current_waits("job-1")
            .expect("waits should list")
            .is_empty());
    }

    #[test]
    fn project_job_events_with_payloads_replays_many_events() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let events = (0..500)
            .map(|index| {
                let job_id = format!("job-{index}");
                let event = created(
                    meta(&context(), &job_id, "2026-03-28T12:00:00.000Z"),
                    json!({ "documentId": format!("doc-{index}") }),
                    3,
                    None,
                );
                let raw_event = serde_json::to_value(&event).expect("event should encode");
                ProjectorEvent {
                    event,
                    raw_event,
                    stream_sequence: index as u64 + 1,
                }
            })
            .collect::<Vec<_>>();

        let projected = project_job_events_with_payloads(&store, &events)
            .expect("batch projection should succeed");

        assert_eq!(projected.len(), 500);
        assert!(projected.iter().all(Option::is_some));
        let stored = store
            .get_job("documents", "document-process", "job-499")
            .expect("get should succeed")
            .expect("last job should be stored");
        assert_eq!(stored.payload, json!({ "documentId": "doc-499" }));
    }

    #[test]
    fn project_admin_reason_into_timeline_message() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let created = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        project_job_event(&store, &created).expect("created projection should succeed");
        let cancelled = cancelled_by_admin(
            meta(&context(), "job-1", "2026-03-28T12:01:00.000Z"),
            0,
            JobState::Pending,
            Some("operator requested maintenance"),
        );

        project_job_event(&store, &cancelled).expect("cancelled projection should succeed");

        let timeline = store
            .list_timeline_events("job-1", 10)
            .expect("timeline should list");
        let admin_event = timeline
            .iter()
            .find(|event| event.event_type == "cancelled")
            .expect("admin action should appear in timeline");
        assert_eq!(admin_event.state, "cancelled");
        assert_eq!(
            admin_event.message.as_deref(),
            Some("operator requested maintenance")
        );
    }

    #[test]
    fn project_job_event_projects_queue_policy_reason_metadata() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let event = created(
            meta(&context(), "job-2", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-2" }),
            3,
            None,
        );
        let mut raw_event = serde_json::to_value(&event).expect("event should encode");
        raw_event["queuePolicy"] = json!({
            "outcome": "coalesced",
            "reason": "queue-full",
            "existingJobId": "job-1"
        });

        project_job_event_with_payload(&store, &event, &raw_event)
            .expect("projection should succeed");

        let metadata = store
            .get_job_metadata("documents", "document-process", "job-2")
            .expect("metadata get should succeed")
            .expect("metadata should exist");
        let queue_policy = metadata.queue_policy.expect("queue policy should project");
        assert_eq!(queue_policy.outcome, "coalesced");
        assert_eq!(queue_policy.reason.as_deref(), Some("queue-full"));
        assert_eq!(queue_policy.existing_job_id.as_deref(), Some("job-1"));
    }

    #[test]
    fn project_job_event_projects_failed_error_detail_and_aggregate() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let created = created(
            meta(&context(), "job-1", "2026-03-28T12:00:00.000Z"),
            json!({ "documentId": "doc-1" }),
            3,
            None,
        );
        project_job_event(&store, &created).expect("created projection should succeed");
        let started = started_with_concurrency(
            meta(&context(), "job-1", "2026-03-28T12:01:00.000Z"),
            1,
            JobState::Pending,
            JobConcurrency {
                key: "tenant-1:document:doc-1".to_string(),
                key_hash: "hash-1".to_string(),
                instance_id: Some("worker-1".to_string()),
                slot_token: Some("slot-1".to_string()),
                heartbeat_at: None,
                lease_expires_at: None,
                stale_takeover_count: None,
            },
        );
        project_job_event(&store, &started).expect("started projection should succeed");
        let failed = failed(
            meta(&context(), "job-1", "2026-03-28T12:02:00.000Z"),
            1,
            JobState::Active,
            "boom\njob id 123",
        );

        let projected = project_job_event(&store, &failed)
            .expect("failed projection should succeed")
            .expect("failed event should reduce");

        let detail = projected.error_detail.expect("error detail should project");
        assert_eq!(detail.message, "boom\njob id 123");
        let aggregate = store
            .get_error_projection(&detail.fingerprint)
            .expect("error projection should read")
            .expect("error projection should exist");
        assert_eq!(aggregate.message, "boom\njob id 123");
        assert_eq!(aggregate.occurrence_count, 1);
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
