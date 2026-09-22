//! SQLite-backed query and mutation helpers for the Jobs admin service.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::jobs::events::{cancelled_by_admin, dismissed, retried_by_admin, EventMeta};
use trellis_rs::jobs::types::{
    Job, JobAdminAction, JobErrorDetail, JobEvent, JobState, JobTrigger, JobTriggerKind,
};
use trellis_rs::jobs::JobsRuntime;
use trellis_rs::jobs::{is_terminal, job_event_subject, reduce_job_event};

use trellis_runtime_apis::types::{
    JobsCancelRequest, JobsCancelResponse, JobsDismissDLQRequest, JobsDismissDLQResponse,
    JobsGetKeyRequest, JobsGetKeyResponse, JobsInspectRequest, JobsInspectResponse,
    JobsInspectResponseerrorsItem as JobsInspectResponseErrorsItem,
    JobsInspectResponserelatedItem as JobsInspectResponseRelatedItem,
    JobsInspectResponsetimelineItem as JobsInspectResponseTimelineItem,
    JobsInspectResponsetimelineItemerrorDetail as JobsInspectResponseTimelineItemErrorDetail,
    JobsListDLQRequest, JobsListDLQResponse, JobsListServicesRequest, JobsListServicesResponse,
    JobsListServicesResponseentriesItem as JobsListServicesResponseEntriesItem,
    JobsListServicesResponseentriesItemworkersItem as JobsListServicesResponseEntriesItemWorkersItem,
    JobsMetricsRequest, JobsMetricsResponse,
    JobsMetricsResponsebucketsItem as JobsMetricsResponseBucketsItem,
    JobsMetricsResponsebucketsItemgroupsItem as JobsMetricsResponseBucketsItemGroupsItem,
    JobsMetricsResponsebucketsItemgroupsItemqueueWait as JobsMetricsResponseBucketsItemGroupsItemQueueWait,
    JobsMetricsResponsebucketsItemgroupsItemruntime as JobsMetricsResponseBucketsItemGroupsItemRuntime,
    JobsMetricsResponsesummaryItem as JobsMetricsResponseSummaryItem,
    JobsMetricsResponsesummaryItemqueueWait as JobsMetricsResponseSummaryItemQueueWait,
    JobsMetricsResponsesummaryItemruntime as JobsMetricsResponseSummaryItemRuntime,
    JobsQueryRequest, JobsQueryResponse,
    JobsQueryResponseentriesItem as JobsQueryResponseEntriesItem,
    JobsQueryResponsegroupsItem as JobsQueryResponseGroupsItem,
    JobsQueryResponsestats as JobsQueryResponseStats, JobsReplayDLQRequest, JobsReplayDLQResponse,
    JobsRetryRequest, JobsRetryResponse, JobsSummaryRequest, JobsSummaryResponse,
};

mod resources;
mod state;
mod wire;
use crate::storage::{
    JobProjectionMetadata, JobTimelineEvent, JobsMetricsBucket, JobsMetricsBucketGroup,
    JobsMetricsFilter, JobsMetricsLatency, JobsMetricsSummaryGroup, JobsWorkbenchEntry,
    JobsWorkbenchFilter, JobsWorkbenchGroup, JobsWorkbenchGroupBy, JobsWorkbenchSort,
    JobsWorkbenchSortField, JobsWorkbenchStats, ListJobsFilter, SqliteJobsStore,
    SqliteJobsStoreError,
};
use crate::worker_presence::WORKER_PRESENCE_FRESH_FOR;
use crate::JobResourceResolver;

pub use resources::jobs_admin_resources;
pub use resources::JobsAdminResources;
use state::{now_timestamp_string, parse_state_filter};
use wire::{
    job_to_cancel_item, job_to_dismiss_item, job_to_dlq_item, job_to_inspect_item,
    job_to_replay_item, job_to_retry_item,
};

/// Errors returned while resolving bindings, reading projection state, or publishing admin events.
#[derive(Debug, thiserror::Error)]
pub enum JobsQueryError {
    #[error("job state conflict for key '{key}': expected '{expected}', found '{actual}'")]
    JobStateConflict {
        key: String,
        expected: String,
        actual: String,
    },
    #[error("projected job not found for key '{key}'")]
    JobNotFound { key: String },
    #[error("failed to encode job event for key '{key}': {details}")]
    EncodeEvent { key: String, details: String },
    #[error("failed to publish job event on subject '{subject}': {details}")]
    PublishEvent { subject: String, details: String },
    #[error("failed to read Jobs SQLite projection: {details}")]
    ProjectionStore { details: String },
    #[error("failed to convert {model} between internal and generated wire shapes: {details}")]
    ConvertWireModel {
        model: &'static str,
        details: String,
    },
    #[error("invalid {field}: {details}")]
    Validation {
        field: &'static str,
        details: String,
    },
}

#[derive(Clone)]
pub struct JobsQuery {
    jobs_runtime: JobsRuntime,
    store: SqliteJobsStore,
    resource_resolver: Arc<dyn JobResourceResolver>,
}

impl JobsQuery {
    /// Create a SQLite-backed Jobs query adapter with an already-open store.
    pub fn with_store(
        jobs_runtime: JobsRuntime,
        store: SqliteJobsStore,
        resource_resolver: Arc<dyn JobResourceResolver>,
    ) -> Self {
        Self {
            jobs_runtime,
            store,
            resource_resolver,
        }
    }

