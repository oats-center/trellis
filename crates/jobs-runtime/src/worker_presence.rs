use std::time::Duration;

use futures_util::StreamExt;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::jobs::{JobsRuntime, WorkerHeartbeat, WORKER_HEARTBEATS_WILDCARD};
use trellis_rs::service::ServerError;

use crate::storage::{SqliteJobsStore, SqliteJobsStoreError};

pub const WORKER_PRESENCE_CONSUMER_NAME: &str = "jobs-worker-presence-projector";
pub const WORKER_PRESENCE_FRESH_FOR: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPresenceRecord {
    pub service: String,
    pub job_type: String,
    pub instance_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub heartbeat_at: String,
}

pub struct WorkerPresenceProjectorHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl WorkerPresenceProjectorHandle {
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
                "worker presence projector loop task failed: {error}"
            ))),
        }
    }
}

pub fn worker_presence_from_heartbeat(heartbeat: &WorkerHeartbeat) -> WorkerPresenceRecord {
    WorkerPresenceRecord {
        service: heartbeat.service.clone(),
        job_type: heartbeat.job_type.clone(),
        instance_id: heartbeat.instance_id.clone(),
        concurrency: heartbeat.concurrency,
        version: heartbeat.version.clone(),
        heartbeat_at: heartbeat.timestamp.clone(),
    }
}

pub fn reduce_worker_presence(
    current: Option<&WorkerPresenceRecord>,
    next: &WorkerPresenceRecord,
) -> Option<WorkerPresenceRecord> {
    match current {
        Some(current)
            if parse_timestamp(&current.heartbeat_at) > parse_timestamp(&next.heartbeat_at) =>
        {
            None
        }
        _ => Some(next.clone()),
    }
}

pub fn worker_presence_is_fresh(record: &WorkerPresenceRecord, now: OffsetDateTime) -> bool {
    parse_timestamp(&record.heartbeat_at) >= now - WORKER_PRESENCE_FRESH_FOR
}

pub async fn start_worker_presence_projector(
    jobs_runtime: JobsRuntime,
    jobs_stream: String,
    store: SqliteJobsStore,
) -> Result<WorkerPresenceProjectorHandle, ServerError> {
    let mut messages = jobs_runtime
        .filtered_messages(&jobs_stream, WORKER_PRESENCE_CONSUMER_NAME, WORKER_HEARTBEATS_WILDCARD)
        .await
        .map_err(|error| {
            ServerError::Nats(format!(
                "failed to start worker presence consumer '{WORKER_PRESENCE_CONSUMER_NAME}' message stream: {error}"
            ))
        })?;
    tracing::info!(
        stream = %jobs_stream,
        consumer = WORKER_PRESENCE_CONSUMER_NAME,
        filter = WORKER_HEARTBEATS_WILDCARD,
        "started worker presence projector"
    );

    let task = tokio::spawn(async move {
        tracing::debug!(
            stream = %jobs_stream,
            consumer = WORKER_PRESENCE_CONSUMER_NAME,
            "worker presence projector loop running"
        );
        while let Some(message) = messages.next().await {
            let message = message.map_err(|error| {
                ServerError::Nats(format!(
                    "worker presence projector failed to pull from consumer '{WORKER_PRESENCE_CONSUMER_NAME}' on stream '{jobs_stream}': {error}"
                ))
            })?;
            let Some((subject_service, job_type, instance_id)) =
                parse_worker_heartbeat_subject(message.subject())
            else {
                tracing::debug!(
                    subject = %message.subject(),
                    "acking non-worker-heartbeat message"
                );
                let _ = message.ack().await;
                continue;
            };
            let heartbeat = match serde_json::from_slice::<WorkerHeartbeat>(message.payload()) {
                Ok(heartbeat) if heartbeat_matches_subject(&heartbeat, &job_type, &instance_id) => {
                    heartbeat
                }
                _ => {
                    tracing::debug!(
                        service = %subject_service,
                        job_type = %job_type,
                        instance_id = %instance_id,
                        "acking invalid worker heartbeat"
                    );
                    let _ = message.ack().await;
                    continue;
                }
            };

            project_worker_heartbeat(&store, &heartbeat).map_err(|error| {
                ServerError::Nats(format!(
                    "worker presence projector failed to project worker '{subject_service}/{job_type}/{instance_id}': {error}"
                ))
            })?;
            tracing::debug!(
                service = %subject_service,
                job_type = %job_type,
                instance_id = %instance_id,
                timestamp = %heartbeat.timestamp,
                "projected worker heartbeat"
            );
            let _ = message.ack().await;
        }
        tracing::info!(
            stream = %jobs_stream,
            consumer = WORKER_PRESENCE_CONSUMER_NAME,
            "worker presence projector loop ended"
        );
        Ok(())
    });

    Ok(WorkerPresenceProjectorHandle { task: Some(task) })
}

