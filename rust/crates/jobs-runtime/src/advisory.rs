use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use futures_util::StreamExt;
use serde::Deserialize;
use trellis_rs::jobs::events::{dead, EventMeta};
use trellis_rs::jobs::types::{Job, JobEvent, JobState};
use trellis_rs::jobs::{is_terminal, job_event_subject, job_from_work_event, JobsRuntime};
use trellis_rs::service::ServerError;

use crate::storage::SqliteJobsStore;
use crate::JobResourceResolver;

const MAX_DELIVERIES_ADVISORY_SUBJECT_WILDCARD: &str =
    "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>";
const ADVISORY_CONSUMER_NAME: &str = "jobs-advisories";
const ADVISORY_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub struct MappedDeadEvent {
    pub subject: String,
    pub event: JobEvent,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MaxDeliveriesAdvisory {
    /// Broker advisory schema identity.
    #[serde(rename = "type")]
    pub advisory_type: String,
    /// Broker advisory instance identity.
    pub id: String,
    pub stream: String,
    pub consumer: String,
    #[serde(rename = "stream_seq", alias = "streamSeq")]
    pub stream_seq: u64,
    #[serde(alias = "num_deliveries")]
    pub deliveries: u64,
    pub timestamp: String,
}

const MAX_DELIVERIES_ADVISORY_TYPE: &str = "io.nats.jetstream.advisory.v1.max_deliver";

fn valid_advisory(subject: &str, advisory: &MaxDeliveriesAdvisory) -> bool {
    advisory.advisory_type == MAX_DELIVERIES_ADVISORY_TYPE
        && !advisory.id.is_empty()
        && advisory.stream_seq > 0
        && advisory.deliveries > 0
        && time::OffsetDateTime::parse(
            &advisory.timestamp,
            &time::format_description::well_known::Rfc3339,
        )
        .is_ok()
        && subject
            == format!(
                "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.{}.{}",
                advisory.stream, advisory.consumer
            )
}

pub struct AdvisoryHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl AdvisoryHandle {
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
                "advisory loop task failed: {error}"
            ))),
        }
    }
}

pub fn map_dead_event_from_advisory_job(
    current: Option<&Job>,
    work_job: &Job,
    advisory: &MaxDeliveriesAdvisory,
) -> Option<MappedDeadEvent> {
    if current.is_some_and(|job| is_terminal(job.state)) {
        return None;
    }

    let previous_state = current.map(|job| job.state);
    let tries = current
        .map(|job| job.tries)
        .unwrap_or(advisory.deliveries)
        .max(advisory.deliveries);
    let reason = format!(
        "max deliveries exceeded: stream={} consumer={} deliveries={}",
        advisory.stream, advisory.consumer, advisory.deliveries
    );

    let mut event = dead(
        EventMeta {
            service: &work_job.service,
            job_type: &work_job.job_type,
            job_id: &work_job.id,
            context: &work_job.context,
            timestamp: &advisory.timestamp,
        },
        tries,
        previous_state.unwrap_or(JobState::Pending),
        &reason,
    );
    // The constructor requires a state, but missing projection evidence must remain unknown.
    event.previous_state = previous_state;
    let subject = job_event_subject(
        &work_job.service,
        &work_job.job_type,
        &work_job.id,
        event.event_type,
    );

    Some(MappedDeadEvent { subject, event })
}