    async fn with_projection<T, F>(&self, f: F) -> Result<T, JobsQueryError>
    where
        T: Send + 'static,
        F: FnOnce(SqliteJobsStore) -> Result<T, JobsQueryError> + Send + 'static,
    {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(store))
            .await
            .map_err(|error| JobsQueryError::ProjectionStore {
                details: format!("projection task failed: {error}"),
            })?
    }

    /// List registered service instances grouped by service name.
    pub async fn list_services(
        &self,
        request: &JobsListServicesRequest,
    ) -> Result<JobsListServicesResponse, JobsQueryError> {
        let started = Instant::now();
        let query_digest = pagination_digest("jobs.ListServices", request)?;
        let limit = u64::from(
            request
                .page
                .as_ref()
                .and_then(|page| page.limit)
                .unwrap_or(100),
        );
        if limit == 0 || limit > 500 {
            return Err(JobsQueryError::Validation {
                field: "page.limit",
                details: "must be between 1 and 500".to_owned(),
            });
        }
        let after = request
            .page
            .as_ref()
            .and_then(|page| page.cursor.as_deref())
            .map(|cursor| {
                trellis_protocol::decode_pagination_cursor::<String>(cursor, &query_digest).map_err(
                    |_| JobsQueryError::Validation {
                        field: "page.cursor",
                        details: "invalid pagination".to_owned(),
                    },
                )
            })
            .transpose()?;
        tracing::debug!(after = ?after, limit, "jobs rpc list_services started");
        let now = OffsetDateTime::now_utc();
        let workers = self
            .with_projection(move |store| {
                Ok(store.list_fresh_workers(now, WORKER_PRESENCE_FRESH_FOR)?)
            })
            .await?;

        let mut grouped =
            BTreeMap::<String, Vec<JobsListServicesResponseEntriesItemWorkersItem>>::new();
        for worker in workers {
            let service_name = worker.service.clone();
            grouped
                .entry(service_name)
                .or_default()
                .push(wire::worker_presence_to_wire(&worker));
        }

        let mut services = Vec::<JobsListServicesResponseEntriesItem>::new();
        for (name, mut workers) in grouped {
            workers.sort_by(|left, right| {
                left.job_type
                    .0
                    .cmp(&right.job_type.0)
                    .then_with(|| left.instance_id.0.cmp(&right.instance_id.0))
            });
            services.push(wire::decode_wire(
                json!({ "healthy": !workers.is_empty(), "name": name, "workers": workers }),
                "jobs list services entry",
            )?);
        }
        let mut services: Vec<_> = services
            .into_iter()
            .filter(|service| after.as_ref().is_none_or(|after| &service.name.0 > after))
            .take(usize::try_from(limit + 1).unwrap_or(usize::MAX))
            .collect();
        let next_cursor = if services.len() > limit as usize {
            services.pop();
            services
                .last()
                .map(|service| {
                    trellis_protocol::encode_pagination_cursor(&query_digest, &service.name.0)
                })
                .transpose()
                .map_err(|_| JobsQueryError::Validation {
                    field: "page.cursor",
                    details: "invalid pagination".to_owned(),
                })?
        } else {
            None
        };
        tracing::debug!(
            limit,
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc list_services completed"
        );

        wire::decode_wire(
            json!({
                "items": services,
                "page": { "nextCursor": next_cursor },
            }),
            "jobs list services response",
        )
    }

    /// Query projected jobs using the generated `Jobs.Query` workbench wire shape.
    pub async fn query_jobs(
        &self,
        request: &JobsQueryRequest,
    ) -> Result<JobsQueryResponse, JobsQueryError> {
        let started = Instant::now();
        let query_digest = pagination_digest("jobs.Query", request)?;
        let limit = u64::from(
            request
                .page
                .as_ref()
                .and_then(|page| page.limit)
                .unwrap_or(100),
        );
        if limit == 0 || limit > 500 {
            return Err(JobsQueryError::Validation {
                field: "page.limit",
                details: "must be between 1 and 500".to_owned(),
            });
        }
        let after = request
            .page
            .as_ref()
            .and_then(|page| page.cursor.as_deref())
            .map(|cursor| {
                trellis_protocol::decode_pagination_cursor::<(
                    Option<i64>,
                    i64,
                    String,
                    String,
                    String,
                )>(cursor, &query_digest)
                .map_err(|_| JobsQueryError::Validation {
                    field: "page.cursor",
                    details: "invalid pagination".to_owned(),
                })
            })
            .transpose()?;
        let since = parse_window_filter(request.window.as_ref().map(AsRef::as_ref))?;
        tracing::debug!(
            service = ?request.service,
            job_type = ?request.r#type,
            state = ?request.state,
            window = ?request.window,
            group_by = ?request.group_by,
            search = request.search.is_some(),
            after = ?after,
            limit,
            "jobs rpc query started"
        );
        let filter = JobsWorkbenchFilter {
            service: request.service.as_ref().map(|value| value.0.clone()),
            job_type: request.r#type.as_ref().map(|value| value.0.clone()),
            states: parse_state_filter(request.state.as_ref())?,
            since,
            search: request.search.clone(),
            queue_key: request.queue_key.clone(),
            runtime_band: request.runtime_band.as_ref().map(wire_token),
            trigger: request.trigger.clone(),
            sort: parse_workbench_sort(request.sort.as_ref())?,
            group_by: parse_group_by(request.group_by.as_ref().map(wire_token).as_deref())?,
            after,
            limit,
        };
        let page = self
            .with_projection(move |store| Ok(store.query_jobs(&filter)?))
            .await?;
        let next_cursor = page
            .next_after
            .as_ref()
            .map(|after| trellis_protocol::encode_pagination_cursor(&query_digest, after))
            .transpose()
            .map_err(|_| JobsQueryError::Validation {
                field: "page.cursor",
                details: "invalid pagination".to_owned(),
            })?;
        tracing::debug!(
            count = page.count,
            entries = page.entries.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc query completed"
        );

        let entries = page
            .entries
            .iter()
            .map(workbench_entry_to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        wire::decode_wire(
            json!({
                "items": entries,
                "page": { "nextCursor": next_cursor },
            }),
            "jobs query response",
        )
    }

    /// Summarize all projected jobs matching the generated `Jobs.Summary` filters.
    pub async fn summary(
        &self,
        request: &JobsSummaryRequest,
    ) -> Result<JobsSummaryResponse, JobsQueryError> {
        let filter = JobsWorkbenchFilter {
            service: request.service.as_ref().map(|value| value.0.clone()),
            job_type: request.r#type.as_ref().map(|value| value.0.clone()),
            states: parse_state_filter(request.state.as_ref())?,
            since: parse_window_filter(request.window.as_deref())?,
            search: request.search.clone(),
            queue_key: request.queue_key.clone(),
            runtime_band: request.runtime_band.as_ref().map(wire_token),
            trigger: request.trigger.clone(),
            sort: JobsWorkbenchSort::default(),
            group_by: parse_group_by(request.group_by.as_ref().map(wire_token).as_deref())?,
            after: None,
            limit: 1,
        };
        let (page, groups) = self
            .with_projection(move |store| {
                Ok((store.query_jobs(&filter)?, store.query_job_groups(&filter)?))
            })
            .await?;
        wire::decode_wire(
            json!({
                "count": page.count,
                "groups": groups.iter().map(workbench_group_to_wire)
                    .collect::<Result<Vec<_>, _>>()?,
                "stats": workbench_stats_to_wire(&page.stats)?,
            }),
            "jobs summary response",
        )
    }

    /// Query grouped operational metrics for Jobs dashboards.
    pub async fn metrics(
        &self,
        request: &JobsMetricsRequest,
    ) -> Result<JobsMetricsResponse, JobsQueryError> {
        let started = Instant::now();
        let until = OffsetDateTime::now_utc();
        let window = parse_metrics_window(request.window.as_str())?;
        tracing::debug!(
            service = ?request.service,
            job_type = ?request.r#type,
            state = ?request.state,
            window = %request.window,
            step = %request.step,
            group_by = %wire_token(&request.group_by),
            "jobs rpc metrics started"
        );
        let filter = JobsMetricsFilter {
            service: request.service.as_ref().map(|value| value.0.clone()),
            job_type: request.r#type.as_ref().map(|value| value.0.clone()),
            states: parse_state_filter(request.state.as_ref())?,
            since: until - window,
            until,
            step_nanos: parse_metrics_step(request.step.as_str())?
                .whole_nanoseconds()
                .try_into()
                .map_err(
                    |error: std::num::TryFromIntError| JobsQueryError::Validation {
                        field: "step",
                        details: error.to_string(),
                    },
                )?,
            queue_key: request.queue_key.clone(),
            trigger: request.trigger.clone(),
            group_by: parse_metrics_group_by(&wire_token(&request.group_by))?,
        };
        let page = self
            .with_projection(move |store| Ok(store.query_metrics(&filter)?))
            .await?;
        tracing::debug!(
            buckets = page.buckets.len(),
            summary = page.summary.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc metrics completed"
        );
        let buckets = page
            .buckets
            .iter()
            .map(metrics_bucket_to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        let generated_at = until
            .format(&Rfc3339)
            .map_err(|error| JobsQueryError::Validation {
                field: "generatedAt",
                details: error.to_string(),
            })?;
        let summary = page
            .summary
            .iter()
            .map(metrics_summary_group_to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        wire::decode_wire(
            json!({
                "buckets": buckets,
                "generatedAt": generated_at,
                "groupBy": wire_token(&request.group_by),
                "step": request.step,
                "summary": summary,
                "window": request.window,
            }),
            "jobs metrics response",
        )
    }

    /// Fetch one projected job and its timeline by globally addressable admin job id.
    pub async fn inspect(
        &self,
        request: &JobsInspectRequest,
    ) -> Result<JobsInspectResponse, JobsQueryError> {
        let started = Instant::now();
        tracing::debug!(job_id = %request.id.0, "jobs rpc inspect started");
        let request_id = request.id.0.clone();
        let job = self
            .with_projection(move |store| {
                store
                    .get_job_by_global_id(&request_id)?
                    .ok_or(JobsQueryError::JobNotFound { key: request_id })
            })
            .await?;

        let metadata = self.job_metadata(&job).await?;
        let errors = self.error_details(&job).await?;
        let job_id = job.id.clone();
        let related_job = job.clone();
        let (lineage, related, timeline, waiting_on) = self
            .with_projection(move |store| {
                Ok((
                    store.get_job_lineage_by_global_id(&job_id)?,
                    store.list_related_jobs(&related_job, 10)?,
                    store.list_timeline_events(&job_id, 200)?,
                    store
                        .list_current_waits(&job_id)?
                        .into_iter()
                        .map(|wait| wait.wait_edge)
                        .collect::<Vec<_>>(),
                ))
            })
            .await?;
        let mut response_job = job.clone();
        response_job.waiting_on = (!waiting_on.is_empty()).then_some(waiting_on);
        tracing::debug!(
            service = %job.service,
            job_type = %job.job_type,
            job_id = %job.id,
            timeline = timeline.len(),
            related = related.len(),
            errors = errors.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc inspect completed"
        );
        let related = related
            .iter()
            .map(related_entry_to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        let timeline = timeline
            .iter()
            .map(timeline_event_to_wire)
            .collect::<Result<Vec<_>, _>>()?;
        wire::decode_wire(
            json!({
                "attempts": [],
                "errors": errors,
                "job": job_to_inspect_item(&response_job, &metadata)?,
                "lineage": lineage.lineage,
                "related": related,
                "timeline": timeline,
                "trigger": lineage.trigger,
            }),
            "jobs inspect response",
        )
    }

    /// Fetch horizon-consistent keyed-concurrency state by service, job type, and display key.
    pub async fn get_key(
        &self,
        request: &JobsGetKeyRequest,
    ) -> Result<JobsGetKeyResponse, JobsQueryError> {
        let started = Instant::now();
        tracing::debug!(
            service = %request.service.0,
            job_type = %request.r#type.0,
            key = %request.key.0,
            "jobs rpc get_key started"
        );
        let service = request.service.0.as_str();
        let job_type = request.r#type.0.as_str();
        let request_key = request.key.0.as_str();
        let snapshot = self
            .resource_resolver
            .key_snapshot(service, job_type, request_key)
            .await
            .map_err(|details| JobsQueryError::ProjectionStore { details })?
            .ok_or_else(|| JobsQueryError::JobNotFound {
                key: format!("{service}/{job_type}/{request_key}"),
            })?;
        let (key, projected) = self
            .await_key_projection(service, job_type, request_key, &snapshot)
            .await?;
        let now = OffsetDateTime::now_utc();
        tracing::debug!(
            service,
            job_type = %key.job_type,
            key = %key.key,
            active = key.active.len(),
            queued = key.queued.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc get_key completed"
        );

        let active = key
            .active
            .iter()
            .map(|active| {
                json!({
                    "heartbeatAgeMs": heartbeat_age_ms(&active.heartbeat_at, now),
                    "heartbeatAt": active.heartbeat_at,
                    "instanceId": active.instance_id,
                    "jobId": active.job_id,
                    "leaseExpiresAt": active.lease_expires_at,
                    "startedAt": active.started_at,
                })
            })
            .collect::<Vec<_>>();
        let queued = key
            .queued
            .iter()
            .map(|queued| json!({ "createdAt": queued.created_at, "jobId": queued.job_id }))
            .collect::<Vec<_>>();
        wire::decode_wire(
            json!({
                "active": active,
                "key": key.key,
                "keyHash": key.key_hash,
                "latestPolicyReason": projected.and_then(|key| key.latest_policy_reason),
                "queuedDepth": key.queued.len(),
                "queued": queued,
                "service": service,
                "staleTakeoverCount": key.stale_takeover_count,
                "type": job_type,
            }),
            "jobs get key response",
        )
    }

    async fn await_key_projection(
        &self,
        service: &str,
        job_type: &str,
        key: &str,
        snapshot: &crate::JobKeySnapshot,
    ) -> Result<
        (
            trellis_rs::jobs::keys::JobKeyState,
            Option<crate::storage::ProjectedJobKey>,
        ),
        JobsQueryError,
    > {
        let started = Instant::now();
        let mut horizon = snapshot.jobs_horizon;
        loop {
            let query_service = service.to_owned();
            let query_job_type = job_type.to_owned();
            let query_key = key.to_owned();
            let (projected_sequence, projected) = self
                .with_projection(move |store| {
                    Ok((
                        store.projected_sequence()?,
                        store.get_projected_key(&query_service, &query_job_type, &query_key)?,
                    ))
                })
                .await?;
            let projected_ids = projected
                .as_ref()
                .map(|key| {
                    key.active
                        .iter()
                        .map(|job| (&job.job_id, true))
                        .chain(key.queued.iter().map(|job| (&job.job_id, false)))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            if projected_sequence >= horizon {
                let snapshot = self
                    .resource_resolver
                    .key_snapshot(service, job_type, key)
                    .await
                    .map_err(|details| JobsQueryError::ProjectionStore { details })?
                    .ok_or_else(|| JobsQueryError::JobNotFound {
                        key: format!("{service}/{job_type}/{key}"),
                    })?;
                let coordinator_ids = snapshot
                    .state
                    .active
                    .iter()
                    .map(|job| (&job.job_id, true))
                    .chain(snapshot.state.queued.iter().map(|job| (&job.job_id, false)))
                    .collect::<BTreeMap<_, _>>();
                if projected_ids == coordinator_ids {
                    return Ok((snapshot.state, projected));
                }
                horizon = snapshot.jobs_horizon;
            }
            if started.elapsed() >= Duration::from_secs(5) {
                return Err(JobsQueryError::ProjectionStore {
                    details: format!(
                        "Jobs projection did not reach coordinator horizon {}",
                        horizon
                    ),
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Cancel a projected job by publishing a `cancelled` event.
    pub async fn cancel_job(
        &self,
        request: &JobsCancelRequest,
    ) -> Result<JobsCancelResponse, JobsQueryError> {
        let started = Instant::now();
        tracing::debug!(job_id = %request.id.0, "jobs rpc cancel started");
        let request_id = request.id.0.clone();
        let existing = self
            .with_projection(move |store| {
                store
                    .get_job_by_global_id(&request_id)?
                    .ok_or(JobsQueryError::JobNotFound { key: request_id })
            })
            .await?;
        let job = if is_terminal(existing.state) {
            existing
        } else {
            self.transition_job(&request.id.0, "pending|retry|active", |job, now| {
                if matches!(
                    job.state,
                    JobState::Pending | JobState::Retry | JobState::Active
                ) {
                    Some(cancelled_by_admin(
                        EventMeta {
                            service: &job.service,
                            job_type: &job.job_type,
                            job_id: &job.id,
                            context: &job.context,
                            timestamp: now,
                        },
                        job.tries,
                        job.state,
                        request.reason.as_ref().map(|value| value.0.as_str()),
                    ))
                } else {
                    None
                }
            })
            .await?
        };
        tracing::debug!(
            service = %job.service,
            job_type = %job.job_type,
            job_id = %job.id,
            state = ?job.state,
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc cancel completed"
        );

        Ok(JobsCancelResponse {
            job: job_to_cancel_item(&job, &self.job_metadata(&job).await?)?,
        })
    }

    /// Retry a failed job by publishing a `retried` event.
    pub async fn retry_job(
        &self,
        request: &JobsRetryRequest,
    ) -> Result<JobsRetryResponse, JobsQueryError> {
        let started = Instant::now();
        tracing::debug!(job_id = %request.id.0, "jobs rpc retry started");
        let job = self
            .transition_job(&request.id.0, "failed", |job, now| match job.state {
                JobState::Failed => Some(retried_by_admin(
                    EventMeta {
                        service: &job.service,
                        job_type: &job.job_type,
                        job_id: &job.id,
                        context: &job.context,
                        timestamp: now,
                    },
                    job.state,
                    Some(job.payload.clone()),
                    Some(job.max_tries),
                    job.deadline.as_deref(),
                    request.reason.as_ref().map(|value| value.0.as_str()),
                )),
                _ => None,
            })
            .await?;
        tracing::debug!(
            service = %job.service,
            job_type = %job.job_type,
            job_id = %job.id,
            state = ?job.state,
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc retry completed"
        );

        Ok(JobsRetryResponse {
            job: job_to_retry_item(&job, &self.job_metadata(&job).await?)?,
        })
    }

    /// List only jobs currently in the DLQ (`dead`) state.
    pub async fn list_dlq(
        &self,
        request: &JobsListDLQRequest,
    ) -> Result<JobsListDLQResponse, JobsQueryError> {
        let started = Instant::now();
        let query_digest = pagination_digest("jobs.ListDLQ", request)?;
        let (after, limit) = parse_page_request_for_query(
            request
                .page
                .as_ref()
                .and_then(|page| page.cursor.as_deref()),
            request.page.as_ref().and_then(|page| page.limit),
            &query_digest,
        )?;
        let since = parse_since_filter(request.since.as_deref())?;
        tracing::debug!(
            service = ?request.service,
            job_type = ?request.r#type,
            since = ?request.since,
            after = ?after,
            limit,
            "jobs rpc list_dlq started"
        );
        let service = request.service.as_ref().map(|value| value.0.clone());
        let job_type = request.r#type.as_ref().map(|value| value.0.clone());
        let page = self
            .with_projection(move |store| {
                Ok(store.list_jobs(&ListJobsFilter {
                    service,
                    job_type,
                    states: Some(vec![JobState::Dead]),
                    since,
                    after,
                    limit,
                })?)
            })
            .await?;
        let mut entries = Vec::new();
        for job in &page.jobs {
            let metadata = self.job_metadata(job).await?;
            entries.push(job_to_dlq_item(job, &metadata)?);
        }
        tracing::debug!(
            count = page.count,
            entries = entries.len(),
            limit = page.limit,
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc list_dlq completed"
        );
        wire::decode_wire(
            json!({
                "items": entries,
                "page": { "nextCursor": page.next_after
                    .map(|after| trellis_protocol::encode_pagination_cursor(&query_digest, &after))
                    .transpose()
                    .map_err(|_| JobsQueryError::Validation { field: "page.cursor", details: "invalid pagination".to_owned() })? },
            }),
            "jobs list DLQ response",
        )
    }

    /// Replay a dead-lettered job by publishing a `retried` event.
    pub async fn replay_dlq(
        &self,
        request: &JobsReplayDLQRequest,
    ) -> Result<JobsReplayDLQResponse, JobsQueryError> {
        let started = Instant::now();
        tracing::debug!(job_id = %request.id.0, "jobs rpc replay_dlq started");
        let job = self
            .transition_job(&request.id.0, "dead", |job, now| match job.state {
                JobState::Dead => {
                    let mut event = retried_by_admin(
                        EventMeta {
                            service: &job.service,
                            job_type: &job.job_type,
                            job_id: &job.id,
                            context: &job.context,
                            timestamp: now,
                        },
                        job.state,
                        Some(job.payload.clone()),
                        Some(job.max_tries),
                        job.deadline.as_deref(),
                        request.reason.as_ref().map(|value| value.0.as_str()),
                    );
                    event.trigger = Some(JobTrigger {
                        kind: JobTriggerKind::ManualReplay,
                        id: None,
                        subject: None,
                        operation_id: job
                            .lineage
                            .as_ref()
                            .and_then(|lineage| lineage.operation_id.clone()),
                        parent_job_id: job
                            .lineage
                            .as_ref()
                            .and_then(|lineage| lineage.parent_job_id.clone()),
                        trace_id: Some(job.context.trace_id.clone()),
                        request_id: Some(job.context.request_id.clone()),
                    });
                    event.lineage = job.lineage.clone();
                    Some(event)
                }
                _ => None,
            })
            .await?;
        tracing::debug!(
            service = %job.service,
            job_type = %job.job_type,
            job_id = %job.id,
            state = ?job.state,
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc replay_dlq completed"
        );

        Ok(JobsReplayDLQResponse {
            job: job_to_replay_item(&job, &self.job_metadata(&job).await?)?,
        })
    }

    /// Dismiss a dead-lettered job by publishing a `dismissed` event.
    pub async fn dismiss_dlq(
        &self,
        request: &JobsDismissDLQRequest,
    ) -> Result<JobsDismissDLQResponse, JobsQueryError> {
        let started = Instant::now();
        tracing::debug!(job_id = %request.id.0, "jobs rpc dismiss_dlq started");
        let job = self
            .transition_job(&request.id.0, "dead", |job, now| match job.state {
                JobState::Dead => {
                    let mut event = dismissed(
                        EventMeta {
                            service: &job.service,
                            job_type: &job.job_type,
                            job_id: &job.id,
                            context: &job.context,
                            timestamp: now,
                        },
                        job.tries,
                        JobState::Dead,
                        request
                            .reason
                            .as_ref()
                            .map(|value| value.0.as_str())
                            .or(job.last_error.as_deref()),
                    );
                    event.admin_action = request.reason.as_ref().map(|reason| JobAdminAction {
                        reason: Some(reason.0.clone()),
                    });
                    Some(event)
                }
                _ => None,
            })
            .await?;
        tracing::debug!(
            service = %job.service,
            job_type = %job.job_type,
            job_id = %job.id,
            state = ?job.state,
            elapsed_ms = started.elapsed().as_millis(),
            "jobs rpc dismiss_dlq completed"
        );

        Ok(JobsDismissDLQResponse {
            job: job_to_dismiss_item(&job, &self.job_metadata(&job).await?)?,
        })
    }

    async fn job_metadata(&self, job: &Job) -> Result<JobProjectionMetadata, JobsQueryError> {
        let service = job.service.clone();
        let job_type = job.job_type.clone();
        let id = job.id.clone();
        self.with_projection(move |store| {
            Ok(store
                .get_job_metadata(&service, &job_type, &id)?
                .unwrap_or_default())
        })
        .await
    }

    async fn error_details(
        &self,
        job: &Job,
    ) -> Result<Vec<JobsInspectResponseErrorsItem>, JobsQueryError> {
        let fallback_detail = job
            .last_error
            .as_deref()
            .map(|message| JobErrorDetail::from_message(&job.service, &job.job_type, message));
        let Some(detail) = job.error_detail.as_ref().or(fallback_detail.as_ref()) else {
            return Ok(Vec::new());
        };
        let fingerprint = detail.fingerprint.clone();
        let projection = self
            .with_projection(move |store| Ok(store.get_error_projection(&fingerprint)?))
            .await?;
        let mut detail = detail.clone();
        if let Some(projection) = projection {
            detail.first_seen = Some(projection.first_seen);
            detail.occurrence_count = Some(projection.occurrence_count);
        }
        Ok(vec![wire::decode_wire(
            serde_json::to_value(detail).map_err(|error| JobsQueryError::ConvertWireModel {
                model: "job error detail",
                details: error.to_string(),
            })?,
            "job error detail",
        )?])
    }

    async fn transition_job<F>(
        &self,
        id: &str,
        expected_states: &str,
        build_event: F,
    ) -> Result<Job, JobsQueryError>
    where
        F: FnOnce(&Job, &str) -> Option<JobEvent>,
    {
        let id_string = id.to_string();
        let job = self
            .with_projection(move |store| {
                store
                    .get_job_by_global_id(&id_string)?
                    .ok_or(JobsQueryError::JobNotFound { key: id_string })
            })
            .await?;
        let key = projection_key(&job);

        let now = now_timestamp_string();
        let event = build_event(&job, &now).ok_or_else(|| JobsQueryError::JobStateConflict {
            key: key.clone(),
            expected: expected_states.to_string(),
            actual: format!("{:?}", job.state).to_lowercase(),
        })?;
        let subject = self.transition_event_subject(&job, &event).await;
        tracing::debug!(
            service = %job.service,
            job_type = %job.job_type,
            job_id = %job.id,
            from_state = ?job.state,
            event_type = event.event_type.as_token(),
            subject = %subject,
            "publishing jobs admin transition event"
        );
        let payload = serde_json::to_vec(&event).map_err(|error| JobsQueryError::EncodeEvent {
            key: key.clone(),
            details: error.to_string(),
        })?;

        self.jobs_runtime
            .publish_event_payload(subject.clone(), job_event_headers(&event), payload)
            .await
            .map_err(|error| JobsQueryError::PublishEvent {
                subject,
                details: error,
            })?;

        let predicted = reduce_job_event(Some(&job), &event).ok_or_else(|| {
            JobsQueryError::JobStateConflict {
                key: key.clone(),
                expected: expected_states.to_string(),
                actual: format!("{:?}", job.state).to_lowercase(),
            }
        })?;
        let projected = self.await_job_projection(&predicted, &event).await?;
        tracing::debug!(
            service = %projected.service,
            job_type = %projected.job_type,
            job_id = %projected.id,
            state = ?projected.state,
            "jobs admin transition projected"
        );

        Ok(projected)
    }

    async fn transition_event_subject(&self, job: &Job, event: &JobEvent) -> String {
        let fallback = job_event_subject(&job.service, &job.job_type, &job.id, event.event_type);
        let job_id = job.id.clone();
        let event_type = event.event_type.as_token().to_string();
        self.with_projection(move |store| {
            Ok(store
                .list_timeline_events(&job_id, 1)
                .ok()
                .and_then(|events| {
                    events.first().and_then(|timeline_event| {
                        raw_event_subject(&timeline_event.raw_event_json)
                    })
                })
                .and_then(|subject| sibling_event_subject(&subject, &event_type)))
        })
        .await
        .ok()
        .flatten()
        .unwrap_or(fallback)
    }

    async fn await_job_projection(
        &self,
        predicted: &Job,
        event: &JobEvent,
    ) -> Result<Job, JobsQueryError> {
        let started = Instant::now();
        for attempt in 0..20 {
            let service = predicted.service.clone();
            let job_type = predicted.job_type.clone();
            let id = predicted.id.clone();
            if let Some(job) = self
                .with_projection(move |store| Ok(store.get_job(&service, &job_type, &id)?))
                .await?
            {
                if plan_mutation_response(Some(&job), predicted, event, false)
                    == MutationResponsePlan::ReturnProjected
                {
                    tracing::debug!(
                        service = %job.service,
                        job_type = %job.job_type,
                        job_id = %job.id,
                        attempts = attempt + 1,
                        elapsed_ms = started.elapsed().as_millis(),
                        "observed projected jobs admin transition"
                    );
                    return Ok(job);
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        tracing::debug!(
            service = %predicted.service,
            job_type = %predicted.job_type,
            job_id = %predicted.id,
            event_type = event.event_type.as_token(),
            elapsed_ms = started.elapsed().as_millis(),
            "returning predicted jobs admin transition before projection caught up"
        );
        Ok(predicted.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationResponsePlan {
    ReturnProjected,
    ReturnPredicted,
    Wait,
}

fn plan_mutation_response(
    projected: Option<&Job>,
    predicted: &Job,
    event: &JobEvent,
    projection_lagged: bool,
) -> MutationResponsePlan {
    if let Some(projected) = projected {
        if projected.state == predicted.state && projected.updated_at == predicted.updated_at {
            return MutationResponsePlan::ReturnProjected;
        }
        if is_terminal_noop_projection(projected, event) {
            return MutationResponsePlan::ReturnProjected;
        }
    }

    if projection_lagged {
        MutationResponsePlan::ReturnPredicted
    } else {
        MutationResponsePlan::Wait
    }
}

fn is_terminal_noop_projection(projected: &Job, event: &JobEvent) -> bool {
    is_terminal(projected.state)
        && reduce_job_event(Some(projected), event).is_some_and(|next| next == projected.clone())
}

fn job_event_headers(event: &JobEvent) -> trellis_rs::jobs::JobEventHeaders {
    trellis_rs::jobs::JobEventHeaders::from(&event.context)
}

impl From<SqliteJobsStoreError> for JobsQueryError {
    fn from(error: SqliteJobsStoreError) -> Self {
        match error {
            SqliteJobsStoreError::Validation { field, details } => {
                Self::Validation { field, details }
            }
            other => Self::ProjectionStore {
                details: other.to_string(),
            },
        }
    }
}

type JobQueryPage = (Option<(i64, String, String, String)>, u64);

fn parse_page_request_for_query(
    cursor: Option<&str>,
    limit: Option<u32>,
    query_digest: &str,
) -> Result<JobQueryPage, JobsQueryError> {
    let after = cursor
        .map(|cursor| trellis_protocol::decode_pagination_cursor(cursor, query_digest))
        .transpose()
        .map_err(|_| JobsQueryError::Validation {
            field: "page.cursor",
            details: "invalid pagination".to_owned(),
        })?;
    let limit = u64::from(limit.unwrap_or(100));
    if limit == 0 || limit > 500 {
        return Err(JobsQueryError::Validation {
            field: "page.limit",
            details: "must be positive".to_owned(),
        });
    }
    Ok((after, limit))
}

fn pagination_digest<T: serde::Serialize>(
    endpoint: &str,
    request: &T,
) -> Result<String, JobsQueryError> {
    let mut query = serde_json::to_value(request).map_err(|error| JobsQueryError::Validation {
        field: "page.cursor",
        details: error.to_string(),
    })?;
    query.as_object_mut().map(|query| query.remove("page"));
    trellis_protocol::pagination_query_digest(endpoint, &query).map_err(|_| {
        JobsQueryError::Validation {
            field: "page.cursor",
            details: "invalid pagination".to_owned(),
        }
    })
}

fn parse_since_filter(value: Option<&str>) -> Result<Option<OffsetDateTime>, JobsQueryError> {
    value
        .map(|since| {
            OffsetDateTime::parse(since, &Rfc3339).map_err(|error| JobsQueryError::Validation {
                field: "since",
                details: error.to_string(),
            })
        })
        .transpose()
}

fn parse_window_filter(value: Option<&str>) -> Result<Option<OffsetDateTime>, JobsQueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let duration = match value {
        "1h" => time::Duration::hours(1),
        "24h" => time::Duration::hours(24),
        "7d" => time::Duration::days(7),
        other => {
            return Err(JobsQueryError::Validation {
                field: "window",
                details: format!("unsupported window '{other}'"),
            })
        }
    };
    Ok(Some(OffsetDateTime::now_utc() - duration))
}

fn parse_metrics_window(value: &str) -> Result<time::Duration, JobsQueryError> {
    match value {
        "15m" => Ok(time::Duration::minutes(15)),
        "1h" => Ok(time::Duration::hours(1)),
        "6h" => Ok(time::Duration::hours(6)),
        "24h" => Ok(time::Duration::hours(24)),
        "7d" => Ok(time::Duration::days(7)),
        other => Err(JobsQueryError::Validation {
            field: "window",
            details: format!("unsupported window '{other}'"),
        }),
    }
}

fn parse_metrics_step(value: &str) -> Result<time::Duration, JobsQueryError> {
    match value {
        "1m" => Ok(time::Duration::minutes(1)),
        "5m" => Ok(time::Duration::minutes(5)),
        "15m" => Ok(time::Duration::minutes(15)),
        "1h" => Ok(time::Duration::hours(1)),
        "6h" => Ok(time::Duration::hours(6)),
        "1d" => Ok(time::Duration::days(1)),
        other => Err(JobsQueryError::Validation {
            field: "step",
            details: format!("unsupported step '{other}'"),
        }),
    }
}

fn parse_metrics_group_by(value: &str) -> Result<JobsWorkbenchGroupBy, JobsQueryError> {
    match value {
        "service" => Ok(JobsWorkbenchGroupBy::Service),
        "type" => Ok(JobsWorkbenchGroupBy::Type),
        "state" => Ok(JobsWorkbenchGroupBy::State),
        "queueKey" => Ok(JobsWorkbenchGroupBy::QueueKey),
        "trigger" => Ok(JobsWorkbenchGroupBy::Trigger),
        other => Err(JobsQueryError::Validation {
            field: "groupBy",
            details: format!("unsupported group '{other}'"),
        }),
    }
}

fn parse_workbench_sort(
    sort: Option<&trellis_runtime_apis::types::JobsQueryRequestsort>,
) -> Result<JobsWorkbenchSort, JobsQueryError> {
    let Some(sort) = sort else {
        return Ok(JobsWorkbenchSort::default());
    };
    let field = match wire_token(&sort.field).as_str() {
        "updatedAt" => JobsWorkbenchSortField::UpdatedAt,
        "queueAge" => JobsWorkbenchSortField::QueueAge,
        "runtime" => JobsWorkbenchSortField::Runtime,
        "retries" => JobsWorkbenchSortField::Retries,
        "depth" => JobsWorkbenchSortField::Depth,
        "failureRate" => JobsWorkbenchSortField::FailureRate,
        other => {
            return Err(JobsQueryError::Validation {
                field: "sort.field",
                details: format!("unsupported sort field '{other}'"),
            })
        }
    };
    let descending = match sort
        .direction
        .as_ref()
        .map(wire_token)
        .unwrap_or_else(|| "desc".to_string())
        .as_str()
    {
        "asc" => false,
        "desc" => true,
        other => {
            return Err(JobsQueryError::Validation {
                field: "sort.direction",
                details: format!("unsupported sort direction '{other}'"),
            })
        }
    };
    Ok(JobsWorkbenchSort { field, descending })
}

fn parse_group_by(value: Option<&str>) -> Result<Option<JobsWorkbenchGroupBy>, JobsQueryError> {
    value
        .map(|value| match value {
            "service" => Ok(JobsWorkbenchGroupBy::Service),
            "type" => Ok(JobsWorkbenchGroupBy::Type),
            "state" => Ok(JobsWorkbenchGroupBy::State),
            "queueKey" => Ok(JobsWorkbenchGroupBy::QueueKey),
            "runtimeBand" => Ok(JobsWorkbenchGroupBy::RuntimeBand),
            "trigger" => Ok(JobsWorkbenchGroupBy::Trigger),
            other => Err(JobsQueryError::Validation {
                field: "groupBy",
                details: format!("unsupported group '{other}'"),
            }),
        })
        .transpose()
}

fn wire_token(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn workbench_entry_to_wire(
    entry: &JobsWorkbenchEntry,
) -> Result<JobsQueryResponseEntriesItem, JobsQueryError> {
    let job = &entry.job;
    let queue_age_ms = entry
        .queue_age_anchor_nanos
        .map(|anchor| {
            (OffsetDateTime::now_utc().unix_timestamp_nanos() - i128::from(anchor)) / 1_000_000
        })
        .and_then(|millis| u64::try_from(millis.max(0)).ok());
    wire::decode_wire(
        json!({
            "completedAt": job.completed_at,
            "context": job.context,
            "createdAt": job.created_at,
            "errorFingerprint": entry.last_error_fingerprint,
            "id": job.id,
            "lastError": job.last_error,
            "lineage": job.lineage,
            "maxTries": job.max_tries,
            "progress": job.progress,
            "queueAgeMs": queue_age_ms,
            "queueKey": entry.queue_key,
            "runtimeBand": entry.runtime_band,
            "runtimeMs": entry.runtime_ms,
            "service": job.service,
            "startedAt": job.started_at,
            "state": job.state,
            "tries": job.tries,
            "trigger": job.trigger,
            "type": job.job_type,
            "updatedAt": job.updated_at,
            "waitingOn": (!entry.waiting_on.is_empty()).then_some(&entry.waiting_on),
        }),
        "job query entry",
    )
}

fn related_entry_to_wire(
    entry: &JobsWorkbenchEntry,
) -> Result<JobsInspectResponseRelatedItem, JobsQueryError> {
    let job = &entry.job;
    let queue_age_ms = entry
        .queue_age_anchor_nanos
        .map(|anchor| {
            (OffsetDateTime::now_utc().unix_timestamp_nanos() - i128::from(anchor)) / 1_000_000
        })
        .and_then(|millis| u64::try_from(millis.max(0)).ok());
    wire::decode_wire(
        json!({
            "completedAt": job.completed_at,
            "context": job.context,
            "createdAt": job.created_at,
            "errorFingerprint": entry.last_error_fingerprint,
            "id": job.id,
            "lastError": job.last_error,
            "lineage": job.lineage,
            "matchedBy": entry.matched_by,
            "maxTries": job.max_tries,
            "progress": job.progress,
            "queueAgeMs": queue_age_ms,
            "queueKey": entry.queue_key,
            "runtimeBand": entry.runtime_band,
            "runtimeMs": entry.runtime_ms,
            "service": job.service,
            "startedAt": job.started_at,
            "state": job.state,
            "tries": job.tries,
            "trigger": job.trigger,
            "type": job.job_type,
            "updatedAt": job.updated_at,
            "waitingOn": (!entry.waiting_on.is_empty()).then_some(&entry.waiting_on),
        }),
        "job inspect related entry",
    )
}

fn timeline_event_to_wire(
    event: &JobTimelineEvent,
) -> Result<JobsInspectResponseTimelineItem, JobsQueryError> {
    let raw_event: serde_json::Value =
        serde_json::from_str(&event.raw_event_json).map_err(|error| {
            JobsQueryError::ConvertWireModel {
                model: "job timeline raw event",
                details: error.to_string(),
            }
        })?;
    let raw_event_bytes = wire::encode_document(&raw_event)?;
    wire::decode_wire(
        json!({
            "error": event.error_message,
            "errorDetail": timeline_error_detail(event)?,
            "logs": decode_optional_json::<serde_json::Value>(&event.logs_json, "job timeline logs")?,
            "message": event.message,
            "previousState": event.previous_state,
            "progress": decode_optional_json::<serde_json::Value>(&event.progress_json, "job timeline progress")?,
            "projected": event.projected,
            "rawEvent": raw_event_bytes,
            "reason": event.reason,
            "sequence": event.sequence,
            "state": event.state,
            "timestamp": event.timestamp,
            "tries": event.tries,
            "type": event.event_type,
            "waitEdge": raw_event.get("waitEdge"),
            "workerInstanceId": event.worker_instance_id,
        }),
        "job timeline event",
    )
}

fn timeline_error_detail(
    event: &JobTimelineEvent,
) -> Result<Option<JobsInspectResponseTimelineItemErrorDetail>, JobsQueryError> {
    let raw_event: serde_json::Value =
        serde_json::from_str(&event.raw_event_json).map_err(|error| {
            JobsQueryError::ConvertWireModel {
                model: "job timeline raw event",
                details: error.to_string(),
            }
        })?;
    if let Some(detail) = raw_event.get("errorDetail") {
        return wire::decode_wire(detail.clone(), "job timeline error detail").map(Some);
    }
    let Some(message) = event.error_message.as_deref() else {
        return Ok(None);
    };
    let service = raw_event
        .get("service")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let job_type = raw_event
        .get("jobType")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    wire::decode_wire(
        serde_json::to_value(JobErrorDetail::from_message(service, job_type, message)).map_err(
            |error| JobsQueryError::ConvertWireModel {
                model: "job timeline error detail",
                details: error.to_string(),
            },
        )?,
        "job timeline error detail",
    )
    .map(Some)
}

fn workbench_group_to_wire(
    group: &JobsWorkbenchGroup,
) -> Result<JobsQueryResponseGroupsItem, JobsQueryError> {
    wire::decode_wire(
        json!({
            "count": group.count,
            "depth": group.depth,
            "failureRate": group.failure_rate,
            "key": group.key,
            "label": group.label,
            "latestUpdatedAt": group.latest_updated_at,
            "oldestCreatedAt": group.oldest_created_at,
            "state": group.state,
        }),
        "job query group",
    )
}

fn workbench_stats_to_wire(
    stats: &JobsWorkbenchStats,
) -> Result<JobsQueryResponseStats, JobsQueryError> {
    wire::decode_wire(
        json!({
            "byState": stats.by_state,
            "dead": stats.dead,
            "failed": stats.failed,
            "queued": stats.queued,
            "running": stats.running,
            "slow": stats.slow,
            "total": stats.total,
        }),
        "job query stats",
    )
}

fn decode_optional_json<T>(
    json: &Option<String>,
    model: &'static str,
) -> Result<Option<T>, JobsQueryError>
where
    T: serde::de::DeserializeOwned,
{
    json.as_ref()
        .map(|json| {
            serde_json::from_str(json).map_err(|error| JobsQueryError::ConvertWireModel {
                model,
                details: error.to_string(),
            })
        })
        .transpose()
}

fn metrics_latency_to_summary_wire(
    latency: &JobsMetricsLatency,
) -> Result<JobsMetricsResponseSummaryItemRuntime, JobsQueryError> {
    wire::decode_wire(metrics_latency_value(latency), "jobs metrics runtime")
}

fn metrics_latency_to_summary_queue_wire(
    latency: &JobsMetricsLatency,
) -> Result<JobsMetricsResponseSummaryItemQueueWait, JobsQueryError> {
    wire::decode_wire(metrics_latency_value(latency), "jobs metrics queue wait")
}

fn metrics_latency_to_bucket_wire(
    latency: &JobsMetricsLatency,
) -> Result<JobsMetricsResponseBucketsItemGroupsItemRuntime, JobsQueryError> {
    wire::decode_wire(
        metrics_latency_value(latency),
        "jobs metrics bucket runtime",
    )
}

fn metrics_latency_to_bucket_queue_wire(
    latency: &JobsMetricsLatency,
) -> Result<JobsMetricsResponseBucketsItemGroupsItemQueueWait, JobsQueryError> {
    wire::decode_wire(
        metrics_latency_value(latency),
        "jobs metrics bucket queue wait",
    )
}

fn metrics_latency_value(latency: &JobsMetricsLatency) -> serde_json::Value {
    json!({
        "count": latency.count,
        "maxMs": latency.max_ms,
        "p50Ms": latency.p50_ms,
        "p95Ms": latency.p95_ms,
    })
}

fn metrics_summary_group_to_wire(
    group: &JobsMetricsSummaryGroup,
) -> Result<JobsMetricsResponseSummaryItem, JobsQueryError> {
    wire::decode_wire(
        json!({
            "byState": group.by_state,
            "dead": group.dead,
            "failed": group.failed,
            "failureRate": group.failure_rate,
            "key": group.key,
            "label": group.label,
            "latestUpdatedAt": group.latest_updated_at,
            "oldestCreatedAt": group.oldest_created_at,
            "queueWait": metrics_latency_to_summary_queue_wire(&group.queue_wait)?,
            "queued": group.queued,
            "running": group.running,
            "runtime": metrics_latency_to_summary_wire(&group.runtime)?,
            "slow": group.slow,
            "total": group.total,
        }),
        "jobs metrics summary",
    )
}

fn metrics_bucket_to_wire(
    bucket: &JobsMetricsBucket,
) -> Result<JobsMetricsResponseBucketsItem, JobsQueryError> {
    let groups = bucket
        .groups
        .iter()
        .map(metrics_bucket_group_to_wire)
        .collect::<Result<Vec<_>, _>>()?;
    wire::decode_wire(
        json!({ "end": bucket.end, "groups": groups, "start": bucket.start }),
        "jobs metrics bucket",
    )
}

fn metrics_bucket_group_to_wire(
    group: &JobsMetricsBucketGroup,
) -> Result<JobsMetricsResponseBucketsItemGroupsItem, JobsQueryError> {
    wire::decode_wire(
        json!({
            "cancelled": group.cancelled,
            "completed": group.completed,
            "dead": group.dead,
            "dismissed": group.dismissed,
            "failed": group.failed,
            "key": group.key,
            "label": group.label,
            "queueWait": metrics_latency_to_bucket_queue_wire(&group.queue_wait)?,
            "retried": group.retried,
            "runtime": metrics_latency_to_bucket_wire(&group.runtime)?,
            "started": group.started,
            "submitted": group.submitted,
        }),
        "jobs metrics bucket group",
    )
}

fn heartbeat_age_ms(heartbeat_at: &str, now: OffsetDateTime) -> i64 {
    let Some(heartbeat_at) = OffsetDateTime::parse(heartbeat_at, &Rfc3339).ok() else {
        return 0;
    };
    let age = (now - heartbeat_at).whole_milliseconds();
    if age < 0 {
        0
    } else {
        i64::try_from(age).unwrap_or(i64::MAX)
    }
}

fn projection_key(job: &Job) -> String {
    format!("{}/{}/{}", job.service, job.job_type, job.id)
}

fn raw_event_subject(raw_event_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(raw_event_json)
        .ok()?
        .get("_trellisSubject")?
        .as_str()
        .map(str::to_string)
}

fn sibling_event_subject(subject: &str, event_type: &str) -> Option<String> {
    Some(format!("{}.{}", subject.rsplit_once('.')?.0, event_type))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_nats::jetstream::{kv, stream};
    use serde_json::json;
    use trellis_rs::jobs::events::{
        cancelled, created, created_with_policy, dismissed, retried, EventMeta,
    };
    use trellis_rs::jobs::keys::JobKeyQueuedEntry;
    use trellis_rs::jobs::types::{
        Job, JobConcurrency, JobContext, JobEvent, JobState, JobWaitEdge, JobWaitTarget,
        JobWaitTargetKind,
    };

    use super::parse_since_filter;
    use super::{
        jobs_admin_resources, plan_mutation_response, reduce_job_event, timeline_event_to_wire,
        workbench_entry_to_wire, JobsQueryError, MutationResponsePlan,
    };
    use crate::storage::{JobTimelineEvent, JobsWorkbenchEntry};
    use crate::test_nats::TestNats;
    use crate::{
        start_jobs_projector, JobResourceResolver, JobsQuery, SqliteJobResourceResolver,
        SqliteJobsStore,
    };

    #[test]
    fn jobs_admin_resources_use_builtin_stream_names() {
        let resources = jobs_admin_resources();

        assert_eq!(resources.jobs_stream, "JOBS");
        assert_eq!(resources.jobs_advisories_stream, "JOBS_ADVISORIES");
    }

    fn sample_job(state: JobState) -> Job {
        Job {
            id: "job-1".to_string(),
            context: JobContext {
                request_id: "request-job-1".to_string(),
                trace_id: "0123456789abcdef0123456789abcdef".to_string(),
                traceparent: "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01".to_string(),
                tracestate: None,
            },
            service: "documents".to_string(),
            job_type: "import".to_string(),
            state,
            payload: json!({ "documentId": "doc-1" }),
            result: None,
            created_at: "2026-03-28T12:00:00Z".to_string(),
            updated_at: "2026-03-28T12:00:00Z".to_string(),
            started_at: None,
            completed_at: None,
            tries: 1,
            max_tries: 5,
            last_error: None,
            error_detail: None,
            deadline: Some("2026-03-29T12:00:00Z".to_string()),
            progress: None,
            logs: None,
            concurrency: None,
            queue_policy: None,
            trigger: None,
            lineage: None,
            waiting_on: None,
        }
    }

    fn sample_wait_edge() -> JobWaitEdge {
        JobWaitEdge {
            id: "wait-1".to_string(),
            target: JobWaitTarget {
                kind: JobWaitTargetKind::Job,
                id: Some("child-job".to_string()),
                operation_id: None,
                label: None,
                service: Some("documents".to_string()),
                target_type: Some("import".to_string()),
                system: None,
                operation: None,
                key: None,
            },
            started_at: "2026-03-28T12:01:00Z".to_string(),
            label: Some("child job".to_string()),
        }
    }

    fn predicted_job(job: &Job, event: &JobEvent) -> Job {
        reduce_job_event(Some(job), event).expect("admin event should reduce")
    }

    #[test]
    fn cancel_response_plan_returns_predicted_job_when_projection_lags() {
        let job = sample_job(JobState::Pending);
        let event = cancelled(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-03-28T12:01:00Z",
            },
            job.tries,
            job.state,
        );
        let predicted = predicted_job(&job, &event);

        assert_eq!(predicted.state, JobState::Cancelled);
        assert_eq!(
            plan_mutation_response(None, &predicted, &event, true),
            MutationResponsePlan::ReturnPredicted
        );
    }

    #[test]
    fn retry_response_plan_returns_predicted_job_when_projection_lags() {
        let job = sample_job(JobState::Failed);
        let event = retried(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-03-28T12:01:00Z",
            },
            job.state,
            Some(job.payload.clone()),
            Some(job.max_tries),
            job.deadline.as_deref(),
        );
        let predicted = predicted_job(&job, &event);

        assert_eq!(predicted.state, JobState::Pending);
        assert_eq!(
            plan_mutation_response(None, &predicted, &event, true),
            MutationResponsePlan::ReturnPredicted
        );
    }

    #[test]
    fn replay_response_plan_returns_predicted_job_when_projection_lags() {
        let job = sample_job(JobState::Dead);
        let event = retried(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-03-28T12:01:00Z",
            },
            job.state,
            Some(job.payload.clone()),
            Some(job.max_tries),
            job.deadline.as_deref(),
        );
        let predicted = predicted_job(&job, &event);

        assert_eq!(predicted.state, JobState::Pending);
        assert_eq!(
            plan_mutation_response(None, &predicted, &event, true),
            MutationResponsePlan::ReturnPredicted
        );
    }

    #[test]
    fn dismiss_response_plan_returns_predicted_job_when_projection_lags() {
        let job = sample_job(JobState::Dead);
        let event = dismissed(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-03-28T12:01:00Z",
            },
            job.tries,
            JobState::Dead,
            job.last_error.as_deref(),
        );
        let predicted = predicted_job(&job, &event);

        assert_eq!(predicted.state, JobState::Dismissed);
        assert_eq!(
            plan_mutation_response(None, &predicted, &event, true),
            MutationResponsePlan::ReturnPredicted
        );
    }

    #[test]
    fn terminal_projection_race_returns_projected_terminal_job() {
        let job = sample_job(JobState::Pending);
        let event = cancelled(
            EventMeta {
                service: &job.service,
                job_type: &job.job_type,
                job_id: &job.id,
                context: &job.context,
                timestamp: "2026-03-28T12:01:00Z",
            },
            job.tries,
            job.state,
        );
        let predicted = predicted_job(&job, &event);
        let mut projected = job.clone();
        projected.state = JobState::Completed;
        projected.updated_at = "2026-03-28T12:00:30Z".to_string();
        projected.completed_at = Some(projected.updated_at.clone());

        assert_eq!(
            plan_mutation_response(Some(&projected), &predicted, &event, false),
            MutationResponsePlan::ReturnProjected
        );
    }

    #[test]
    fn query_wire_maps_waiting_on_and_timeline_wait_edge() {
        let entry = JobsWorkbenchEntry {
            job: sample_job(JobState::Active),
            runtime_ms: None,
            queue_age_anchor_nanos: None,
            queue_key: None,
            runtime_band: None,
            last_error_fingerprint: None,
            matched_by: None,
            waiting_on: vec![sample_wait_edge()],
        };

        let row = workbench_entry_to_wire(&entry).expect("query row should map");
        assert_eq!(
            row.waiting_on
                .as_ref()
                .and_then(|waits| waits.first())
                .map(|wait| wait.id.0.as_str()),
            Some("wait-1")
        );

        let timeline = JobTimelineEvent {
            sequence: 1,
            event_type: "waiting".to_string(),
            state: "active".to_string(),
            previous_state: Some("active".to_string()),
            timestamp: "2026-03-28T12:01:00Z".to_string(),
            tries: 1,
            message: None,
            error_message: None,
            progress_json: None,
            logs_json: None,
            worker_instance_id: None,
            raw_event_json: json!({ "waitEdge": sample_wait_edge() }).to_string(),
            projected: Some(true),
            reason: None,
        };

        let timeline_row = timeline_event_to_wire(&timeline).expect("timeline row should map");
        assert_eq!(
            timeline_row.wait_edge.map(|wait| wait.id.0),
            Some("wait-1".to_string())
        );
    }

    #[test]
    fn parse_since_filter_accepts_rfc3339_offset_timestamps() {
        let parsed = parse_since_filter(Some("2025-12-31T19:00:30-05:00"))
            .expect("offset timestamp should parse")
            .expect("since should be present");

        assert_eq!(parsed.unix_timestamp(), 1_767_225_630);
    }

    #[test]
    fn parse_since_filter_rejects_invalid_timestamps_as_validation_errors() {
        let error =
            parse_since_filter(Some("not-a-timestamp")).expect_err("invalid timestamp should fail");

        assert!(matches!(
            error,
            JobsQueryError::Validation { field: "since", .. }
        ));
    }

    #[tokio::test]
    async fn get_key_stabilizes_a_concurrent_transition_after_projection_catches_up() {
        let nats = TestNats::start().await;
        let jetstream = async_nats::jetstream::new(nats.client.clone());
        jetstream
            .create_stream(stream::Config {
                name: "JOBS".to_owned(),
                subjects: vec!["trellis.jobs.>".to_owned()],
                ..Default::default()
            })
            .await
            .expect("Jobs stream should be created");
        let keys = jetstream
            .create_key_value(kv::Config {
                bucket: "JOBS_KEYS_documents".to_owned(),
                ..Default::default()
            })
            .await
            .expect("key bucket should be created");
        let key_state = trellis_rs::jobs::keys::JobKeyState {
            version: 1,
            service: "documents".to_owned(),
            job_type: "import".to_owned(),
            key: "customer-42".to_owned(),
            key_hash: "hash-42".to_owned(),
            max_active: 1,
            max_queued_per_key: Some(1),
            active: Vec::new(),
            queued: Vec::new(),
            stale_takeover_count: 3,
            updated_at: "2026-03-28T12:00:00Z".to_owned(),
        };
        keys.put(
            "documents.import.hash-42",
            serde_json::to_vec(&key_state)
                .expect("key state should encode")
                .into(),
        )
        .await
        .expect("key state should persist");
        let unrelated = sample_job(JobState::Pending);
        let horizon_event = created(
            EventMeta {
                service: "other-service",
                job_type: "other-job",
                job_id: "horizon-job",
                context: &unrelated.context,
                timestamp: "2026-03-28T12:00:00Z",
            },
            json!({}),
            1,
            None,
        );
        jetstream
            .publish(
                "trellis.jobs.other-service.other-job.horizon-job.created",
                serde_json::to_vec(&horizon_event)
                    .expect("horizon event should encode")
                    .into(),
            )
            .await
            .expect("horizon should publish")
            .await
            .expect("horizon should persist");

        let catalog = tempfile::NamedTempFile::new().expect("catalog path should exist");
        let connection = rusqlite::Connection::open(catalog.path()).expect("catalog should open");
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
        connection
            .execute(
                "INSERT INTO auth_resource_binding_evidence VALUES (?1,?2,?3,'available','jobQueue')",
                rusqlite::params![
                    "documents",
                    "import",
                    json!({
                        "namespace": "documents",
                        "work_stream": "JOBS_WORK",
                        "consumer": "documents-import"
                    })
                    .to_string()
                ],
            )
            .expect("catalog binding should insert");
        drop(connection);
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let resolver: Arc<dyn JobResourceResolver> = Arc::new(SqliteJobResourceResolver::new(
            catalog.path().to_owned(),
            nats.client.clone(),
            "JOBS".to_owned(),
        ));
        let query = JobsQuery::with_store(
            trellis_rs::jobs::JobsRuntime::from_nats(nats.client.clone()),
            store.clone(),
            resolver,
        );
        let request = serde_json::from_value(json!({
            "service": "documents",
            "type": "import",
            "key": "customer-42"
        }))
        .expect("request should decode");
        let mut response = tokio::spawn(async move { query.get_key(&request).await });

        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut response)
                .await
                .is_err()
        );
        let mut transitioned_key_state = key_state;
        transitioned_key_state.queued.push(JobKeyQueuedEntry {
            job_id: "queued-job".to_owned(),
            created_at: "2026-03-28T12:00:01Z".to_owned(),
            request_id: "request-queued-job".to_owned(),
            context: None,
        });
        transitioned_key_state.stale_takeover_count = 4;
        transitioned_key_state.updated_at = "2026-03-28T12:00:01Z".to_owned();
        keys.put(
            "documents.import.hash-42",
            serde_json::to_vec(&transitioned_key_state)
                .expect("transitioned key state should encode")
                .into(),
        )
        .await
        .expect("transitioned key state should persist");
        let transitioned_job = sample_job(JobState::Pending);
        let transition_event = created_with_policy(
            EventMeta {
                service: "documents",
                job_type: "import",
                job_id: "queued-job",
                context: &transitioned_job.context,
                timestamp: "2026-03-28T12:00:01Z",
            },
            json!({}),
            1,
            None,
            Some(JobConcurrency {
                key: "customer-42".to_owned(),
                key_hash: "hash-42".to_owned(),
                instance_id: None,
                slot_token: None,
                heartbeat_at: None,
                lease_expires_at: None,
                stale_takeover_count: Some(4),
            }),
            None,
        );
        jetstream
            .publish(
                "trellis.jobs.documents.import.queued-job.created",
                serde_json::to_vec(&transition_event)
                    .expect("transition event should encode")
                    .into(),
            )
            .await
            .expect("transition should publish")
            .await
            .expect("transition should persist");
        let projector = start_jobs_projector(
            trellis_rs::jobs::JobsRuntime::from_nats(nats.client.clone()),
            store,
            "JOBS".to_owned(),
        )
        .await
        .expect("projector should start");
        let response = tokio::time::timeout(Duration::from_secs(2), response)
            .await
            .expect("query should finish after projection catches up")
            .expect("query task should succeed")
            .expect("get key should succeed");
        let response = serde_json::to_value(response).expect("response should encode");
        assert_eq!(response["keyHash"], "hash-42");
        assert_eq!(response["queued"][0]["jobId"], "queued-job");
        assert_eq!(response["staleTakeoverCount"], "4");
        projector.stop().await;
    }
}
