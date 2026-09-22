use std::time::Duration;

use async_nats::jetstream::{self, stream};
use futures_util::stream::BoxStream;
use futures_util::{FutureExt, StreamExt};

use crate::jobs::bindings::JobsQueueBinding;
use crate::jobs::projection::{is_terminal, reduce_job_event};
use crate::jobs::types::{Job, JobEvent, JobEventType};
use crate::jobs::updates::{validate_update, JobUpdate, JobUpdateDescriptor};
use crate::jobs::JobsError;

const JOBS_STREAM: &str = "JOBS";

/// NATS-backed implementation of Trellis service-local job waiting.
#[derive(Clone)]
pub struct NatsJobWaiter {
    nats: async_nats::Client,
    queue: JobsQueueBinding,
    timeout: Duration,
}

impl NatsJobWaiter {
    /// Create a waiter for one bound service-local jobs queue.
    pub fn new(nats: async_nats::Client, queue: JobsQueueBinding, timeout: Duration) -> Self {
        Self {
            nats,
            queue,
            timeout,
        }
    }

    /// Wait until the given job reaches a terminal lifecycle state.
    pub async fn wait_for_terminal(&self, seed: Job) -> Result<Job, JobsError> {
        let subject = format!("{}.{}.*", self.queue.publish_prefix, seed.id);
        let mut subscriber =
            self.nats.subscribe(subject).await.map_err(|error| {
                jobs_message(format!("job lifecycle subscribe failed: {error}"))
            })?;
        self.nats.flush().await.map_err(|error| {
            jobs_message(format!("job lifecycle subscribe flush failed: {error}"))
        })?;
        let jetstream = jetstream::new(self.nats.clone());
        let lifecycle_stream = jetstream
            .get_stream_no_info(JOBS_STREAM)
            .await
            .map_err(|error| jobs_message(format!("open jobs lifecycle stream failed: {error}")))?;

        let mut current = seed;
        if let Some(message) =
            latest_terminal_message(&lifecycle_stream, &self.queue.publish_prefix, &current.id)
                .await?
        {
            let event: JobEvent = serde_json::from_slice(&message.payload)
                .map_err(|error| jobs_message(format!("decode job lifecycle event: {error}")))?;
            current = apply_lifecycle_event(&current, &event);
            if is_terminal(current.state) {
                return Ok(current);
            }
        }

        let timeout_job_id = current.id.clone();
        let wait = async {
            while let Some(message) = subscriber.next().await {
                let event: JobEvent =
                    serde_json::from_slice(&message.payload).map_err(|error| {
                        jobs_message(format!("decode job lifecycle event: {error}"))
                    })?;
                if event.job_id != current.id || event.job_type != self.queue.queue_type {
                    continue;
                }
                current = apply_lifecycle_event(&current, &event);
                if is_terminal(current.state) {
                    return Ok(current);
                }
            }
            Err(jobs_message(format!(
                "job lifecycle subscription ended before terminal event for job '{}'",
                current.id
            )))
        };

        tokio::time::timeout(self.timeout, wait)
            .await
            .map_err(|_| jobs_message(format!("job '{timeout_job_id}' timed out")))?
    }

    /// Read the latest lifecycle snapshot for one service-local job.
    pub async fn get(&self, seed: Job) -> Result<Job, JobsError> {
        let subject = format!("{}.{}.*", self.queue.publish_prefix, seed.id);
        let jetstream = jetstream::new(self.nats.clone());
        let lifecycle_stream = jetstream
            .get_stream_no_info(JOBS_STREAM)
            .await
            .map_err(|error| jobs_message(format!("open jobs lifecycle stream failed: {error}")))?;
        project_job_from_lifecycle(&lifecycle_stream, &subject, seed).await
    }