pub async fn start_advisory_loop(
    jobs_runtime: JobsRuntime,
    store: SqliteJobsStore,
    jobs_advisories_stream: String,
    resolver: Arc<dyn JobResourceResolver>,
) -> Result<AdvisoryHandle, ServerError> {
    let mut messages = jobs_runtime
        .filtered_messages(
            &jobs_advisories_stream,
            ADVISORY_CONSUMER_NAME,
            MAX_DELIVERIES_ADVISORY_SUBJECT_WILDCARD,
        )
        .await
        .map_err(|error| {
            ServerError::Nats(format!(
                "failed to start jobs advisory consumer '{ADVISORY_CONSUMER_NAME}' message stream: {error}"
            ))
        })?;
    tracing::info!(
        stream = %jobs_advisories_stream,
        consumer = ADVISORY_CONSUMER_NAME,
        filter = MAX_DELIVERIES_ADVISORY_SUBJECT_WILDCARD,
        "started jobs advisory consumer"
    );

    let task = tokio::spawn(async move {
        tracing::debug!(
            stream = %jobs_advisories_stream,
            consumer = ADVISORY_CONSUMER_NAME,
            "jobs advisory loop running"
        );
        while let Some(message) = messages.next().await {
            let message = message.map_err(|error| {
                ServerError::Nats(format!(
                    "jobs advisory loop failed to pull from consumer '{ADVISORY_CONSUMER_NAME}' on stream '{jobs_advisories_stream}': {error}"
                ))
            })?;
            let Ok(advisory) = serde_json::from_slice::<MaxDeliveriesAdvisory>(message.payload())
            else {
                tracing::debug!(
                    subject = %message.subject(),
                    "acking non-advisory jobs message"
                );
                let _ = message.ack().await;
                continue;
            };
            if !valid_advisory(message.subject(), &advisory) {
                tracing::warn!(subject = %message.subject(), "acking invalid Jobs exhaustion advisory evidence");
                let _ = message.ack().await;
                continue;
            }
            tracing::debug!(
                stream = %advisory.stream,
                stream_seq = advisory.stream_seq,
                consumer = %advisory.consumer,
                deliveries = advisory.deliveries,
                "processing max-deliveries advisory"
            );

            let binding = match resolver
                .by_consumer(&advisory.stream, &advisory.consumer)
                .await
            {
                Ok(Some(binding)) => binding,
                Ok(None) => {
                    tracing::debug!(
                        stream = %advisory.stream,
                        consumer = %advisory.consumer,
                        "acking advisory for an unmanaged consumer"
                    );
                    let _ = message.ack().await;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, "retrying unclassified Jobs exhaustion advisory");
                    let _ = message
                        .ack_with(AckKind::Nak(Some(ADVISORY_RETRY_DELAY)))
                        .await;
                    continue;
                }
            };

            let raw_payload = match jobs_runtime
                .raw_payload(&advisory.stream, advisory.stream_seq)
                .await
            {
                Ok(payload) => payload,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        stream = %advisory.stream,
                        stream_seq = advisory.stream_seq,
                        "retrying Jobs exhaustion advisory with unavailable source evidence"
                    );
                    let _ = message
                        .ack_with(AckKind::Nak(Some(ADVISORY_RETRY_DELAY)))
                        .await;
                    continue;
                }
            };
            let Ok(work_event) = serde_json::from_slice::<JobEvent>(&raw_payload) else {
                tracing::debug!(
                    stream = %advisory.stream,
                    stream_seq = advisory.stream_seq,
                    "acking advisory for non-job payload"
                );
                let _ = message.ack().await;
                continue;
            };
            let Some(work_job) = job_from_work_event(&work_event) else {
                tracing::debug!(
                    service = %work_event.service,
                    job_type = %work_event.job_type,
                    job_id = %work_event.job_id,
                    "acking advisory for non-work event"
                );
                let _ = message.ack().await;
                continue;
            };
            if work_job.service != binding.service || work_job.job_type != binding.job_type {
                tracing::error!(
                    stream = %advisory.stream,
                    consumer = %advisory.consumer,
                    service = %work_job.service,
                    job_type = %work_job.job_type,
                    "acking Jobs advisory whose source does not match its managed binding"
                );
                let _ = message.ack().await;
                continue;
            }

            let mapped = match map_dead_event_from_store(&store, &work_job, &advisory) {
                Ok(mapped) => mapped,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        service = %work_job.service,
                        job_type = %work_job.job_type,
                        job_id = %work_job.id,
                        "retrying Jobs exhaustion advisory with unavailable projection evidence"
                    );
                    let _ = message
                        .ack_with(AckKind::Nak(Some(ADVISORY_RETRY_DELAY)))
                        .await;
                    continue;
                }
            };
            let Some(mapped) = mapped else {
                tracing::debug!(
                    service = %work_job.service,
                    job_type = %work_job.job_type,
                    job_id = %work_job.id,
                    "advisory did not map to dead event"
                );
                let _ = message.ack().await;
                continue;
            };

            let subject = mapped.subject.clone();
            if let Err(error) = jobs_runtime
                .publish_event(mapped.subject, &mapped.event)
                .await
            {
                tracing::warn!(%error, "retrying Jobs exhaustion advisory after publish failure");
                let _ = message
                    .ack_with(AckKind::Nak(Some(ADVISORY_RETRY_DELAY)))
                    .await;
                continue;
            }
            tracing::debug!(
                service = %work_job.service,
                job_type = %work_job.job_type,
                job_id = %work_job.id,
                subject = %subject,
                "published dead event from max-deliveries advisory"
            );
            let _ = message.ack().await;
        }
        tracing::info!(
            stream = %jobs_advisories_stream,
            consumer = ADVISORY_CONSUMER_NAME,
            "jobs advisory loop ended"
        );
        Ok(())
    });

    Ok(AdvisoryHandle { task: Some(task) })
}

