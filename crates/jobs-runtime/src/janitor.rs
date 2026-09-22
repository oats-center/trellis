use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::jobs::events::{expired, EventMeta};
use trellis_rs::jobs::types::{Job, JobEvent};
use trellis_rs::jobs::JobsRuntime;
use trellis_rs::jobs::{is_terminal, job_event_subject, job_key};
use trellis_rs::service::ServerError;

use crate::storage::{SqliteJobsStore, SqliteJobsStoreError};

const EXPIRED_REASON: &str = "job exceeded deadline";

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedExpiredEvent {
    pub key: String,
    pub subject: String,
    pub event: JobEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JanitorRunStats {
    pub scanned: usize,
    pub eligible: usize,
    pub published: usize,
}

pub struct JanitorHandle {
    task: Option<tokio::task::JoinHandle<Result<(), ServerError>>>,
}

impl JanitorHandle {
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
                "janitor loop task failed: {error}"
            ))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JanitorError {
    #[error("failed to scan jobs SQLite projection: {0}")]
    Store(#[from] SqliteJobsStoreError),
    #[error("failed to publish expired event on subject '{subject}': {details}")]
    Publish { subject: String, details: String },
    #[error("failed to encode expired event payload for key '{key}': {details}")]
    EncodeEvent { key: String, details: String },
}

pub fn plan_expired_events(
    jobs_by_key: &[(String, Job)],
    now_iso: &str,
    reason: &str,
) -> Vec<PlannedExpiredEvent> {
    let now = parse_timestamp(now_iso);
    jobs_by_key
        .iter()
        .filter_map(|(key, job)| {
            let deadline = job.deadline.as_deref()?;
            if parse_timestamp(deadline) > now {
                return None;
            }
            if is_terminal(job.state) {
                return None;
            }

            let event = expired(
                EventMeta {
                    service: &job.service,
                    job_type: &job.job_type,
                    job_id: &job.id,
                    context: &job.context,
                    timestamp: now_iso,
                },
                job.tries,
                job.state,
                reason,
            );
            let subject = job_event_subject(&job.service, &job.job_type, &job.id, event.event_type);
            Some(PlannedExpiredEvent {
                key: key.clone(),
                subject,
                event,
            })
        })
        .collect()
}

pub async fn run_janitor_once(
    jobs_runtime: JobsRuntime,
    store: &SqliteJobsStore,
    now_iso: &str,
) -> Result<JanitorRunStats, JanitorError> {
    let (scanned, planned) = plan_expired_events_from_store(store, now_iso)?;
    let eligible = planned.len();

    let mut published = 0usize;
    for plan in planned {
        jobs_runtime
            .publish_event(plan.subject.clone(), &plan.event)
            .await
            .map_err(|error| JanitorError::Publish {
                subject: plan.subject,
                details: error,
            })?;
        published += 1;
    }

    Ok(JanitorRunStats {
        scanned,
        eligible,
        published,
    })
}

fn plan_expired_events_from_store(
    store: &SqliteJobsStore,
    now_iso: &str,
) -> Result<(usize, Vec<PlannedExpiredEvent>), JanitorError> {
    let jobs = store.scan_expired_jobs(now_iso)?;
    let jobs_by_key = jobs
        .into_iter()
        .map(|job| (job_key(&job.service, &job.job_type, &job.id), job))
        .collect::<Vec<_>>();
    let scanned = jobs_by_key.len();
    let planned = plan_expired_events(&jobs_by_key, now_iso, EXPIRED_REASON);
    Ok((scanned, planned))
}

fn parse_timestamp(timestamp: &str) -> OffsetDateTime {
    OffsetDateTime::parse(timestamp, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

pub async fn start_janitor_loop(
    jobs_runtime: JobsRuntime,
    store: SqliteJobsStore,
    interval: std::time::Duration,
) -> Result<JanitorHandle, ServerError> {
    tracing::info!(
        interval_ms = interval.as_millis(),
        "started jobs janitor loop"
    );
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        loop {
            ticker.tick().await;
            let now = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
            let stats = run_janitor_once(jobs_runtime.clone(), &store, &now)
                .await
                .map_err(|error| {
                    ServerError::Nats(format!(
                        "jobs janitor loop failed for SQLite projection: {error}"
                    ))
                })?;
            tracing::debug!(
                scanned = stats.scanned,
                eligible = stats.eligible,
                published = stats.published,
                now = %now,
                "completed jobs janitor run"
            );
        }
    });

    Ok(JanitorHandle { task: Some(task) })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_rs::jobs::types::{JobContext, JobState};

    use super::*;

    fn job(id: &str, state: JobState, deadline: Option<&str>) -> Job {
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
            tries: 1,
            max_tries: 3,
            last_error: None,
            error_detail: None,
            deadline: deadline.map(str::to_string),
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
    fn janitor_plans_expired_events_from_sql_projection() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        for projected in [
            job(
                "overdue-active",
                JobState::Active,
                Some("2026-03-28T12:00:00.000Z"),
            ),
            job(
                "future-active",
                JobState::Active,
                Some("2026-03-28T12:10:00.000Z"),
            ),
            job(
                "overdue-completed",
                JobState::Completed,
                Some("2026-03-28T12:00:00.000Z"),
            ),
        ] {
            store.upsert_job(&projected).expect("upsert should succeed");
        }

        let (scanned, planned) = plan_expired_events_from_store(&store, "2026-03-28T12:01:00.000Z")
            .expect("planning should succeed");

        assert_eq!(scanned, 1);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].event.job_id, "overdue-active");
        assert_eq!(planned[0].event.state, JobState::Expired);
    }

    #[test]
    fn janitor_plans_equivalent_deadline_timestamp_variants() {
        let jobs = vec![
            (
                "documents.document-process.at-millis".to_string(),
                job(
                    "at-millis",
                    JobState::Pending,
                    Some("2026-03-28T12:01:00.000Z"),
                ),
            ),
            (
                "documents.document-process.at-offset".to_string(),
                job(
                    "at-offset",
                    JobState::Pending,
                    Some("2026-03-28T08:01:00-04:00"),
                ),
            ),
            (
                "documents.document-process.future-offset".to_string(),
                job(
                    "future-offset",
                    JobState::Pending,
                    Some("2026-03-28T08:01:01-04:00"),
                ),
            ),
        ];

        let planned = plan_expired_events(&jobs, "2026-03-28T12:01:00Z", EXPIRED_REASON);

        assert_eq!(
            planned
                .iter()
                .map(|event| event.event.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["at-millis", "at-offset"]
        );
    }
}