    /// Subscribe to validated live-only updates until terminal lifecycle state.
    pub async fn updates<D>(
        &self,
        job_id: impl Into<String>,
    ) -> Result<BoxStream<'static, Result<JobUpdate<D::Update>, JobsError>>, JobsError>
    where
        D: JobUpdateDescriptor,
    {
        let job_id = job_id.into();
        let bound_schema = self.queue.update.as_deref().ok_or_else(|| {
            jobs_message(format!(
                "jobs queue '{}' does not declare updates",
                self.queue.queue_type
            ))
        })?;
        let updates_prefix = self.queue.updates_prefix.as_deref().ok_or_else(|| {
            jobs_message(format!(
                "jobs queue '{}' has no updates prefix",
                self.queue.queue_type
            ))
        })?;
        if D::QUEUE_TYPE != self.queue.queue_type || D::UPDATE_SCHEMA != bound_schema {
            return Err(jobs_message(format!(
                "job update descriptor does not match queue '{}'",
                self.queue.queue_type
            )));
        }
        let lifecycle_subject = format!("{}.{}.*", self.queue.publish_prefix, job_id);
        let lifecycle = self
            .nats
            .subscribe(lifecycle_subject.clone())
            .await
            .map_err(|error| jobs_message(format!("job lifecycle subscribe failed: {error}")))?;
        let updates_subject = format!("{updates_prefix}.{job_id}");
        let updates = self
            .nats
            .subscribe(updates_subject)
            .await
            .map_err(|error| jobs_message(format!("job updates subscribe failed: {error}")))?;
        tokio::time::timeout(self.timeout, self.nats.flush())
            .await
            .map_err(|_| jobs_message("flush job update subscriptions timed out".to_string()))?
            .map_err(|error| {
                jobs_message(format!("flush job update subscriptions failed: {error}"))
            })?;
        let lifecycle_stream = jetstream::new(self.nats.clone())
            .get_stream_no_info(JOBS_STREAM)
            .await
            .map_err(|error| jobs_message(format!("open jobs lifecycle stream failed: {error}")))?;
        let latest = latest_lifecycle_message(&lifecycle_stream, &lifecycle_subject).await?;
        let mut attempt = 0;
        let mut terminal = false;
        if let Some(message) = latest {
            let event: JobEvent = serde_json::from_slice(&message.payload)
                .map_err(|error| jobs_message(format!("decode job lifecycle event: {error}")))?;
            if event.job_id == job_id && event.job_type == self.queue.queue_type {
                attempt = event.tries;
                terminal = matches!(
                    event.event_type,
                    JobEventType::Completed
                        | JobEventType::Failed
                        | JobEventType::Cancelled
                        | JobEventType::Expired
                        | JobEventType::Skipped
                        | JobEventType::Dead
                        | JobEventType::Dismissed
                );
            }
        }

        let queue_type = self.queue.queue_type.clone();
        Ok(Box::pin(futures_util::stream::unfold(
            (lifecycle, updates, attempt, 0_u64, terminal),
            move |(mut lifecycle, mut updates, mut attempt, mut sequence, mut terminal)| {
                let job_id = job_id.clone();
                let queue_type = queue_type.clone();
                async move {
                    loop {
                        let message = if terminal {
                            updates.next().now_or_never().flatten()?
                        } else {
                            tokio::select! {
                                biased;
                                message = updates.next() => message?,
                                message = lifecycle.next() => {
                                    let message = message?;
                                    let event: JobEvent = match serde_json::from_slice(&message.payload) {
                                        Ok(event) => event,
                                        Err(error) => return Some((Err(jobs_message(format!("decode job lifecycle event: {error}"))), (lifecycle, updates, attempt, sequence, true))),
                                    };
                                    if event.job_id != job_id || event.job_type != queue_type {
                                        continue;
                                    }
                                    if event.tries > attempt {
                                        attempt = event.tries;
                                        sequence = 0;
                                    }
                                    if matches!(event.event_type, JobEventType::Completed | JobEventType::Failed | JobEventType::Cancelled | JobEventType::Expired | JobEventType::Skipped | JobEventType::Dead | JobEventType::Dismissed) {
                                        terminal = true;
                                    }
                                    continue;
                                }
                            }
                        };
                        let value: serde_json::Value =
                            match serde_json::from_slice(&message.payload) {
                                Ok(value) => value,
                                Err(error) => {
                                    return Some((
                                        Err(jobs_message(format!("decode job update: {error}"))),
                                        (lifecycle, updates, attempt, sequence, true),
                                    ))
                                }
                            };
                        let Some(update_value) = value.get("update") else {
                            return Some((
                                Err(jobs_message(
                                    "job update envelope is missing update".to_string(),
                                )),
                                (lifecycle, updates, attempt, sequence, true),
                            ));
                        };
                        if let Err(error) = validate_update(D::UPDATE_SCHEMA_JSON, update_value) {
                            return Some((
                                Err(jobs_message(error.to_string())),
                                (lifecycle, updates, attempt, sequence, true),
                            ));
                        }
                        let update: JobUpdate<D::Update> = match serde_json::from_value(value) {
                            Ok(update) => update,
                            Err(error) => {
                                return Some((
                                    Err(jobs_message(format!("decode typed job update: {error}"))),
                                    (lifecycle, updates, attempt, sequence, true),
                                ))
                            }
                        };
                        if update.job_id != job_id
                            || update.attempt < attempt
                            || (update.attempt == attempt && update.sequence <= sequence)
                        {
                            continue;
                        }
                        if update.attempt > attempt {
                            attempt = update.attempt;
                        }
                        sequence = update.sequence;
                        return Some((
                            Ok(update),
                            (lifecycle, updates, attempt, sequence, terminal),
                        ));
                    }
                }
            },
        )))
    }
}