fn map_dead_event_from_store(
    store: &SqliteJobsStore,
    work_job: &Job,
    advisory: &MaxDeliveriesAdvisory,
) -> Result<Option<MappedDeadEvent>, crate::storage::SqliteJobsStoreError> {
    let current = store.get_job(&work_job.service, &work_job.job_type, &work_job.id)?;
    Ok(map_dead_event_from_advisory_job(
        current.as_ref(),
        work_job,
        advisory,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_nats::jetstream::stream;
    use serde_json::json;
    use trellis_rs::jobs::events::created;
    use trellis_rs::jobs::types::{JobContext, JobState};

    use super::*;
    use crate::test_nats::TestNats;
    use crate::SqliteJobResourceResolver;

    fn job(id: &str, state: JobState) -> Job {
        Job {
            id: id.to_string(),
            context: context(id),
            service: "documents".to_string(),
            job_type: "document-process".to_string(),
            state,
            payload: json!({ "id": id }),
            result: None,
            created_at: "2026-03-28T11:00:00.000Z".to_string(),
            updated_at: "2026-03-28T11:59:00.000Z".to_string(),
            started_at: None,
            completed_at: None,
            tries: 2,
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

    fn advisory() -> MaxDeliveriesAdvisory {
        MaxDeliveriesAdvisory {
            advisory_type: MAX_DELIVERIES_ADVISORY_TYPE.to_owned(),
            id: "advisory-1".to_owned(),
            stream: "JOBS_WORK".to_string(),
            consumer: "documents-document-process".to_string(),
            stream_seq: 42,
            deliveries: 5,
            timestamp: "2026-03-28T12:03:00.000Z".to_string(),
        }
    }

    #[test]
    fn advisory_evidence_matches_its_broker_subject() {
        let advisory = advisory();
        assert!(valid_advisory(
            "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.JOBS_WORK.documents-document-process",
            &advisory
        ));
        assert!(!valid_advisory(
            "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.JOBS_WORK.other",
            &advisory
        ));
    }

    fn create_catalog(path: &std::path::Path, include_binding: bool) {
        let connection = rusqlite::Connection::open(path).expect("catalog should open");
        connection
            .execute_batch(
                "CREATE TABLE auth_resource_binding_evidence (
                    participant_id TEXT NOT NULL,
                    local_name TEXT NOT NULL,
                    provider_identity TEXT NOT NULL,
                    state TEXT NOT NULL,
                    resource_kind TEXT NOT NULL
                );",
            )
            .expect("catalog schema should initialize");
        if include_binding {
            connection
                .execute(
                    "INSERT INTO auth_resource_binding_evidence VALUES (?1,?2,?3,'available','jobQueue')",
                    rusqlite::params![
                        "documents",
                        "document-process",
                        json!({
                            "namespace": "documents",
                            "work_stream": "JOBS_WORK",
                            "consumer": "documents-document-process"
                        })
                        .to_string()
                    ],
                )
                .expect("catalog binding should insert");
        }
    }

    async fn create_advisory_streams(client: &async_nats::Client, include_work: bool) {
        let jetstream = async_nats::jetstream::new(client.clone());
        jetstream
            .create_stream(stream::Config {
                name: "JOBS_ADVISORIES".to_owned(),
                subjects: vec![MAX_DELIVERIES_ADVISORY_SUBJECT_WILDCARD.to_owned()],
                ..Default::default()
            })
            .await
            .expect("advisory stream should be created");
        jetstream
            .create_stream(stream::Config {
                name: "JOBS".to_owned(),
                subjects: vec!["trellis.jobs.>".to_owned()],
                ..Default::default()
            })
            .await
            .expect("Jobs stream should be created");
        if include_work {
            jetstream
                .create_stream(stream::Config {
                    name: "JOBS_WORK".to_owned(),
                    subjects: vec!["trellis.work.>".to_owned()],
                    allow_direct: true,
                    ..Default::default()
                })
                .await
                .expect("work stream should be created");
        }
    }

    async fn publish_advisory(client: &async_nats::Client) {
        let advisory = advisory();
        async_nats::jetstream::new(client.clone())
            .publish(
                "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.JOBS_WORK.documents-document-process",
                serde_json::to_vec(&json!({
                    "type": advisory.advisory_type,
                    "id": advisory.id,
                    "stream": advisory.stream,
                    "consumer": advisory.consumer,
                    "stream_seq": 1,
                    "deliveries": advisory.deliveries,
                    "timestamp": advisory.timestamp
                }))
                .expect("advisory should encode")
                .into(),
            )
            .await
            .expect("advisory should publish")
            .await
            .expect("advisory should persist");
    }

    #[test]
    fn advisory_maps_dead_event_using_sql_current_state() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let current = job("job-1", JobState::Active);
        store.upsert_job(&current).expect("upsert should succeed");

        let mapped = map_dead_event_from_store(&store, &current, &advisory())
            .expect("mapping should succeed")
            .expect("active job should map");

        assert_eq!(mapped.event.job_id, "job-1");
        assert_eq!(mapped.event.previous_state, Some(JobState::Active));
        assert_eq!(mapped.event.state, JobState::Dead);
        assert_eq!(mapped.event.tries, 5);
    }

    #[test]
    fn advisory_skips_terminal_sql_current_state() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let completed = job("job-1", JobState::Completed);
        store.upsert_job(&completed).expect("upsert should succeed");

        let mapped = map_dead_event_from_store(&store, &completed, &advisory())
            .expect("mapping should succeed");

        assert!(mapped.is_none());
    }

    #[tokio::test]
    async fn unknown_external_advisory_is_acked_without_fabricating_death() {
        let nats = TestNats::start().await;
        create_advisory_streams(&nats.client, false).await;
        let catalog = tempfile::NamedTempFile::new().expect("catalog path should exist");
        create_catalog(catalog.path(), false);
        let resolver: Arc<dyn JobResourceResolver> = Arc::new(SqliteJobResourceResolver::new(
            catalog.path().to_owned(),
            nats.client.clone(),
            "JOBS".to_owned(),
        ));
        let mut deaths = nats
            .client
            .subscribe("trellis.jobs.>")
            .await
            .expect("death subscription should start");
        let loop_handle = start_advisory_loop(
            JobsRuntime::from_nats(nats.client.clone()),
            SqliteJobsStore::open_in_memory().expect("store should open"),
            "JOBS_ADVISORIES".to_owned(),
            resolver,
        )
        .await
        .expect("advisory loop should start");

        publish_advisory(&nats.client).await;

        assert!(
            tokio::time::timeout(Duration::from_millis(500), deaths.next())
                .await
                .is_err()
        );
        loop_handle.stop().await;
    }

    #[tokio::test]
    async fn temporary_catalog_failure_is_retried_before_death_is_published() {
        let nats = TestNats::start().await;
        create_advisory_streams(&nats.client, true).await;
        let catalog = tempfile::NamedTempFile::new().expect("catalog path should exist");
        create_catalog(catalog.path(), true);
        let catalog_lock = rusqlite::Connection::open(catalog.path()).expect("catalog should open");
        catalog_lock
            .execute_batch("BEGIN EXCLUSIVE")
            .expect("catalog should lock");
        let resolver: Arc<dyn JobResourceResolver> = Arc::new(SqliteJobResourceResolver::new(
            catalog.path().to_owned(),
            nats.client.clone(),
            "JOBS".to_owned(),
        ));
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let work_event = created(
            EventMeta {
                service: "documents",
                job_type: "document-process",
                job_id: "job-1",
                context: &context("job-1"),
                timestamp: "2026-03-28T12:00:00.000Z",
            },
            json!({ "id": "job-1" }),
            5,
            None,
        );
        store
            .upsert_job(&job_from_work_event(&work_event).expect("work event should map"))
            .expect("job projection should persist");
        let mut deaths = nats
            .client
            .subscribe("trellis.jobs.>")
            .await
            .expect("death subscription should start");
        let loop_handle = start_advisory_loop(
            JobsRuntime::from_nats(nats.client.clone()),
            store,
            "JOBS_ADVISORIES".to_owned(),
            resolver,
        )
        .await
        .expect("advisory loop should start");
        async_nats::jetstream::new(nats.client.clone())
            .publish(
                "trellis.work.documents.document-process",
                serde_json::to_vec(&work_event)
                    .expect("work event should encode")
                    .into(),
            )
            .await
            .expect("work event should publish")
            .await
            .expect("work event should persist");
        publish_advisory(&nats.client).await;
        tokio::time::sleep(Duration::from_secs(6)).await;
        catalog_lock
            .execute_batch("COMMIT")
            .expect("catalog lock should release");

        let death = tokio::time::timeout(Duration::from_secs(20), deaths.next())
            .await
            .expect("advisory should retry")
            .expect("death subscription should remain open");
        let event: JobEvent = serde_json::from_slice(&death.payload).expect("death should decode");
        assert_eq!(event.job_id, "job-1");
        assert_eq!(event.state, JobState::Dead);
        loop_handle.stop().await;
    }
}