pub fn project_worker_heartbeat(
    store: &SqliteJobsStore,
    heartbeat: &WorkerHeartbeat,
) -> Result<Option<WorkerPresenceRecord>, SqliteJobsStoreError> {
    let current = store.get_worker_presence(
        &heartbeat.service,
        &heartbeat.job_type,
        &heartbeat.instance_id,
    )?;
    let next = worker_presence_from_heartbeat(heartbeat);
    let Some(next) = reduce_worker_presence(current.as_ref(), &next) else {
        return Ok(None);
    };
    store.upsert_worker_presence(&next)?;
    Ok(Some(next))
}

fn parse_timestamp(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn parse_worker_heartbeat_subject(subject: &str) -> Option<(String, String, String)> {
    let mut parts = subject.split('.');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (
            Some("trellis"),
            Some("jobs"),
            Some("workers"),
            Some(service),
            Some(job_type),
            Some(instance_id),
            Some("heartbeat"),
        ) if parts.next().is_none() => Some((
            service.to_string(),
            job_type.to_string(),
            instance_id.to_string(),
        )),
        _ => None,
    }
}

fn heartbeat_matches_subject(
    heartbeat: &WorkerHeartbeat,
    subject_job_type: &str,
    subject_instance_id: &str,
) -> bool {
    heartbeat.job_type == subject_job_type && heartbeat.instance_id == subject_instance_id
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;
    use trellis_rs::jobs::WorkerHeartbeat;

    use super::{
        heartbeat_matches_subject, parse_worker_heartbeat_subject, project_worker_heartbeat,
        WORKER_PRESENCE_FRESH_FOR,
    };
    use crate::storage::SqliteJobsStore;
    use trellis_rs::jobs::worker_heartbeat_subject;

    #[test]
    fn parse_worker_heartbeat_subject_extracts_components() {
        let parsed = parse_worker_heartbeat_subject(&worker_heartbeat_subject(
            "documents",
            "document-process",
            "instance-1",
        ))
        .expect("subject should parse");

        assert_eq!(parsed.0, "documents");
        assert_eq!(parsed.1, "document-process");
        assert_eq!(parsed.2, "instance-1");
    }

    #[test]
    fn project_worker_heartbeat_upserts_sql_presence_and_ignores_stale_heartbeat() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let fresh = WorkerHeartbeat {
            service: "documents".to_string(),
            job_type: "document-process".to_string(),
            instance_id: "instance-1".to_string(),
            concurrency: Some(4),
            version: Some("1.2.3".to_string()),
            timestamp: "2026-03-28T12:00:00.000Z".to_string(),
        };

        let projected = project_worker_heartbeat(&store, &fresh)
            .expect("projection should succeed")
            .expect("fresh heartbeat should project");
        assert_eq!(projected.concurrency, Some(4));

        let stale = WorkerHeartbeat {
            timestamp: "2026-03-28T11:59:00.000Z".to_string(),
            concurrency: Some(1),
            ..fresh
        };
        assert!(project_worker_heartbeat(&store, &stale)
            .expect("projection should succeed")
            .is_none());

        let workers = store
            .list_fresh_workers(
                OffsetDateTime::parse(
                    "2026-03-28T12:00:30.000Z",
                    &time::format_description::well_known::Rfc3339,
                )
                .expect("timestamp should parse"),
                WORKER_PRESENCE_FRESH_FOR,
            )
            .expect("fresh worker list should succeed");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].concurrency, Some(4));
    }

    #[test]
    fn heartbeat_subject_match_ignores_physical_service_namespace() {
        let heartbeat = WorkerHeartbeat {
            service: "trellis/zendesk".to_string(),
            job_type: "sync".to_string(),
            instance_id: "instance-1".to_string(),
            concurrency: Some(1),
            version: None,
            timestamp: "2026-03-28T12:00:00.000Z".to_string(),
        };

        assert!(heartbeat_matches_subject(&heartbeat, "sync", "instance-1"));
        assert!(!heartbeat_matches_subject(
            &heartbeat,
            "other",
            "instance-1"
        ));
    }
}