async fn project_job_from_lifecycle(
    lifecycle_stream: &stream::Stream<()>,
    subject: &str,
    mut current: Job,
) -> Result<Job, JobsError> {
    let mut sequence = 1_u64;
    loop {
        let message = match lifecycle_stream
            .get_first_raw_message_by_subject(subject, sequence)
            .await
        {
            Ok(message) => message,
            Err(error) if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound) => {
                return Ok(current);
            }
            Err(error) => {
                return Err(jobs_message(format!(
                    "read job lifecycle history failed: {error}"
                )));
            }
        };
        sequence = message
            .sequence
            .checked_add(1)
            .ok_or_else(|| jobs_message("job lifecycle stream sequence overflow".to_string()))?;
        let event: JobEvent = serde_json::from_slice(&message.payload)
            .map_err(|error| jobs_message(format!("decode job lifecycle event: {error}")))?;
        current = apply_lifecycle_event(&current, &event);
    }
}

async fn latest_terminal_message(
    lifecycle_stream: &stream::Stream<()>,
    publish_prefix: &str,
    job_id: &str,
) -> Result<Option<async_nats::jetstream::message::StreamMessage>, JobsError> {
    let mut latest = None;
    for event_type in [
        JobEventType::Completed,
        JobEventType::Failed,
        JobEventType::Cancelled,
        JobEventType::Expired,
        JobEventType::Skipped,
        JobEventType::Stale,
        JobEventType::Dead,
        JobEventType::Dismissed,
    ] {
        let subject = format!("{publish_prefix}.{job_id}.{}", event_type.as_token());
        if let Some(message) = latest_lifecycle_message(lifecycle_stream, &subject).await? {
            if latest.as_ref().is_none_or(
                |latest: &async_nats::jetstream::message::StreamMessage| {
                    message.sequence > latest.sequence
                },
            ) {
                latest = Some(message);
            }
        }
    }
    Ok(latest)
}

async fn latest_lifecycle_message(
    lifecycle_stream: &stream::Stream<()>,
    subject: &str,
) -> Result<Option<async_nats::jetstream::message::StreamMessage>, JobsError> {
    match lifecycle_stream.direct_get_last_for_subject(subject).await {
        Ok(message) => return Ok(Some(message)),
        Err(error) if matches!(error.kind(), stream::DirectGetErrorKind::NotFound) => {}
        Err(direct_error) => match lifecycle_stream
            .get_last_raw_message_by_subject(subject)
            .await
        {
            Ok(message) => return Ok(Some(message)),
            Err(error)
                if matches!(
                    error.kind(),
                    stream::LastRawMessageErrorKind::NoMessageFound
                ) => {}
            Err(error) => {
                return Err(jobs_message(format!(
                    "direct get failed: {direct_error}; raw get failed: {error}"
                )));
            }
        },
    }

    match lifecycle_stream
        .get_last_raw_message_by_subject(subject)
        .await
    {
        Ok(message) => Ok(Some(message)),
        Err(error)
            if matches!(
                error.kind(),
                stream::LastRawMessageErrorKind::NoMessageFound
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(jobs_message(format!("raw get failed: {error}"))),
    }
}

fn apply_lifecycle_event(current: &Job, event: &JobEvent) -> Job {
    if event.service != current.service
        || event.job_type != current.job_type
        || event.job_id != current.id
    {
        return current.clone();
    }
    let next = reduce_job_event(Some(current), event).unwrap_or_else(|| current.clone());
    if next.state == current.state && is_terminal(event.state) {
        return terminal_job_from_event(current, event);
    }
    next
}

fn terminal_job_from_event(current: &Job, event: &JobEvent) -> Job {
    let mut next = current.clone();
    next.state = event.state;
    next.updated_at = event.timestamp.clone();
    next.completed_at = Some(event.timestamp.clone());
    next.tries = event.tries;
    next.max_tries = event.max_tries.unwrap_or(current.max_tries);
    match event.event_type {
        JobEventType::Completed => {
            next.result = event.result.clone();
        }
        JobEventType::Failed
        | JobEventType::Cancelled
        | JobEventType::Expired
        | JobEventType::Skipped
        | JobEventType::Stale
        | JobEventType::Dead
        | JobEventType::Dismissed => {
            next.last_error = event.error.clone();
            next.error_detail = event.error_detail.clone().or_else(|| {
                event.error.as_deref().map(|message| {
                    crate::jobs::types::JobErrorDetail::from_message(
                        &event.service,
                        &event.job_type,
                        message,
                    )
                })
            });
        }
        _ => {}
    }
    next
}

fn jobs_message(message: String) -> JobsError {
    JobsError::Message { message }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{apply_lifecycle_event, terminal_job_from_event};
    use crate::jobs::events::{completed, started, EventMeta};
    use crate::jobs::types::{Job, JobContext, JobState};

    fn sample_context() -> JobContext {
        JobContext {
            request_id: "request-1".to_string(),
            trace_id: "0123456789abcdef0123456789abcdef".to_string(),
            traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
            tracestate: None,
        }
    }

    fn sample_job() -> Job {
        Job {
            id: "job-1".to_string(),
            context: sample_context(),
            service: "svc".to_string(),
            job_type: "refresh".to_string(),
            state: JobState::Pending,
            payload: json!({ "siteId": "site-1" }),
            result: None,
            created_at: "2026-05-03T00:00:00.000Z".to_string(),
            updated_at: "2026-05-03T00:00:00.000Z".to_string(),
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
        }
    }

    #[test]
    fn apply_lifecycle_event_applies_legal_transition() {
        let job = sample_job();
        let event = started(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-05-03T00:00:01.000Z",
            },
            1,
            JobState::Pending,
        );

        let next = apply_lifecycle_event(&job, &event);

        assert_eq!(next.state, JobState::Active);
        assert_eq!(next.started_at.as_deref(), Some("2026-05-03T00:00:01.000Z"));
    }

    #[test]
    fn terminal_job_from_event_handles_latest_terminal_event_without_prior_events() {
        let job = sample_job();
        let event = completed(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-05-03T00:00:02.000Z",
            },
            1,
            json!({ "ok": true }),
        );

        let next = terminal_job_from_event(&job, &event);

        assert_eq!(next.state, JobState::Completed);
        assert_eq!(next.result, Some(json!({ "ok": true })));
        assert_eq!(
            next.completed_at.as_deref(),
            Some("2026-05-03T00:00:02.000Z")
        );
    }
}
