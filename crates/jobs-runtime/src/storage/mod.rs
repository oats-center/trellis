use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Transaction,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_rs::jobs::types::{
    error_fingerprint, Job, JobErrorDetail, JobEvent, JobEventType, JobLineage, JobState,
    JobTrigger, JobTriggerKind, JobWaitEdge, JobWaitTargetKind,
};

use crate::worker_presence::WorkerPresenceRecord;

/// SQLite-backed Jobs projection store.
#[derive(Debug, Clone)]
pub struct SqliteJobsStore {
    connection: Arc<Mutex<Connection>>,
}

/// Filter used when listing projected jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListJobsFilter {
    pub service: Option<String>,
    pub job_type: Option<String>,
    pub states: Option<Vec<JobState>>,
    pub since: Option<OffsetDateTime>,
    pub after: Option<(i64, String, String, String)>,
    pub limit: u64,
}

impl Default for ListJobsFilter {
    fn default() -> Self {
        Self {
            service: None,
            job_type: None,
            states: None,
            since: None,
            after: None,
            limit: u64::MAX,
        }
    }
}

/// Page of projected jobs using the public Jobs list ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct JobsPage {
    pub jobs: Vec<Job>,
    pub count: u64,
    pub limit: u64,
    pub next_after: Option<(i64, String, String, String)>,
}

/// Filter used by the Jobs workbench query surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsWorkbenchFilter {
    pub service: Option<String>,
    pub job_type: Option<String>,
    pub states: Option<Vec<JobState>>,
    pub since: Option<OffsetDateTime>,
    pub search: Option<String>,
    pub queue_key: Option<String>,
    pub runtime_band: Option<String>,
    pub trigger: Option<String>,
    pub sort: JobsWorkbenchSort,
    pub group_by: Option<JobsWorkbenchGroupBy>,
    pub after: Option<(Option<i64>, i64, String, String, String)>,
    pub limit: u64,
}

/// Sort order used by the Jobs workbench query surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobsWorkbenchSort {
    pub field: JobsWorkbenchSortField,
    pub descending: bool,
}

impl Default for JobsWorkbenchSort {
    fn default() -> Self {
        Self {
            field: JobsWorkbenchSortField::UpdatedAt,
            descending: true,
        }
    }
}

/// Sortable scalar fields for Jobs workbench rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobsWorkbenchSortField {
    UpdatedAt,
    QueueAge,
    Runtime,
    Retries,
    Depth,
    FailureRate,
}

/// Supported server-side grouping keys for Jobs workbench rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobsWorkbenchGroupBy {
    Service,
    Type,
    State,
    QueueKey,
    RuntimeBand,
    Trigger,
}

/// Page of projected jobs with scalar workbench fields.
#[derive(Debug, Clone, PartialEq)]
pub struct JobsWorkbenchPage {
    pub entries: Vec<JobsWorkbenchEntry>,
    pub count: u64,
    pub limit: u64,
    pub next_after: Option<(Option<i64>, i64, String, String, String)>,
    pub stats: JobsWorkbenchStats,
}

/// One projected Jobs workbench row.
#[derive(Debug, Clone, PartialEq)]
pub struct JobsWorkbenchEntry {
    pub job: Job,
    pub runtime_ms: Option<i64>,
    pub queue_age_anchor_nanos: Option<i64>,
    pub queue_key: Option<String>,
    pub runtime_band: Option<String>,
    pub last_error_fingerprint: Option<String>,
    pub matched_by: Option<String>,
    pub waiting_on: Vec<JobWaitEdge>,
}

/// One active wait projected for a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobWaitProjection {
    pub service: String,
    pub job_type: String,
    pub job_id: String,
    pub wait_edge: JobWaitEdge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Aggregate projection for one normalized job error fingerprint.
pub struct JobErrorProjection {
    pub fingerprint: String,
    pub message: String,
    pub first_seen: String,
    pub last_seen: String,
    pub occurrence_count: u64,
    pub sample_service: String,
    pub sample_job_type: String,
    pub sample_state: String,
}

/// One grouped Jobs workbench aggregate row.
#[derive(Debug, Clone, PartialEq)]
pub struct JobsWorkbenchGroup {
    pub key: String,
    pub label: String,
    pub count: u64,
    pub depth: Option<u64>,
    pub failure_rate: Option<f64>,
    pub latest_updated_at: Option<String>,
    pub oldest_created_at: Option<String>,
    pub state: Option<String>,
}

/// Jobs workbench aggregate counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobsWorkbenchStats {
    pub total: u64,
    pub by_state: BTreeMap<String, u64>,
    pub queued: Option<u64>,
    pub running: Option<u64>,
    pub failed: Option<u64>,
    pub dead: Option<u64>,
    pub slow: Option<u64>,
}

/// Filter used by the Jobs metrics dashboard surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsMetricsFilter {
    pub service: Option<String>,
    pub job_type: Option<String>,
    pub states: Option<Vec<JobState>>,
    pub since: OffsetDateTime,
    pub until: OffsetDateTime,
    pub step_nanos: i64,
    pub queue_key: Option<String>,
    pub trigger: Option<String>,
    pub group_by: JobsWorkbenchGroupBy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobsMetricsLatency {
    pub count: u64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobsMetricsSummaryGroup {
    pub key: String,
    pub label: String,
    pub total: u64,
    pub by_state: BTreeMap<String, u64>,
    pub running: Option<u64>,
    pub queued: Option<u64>,
    pub failed: Option<u64>,
    pub dead: Option<u64>,
    pub slow: Option<u64>,
    pub failure_rate: Option<f64>,
    pub runtime: JobsMetricsLatency,
    pub queue_wait: JobsMetricsLatency,
    pub oldest_created_at: Option<String>,
    pub latest_updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobsMetricsBucketGroup {
    pub key: String,
    pub label: String,
    pub submitted: u64,
    pub started: u64,
    pub completed: u64,
    pub failed: u64,
    pub retried: u64,
    pub dead: u64,
    pub cancelled: u64,
    pub dismissed: u64,
    pub runtime: JobsMetricsLatency,
    pub queue_wait: JobsMetricsLatency,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsMetricsBucket {
    pub start: String,
    pub end: String,
    pub groups: Vec<JobsMetricsBucketGroup>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobsMetricsPage {
    pub summary: Vec<JobsMetricsSummaryGroup>,
    pub buckets: Vec<JobsMetricsBucket>,
}

/// One projected lifecycle event for a job evidence timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct JobTimelineEvent {
    pub sequence: u64,
    pub event_type: String,
    pub state: String,
    pub previous_state: Option<String>,
    pub timestamp: String,
    pub tries: u64,
    pub message: Option<String>,
    pub error_message: Option<String>,
    pub progress_json: Option<String>,
    pub logs_json: Option<String>,
    pub worker_instance_id: Option<String>,
    pub raw_event_json: String,
    pub projected: Option<bool>,
    pub reason: Option<String>,
}

/// Keyed concurrency metadata projected from job lifecycle event metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobConcurrencyMetadata {
    pub key: String,
    pub key_hash: String,
    pub instance_id: Option<String>,
    pub heartbeat_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub stale_takeover_count: Option<u64>,
}

/// Queue policy metadata projected from job lifecycle event metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobQueuePolicyMetadata {
    pub outcome: String,
    pub reason: Option<String>,
    pub existing_job_id: Option<String>,
    pub replaced_job_id: Option<String>,
}

/// Optional admin metadata associated with one projected job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobProjectionMetadata {
    pub concurrency: Option<JobConcurrencyMetadata>,
    pub queue_policy: Option<JobQueuePolicyMetadata>,
}

/// Sparse metadata patch extracted from one lifecycle event payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobProjectionMetadataPatch {
    pub concurrency: Option<JobConcurrencyMetadata>,
    pub queue_policy: Option<JobQueuePolicyMetadata>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobLineageProjection {
    pub trigger: Option<JobTrigger>,
    pub lineage: Option<JobLineage>,
}

/// Projection-backed active job entry for one keyed-concurrency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedJobKeyActive {
    pub job_id: String,
    pub instance_id: Option<String>,
    pub started_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub lease_expires_at: Option<String>,
}

/// Projection-backed queued job entry for one keyed-concurrency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedJobKeyQueued {
    pub job_id: String,
    pub created_at: String,
}

/// Projection-backed state for one keyed-concurrency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedJobKey {
    pub service: String,
    pub job_type: String,
    pub key: String,
    pub key_hash: String,
    pub active: Vec<ProjectedJobKeyActive>,
    pub queued: Vec<ProjectedJobKeyQueued>,
    pub stale_takeover_count: u64,
    pub latest_policy_reason: Option<String>,
}

/// Errors returned by the SQLite Jobs projection store.
#[derive(Debug, thiserror::Error)]
pub enum SqliteJobsStoreError {
    #[error("sqlite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("sqlite connection lock is poisoned")]
    Poisoned,
    #[error("failed to encode {model}: {details}")]
    EncodeJson {
        model: &'static str,
        details: String,
    },
    #[error("failed to decode {model}: {details}")]
    DecodeJson {
        model: &'static str,
        details: String,
    },
    #[error("invalid {field}: {details}")]
    Validation {
        field: &'static str,
        details: String,
    },
}

impl SqliteJobsStore {
    /// Open a store at `path` and initialize the schema if needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteJobsStoreError> {
        let store = Self {
            connection: Arc::new(Mutex::new(Connection::open(path)?)),
        };
        store.initialize_schema()?;
        Ok(store)
    }

    /// Open an in-memory store and initialize the schema. Intended for tests.
    pub fn open_in_memory() -> Result<Self, SqliteJobsStoreError> {
        let store = Self {
            connection: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        store.initialize_schema()?;
        Ok(store)
    }

    /// Initialize the projection schema. Safe to call more than once.
    ///
    /// The runtime migration baseline is the single authoritative schema
    /// definition; persistent setup applies it through the migration runner.
    pub fn initialize_schema(&self) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        connection.execute_batch(include_str!(
            "../../../runtime/src/storage/sqlite/jobs/V2000__jobs_projection_init.sql"
        ))?;
        Ok(())
    }

    /// Return the stable identity for this local projection database.
    pub fn projection_id(&self) -> Result<String, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        connection.execute(
            "INSERT OR IGNORE INTO projection_metadata (name, value) VALUES ('projection_id', lower(hex(randomblob(16))))",
            [],
        )?;
        connection
            .query_row(
                "SELECT value FROM projection_metadata WHERE name = 'projection_id'",
                [],
                |row| row.get(0),
            )
            .map_err(SqliteJobsStoreError::from)
    }

    /// Return the latest Jobs stream sequence durably covered by this projection.
    pub fn projected_sequence(&self) -> Result<u64, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let value: String = connection.query_row(
            "SELECT value FROM projection_metadata WHERE name = 'last_projected_sequence'",
            [],
            |row| row.get(0),
        )?;
        value
            .parse::<u64>()
            .map_err(|error| SqliteJobsStoreError::DecodeJson {
                model: "Jobs projection checkpoint",
                details: error.to_string(),
            })
    }

    /// Advance the durable Jobs stream projection checkpoint monotonically.
    pub fn advance_projected_sequence(
        &self,
        stream_sequence: u64,
    ) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        connection.execute(
            "UPDATE projection_metadata SET value = max(CAST(value AS INTEGER), ?1) WHERE name = 'last_projected_sequence'",
            [stream_sequence],
        )?;
        Ok(())
    }

    pub(crate) fn with_write_transaction<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, SqliteJobsStoreError>,
    ) -> Result<T, SqliteJobsStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let value = f(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    /// Upsert one projected job row.
    pub fn upsert_job(&self, job: &Job) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        upsert_job_on_connection(&connection, job)
    }

    /// Fetch a job by its fully-qualified projection key.
    pub fn get_job(
        &self,
        service: &str,
        job_type: &str,
        id: &str,
    ) -> Result<Option<Job>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        get_job_from_connection(&connection, service, job_type, id)
    }

    pub fn upsert_job_lineage(&self, job: &Job) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        upsert_job_lineage_on_connection(&connection, job)
    }

    pub fn get_job_lineage_by_global_id(
        &self,
        id: &str,
    ) -> Result<JobLineageProjection, SqliteJobsStoreError> {
        let Some(job) = self.get_job_by_global_id(id)? else {
            return Ok(JobLineageProjection::default());
        };
        Ok(JobLineageProjection {
            trigger: job.trigger,
            lineage: job.lineage,
        })
    }

    /// Merge one sparse metadata patch into a projected job metadata row.
    pub fn apply_job_metadata_patch(
        &self,
        service: &str,
        job_type: &str,
        id: &str,
        timestamp: &str,
        patch: &JobProjectionMetadataPatch,
    ) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        apply_job_metadata_patch_on_connection(&connection, service, job_type, id, timestamp, patch)
    }

    /// Fetch projected admin metadata for one job.
    pub fn get_job_metadata(
        &self,
        service: &str,
        job_type: &str,
        id: &str,
    ) -> Result<Option<JobProjectionMetadata>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        get_job_metadata_from_connection(&connection, service, job_type, id)
    }

    /// Fetch a job by the globally addressable admin id.
    pub fn get_job_by_global_id(&self, id: &str) -> Result<Option<Job>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        get_job_by_global_id_from_connection(&connection, id)
    }

    /// Append one lifecycle event row to a job evidence timeline.
    pub fn project_timeline_event(
        &self,
        event: &JobEvent,
        raw_event: &serde_json::Value,
        projected: Option<bool>,
        reason: Option<&str>,
    ) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        project_timeline_event_on_connection(&connection, event, raw_event, projected, reason)
    }

    /// Upsert one observed job error into the fingerprint aggregate projection.
    pub fn upsert_error_projection(
        &self,
        service: &str,
        job_type: &str,
        state: JobState,
        timestamp: &str,
        detail: &JobErrorDetail,
    ) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        upsert_error_projection_on_connection(
            &connection,
            service,
            job_type,
            state,
            timestamp,
            detail,
        )
    }

    /// Project one wait-edge lifecycle event into the current active waits table.
    pub fn project_wait_edge(&self, event: &JobEvent) -> Result<(), SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        project_wait_edge_on_connection(&connection, event)
    }

    /// Fetch current active waits for one globally addressable job id.
    pub fn list_current_waits(
        &self,
        id: &str,
    ) -> Result<Vec<JobWaitProjection>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        list_current_waits_on_connection(&connection, id)
    }

    /// Fetch one current active wait by globally addressable job id and wait id.
    pub fn get_current_wait(
        &self,
        id: &str,
        wait_id: &str,
    ) -> Result<Option<JobWaitProjection>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        Ok(connection
            .query_row(
                r#"
                SELECT service, job_type, id, wait_edge_json
                FROM jobs_wait_projection
                WHERE id = ?1 AND wait_id = ?2
                "#,
                params![id, wait_id],
                wait_projection_from_row,
            )
            .optional()?)
    }

    /// Fetch one projected error aggregate by fingerprint.
    pub fn get_error_projection(
        &self,
        fingerprint: &str,
    ) -> Result<Option<JobErrorProjection>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        Ok(connection
            .query_row(
                r#"
                SELECT fingerprint, message, first_seen, last_seen, occurrence_count,
                       sample_service, sample_job_type, sample_state
                FROM jobs_error_projection
                WHERE fingerprint = ?1
                "#,
                params![fingerprint],
                error_projection_from_row,
            )
            .optional()?)
    }

    /// List timeline events for one globally addressable job id.
    pub fn list_timeline_events(
        &self,
        id: &str,
        limit: u64,
    ) -> Result<Vec<JobTimelineEvent>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            r#"
            SELECT sequence, event_type, state, previous_state, timestamp, tries, message,
                   error_message, progress_json, logs_json, worker_instance_id, raw_event_json,
                   projected, reason
            FROM jobs_events_projection
            WHERE id = ?1
            ORDER BY timestamp_nanos ASC, sequence ASC
            LIMIT ?2
            "#,
        )?;
        let rows = statement.query_map(params![id, sql_u64(limit)], timeline_event_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(SqliteJobsStoreError::from)
    }

    /// List a small set of jobs related by trace id, lineage, operation, or queue key.
    pub fn list_related_jobs(
        &self,
        job: &Job,
        limit: u64,
    ) -> Result<Vec<JobsWorkbenchEntry>, SqliteJobsStoreError> {
        let metadata = self.get_job_metadata(&job.service, &job.job_type, &job.id)?;
        let queue_key = metadata
            .as_ref()
            .and_then(|metadata| metadata.concurrency.as_ref())
            .map(|concurrency| concurrency.key.clone());
        let lineage = self.get_job_lineage_by_global_id(&job.id)?;
        let mut clauses = Vec::new();
        let mut match_arms = Vec::new();
        let mut query_params = vec![SqlValue::Text(job.id.clone())];
        let wait_target_job_ids = self
            .list_current_waits(&job.id)?
            .into_iter()
            .filter_map(|wait| match wait.wait_edge.target.kind {
                JobWaitTargetKind::Job => wait.wait_edge.target.id,
                JobWaitTargetKind::Operation | JobWaitTargetKind::External => None,
            })
            .collect::<Vec<_>>();
        if !wait_target_job_ids.is_empty() {
            let placeholders = wait_target_job_ids
                .iter()
                .map(|id| {
                    query_params.push(SqlValue::Text(id.clone()));
                    format!("?{}", query_params.len())
                })
                .collect::<Vec<_>>()
                .join(", ");
            let clause = format!("j.id IN ({placeholders})");
            match_arms.push(format!("WHEN {clause} THEN 'wait'"));
            clauses.push(clause);
        }
        query_params.push(SqlValue::Text(job.id.clone()));
        let reverse_wait_clause = format!(
            "EXISTS (SELECT 1 FROM jobs_wait_projection w WHERE w.id = j.id AND w.target_kind = 'job' AND w.target_id = ?{})",
            query_params.len()
        );
        match_arms.push(format!("WHEN {reverse_wait_clause} THEN 'wait'"));
        clauses.push(reverse_wait_clause);
        if !job.context.trace_id.is_empty() {
            query_params.push(SqlValue::Text(job.context.trace_id.clone()));
            let clause = format!("COALESCE(l.trace_id, j.trace_id) = ?{}", query_params.len());
            match_arms.push(format!("WHEN {clause} THEN 'trace'"));
            clauses.push(clause);
        }
        if let Some(parent_job_id) = lineage
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_job_id.clone())
        {
            query_params.push(SqlValue::Text(parent_job_id));
            let clause = format!(
                "(l.parent_job_id = ?{} OR j.id = ?{})",
                query_params.len(),
                query_params.len()
            );
            match_arms.push(format!("WHEN {clause} THEN 'parent'"));
            clauses.push(clause);
        }
        if let Some(root_job_id) = lineage
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.root_job_id.clone())
        {
            query_params.push(SqlValue::Text(root_job_id));
            let clause = format!(
                "(l.root_job_id = ?{} OR j.id = ?{})",
                query_params.len(),
                query_params.len()
            );
            match_arms.push(format!("WHEN {clause} THEN 'root'"));
            clauses.push(clause);
        }
        if let Some(operation_id) = lineage
            .lineage
            .as_ref()
            .and_then(|lineage| lineage.operation_id.clone())
        {
            query_params.push(SqlValue::Text(operation_id));
            let clause = format!("l.operation_id = ?{}", query_params.len());
            match_arms.push(format!("WHEN {clause} THEN 'operation'"));
            clauses.push(clause);
        }
        if let Some(queue_key) = queue_key {
            query_params.push(SqlValue::Text(queue_key));
            let clause = format!("m.concurrency_key = ?{}", query_params.len());
            match_arms.push(format!("WHEN {clause} THEN 'concurrency'"));
            clauses.push(clause);
        }
        if clauses.is_empty() {
            return Ok(Vec::new());
        }
        query_params.push(sql_u64(limit));
        let limit_param = query_params.len();
        let sql = format!(
            "SELECT j.job_json, j.runtime_ms, j.queue_age_anchor_nanos, m.concurrency_key, \
                    {RUNTIME_BAND_SQL}, j.last_error_fingerprint, \
                    CASE {} ELSE NULL END AS matched_by \
             FROM jobs_projection j \
               LEFT JOIN jobs_metadata_projection m \
                 ON m.service = j.service AND m.job_type = j.job_type AND m.id = j.id \
               LEFT JOIN jobs_lineage_projection l \
                 ON l.service = j.service AND l.job_type = j.job_type AND l.id = j.id \
               WHERE j.id <> ?1 AND ({}) \
              ORDER BY j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC \
              LIMIT ?{limit_param}",
            match_arms.join(" "),
            clauses.join(" OR ")
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(query_params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                job_json,
                runtime_ms,
                queue_age_anchor_nanos,
                queue_key,
                runtime_band,
                last_error_fingerprint,
                matched_by,
            ) = row?;
            entries.push(JobsWorkbenchEntry {
                job: decode_job_json(&job_json)?,
                runtime_ms,
                queue_age_anchor_nanos,
                queue_key,
                runtime_band,
                last_error_fingerprint,
                matched_by,
                waiting_on: Vec::new(),
            });
        }
        attach_waits_to_entries(&connection, &mut entries)?;
        Ok(entries)
    }

    /// Fetch projected admin metadata by globally addressable admin job id.
    pub fn get_job_metadata_by_global_id(
        &self,
        id: &str,
    ) -> Result<Option<JobProjectionMetadata>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        Ok(connection
            .query_row(
                r#"
                SELECT concurrency_key, concurrency_key_hash, concurrency_instance_id,
                       concurrency_heartbeat_at, concurrency_lease_expires_at,
                       concurrency_stale_takeover_count, queue_policy_outcome,
                       queue_policy_reason, queue_policy_existing_job_id,
                       queue_policy_replaced_job_id
                FROM jobs_metadata_projection
                WHERE id = ?1
                "#,
                params![id],
                metadata_from_row,
            )
            .optional()?)
    }

    /// List projected jobs with stable offset pagination.
    pub fn list_jobs(&self, filter: &ListJobsFilter) -> Result<JobsPage, SqliteJobsStoreError> {
        let (mut where_sql, mut query_params) = list_jobs_where_clause(filter);
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;

        let count_sql = format!("SELECT COUNT(*) FROM jobs_projection{where_sql}");
        let count =
            connection.query_row(&count_sql, params_from_iter(query_params.iter()), |row| {
                row.get::<_, i64>(0)
            })?;
        let count = u64::try_from(count).unwrap_or(0);

        if let Some((updated_at, service, job_type, id)) = &filter.after {
            let predicate = "(updated_at_nanos < ? OR (updated_at_nanos = ? AND (service > ? OR (service = ? AND (job_type > ? OR (job_type = ? AND id > ?))))))";
            where_sql = if where_sql.is_empty() {
                format!(" WHERE {predicate}")
            } else {
                format!("{where_sql} AND {predicate}")
            };
            query_params.extend([
                SqlValue::Integer(*updated_at),
                SqlValue::Integer(*updated_at),
                SqlValue::Text(service.clone()),
                SqlValue::Text(service.clone()),
                SqlValue::Text(job_type.clone()),
                SqlValue::Text(job_type.clone()),
                SqlValue::Text(id.clone()),
            ]);
        }
        query_params.push(sql_u64(filter.limit.saturating_add(1)));
        let list_sql = format!(
            "SELECT job_json FROM jobs_projection{where_sql} \
                   ORDER BY updated_at_nanos DESC, service ASC, job_type ASC, id ASC \
                   LIMIT ?"
        );
        let mut statement = connection.prepare(&list_sql)?;
        let rows = statement.query_map(params_from_iter(query_params.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(decode_job_json(&row?)?);
        }

        let next_after = (jobs.len() > filter.limit as usize).then(|| {
            jobs.pop();
            let job = jobs.last().expect("non-empty full page");
            (
                timestamp_str_nanos(&job.updated_at),
                job.service.clone(),
                job.job_type.clone(),
                job.id.clone(),
            )
        });

        Ok(JobsPage {
            jobs,
            count,
            limit: filter.limit,
            next_after,
        })
    }

    /// Query projected jobs for the Jobs workbench.
    pub fn query_jobs(
        &self,
        filter: &JobsWorkbenchFilter,
    ) -> Result<JobsWorkbenchPage, SqliteJobsStoreError> {
        let (from_sql, mut where_sql, mut query_params) = workbench_from_where_clause(filter)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;

        let count_sql = format!("SELECT COUNT(*) {from_sql}{where_sql}");
        let count =
            connection.query_row(&count_sql, params_from_iter(query_params.iter()), |row| {
                row.get::<_, i64>(0)
            })?;
        let count = u64::try_from(count).unwrap_or(0);
        let stats = query_workbench_stats(&connection, &from_sql, &where_sql, &query_params)?;

        if let Some((primary, updated_at, service, job_type, id)) = &filter.after {
            let (predicate, mut cursor_params) = workbench_after_predicate(
                filter.sort,
                *primary,
                *updated_at,
                service,
                job_type,
                id,
            );
            where_sql = if where_sql.is_empty() {
                format!(" WHERE {predicate}")
            } else {
                format!("{where_sql} AND {predicate}")
            };
            query_params.append(&mut cursor_params);
        }
        query_params.push(sql_u64(filter.limit.saturating_add(1)));
        let list_sql = format!(
            "SELECT j.job_json, j.runtime_ms, j.queue_age_anchor_nanos, m.concurrency_key, \
                    {RUNTIME_BAND_SQL}, j.last_error_fingerprint \
             {from_sql}{where_sql} ORDER BY {} LIMIT ?",
            workbench_order_sql(filter.sort)
        );
        let mut statement = connection.prepare(&list_sql)?;
        let rows = statement.query_map(params_from_iter(query_params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                job_json,
                runtime_ms,
                queue_age_anchor_nanos,
                queue_key,
                runtime_band,
                last_error_fingerprint,
            ) = row?;
            entries.push(JobsWorkbenchEntry {
                job: decode_job_json(&job_json)?,
                runtime_ms,
                queue_age_anchor_nanos,
                queue_key,
                runtime_band,
                last_error_fingerprint,
                matched_by: None,
                waiting_on: Vec::new(),
            });
        }
        attach_waits_to_entries(&connection, &mut entries)?;

        let next_after = (entries.len() > filter.limit as usize).then(|| {
            entries.pop();
            workbench_cursor(entries.last().expect("non-empty full page"), filter.sort)
        });

        Ok(JobsWorkbenchPage {
            entries,
            count,
            limit: filter.limit,
            next_after,
            stats,
        })
    }

    /// Query grouped projected jobs for the Jobs workbench.
    pub fn query_job_groups(
        &self,
        filter: &JobsWorkbenchFilter,
    ) -> Result<Vec<JobsWorkbenchGroup>, SqliteJobsStoreError> {
        let Some(group_by) = filter.group_by else {
            return Ok(Vec::new());
        };
        let (from_sql, where_sql, query_params) = workbench_from_where_clause(filter)?;
        let (key_sql, label_sql, state_sql) = workbench_group_sql(group_by);
        let order_sql = match filter.sort.field {
            JobsWorkbenchSortField::Depth => "depth DESC, key ASC",
            JobsWorkbenchSortField::FailureRate => "failure_rate DESC, key ASC",
            _ => "count DESC, key ASC",
        };
        let sql = format!(
            "SELECT {key_sql} AS key, {label_sql} AS label, COUNT(*) AS count, \
                    SUM(CASE WHEN j.state IN ('pending', 'retry') THEN 1 ELSE 0 END) AS depth, \
                    CAST(SUM(CASE WHEN j.state IN ('failed', 'dead') THEN 1 ELSE 0 END) AS REAL) / COUNT(*) AS failure_rate, \
                    MAX(j.updated_at) AS latest_updated_at, MIN(j.created_at) AS oldest_created_at, {state_sql} AS state \
             {from_sql}{where_sql} GROUP BY key ORDER BY {order_sql}"
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(query_params.iter()), |row| {
            Ok(JobsWorkbenchGroup {
                key: row.get(0)?,
                label: row.get(1)?,
                count: row.get::<_, i64>(2).ok().and_then(i64_to_u64).unwrap_or(0),
                depth: row.get::<_, Option<i64>>(3)?.and_then(i64_to_u64),
                failure_rate: row.get(4)?,
                latest_updated_at: row.get(5)?,
                oldest_created_at: row.get(6)?,
                state: row.get(7)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(SqliteJobsStoreError::from)
    }

    /// Query grouped current summaries and time buckets for the Jobs metrics dashboard.
    pub fn query_metrics(
        &self,
        filter: &JobsMetricsFilter,
    ) -> Result<JobsMetricsPage, SqliteJobsStoreError> {
        Ok(JobsMetricsPage {
            summary: self.query_metrics_summary(filter)?,
            buckets: self.query_metrics_buckets(filter)?,
        })
    }

    fn query_metrics_summary(
        &self,
        filter: &JobsMetricsFilter,
    ) -> Result<Vec<JobsMetricsSummaryGroup>, SqliteJobsStoreError> {
        let (from_sql, where_sql, params) =
            metrics_jobs_where_clause(filter, Some("j.updated_at_nanos"))?;
        let (key_sql, label_sql, _) = workbench_group_sql(filter.group_by);
        let sql = format!(
            "SELECT {key_sql} AS key, {label_sql} AS label, j.state, COUNT(*) AS count, \
                    SUM(CASE WHEN j.state IN ('pending', 'retry') THEN 1 ELSE 0 END) AS queued, \
                    SUM(CASE WHEN j.state = 'active' THEN 1 ELSE 0 END) AS running, \
                    SUM(CASE WHEN j.state = 'failed' THEN 1 ELSE 0 END) AS failed, \
                    SUM(CASE WHEN j.state = 'dead' THEN 1 ELSE 0 END) AS dead, \
                    SUM(CASE WHEN j.state = 'active' AND j.runtime_ms >= 60000 THEN 1 ELSE 0 END) AS slow, \
                    MIN(j.created_at) AS oldest_created_at, MAX(j.updated_at) AS latest_updated_at \
             {from_sql}{where_sql} GROUP BY key, j.state ORDER BY count DESC, key ASC"
        );
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut groups = BTreeMap::<String, JobsMetricsSummaryGroup>::new();
        {
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            })?;
            for row in rows {
                let (key, label, state, count, queued, running, failed, dead, slow, oldest, latest) =
                    row?;
                let count = i64_to_u64(count).unwrap_or(0);
                let entry = groups
                    .entry(key.clone())
                    .or_insert_with(|| JobsMetricsSummaryGroup {
                        key,
                        label,
                        total: 0,
                        by_state: BTreeMap::new(),
                        running: None,
                        queued: None,
                        failed: None,
                        dead: None,
                        slow: None,
                        failure_rate: None,
                        runtime: JobsMetricsLatency::default(),
                        queue_wait: JobsMetricsLatency::default(),
                        oldest_created_at: oldest.clone(),
                        latest_updated_at: latest.clone(),
                    });
                entry.total = entry.total.saturating_add(count);
                entry.by_state.insert(state, count);
                add_optional_count(&mut entry.queued, queued.and_then(i64_to_u64));
                add_optional_count(&mut entry.running, running.and_then(i64_to_u64));
                add_optional_count(&mut entry.failed, failed.and_then(i64_to_u64));
                add_optional_count(&mut entry.dead, dead.and_then(i64_to_u64));
                add_optional_count(&mut entry.slow, slow.and_then(i64_to_u64));
                if entry.oldest_created_at.as_ref() > oldest.as_ref() {
                    entry.oldest_created_at = oldest;
                }
                if entry.latest_updated_at.as_ref() < latest.as_ref() {
                    entry.latest_updated_at = latest;
                }
            }
        }

        for group in groups.values_mut() {
            let failures = group
                .failed
                .unwrap_or(0)
                .saturating_add(group.dead.unwrap_or(0));
            group.failure_rate = (group.total > 0).then(|| failures as f64 / group.total as f64);
            group.runtime = query_group_latency(
                &connection,
                filter,
                &group.key,
                "j.runtime_ms",
                "j.updated_at_nanos",
            )?;
            group.queue_wait = query_group_latency(
                &connection,
                filter,
                &group.key,
                "((j.started_at_nanos - j.created_at_nanos) / 1000000)",
                "j.started_at_nanos",
            )?;
        }

        let mut groups = groups.into_values().collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            right
                .total
                .cmp(&left.total)
                .then_with(|| left.key.cmp(&right.key))
        });
        Ok(groups)
    }

    fn query_metrics_buckets(
        &self,
        filter: &JobsMetricsFilter,
    ) -> Result<Vec<JobsMetricsBucket>, SqliteJobsStoreError> {
        let since_nanos = timestamp_nanos(filter.since);
        let until_nanos = timestamp_nanos(filter.until);
        let mut buckets = BTreeMap::<i64, BTreeMap<String, JobsMetricsBucketGroup>>::new();
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        collect_event_counts(&connection, filter, &mut buckets)?;
        collect_latency_buckets(
            &connection,
            filter,
            &mut buckets,
            "runtime",
            "j.runtime_ms",
            "COALESCE(j.completed_at_nanos, j.updated_at_nanos)",
        )?;
        collect_latency_buckets(
            &connection,
            filter,
            &mut buckets,
            "queueWait",
            "((j.started_at_nanos - j.created_at_nanos) / 1000000)",
            "j.started_at_nanos",
        )?;

        let mut output = Vec::new();
        let mut start = since_nanos;
        while start < until_nanos {
            let end = start.saturating_add(filter.step_nanos).min(until_nanos);
            let mut groups = buckets
                .remove(&start)
                .map(|groups| groups.into_values().collect::<Vec<_>>())
                .unwrap_or_default();
            groups.sort_by(|left, right| left.label.cmp(&right.label));
            output.push(JobsMetricsBucket {
                start: timestamp_nanos_to_string(start),
                end: timestamp_nanos_to_string(end),
                groups,
            });
            start = end;
        }
        Ok(output)
    }

    /// List non-terminal jobs whose business deadline is at or before `now`.
    pub fn scan_expired_jobs(&self, now: &str) -> Result<Vec<Job>, SqliteJobsStoreError> {
        let now_nanos = timestamp_str_nanos(now);
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            r#"
            SELECT job_json FROM jobs_projection
            WHERE state IN ('pending', 'retry', 'active')
              AND deadline_nanos IS NOT NULL
              AND deadline_nanos <= ?1
            ORDER BY deadline_nanos ASC, service ASC, job_type ASC, id ASC
            "#,
        )?;
        let rows = statement.query_map(params![now_nanos], |row| row.get::<_, String>(0))?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(decode_job_json(&row?)?);
        }
        Ok(jobs)
    }

    /// Fetch projection-backed keyed-concurrency state for one service/job-type/key.
    pub fn get_projected_key(
        &self,
        service: &str,
        job_type: &str,
        key: &str,
    ) -> Result<Option<ProjectedJobKey>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            r#"
            SELECT j.job_json, m.concurrency_key_hash, m.concurrency_instance_id,
                   m.concurrency_heartbeat_at, m.concurrency_lease_expires_at,
                   m.concurrency_stale_takeover_count, m.queue_policy_reason,
                   m.updated_at_nanos
            FROM jobs_metadata_projection m
            JOIN jobs_projection j
              ON j.service = m.service AND j.job_type = m.job_type AND j.id = m.id
            WHERE m.service = ?1 AND m.job_type = ?2 AND m.concurrency_key = ?3
            ORDER BY j.updated_at_nanos DESC, j.id ASC
            "#,
        )?;
        let rows = statement.query_map(params![service, job_type, key], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        })?;

        let mut key_hash = None;
        let mut active = Vec::new();
        let mut queued = Vec::new();
        let mut stale_takeover_count = 0;
        let mut latest_policy_reason = None;
        let mut latest_policy_reason_nanos = i64::MIN;

        for row in rows {
            let (
                job_json,
                row_key_hash,
                instance_id,
                heartbeat_at,
                lease_expires_at,
                row_stale_takeover_count,
                policy_reason,
                metadata_updated_at_nanos,
            ) = row?;
            let job = decode_job_json(&job_json)?;
            if key_hash.is_none() {
                key_hash = row_key_hash;
            }
            if let Some(count) = row_stale_takeover_count.and_then(i64_to_u64) {
                stale_takeover_count = stale_takeover_count.max(count);
            }
            if let Some(reason) = policy_reason {
                let updated_at_nanos = metadata_updated_at_nanos.unwrap_or(i64::MIN);
                if latest_policy_reason.is_none() || updated_at_nanos >= latest_policy_reason_nanos
                {
                    latest_policy_reason = Some(reason);
                    latest_policy_reason_nanos = updated_at_nanos;
                }
            }
            match job.state {
                JobState::Active => active.push(ProjectedJobKeyActive {
                    job_id: job.id,
                    instance_id,
                    started_at: job.started_at,
                    heartbeat_at,
                    lease_expires_at,
                }),
                JobState::Pending | JobState::Retry => queued.push(ProjectedJobKeyQueued {
                    job_id: job.id,
                    created_at: job.created_at,
                }),
                _ => {}
            }
        }

        let Some(key_hash) = key_hash else {
            return Ok(None);
        };

        active.sort_by(|left, right| left.job_id.cmp(&right.job_id));
        queued.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.job_id.cmp(&right.job_id))
        });

        Ok(Some(ProjectedJobKey {
            service: service.to_string(),
            job_type: job_type.to_string(),
            key: key.to_string(),
            key_hash,
            active,
            queued,
            stale_takeover_count,
            latest_policy_reason,
        }))
    }

    /// Upsert one worker-presence projection row.
    pub fn upsert_worker_presence(
        &self,
        worker: &WorkerPresenceRecord,
    ) -> Result<(), SqliteJobsStoreError> {
        let record_json =
            serde_json::to_string(worker).map_err(|error| SqliteJobsStoreError::EncodeJson {
                model: "worker presence",
                details: error.to_string(),
            })?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        connection.execute(
            r#"
            INSERT INTO worker_presence_projection
                (service, job_type, instance_id, concurrency, version, heartbeat_at, record_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(service, job_type, instance_id) DO UPDATE SET
                concurrency = excluded.concurrency,
                version = excluded.version,
                heartbeat_at = excluded.heartbeat_at,
                record_json = excluded.record_json
            "#,
            params![
                worker.service,
                worker.job_type,
                worker.instance_id,
                worker.concurrency,
                worker.version,
                worker.heartbeat_at,
                record_json
            ],
        )?;
        Ok(())
    }

    /// Fetch one worker-presence projection row by its fully-qualified key.
    pub fn get_worker_presence(
        &self,
        service: &str,
        job_type: &str,
        instance_id: &str,
    ) -> Result<Option<WorkerPresenceRecord>, SqliteJobsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT record_json FROM worker_presence_projection WHERE service = ?1 AND job_type = ?2 AND instance_id = ?3",
                params![service, job_type, instance_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| {
                serde_json::from_str::<WorkerPresenceRecord>(&json).map_err(|error| {
                    SqliteJobsStoreError::DecodeJson {
                        model: "worker presence",
                        details: error.to_string(),
                    }
                })
            })
            .transpose()
    }

    /// List worker-presence rows whose heartbeat is still fresh at `now`.
    pub fn list_fresh_workers(
        &self,
        now: OffsetDateTime,
        fresh_for: Duration,
    ) -> Result<Vec<WorkerPresenceRecord>, SqliteJobsStoreError> {
        let threshold = now - fresh_for;
        let connection = self
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT record_json FROM worker_presence_projection ORDER BY service ASC, job_type ASC, instance_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut workers = Vec::new();
        for row in rows {
            let worker = serde_json::from_str::<WorkerPresenceRecord>(&row?).map_err(|error| {
                SqliteJobsStoreError::DecodeJson {
                    model: "worker presence",
                    details: error.to_string(),
                }
            })?;
            if parse_timestamp(&worker.heartbeat_at) >= threshold {
                workers.push(worker);
            }
        }
        Ok(workers)
    }
}

pub(crate) fn upsert_job_on_connection(
    connection: &Connection,
    job: &Job,
) -> Result<(), SqliteJobsStoreError> {
    let state = job_state_token(job.state);
    let updated_at_nanos = timestamp_str_nanos(&job.updated_at);
    let created_at_nanos = timestamp_str_nanos(&job.created_at);
    let started_at_nanos = job.started_at.as_deref().map(timestamp_str_nanos);
    let completed_at_nanos = job.completed_at.as_deref().map(timestamp_str_nanos);
    let runtime_ms = runtime_ms(started_at_nanos, completed_at_nanos, updated_at_nanos);
    let queue_age_anchor_nanos = queue_age_anchor_nanos(job, created_at_nanos);
    let last_error_fingerprint = job
        .error_detail
        .as_ref()
        .map(|detail| detail.fingerprint.clone())
        .or_else(|| {
            job.last_error
                .as_deref()
                .map(|message| error_fingerprint(&job.service, &job.job_type, message))
        });
    let deadline_nanos = job.deadline.as_deref().map(timestamp_str_nanos);
    let payload_json =
        serde_json::to_string(&job.payload).map_err(|error| SqliteJobsStoreError::EncodeJson {
            model: "job payload",
            details: error.to_string(),
        })?;
    let job_json =
        serde_json::to_string(job).map_err(|error| SqliteJobsStoreError::EncodeJson {
            model: "job",
            details: error.to_string(),
        })?;
    let old_fts = fts_row_for_job(connection, &job.service, &job.job_type, &job.id)?;
    connection.execute(
        r#"
        INSERT INTO jobs_projection
            (service, job_type, id, state, created_at, updated_at, updated_at_nanos,
             request_id, trace_id, traceparent, started_at_nanos, completed_at_nanos,
             created_at_nanos, runtime_ms, queue_age_anchor_nanos, last_error_message,
             last_error_fingerprint, deadline, deadline_nanos, payload_json, job_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
        ON CONFLICT(service, job_type, id) DO UPDATE SET
            state = excluded.state,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            updated_at_nanos = excluded.updated_at_nanos,
            request_id = excluded.request_id,
            trace_id = excluded.trace_id,
            traceparent = excluded.traceparent,
            started_at_nanos = excluded.started_at_nanos,
            completed_at_nanos = excluded.completed_at_nanos,
            created_at_nanos = excluded.created_at_nanos,
            runtime_ms = excluded.runtime_ms,
            queue_age_anchor_nanos = excluded.queue_age_anchor_nanos,
            last_error_message = excluded.last_error_message,
            last_error_fingerprint = excluded.last_error_fingerprint,
            deadline = excluded.deadline,
            deadline_nanos = excluded.deadline_nanos,
            payload_json = excluded.payload_json,
            job_json = excluded.job_json
        "#,
        params![
            job.service,
            job.job_type,
            job.id,
            state,
            job.created_at,
            job.updated_at,
            updated_at_nanos,
            job.context.request_id,
            job.context.trace_id,
            job.context.traceparent,
            started_at_nanos,
            completed_at_nanos,
            created_at_nanos,
            runtime_ms,
            queue_age_anchor_nanos,
            job.last_error.as_deref(),
            last_error_fingerprint,
            job.deadline,
            deadline_nanos,
            payload_json,
            job_json
        ],
    )?;
    let new_fts = fts_row_for_job(connection, &job.service, &job.job_type, &job.id)?;
    replace_fts_row(connection, old_fts.as_ref(), new_fts.as_ref())?;
    Ok(())
}

pub(crate) fn get_job_from_connection(
    connection: &Connection,
    service: &str,
    job_type: &str,
    id: &str,
) -> Result<Option<Job>, SqliteJobsStoreError> {
    connection
        .query_row(
            "SELECT job_json FROM jobs_projection WHERE service = ?1 AND job_type = ?2 AND id = ?3",
            params![service, job_type, id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| decode_job_json(&json))
        .transpose()
}

pub(crate) fn upsert_job_lineage_on_connection(
    connection: &Connection,
    job: &Job,
) -> Result<(), SqliteJobsStoreError> {
    if job.trigger.is_none() && job.lineage.is_none() {
        return Ok(());
    }
    let trigger_kind = job
        .trigger
        .as_ref()
        .map(|trigger| trigger_kind_token(trigger.kind));
    connection.execute(
        r#"
        INSERT INTO jobs_lineage_projection
            (service, job_type, id, parent_job_id, root_job_id, operation_id,
             trigger_kind, trigger_id, trace_id, request_id, updated_at_nanos)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(service, job_type, id) DO UPDATE SET
            parent_job_id = excluded.parent_job_id,
            root_job_id = excluded.root_job_id,
            operation_id = excluded.operation_id,
            trigger_kind = excluded.trigger_kind,
            trigger_id = excluded.trigger_id,
            trace_id = excluded.trace_id,
            request_id = excluded.request_id,
            updated_at_nanos = excluded.updated_at_nanos
        "#,
        params![
            job.service,
            job.job_type,
            job.id,
            job.lineage
                .as_ref()
                .and_then(|lineage| lineage.parent_job_id.as_deref()),
            job.lineage
                .as_ref()
                .and_then(|lineage| lineage.root_job_id.as_deref()),
            job.lineage
                .as_ref()
                .and_then(|lineage| lineage.operation_id.as_deref())
                .or_else(|| job
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.operation_id.as_deref())),
            trigger_kind,
            job.trigger
                .as_ref()
                .and_then(|trigger| trigger.id.as_deref()),
            job.trigger
                .as_ref()
                .and_then(|trigger| trigger.trace_id.as_deref())
                .or(Some(job.context.trace_id.as_str())),
            job.trigger
                .as_ref()
                .and_then(|trigger| trigger.request_id.as_deref())
                .or(Some(job.context.request_id.as_str())),
            timestamp_str_nanos(&job.updated_at),
        ],
    )?;
    Ok(())
}

pub(crate) fn apply_job_metadata_patch_on_connection(
    connection: &Connection,
    service: &str,
    job_type: &str,
    id: &str,
    timestamp: &str,
    patch: &JobProjectionMetadataPatch,
) -> Result<(), SqliteJobsStoreError> {
    if patch.concurrency.is_none() && patch.queue_policy.is_none() {
        return Ok(());
    }

    let existing =
        get_job_metadata_from_connection(connection, service, job_type, id)?.unwrap_or_default();
    let concurrency = merge_concurrency_metadata(existing.concurrency, patch.concurrency.clone());
    let queue_policy =
        merge_queue_policy_metadata(existing.queue_policy, patch.queue_policy.clone());

    let stale_takeover_count = concurrency
        .as_ref()
        .and_then(|metadata| metadata.stale_takeover_count)
        .map(|count| i64::try_from(count).unwrap_or(i64::MAX));
    let old_fts = fts_row_for_job(connection, service, job_type, id)?;
    connection.execute(
        r#"
        INSERT INTO jobs_metadata_projection
            (service, job_type, id, concurrency_key, concurrency_key_hash, concurrency_instance_id,
             concurrency_heartbeat_at, concurrency_lease_expires_at, concurrency_stale_takeover_count,
             queue_policy_outcome, queue_policy_reason, queue_policy_existing_job_id,
             queue_policy_replaced_job_id, updated_at, updated_at_nanos)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(service, job_type, id) DO UPDATE SET
            concurrency_key = excluded.concurrency_key,
            concurrency_key_hash = excluded.concurrency_key_hash,
            concurrency_instance_id = excluded.concurrency_instance_id,
            concurrency_heartbeat_at = excluded.concurrency_heartbeat_at,
            concurrency_lease_expires_at = excluded.concurrency_lease_expires_at,
            concurrency_stale_takeover_count = excluded.concurrency_stale_takeover_count,
            queue_policy_outcome = excluded.queue_policy_outcome,
            queue_policy_reason = excluded.queue_policy_reason,
            queue_policy_existing_job_id = excluded.queue_policy_existing_job_id,
            queue_policy_replaced_job_id = excluded.queue_policy_replaced_job_id,
            updated_at = excluded.updated_at,
            updated_at_nanos = excluded.updated_at_nanos
        "#,
        params![
            service,
            job_type,
            id,
            concurrency.as_ref().map(|metadata| metadata.key.as_str()),
            concurrency.as_ref().map(|metadata| metadata.key_hash.as_str()),
            concurrency
                .as_ref()
                .and_then(|metadata| metadata.instance_id.as_deref()),
            concurrency
                .as_ref()
                .and_then(|metadata| metadata.heartbeat_at.as_deref()),
            concurrency
                .as_ref()
                .and_then(|metadata| metadata.lease_expires_at.as_deref()),
            stale_takeover_count,
            queue_policy.as_ref().map(|metadata| metadata.outcome.as_str()),
            queue_policy
                .as_ref()
                .and_then(|metadata| metadata.reason.as_deref()),
            queue_policy
                .as_ref()
                .and_then(|metadata| metadata.existing_job_id.as_deref()),
            queue_policy
                .as_ref()
                .and_then(|metadata| metadata.replaced_job_id.as_deref()),
            timestamp,
            timestamp_str_nanos(timestamp),
        ],
    )?;
    let new_fts = fts_row_for_job(connection, service, job_type, id)?;
    replace_fts_row(connection, old_fts.as_ref(), new_fts.as_ref())?;
    Ok(())
}

pub(crate) fn get_job_metadata_from_connection(
    connection: &Connection,
    service: &str,
    job_type: &str,
    id: &str,
) -> Result<Option<JobProjectionMetadata>, SqliteJobsStoreError> {
    Ok(connection
        .query_row(
            r#"
            SELECT concurrency_key, concurrency_key_hash, concurrency_instance_id,
                   concurrency_heartbeat_at, concurrency_lease_expires_at,
                   concurrency_stale_takeover_count, queue_policy_outcome,
                   queue_policy_reason, queue_policy_existing_job_id,
                   queue_policy_replaced_job_id
            FROM jobs_metadata_projection
            WHERE service = ?1 AND job_type = ?2 AND id = ?3
            "#,
            params![service, job_type, id],
            metadata_from_row,
        )
        .optional()?)
}

pub(crate) fn get_job_by_global_id_from_connection(
    connection: &Connection,
    id: &str,
) -> Result<Option<Job>, SqliteJobsStoreError> {
    connection
        .query_row(
            "SELECT job_json FROM jobs_projection WHERE id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| decode_job_json(&json))
        .transpose()
}

pub(crate) fn project_timeline_event_on_connection(
    connection: &Connection,
    event: &JobEvent,
    raw_event: &serde_json::Value,
    projected: Option<bool>,
    reason: Option<&str>,
) -> Result<(), SqliteJobsStoreError> {
    let raw_event_json =
        serde_json::to_string(raw_event).map_err(|error| SqliteJobsStoreError::EncodeJson {
            model: "job raw event",
            details: error.to_string(),
        })?;
    let progress_json = event
        .progress
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| SqliteJobsStoreError::EncodeJson {
            model: "job event progress",
            details: error.to_string(),
        })?;
    let logs_json = event
        .logs
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| SqliteJobsStoreError::EncodeJson {
            model: "job event logs",
            details: error.to_string(),
        })?;
    let admin_reason = event
        .admin_action
        .as_ref()
        .and_then(|action| action.reason.as_deref());
    let message = event
        .progress
        .as_ref()
        .and_then(|progress| progress.message.as_deref())
        .or(admin_reason);
    let sequence = connection.query_row(
        r#"
        SELECT COALESCE(MAX(sequence), 0) + 1
        FROM jobs_events_projection
        WHERE service = ?1 AND job_type = ?2 AND id = ?3
        "#,
        params![event.service, event.job_type, event.job_id],
        |row| row.get::<_, i64>(0),
    )?;
    connection.execute(
        r#"
        INSERT INTO jobs_events_projection
            (service, job_type, id, sequence, event_type, state, previous_state,
             timestamp, timestamp_nanos, tries, message, error_message, progress_json,
             logs_json, worker_instance_id, raw_event_json, projected, reason)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        "#,
        params![
            event.service,
            event.job_type,
            event.job_id,
            sequence,
            event.event_type.as_token(),
            job_state_token(event.state),
            event.previous_state.map(job_state_token),
            event.timestamp,
            timestamp_str_nanos(&event.timestamp),
            sql_u64(event.tries),
            message,
            event.error.as_deref(),
            progress_json,
            logs_json,
            event
                .concurrency
                .as_ref()
                .and_then(|value| value.instance_id.as_deref()),
            raw_event_json,
            projected.map(i64::from),
            reason,
        ],
    )?;
    Ok(())
}

pub(crate) fn project_wait_edge_on_connection(
    connection: &Connection,
    event: &JobEvent,
) -> Result<(), SqliteJobsStoreError> {
    if trellis_rs::jobs::is_terminal(event.state) {
        connection.execute(
            r#"
            DELETE FROM jobs_wait_projection
            WHERE service = ?1 AND job_type = ?2 AND id = ?3
            "#,
            params![event.service, event.job_type, event.job_id],
        )?;
        return Ok(());
    }

    match (event.event_type, event.wait_edge.as_ref()) {
        (JobEventType::Waiting, Some(wait_edge)) => {
            let wait_edge_json = serde_json::to_string(wait_edge).map_err(|error| {
                SqliteJobsStoreError::EncodeJson {
                    model: "job wait edge",
                    details: error.to_string(),
                }
            })?;
            connection.execute(
                r#"
                INSERT OR REPLACE INTO jobs_wait_projection
                    (service, job_type, id, wait_id, started_at, target_kind, target_id,
                     target_service, target_type, wait_edge_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    event.service,
                    event.job_type,
                    event.job_id,
                    wait_edge.id,
                    wait_edge.started_at,
                    wait_target_kind_token(wait_edge.target.kind),
                    wait_edge
                        .target
                        .id
                        .as_deref()
                        .or(wait_edge.target.operation_id.as_deref()),
                    wait_edge.target.service.as_deref(),
                    wait_edge.target.target_type.as_deref(),
                    wait_edge_json,
                ],
            )?;
        }
        (JobEventType::Resumed, Some(wait_edge)) => {
            connection.execute(
                r#"
                DELETE FROM jobs_wait_projection
                WHERE service = ?1 AND job_type = ?2 AND id = ?3 AND wait_id = ?4
                "#,
                params![event.service, event.job_type, event.job_id, wait_edge.id],
            )?;
        }
        _ => {}
    }

    Ok(())
}

pub(crate) fn upsert_error_projection_on_connection(
    connection: &Connection,
    service: &str,
    job_type: &str,
    state: JobState,
    timestamp: &str,
    detail: &JobErrorDetail,
) -> Result<(), SqliteJobsStoreError> {
    connection.execute(
        r#"
        INSERT INTO jobs_error_projection
            (fingerprint, message, first_seen, first_seen_nanos, last_seen, last_seen_nanos,
             occurrence_count, sample_service, sample_job_type, sample_state)
        VALUES (?1, ?2, ?3, ?4, ?3, ?4, 1, ?5, ?6, ?7)
        ON CONFLICT(fingerprint) DO UPDATE SET
            last_seen = excluded.last_seen,
            last_seen_nanos = excluded.last_seen_nanos,
            occurrence_count = jobs_error_projection.occurrence_count + 1,
            sample_service = excluded.sample_service,
            sample_job_type = excluded.sample_job_type,
            sample_state = excluded.sample_state
        "#,
        params![
            detail.fingerprint,
            detail.message,
            timestamp,
            timestamp_str_nanos(timestamp),
            service,
            job_type,
            job_state_token(state),
        ],
    )?;
    Ok(())
}

fn timeline_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobTimelineEvent> {
    Ok(JobTimelineEvent {
        sequence: row.get::<_, i64>(0).ok().and_then(i64_to_u64).unwrap_or(0),
        event_type: row.get(1)?,
        state: row.get(2)?,
        previous_state: row.get(3)?,
        timestamp: row.get(4)?,
        tries: row.get::<_, i64>(5).ok().and_then(i64_to_u64).unwrap_or(0),
        message: row.get(6)?,
        error_message: row.get(7)?,
        progress_json: row.get(8)?,
        logs_json: row.get(9)?,
        worker_instance_id: row.get(10)?,
        raw_event_json: row.get(11)?,
        projected: row.get::<_, Option<i64>>(12)?.map(|value| value != 0),
        reason: row.get(13)?,
    })
}

fn list_current_waits_on_connection(
    connection: &Connection,
    id: &str,
) -> Result<Vec<JobWaitProjection>, SqliteJobsStoreError> {
    let mut statement = connection.prepare(
        r#"
        SELECT service, job_type, id, wait_edge_json
        FROM jobs_wait_projection
        WHERE id = ?1
        ORDER BY started_at ASC, wait_id ASC
        "#,
    )?;
    let rows = statement.query_map(params![id], wait_projection_from_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteJobsStoreError::from)
}

fn wait_projection_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobWaitProjection> {
    let wait_edge_json: String = row.get(3)?;
    let wait_edge = serde_json::from_str(&wait_edge_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(JobWaitProjection {
        service: row.get(0)?,
        job_type: row.get(1)?,
        job_id: row.get(2)?,
        wait_edge,
    })
}

fn attach_waits_to_entries(
    connection: &Connection,
    entries: &mut [JobsWorkbenchEntry],
) -> Result<(), SqliteJobsStoreError> {
    for entry in entries {
        entry.waiting_on = list_current_waits_on_connection(connection, &entry.job.id)?
            .into_iter()
            .map(|wait| wait.wait_edge)
            .collect();
    }
    Ok(())
}

fn error_projection_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobErrorProjection> {
    Ok(JobErrorProjection {
        fingerprint: row.get(0)?,
        message: row.get(1)?,
        first_seen: row.get(2)?,
        last_seen: row.get(3)?,
        occurrence_count: row.get::<_, i64>(4).ok().and_then(i64_to_u64).unwrap_or(0),
        sample_service: row.get(5)?,
        sample_job_type: row.get(6)?,
        sample_state: row.get(7)?,
    })
}

#[derive(Debug, Clone)]
struct SearchFtsRow {
    rowid: i64,
    service: String,
    job_type: String,
    id: String,
    state: String,
    request_id: Option<String>,
    trace_id: Option<String>,
    traceparent: Option<String>,
    concurrency_key: Option<String>,
    queue_policy_reason: Option<String>,
    progress_text: Option<String>,
    log_text: Option<String>,
    error_text: Option<String>,
}

fn fts_row_for_job(
    connection: &Connection,
    service: &str,
    job_type: &str,
    id: &str,
) -> Result<Option<SearchFtsRow>, SqliteJobsStoreError> {
    connection
        .query_row(
            fts_row_select_sql("WHERE j.service = ?1 AND j.job_type = ?2 AND j.id = ?3"),
            params![service, job_type, id],
            fts_row_from_query_row,
        )
        .optional()
        .map_err(SqliteJobsStoreError::from)
}

fn fts_row_select_sql(where_sql: &str) -> &'static str {
    match where_sql {
        "" => {
            r#"
            SELECT j.rowid, j.service, j.job_type, j.id, j.state, j.request_id, j.trace_id,
                   j.traceparent, m.concurrency_key, m.queue_policy_reason, j.job_json
            FROM jobs_projection j
            LEFT JOIN jobs_metadata_projection m
              ON m.service = j.service AND m.job_type = j.job_type AND m.id = j.id
            "#
        }
        _ => {
            r#"
            SELECT j.rowid, j.service, j.job_type, j.id, j.state, j.request_id, j.trace_id,
                   j.traceparent, m.concurrency_key, m.queue_policy_reason, j.job_json
            FROM jobs_projection j
            LEFT JOIN jobs_metadata_projection m
              ON m.service = j.service AND m.job_type = j.job_type AND m.id = j.id
            WHERE j.service = ?1 AND j.job_type = ?2 AND j.id = ?3
            "#
        }
    }
}

fn fts_row_from_query_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SearchFtsRow> {
    let job_json = row.get::<_, String>(10)?;
    let job = serde_json::from_str::<Job>(&job_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(SearchFtsRow {
        rowid: row.get(0)?,
        service: row.get(1)?,
        job_type: row.get(2)?,
        id: row.get(3)?,
        state: row.get(4)?,
        request_id: row.get(5)?,
        trace_id: row.get(6)?,
        traceparent: row.get(7)?,
        concurrency_key: row.get(8)?,
        queue_policy_reason: row.get(9)?,
        progress_text: job.progress.as_ref().map(|progress| {
            [progress.step.as_deref(), progress.message.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
        }),
        log_text: job.logs.as_ref().map(|logs| {
            logs.iter()
                .map(|log| format!("{:?} {}", log.level, log.message))
                .collect::<Vec<_>>()
                .join(" ")
        }),
        error_text: job.last_error,
    })
}

fn replace_fts_row(
    connection: &Connection,
    old: Option<&SearchFtsRow>,
    new: Option<&SearchFtsRow>,
) -> Result<(), SqliteJobsStoreError> {
    if let Some(old) = old {
        connection.execute(
            r#"
            INSERT INTO jobs_search_fts(jobs_search_fts, rowid, service, job_type, id, state,
                request_id, trace_id, traceparent, concurrency_key, queue_policy_reason,
                progress_text, log_text, error_text)
            VALUES('delete', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            "#,
            fts_params(old),
        )?;
    }
    if let Some(new) = new {
        insert_fts_row(connection, new)?;
    }
    Ok(())
}

fn insert_fts_row(connection: &Connection, row: &SearchFtsRow) -> Result<(), SqliteJobsStoreError> {
    connection.execute(
        r#"
        INSERT INTO jobs_search_fts(rowid, service, job_type, id, state, request_id, trace_id,
            traceparent, concurrency_key, queue_policy_reason, progress_text, log_text, error_text)
        VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        "#,
        fts_params(row),
    )?;
    Ok(())
}

fn fts_params(row: &SearchFtsRow) -> [SqlValue; 13] {
    [
        SqlValue::Integer(row.rowid),
        SqlValue::Text(row.service.clone()),
        SqlValue::Text(row.job_type.clone()),
        SqlValue::Text(row.id.clone()),
        SqlValue::Text(row.state.clone()),
        option_text_value(&row.request_id),
        option_text_value(&row.trace_id),
        option_text_value(&row.traceparent),
        option_text_value(&row.concurrency_key),
        option_text_value(&row.queue_policy_reason),
        option_text_value(&row.progress_text),
        option_text_value(&row.log_text),
        option_text_value(&row.error_text),
    ]
}

fn option_text_value(value: &Option<String>) -> SqlValue {
    value
        .as_ref()
        .map_or(SqlValue::Null, |value| SqlValue::Text(value.clone()))
}

fn metadata_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobProjectionMetadata> {
    let key = row.get::<_, Option<String>>(0)?;
    let key_hash = row.get::<_, Option<String>>(1)?;
    let concurrency = match (key, key_hash) {
        (Some(key), Some(key_hash)) => Some(JobConcurrencyMetadata {
            key,
            key_hash,
            instance_id: row.get(2)?,
            heartbeat_at: row.get(3)?,
            lease_expires_at: row.get(4)?,
            stale_takeover_count: row.get::<_, Option<i64>>(5)?.and_then(i64_to_u64),
        }),
        _ => None,
    };
    let queue_policy = row
        .get::<_, Option<String>>(6)?
        .map(|outcome| JobQueuePolicyMetadata {
            outcome,
            reason: row.get::<_, Option<String>>(7).ok().flatten(),
            existing_job_id: row.get::<_, Option<String>>(8).ok().flatten(),
            replaced_job_id: row.get::<_, Option<String>>(9).ok().flatten(),
        });
    Ok(JobProjectionMetadata {
        concurrency,
        queue_policy,
    })
}

fn merge_concurrency_metadata(
    existing: Option<JobConcurrencyMetadata>,
    patch: Option<JobConcurrencyMetadata>,
) -> Option<JobConcurrencyMetadata> {
    match (existing, patch) {
        (None, None) => None,
        (Some(existing), None) => Some(existing),
        (None, Some(patch)) => Some(patch),
        (Some(existing), Some(patch)) => Some(JobConcurrencyMetadata {
            key: patch.key,
            key_hash: patch.key_hash,
            instance_id: patch.instance_id.or(existing.instance_id),
            heartbeat_at: patch.heartbeat_at.or(existing.heartbeat_at),
            lease_expires_at: patch.lease_expires_at.or(existing.lease_expires_at),
            stale_takeover_count: patch.stale_takeover_count.or(existing.stale_takeover_count),
        }),
    }
}

fn merge_queue_policy_metadata(
    existing: Option<JobQueuePolicyMetadata>,
    patch: Option<JobQueuePolicyMetadata>,
) -> Option<JobQueuePolicyMetadata> {
    match (existing, patch) {
        (None, None) => None,
        (Some(existing), None) => Some(existing),
        (None, Some(patch)) => Some(patch),
        (Some(existing), Some(patch)) => Some(JobQueuePolicyMetadata {
            outcome: patch.outcome,
            reason: patch.reason.or(existing.reason),
            existing_job_id: patch.existing_job_id.or(existing.existing_job_id),
            replaced_job_id: patch.replaced_job_id.or(existing.replaced_job_id),
        }),
    }
}

fn i64_to_u64(value: i64) -> Option<u64> {
    u64::try_from(value).ok()
}

fn runtime_ms(
    started_at_nanos: Option<i64>,
    completed_at_nanos: Option<i64>,
    updated_at_nanos: i64,
) -> Option<i64> {
    let started = started_at_nanos?;
    let ended = completed_at_nanos.unwrap_or(updated_at_nanos);
    ended
        .checked_sub(started)
        .map(|nanos| nanos.max(0) / 1_000_000)
}

fn queue_age_anchor_nanos(job: &Job, created_at_nanos: i64) -> Option<i64> {
    match job.state {
        JobState::Pending | JobState::Retry => Some(created_at_nanos),
        _ => None,
    }
}

fn decode_job_json(json: &str) -> Result<Job, SqliteJobsStoreError> {
    serde_json::from_str(json).map_err(|error| SqliteJobsStoreError::DecodeJson {
        model: "job",
        details: error.to_string(),
    })
}

const MAX_FTS_SEARCH_CHARS: usize = 512;
const RUNTIME_BAND_SQL: &str = "CASE WHEN j.state IN ('pending', 'retry') THEN 'queued' WHEN j.state = 'active' THEN CASE WHEN j.runtime_ms >= 60000 THEN 'slow' ELSE 'running' END ELSE 'terminal' END";

fn workbench_from_where_clause(
    filter: &JobsWorkbenchFilter,
) -> Result<(String, String, Vec<SqlValue>), SqliteJobsStoreError> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    let from_sql = "FROM jobs_projection j LEFT JOIN jobs_metadata_projection m ON m.service = j.service AND m.job_type = j.job_type AND m.id = j.id LEFT JOIN jobs_lineage_projection l ON l.service = j.service AND l.job_type = j.job_type AND l.id = j.id".to_string();

    if let Some(service) = &filter.service {
        clauses.push("j.service = ?".to_string());
        params.push(SqlValue::Text(service.clone()));
    }
    if let Some(job_type) = &filter.job_type {
        clauses.push("j.job_type = ?".to_string());
        params.push(SqlValue::Text(job_type.clone()));
    }
    if let Some(states) = &filter.states {
        if states.is_empty() {
            clauses.push("1 = 0".to_string());
        } else {
            clauses.push(format!(
                "j.state IN ({})",
                vec!["?"; states.len()].join(", ")
            ));
            params.extend(
                states
                    .iter()
                    .map(|state| SqlValue::Text(job_state_token(*state).to_string())),
            );
        }
    }
    if let Some(since) = filter.since {
        clauses.push("j.updated_at_nanos >= ?".to_string());
        params.push(SqlValue::Integer(timestamp_nanos(since)));
    }
    if let Some(search) = sanitize_fts_query(filter.search.as_deref())? {
        clauses.push(
            "j.rowid IN (SELECT rowid FROM jobs_search_fts WHERE jobs_search_fts MATCH ?)"
                .to_string(),
        );
        params.push(SqlValue::Text(search));
    }
    if let Some(queue_key) = &filter.queue_key {
        clauses.push("COALESCE(m.concurrency_key, 'unkeyed') = ?".to_string());
        params.push(SqlValue::Text(queue_key.clone()));
    }
    if let Some(runtime_band) = &filter.runtime_band {
        clauses.push(format!("{RUNTIME_BAND_SQL} = ?"));
        params.push(SqlValue::Text(runtime_band.clone()));
    }
    if let Some(trigger) = &filter.trigger {
        clauses.push("COALESCE(l.trigger_kind, 'unknown') = ?".to_string());
        params.push(SqlValue::Text(trigger.clone()));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok((from_sql, where_sql, params))
}

fn sanitize_fts_query(value: Option<&str>) -> Result<Option<String>, SqliteJobsStoreError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > MAX_FTS_SEARCH_CHARS {
        return Err(SqliteJobsStoreError::Validation {
            field: "search",
            details: format!("must be at most {MAX_FTS_SEARCH_CHARS} characters"),
        });
    }
    let terms = value
        .split_whitespace()
        .filter_map(|term| {
            let term = term.replace('"', "");
            (!term.is_empty()).then(|| format!("\"{term}\""))
        })
        .collect::<Vec<_>>();
    if terms.is_empty() {
        Ok(None)
    } else {
        Ok(Some(terms.join(" AND ")))
    }
}

fn workbench_order_sql(sort: JobsWorkbenchSort) -> &'static str {
    match (sort.field, sort.descending) {
        (JobsWorkbenchSortField::QueueAge, false) => {
            "CASE WHEN j.state IN ('pending', 'retry') THEN j.created_at_nanos END ASC, j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC"
        }
        (JobsWorkbenchSortField::QueueAge, true) => {
            "CASE WHEN j.state IN ('pending', 'retry') THEN j.created_at_nanos END DESC, j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC"
        }
        (JobsWorkbenchSortField::Runtime, false) => {
            "j.runtime_ms ASC, j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC"
        }
        (JobsWorkbenchSortField::Runtime, true) => {
            "j.runtime_ms DESC, j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC"
        }
        (JobsWorkbenchSortField::Retries, false) => {
            "json_extract(j.job_json, '$.tries') ASC, j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC"
        }
        (JobsWorkbenchSortField::Retries, true) => {
            "json_extract(j.job_json, '$.tries') DESC, j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC"
        }
        (_, false) => "j.updated_at_nanos ASC, j.service ASC, j.job_type ASC, j.id ASC",
        (_, true) => "j.updated_at_nanos DESC, j.service ASC, j.job_type ASC, j.id ASC",
    }
}

fn workbench_cursor(
    entry: &JobsWorkbenchEntry,
    sort: JobsWorkbenchSort,
) -> (Option<i64>, i64, String, String, String) {
    let updated_at = timestamp_str_nanos(&entry.job.updated_at);
    let primary = match sort.field {
        JobsWorkbenchSortField::QueueAge => entry.queue_age_anchor_nanos,
        JobsWorkbenchSortField::Runtime => entry.runtime_ms,
        JobsWorkbenchSortField::Retries => Some(i64::try_from(entry.job.tries).unwrap_or(i64::MAX)),
        _ => Some(updated_at),
    };
    (
        primary,
        updated_at,
        entry.job.service.clone(),
        entry.job.job_type.clone(),
        entry.job.id.clone(),
    )
}

fn workbench_after_predicate(
    sort: JobsWorkbenchSort,
    primary: Option<i64>,
    updated_at: i64,
    service: &str,
    job_type: &str,
    id: &str,
) -> (String, Vec<SqlValue>) {
    let field = match sort.field {
        JobsWorkbenchSortField::QueueAge => {
            "CASE WHEN j.state IN ('pending', 'retry') THEN j.created_at_nanos END"
        }
        JobsWorkbenchSortField::Runtime => "j.runtime_ms",
        JobsWorkbenchSortField::Retries => "json_extract(j.job_json, '$.tries')",
        _ => "j.updated_at_nanos",
    };
    let tie = "(j.updated_at_nanos < ? OR (j.updated_at_nanos = ? AND (j.service > ? OR (j.service = ? AND (j.job_type > ? OR (j.job_type = ? AND j.id > ?))))))";
    let mut params = Vec::new();
    let predicate = match (primary, sort.descending) {
        (Some(primary), false) => {
            params.extend([SqlValue::Integer(primary), SqlValue::Integer(primary)]);
            format!("({field} > ? OR ({field} = ? AND {tie}))")
        }
        (Some(primary), true) => {
            params.extend([SqlValue::Integer(primary), SqlValue::Integer(primary)]);
            format!("({field} < ? OR {field} IS NULL OR ({field} = ? AND {tie}))")
        }
        (None, false) => format!("({field} IS NOT NULL OR ({field} IS NULL AND {tie}))"),
        (None, true) => format!("({field} IS NULL AND {tie})"),
    };
    params.extend([
        SqlValue::Integer(updated_at),
        SqlValue::Integer(updated_at),
        SqlValue::Text(service.to_owned()),
        SqlValue::Text(service.to_owned()),
        SqlValue::Text(job_type.to_owned()),
        SqlValue::Text(job_type.to_owned()),
        SqlValue::Text(id.to_owned()),
    ]);
    (predicate, params)
}

fn workbench_group_sql(
    group_by: JobsWorkbenchGroupBy,
) -> (&'static str, &'static str, &'static str) {
    match group_by {
        JobsWorkbenchGroupBy::Service => ("j.service", "j.service", "NULL"),
        JobsWorkbenchGroupBy::Type => ("j.job_type", "j.job_type", "NULL"),
        JobsWorkbenchGroupBy::State => ("j.state", "j.state", "j.state"),
        JobsWorkbenchGroupBy::QueueKey => (
            "COALESCE(m.concurrency_key, 'unkeyed')",
            "COALESCE(m.concurrency_key, 'unkeyed')",
            "NULL",
        ),
        JobsWorkbenchGroupBy::RuntimeBand => (RUNTIME_BAND_SQL, RUNTIME_BAND_SQL, "NULL"),
        JobsWorkbenchGroupBy::Trigger => (
            "COALESCE(l.trigger_kind, 'unknown')",
            "COALESCE(l.trigger_kind, 'unknown')",
            "NULL",
        ),
    }
}

fn metrics_jobs_where_clause(
    filter: &JobsMetricsFilter,
    time_column: Option<&str>,
) -> Result<(String, String, Vec<SqlValue>), SqliteJobsStoreError> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    let from_sql = "FROM jobs_projection j LEFT JOIN jobs_metadata_projection m ON m.service = j.service AND m.job_type = j.job_type AND m.id = j.id LEFT JOIN jobs_lineage_projection l ON l.service = j.service AND l.job_type = j.job_type AND l.id = j.id".to_string();

    if let Some(service) = &filter.service {
        clauses.push("j.service = ?".to_string());
        params.push(SqlValue::Text(service.clone()));
    }
    if let Some(job_type) = &filter.job_type {
        clauses.push("j.job_type = ?".to_string());
        params.push(SqlValue::Text(job_type.clone()));
    }
    if let Some(states) = &filter.states {
        if states.is_empty() {
            clauses.push("1 = 0".to_string());
        } else {
            clauses.push(format!(
                "j.state IN ({})",
                vec!["?"; states.len()].join(", ")
            ));
            params.extend(
                states
                    .iter()
                    .map(|state| SqlValue::Text(job_state_token(*state).to_string())),
            );
        }
    }
    if let Some(queue_key) = &filter.queue_key {
        clauses.push("COALESCE(m.concurrency_key, 'unkeyed') = ?".to_string());
        params.push(SqlValue::Text(queue_key.clone()));
    }
    if let Some(trigger) = &filter.trigger {
        clauses.push("COALESCE(l.trigger_kind, 'unknown') = ?".to_string());
        params.push(SqlValue::Text(trigger.clone()));
    }
    if let Some(time_column) = time_column {
        clauses.push(format!("{time_column} >= ? AND {time_column} < ?"));
        params.push(SqlValue::Integer(timestamp_nanos(filter.since)));
        params.push(SqlValue::Integer(timestamp_nanos(filter.until)));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    Ok((from_sql, where_sql, params))
}

fn query_group_latency(
    connection: &Connection,
    filter: &JobsMetricsFilter,
    group_key: &str,
    value_sql: &str,
    time_column: &str,
) -> Result<JobsMetricsLatency, SqliteJobsStoreError> {
    let (from_sql, mut where_sql, mut params) =
        metrics_jobs_where_clause(filter, Some(time_column))?;
    let (key_sql, _, _) = workbench_group_sql(filter.group_by);
    if where_sql.is_empty() {
        where_sql = format!(" WHERE {key_sql} = ? AND {value_sql} IS NOT NULL");
    } else {
        where_sql.push_str(&format!(" AND {key_sql} = ? AND {value_sql} IS NOT NULL"));
    }
    params.push(SqlValue::Text(group_key.to_string()));
    let sql = format!("SELECT {value_sql} {from_sql}{where_sql} ORDER BY {value_sql} ASC");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), |row| row.get::<_, i64>(0))?;
    let mut values = Vec::new();
    for row in rows {
        if let Some(value) = i64_to_u64(row?) {
            values.push(value);
        }
    }
    Ok(latency_stats(values))
}

fn collect_event_counts(
    connection: &Connection,
    filter: &JobsMetricsFilter,
    buckets: &mut BTreeMap<i64, BTreeMap<String, JobsMetricsBucketGroup>>,
) -> Result<(), SqliteJobsStoreError> {
    let (from_sql, where_sql, params) = metrics_jobs_where_clause(filter, None)?;
    let (key_sql, label_sql, _) = workbench_group_sql(filter.group_by);
    let time_bucket_sql = bucket_sql(
        "e.timestamp_nanos",
        timestamp_nanos(filter.since),
        filter.step_nanos,
    );
    let event_from_sql = from_sql.replacen("FROM jobs_projection j", "FROM jobs_events_projection e JOIN jobs_projection j ON j.service = e.service AND j.job_type = e.job_type AND j.id = e.id", 1);
    let mut clauses = Vec::new();
    if !where_sql.is_empty() {
        clauses.push(where_sql.trim_start_matches(" WHERE ").to_string());
    }
    clauses.push("e.timestamp_nanos >= ?".to_string());
    clauses.push("e.timestamp_nanos < ?".to_string());
    let mut params = params;
    params.push(SqlValue::Integer(timestamp_nanos(filter.since)));
    params.push(SqlValue::Integer(timestamp_nanos(filter.until)));
    let sql = format!(
        "SELECT {time_bucket_sql} AS bucket, {key_sql} AS key, {label_sql} AS label, e.event_type, COUNT(*) \
         {event_from_sql} WHERE {} GROUP BY bucket, key, e.event_type",
        clauses.join(" AND ")
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (bucket, key, label, event_type, count) = row?;
        let count = i64_to_u64(count).unwrap_or(0);
        let group = bucket_group(buckets, bucket, key, label);
        match event_type.as_str() {
            "created" => group.submitted = group.submitted.saturating_add(count),
            "started" => group.started = group.started.saturating_add(count),
            "completed" => group.completed = group.completed.saturating_add(count),
            "failed" => group.failed = group.failed.saturating_add(count),
            "retried" | "retry" => group.retried = group.retried.saturating_add(count),
            "dead" => group.dead = group.dead.saturating_add(count),
            "cancelled" | "expired" | "skipped" | "stale" => {
                group.cancelled = group.cancelled.saturating_add(count);
            }
            "dismissed" => group.dismissed = group.dismissed.saturating_add(count),
            _ => {}
        }
    }
    Ok(())
}

fn collect_latency_buckets(
    connection: &Connection,
    filter: &JobsMetricsFilter,
    buckets: &mut BTreeMap<i64, BTreeMap<String, JobsMetricsBucketGroup>>,
    target: &str,
    value_sql: &str,
    time_column: &str,
) -> Result<(), SqliteJobsStoreError> {
    let (from_sql, mut where_sql, params) = metrics_jobs_where_clause(filter, Some(time_column))?;
    let (key_sql, label_sql, _) = workbench_group_sql(filter.group_by);
    if where_sql.is_empty() {
        where_sql = format!(" WHERE {value_sql} IS NOT NULL");
    } else {
        where_sql.push_str(&format!(" AND {value_sql} IS NOT NULL"));
    }
    let sql = format!(
        "SELECT {} AS bucket, {key_sql} AS key, {label_sql} AS label, {value_sql} \
         {from_sql}{where_sql} ORDER BY bucket ASC, key ASC, {value_sql} ASC",
        bucket_sql(
            time_column,
            timestamp_nanos(filter.since),
            filter.step_nanos
        )
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut values = BTreeMap::<(i64, String), Vec<u64>>::new();
    let mut labels = BTreeMap::<(i64, String), String>::new();
    for row in rows {
        let (bucket, key, label, value) = row?;
        if let Some(value) = i64_to_u64(value) {
            values.entry((bucket, key.clone())).or_default().push(value);
            labels.insert((bucket, key), label);
        }
    }
    for ((bucket, key), values) in values {
        let label = labels
            .remove(&(bucket, key.clone()))
            .unwrap_or_else(|| key.clone());
        let group = bucket_group(buckets, bucket, key, label);
        let stats = latency_stats(values);
        if target == "runtime" {
            group.runtime = stats;
        } else {
            group.queue_wait = stats;
        }
    }
    Ok(())
}

fn bucket_group(
    buckets: &mut BTreeMap<i64, BTreeMap<String, JobsMetricsBucketGroup>>,
    bucket: i64,
    key: String,
    label: String,
) -> &mut JobsMetricsBucketGroup {
    buckets
        .entry(bucket)
        .or_default()
        .entry(key.clone())
        .or_insert_with(|| JobsMetricsBucketGroup {
            key,
            label,
            ..JobsMetricsBucketGroup::default()
        })
}

fn bucket_sql(column: &str, since_nanos: i64, step_nanos: i64) -> String {
    format!("((({column} - {since_nanos}) / {step_nanos}) * {step_nanos} + {since_nanos})")
}

fn latency_stats(mut values: Vec<u64>) -> JobsMetricsLatency {
    values.sort_unstable();
    if values.is_empty() {
        return JobsMetricsLatency::default();
    }
    JobsMetricsLatency {
        count: u64::try_from(values.len()).unwrap_or(u64::MAX),
        p50_ms: percentile(&values, 50),
        p95_ms: percentile(&values, 95),
        max_ms: values.last().copied(),
    }
}

fn percentile(values: &[u64], pct: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let index = ((values.len() - 1) * pct) / 100;
    values.get(index).copied()
}

fn add_optional_count(target: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *target = Some(target.unwrap_or(0).saturating_add(value));
    }
}

fn timestamp_nanos_to_string(nanos: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn query_workbench_stats(
    connection: &Connection,
    from_sql: &str,
    where_sql: &str,
    query_params: &[SqlValue],
) -> Result<JobsWorkbenchStats, SqliteJobsStoreError> {
    let sql = format!("SELECT j.state, COUNT(*) {from_sql}{where_sql} GROUP BY j.state");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(query_params.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut stats = JobsWorkbenchStats::default();
    for row in rows {
        let (state, count) = row?;
        let count = i64_to_u64(count).unwrap_or(0);
        stats.total = stats.total.saturating_add(count);
        stats.by_state.insert(state.clone(), count);
        match state.as_str() {
            "pending" | "retry" => stats.queued = Some(stats.queued.unwrap_or(0) + count),
            "active" => stats.running = Some(stats.running.unwrap_or(0) + count),
            "failed" => stats.failed = Some(stats.failed.unwrap_or(0) + count),
            "dead" => stats.dead = Some(stats.dead.unwrap_or(0) + count),
            _ => {}
        }
    }
    let slow_sql = format!(
        "SELECT COUNT(*) {from_sql}{where_sql} AND j.state = 'active' AND j.runtime_ms >= 60000"
    );
    let slow_sql = if where_sql.is_empty() {
        slow_sql.replace(" AND j.state", " WHERE j.state")
    } else {
        slow_sql
    };
    let slow = connection.query_row(&slow_sql, params_from_iter(query_params.iter()), |row| {
        row.get::<_, i64>(0)
    })?;
    stats.slow = i64_to_u64(slow).filter(|count| *count > 0);
    Ok(stats)
}

fn list_jobs_where_clause(filter: &ListJobsFilter) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();

    if let Some(service) = &filter.service {
        clauses.push("service = ?".to_string());
        params.push(SqlValue::Text(service.clone()));
    }
    if let Some(job_type) = &filter.job_type {
        clauses.push("job_type = ?".to_string());
        params.push(SqlValue::Text(job_type.clone()));
    }
    if let Some(states) = &filter.states {
        if states.is_empty() {
            clauses.push("1 = 0".to_string());
        } else {
            let placeholders = vec!["?"; states.len()].join(", ");
            clauses.push(format!("state IN ({placeholders})"));
            params.extend(
                states
                    .iter()
                    .map(|state| SqlValue::Text(job_state_token(*state).to_string())),
            );
        }
    }
    if let Some(since) = filter.since {
        clauses.push("updated_at_nanos >= ?".to_string());
        params.push(SqlValue::Integer(timestamp_nanos(since)));
    }

    if clauses.is_empty() {
        (String::new(), params)
    } else {
        (format!(" WHERE {}", clauses.join(" AND ")), params)
    }
}

fn sql_u64(value: u64) -> SqlValue {
    SqlValue::Integer(i64::try_from(value).unwrap_or(i64::MAX))
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

fn wait_target_kind_token(kind: JobWaitTargetKind) -> &'static str {
    match kind {
        JobWaitTargetKind::Job => "job",
        JobWaitTargetKind::Operation => "operation",
        JobWaitTargetKind::External => "external",
    }
}

fn parse_timestamp(timestamp: &str) -> OffsetDateTime {
    OffsetDateTime::parse(timestamp, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn timestamp_str_nanos(timestamp: &str) -> i64 {
    timestamp_nanos(parse_timestamp(timestamp))
}

fn timestamp_nanos(timestamp: OffsetDateTime) -> i64 {
    i64::try_from(timestamp.unix_timestamp_nanos()).unwrap_or_else(|_| {
        if timestamp < OffsetDateTime::UNIX_EPOCH {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trellis_rs::jobs::types::{
        JobContext, JobEvent, JobEventType, JobLineage, JobLogEntry, JobLogLevel, JobWaitEdge,
        JobWaitTarget, JobWaitTargetKind,
    };

    use super::*;

    fn job(id: &str, service: &str, job_type: &str, updated_at: &str, state: JobState) -> Job {
        Job {
            id: id.to_string(),
            context: context(id),
            service: service.to_string(),
            job_type: job_type.to_string(),
            state,
            payload: json!({ "id": id }),
            result: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            started_at: None,
            completed_at: None,
            tries: 0,
            max_tries: 3,
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

    fn set_trace(job: &mut Job, trace_id: &str) {
        job.context.trace_id = trace_id.to_string();
        job.context.traceparent = format!("00-{trace_id}-0123456789abcdef-01");
    }

    fn event(job: &Job, event_type: JobEventType, state: JobState, timestamp: &str) -> JobEvent {
        JobEvent {
            job_id: job.id.clone(),
            context: job.context.clone(),
            service: job.service.clone(),
            job_type: job.job_type.clone(),
            event_type,
            state,
            previous_state: None,
            tries: job.tries,
            max_tries: Some(job.max_tries),
            error: job.last_error.clone(),
            error_detail: job.error_detail.clone(),
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
            timestamp: timestamp.to_string(),
        }
    }

    fn wait_edge(id: &str, target_job_id: &str) -> JobWaitEdge {
        JobWaitEdge {
            id: id.to_string(),
            target: JobWaitTarget {
                kind: JobWaitTargetKind::Job,
                id: Some(target_job_id.to_string()),
                operation_id: None,
                label: None,
                service: Some("svc".to_string()),
                target_type: Some("sync".to_string()),
                system: None,
                operation: None,
                key: None,
            },
            started_at: "2026-01-01T00:01:00Z".to_string(),
            label: Some("child job".to_string()),
        }
    }

    fn worker(
        service: &str,
        job_type: &str,
        instance_id: &str,
        heartbeat_at: &str,
    ) -> WorkerPresenceRecord {
        WorkerPresenceRecord {
            service: service.to_string(),
            job_type: job_type.to_string(),
            instance_id: instance_id.to_string(),
            concurrency: Some(2),
            version: Some("1.0.0".to_string()),
            heartbeat_at: heartbeat_at.to_string(),
        }
    }

    fn temp_db_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "trellis-jobs-runtime-{name}-{}-{}.sqlite3",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn insert_raw_projected_job_json(
        store: &SqliteJobsStore,
        service: &str,
        job_type: &str,
        id: &str,
        state: JobState,
        updated_at: &str,
        job_json: &str,
    ) -> Result<(), SqliteJobsStoreError> {
        let connection = store
            .connection
            .lock()
            .map_err(|_| SqliteJobsStoreError::Poisoned)?;
        connection.execute(
            r#"
            INSERT INTO jobs_projection
                (service, job_type, id, state, created_at, updated_at, updated_at_nanos, deadline, deadline_nanos, payload_json, job_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9)
            "#,
            params![
                service,
                job_type,
                id,
                job_state_token(state),
                "2026-01-01T00:00:00Z",
                updated_at,
                timestamp_str_nanos(updated_at),
                "{}",
                job_json,
            ],
        )?;
        Ok(())
    }

    #[test]
    fn schema_init_is_idempotent() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        store
            .initialize_schema()
            .expect("schema init should be idempotent");
        let connection = store.connection.lock().expect("connection lock");
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('jobs_projection', 'jobs_events_projection', 'projection_metadata')",
                [],
                |row| row.get(0),
            )
            .expect("schema tables");
        assert_eq!(tables, 3);
    }

    #[test]
    fn projection_id_is_stable_for_one_database() {
        let path = temp_db_path("projection-id");
        let _ = std::fs::remove_file(&path);
        let first = SqliteJobsStore::open(&path).expect("store should open");
        let first_id = first.projection_id().expect("projection id should exist");
        drop(first);

        let second = SqliteJobsStore::open(&path).expect("store should reopen");
        let second_id = second
            .projection_id()
            .expect("projection id should persist");

        assert_eq!(first_id, second_id);
        assert_eq!(first_id.len(), 32);
        assert!(first_id.chars().all(|value| value.is_ascii_hexdigit()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn upsert_and_get_job() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut projected = job(
            "job-1",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Pending,
        );
        store.upsert_job(&projected).expect("insert should succeed");
        projected.state = JobState::Active;
        projected.updated_at = "2026-01-01T00:01:00Z".to_string();
        store.upsert_job(&projected).expect("update should succeed");

        let fetched = store
            .get_job("svc", "import", "job-1")
            .expect("get should succeed")
            .expect("job should exist");
        assert_eq!(fetched.state, JobState::Active);
        assert_eq!(fetched.updated_at, "2026-01-01T00:01:00Z");
    }

    #[test]
    fn list_jobs_applies_filters() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        for projected in [
            job(
                "a",
                "svc",
                "import",
                "2026-01-01T00:01:00Z",
                JobState::Pending,
            ),
            job("b", "svc", "export", "2026-01-01T00:02:00Z", JobState::Dead),
            job(
                "c",
                "other",
                "import",
                "2026-01-01T00:03:00Z",
                JobState::Pending,
            ),
        ] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let page = store
            .list_jobs(&ListJobsFilter {
                service: Some("svc".to_string()),
                states: Some(vec![JobState::Pending]),
                since: Some(parse_timestamp("2026-01-01T00:00:30Z")),
                ..Default::default()
            })
            .expect("list should succeed");
        assert_eq!(
            page.jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"]
        );
    }

    #[test]
    fn list_jobs_filters_in_sql_before_decoding_job_json() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        store
            .upsert_job(&job(
                "valid",
                "svc",
                "import",
                "2026-01-01T00:01:00Z",
                JobState::Pending,
            ))
            .expect("valid job should insert");
        insert_raw_projected_job_json(
            &store,
            "other",
            "import",
            "invalid-json",
            JobState::Pending,
            "2026-01-01T00:02:00Z",
            "not valid json",
        )
        .expect("raw invalid row should insert");

        let filtered = store
            .list_jobs(&ListJobsFilter {
                service: Some("svc".to_string()),
                ..Default::default()
            })
            .expect("filtered list should not decode unrelated rows");

        assert_eq!(
            filtered
                .jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["valid"]
        );
        assert_eq!(filtered.count, 1);
        assert!(matches!(
            store.list_jobs(&ListJobsFilter::default()),
            Err(SqliteJobsStoreError::DecodeJson { .. })
        ));
    }

    #[test]
    fn list_jobs_since_compares_equivalent_offset_timestamps() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        for projected in [
            job(
                "before",
                "svc",
                "import",
                "2026-01-01T00:00:29Z",
                JobState::Pending,
            ),
            job(
                "at-z",
                "svc",
                "import",
                "2026-01-01T00:00:30Z",
                JobState::Pending,
            ),
            job(
                "at-millis",
                "svc",
                "import",
                "2026-01-01T00:00:30.000Z",
                JobState::Pending,
            ),
            job(
                "at-offset",
                "svc",
                "import",
                "2025-12-31T19:00:30-05:00",
                JobState::Pending,
            ),
        ] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let page = store
            .list_jobs(&ListJobsFilter {
                since: Some(parse_timestamp("2025-12-31T19:00:30-05:00")),
                ..Default::default()
            })
            .expect("list should succeed");

        assert_eq!(
            page.jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["at-millis", "at-offset", "at-z"]
        );
    }

    #[test]
    fn list_jobs_order_by_compares_equivalent_timestamp_variants() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        for projected in [
            job(
                "same-z",
                "svc",
                "import",
                "2026-01-01T00:00:30Z",
                JobState::Pending,
            ),
            job(
                "same-millis",
                "svc",
                "import",
                "2026-01-01T00:00:30.000Z",
                JobState::Pending,
            ),
            job(
                "same-offset",
                "svc",
                "import",
                "2025-12-31T19:00:30-05:00",
                JobState::Pending,
            ),
            job(
                "latest",
                "svc",
                "import",
                "2026-01-01T00:00:31Z",
                JobState::Pending,
            ),
        ] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let page = store
            .list_jobs(&ListJobsFilter::default())
            .expect("list should succeed");

        assert_eq!(
            page.jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["latest", "same-millis", "same-offset", "same-z"]
        );
    }

    #[test]
    fn list_jobs_uses_mutation_safe_keyset_pagination() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        for projected in [
            job(
                "a",
                "svc",
                "import",
                "2026-01-03T00:00:00Z",
                JobState::Pending,
            ),
            job(
                "b",
                "svc",
                "import",
                "2026-01-02T00:00:00Z",
                JobState::Pending,
            ),
            job(
                "c",
                "svc",
                "import",
                "2026-01-01T00:00:00Z",
                JobState::Pending,
            ),
        ] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let first = store
            .list_jobs(&ListJobsFilter {
                limit: 2,
                ..Default::default()
            })
            .expect("first page should succeed");
        assert_eq!(
            first
                .jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(first.count, 3);
        assert_eq!(first.limit, 2);
        assert!(first.next_after.is_some());
        store
            .upsert_job(&job(
                "aa",
                "svc",
                "import",
                "2026-01-03T00:00:00Z",
                JobState::Pending,
            ))
            .expect("concurrent insert should succeed");
        let second = store
            .list_jobs(&ListJobsFilter {
                after: first.next_after,
                limit: 2,
                ..Default::default()
            })
            .expect("second page should succeed");
        assert_eq!(
            second
                .jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c"]
        );
        assert_eq!(second.count, 4);
        assert_eq!(second.limit, 2);
        assert_eq!(second.next_after, None);
    }

    #[test]
    fn workbench_keyset_uses_equal_sort_tie_breakers_across_mutation() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        for id in ["b", "c", "d"] {
            store
                .upsert_job(&job(
                    id,
                    "svc",
                    "type",
                    "2026-01-01T00:00:00Z",
                    JobState::Pending,
                ))
                .unwrap();
        }
        let mut filter = JobsWorkbenchFilter {
            service: None,
            job_type: None,
            states: None,
            since: None,
            search: None,
            queue_key: None,
            runtime_band: None,
            trigger: None,
            sort: JobsWorkbenchSort::default(),
            group_by: None,
            after: None,
            limit: 2,
        };
        let first = store.query_jobs(&filter).unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.job.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "c"]
        );
        store
            .upsert_job(&job(
                "a",
                "svc",
                "type",
                "2026-01-01T00:00:00Z",
                JobState::Pending,
            ))
            .unwrap();
        filter.after = first.next_after;
        let second = store.query_jobs(&filter).unwrap();
        assert_eq!(
            second
                .entries
                .iter()
                .map(|entry| entry.job.id.as_str())
                .collect::<Vec<_>>(),
            ["d"]
        );
    }

    #[test]
    fn query_jobs_sanitizes_fts_search() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut projected = job(
            "job-1",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Failed,
        );
        projected.last_error = Some("syntax near weird token".to_string());
        projected.logs = Some(vec![JobLogEntry {
            timestamp: projected.updated_at.clone(),
            level: JobLogLevel::Error,
            message: "operator-visible log text".to_string(),
        }]);
        store.upsert_job(&projected).expect("insert should succeed");

        let page = store
            .query_jobs(&JobsWorkbenchFilter {
                search: Some("weird\"".to_string()),
                sort: JobsWorkbenchSort::default(),
                after: None,
                limit: 10,
                service: None,
                job_type: None,
                states: None,
                since: None,
                queue_key: None,
                runtime_band: None,
                trigger: None,
                group_by: None,
            })
            .expect("sanitized search should not leak sqlite syntax errors");

        assert_eq!(page.count, 1);
        assert_eq!(page.entries[0].job.id, "job-1");
        store
            .query_jobs(&JobsWorkbenchFilter {
                search: Some("\"".to_string()),
                sort: JobsWorkbenchSort::default(),
                after: None,
                limit: 10,
                service: None,
                job_type: None,
                states: None,
                since: None,
                queue_key: None,
                runtime_band: None,
                trigger: None,
                group_by: None,
            })
            .expect("quote-only search should behave as absent");
        assert!(matches!(
            store.query_jobs(&JobsWorkbenchFilter {
                search: Some("x".repeat(MAX_FTS_SEARCH_CHARS + 1)),
                sort: JobsWorkbenchSort::default(),
                after: None,
                limit: 10,
                service: None,
                job_type: None,
                states: None,
                since: None,
                queue_key: None,
                runtime_band: None,
                trigger: None,
                group_by: None,
            }),
            Err(SqliteJobsStoreError::Validation {
                field: "search",
                ..
            })
        ));
    }

    #[test]
    fn wait_projection_tracks_current_wait_by_job_and_wait_id() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let current = job(
            "current",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Active,
        );
        let mut waiting = event(
            &current,
            JobEventType::Waiting,
            JobState::Active,
            "2026-01-01T00:01:00Z",
        );
        waiting.wait_edge = Some(wait_edge("wait-1", "child"));

        store
            .project_wait_edge(&waiting)
            .expect("wait should project");

        let waits = store
            .list_current_waits("current")
            .expect("waits should list");
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0].job_id, "current");
        assert_eq!(waits[0].wait_edge.id, "wait-1");
        assert_eq!(
            store
                .get_current_wait("current", "wait-1")
                .expect("wait should read")
                .expect("wait should exist")
                .wait_edge
                .target
                .id
                .as_deref(),
            Some("child")
        );

        let terminal = event(
            &current,
            JobEventType::Completed,
            JobState::Completed,
            "2026-01-01T00:02:00Z",
        );
        store
            .project_wait_edge(&terminal)
            .expect("terminal should clear waits");
        assert!(store
            .list_current_waits("current")
            .expect("waits should list")
            .is_empty());
    }

    #[test]
    fn query_jobs_groups_and_sorts() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut active = job(
            "active",
            "svc",
            "import",
            "2026-01-01T00:03:00Z",
            JobState::Active,
        );
        active.started_at = Some("2026-01-01T00:00:00Z".to_string());
        let mut failed = job(
            "failed",
            "svc",
            "export",
            "2026-01-01T00:02:00Z",
            JobState::Failed,
        );
        failed.last_error = Some("boom".to_string());
        let pending = job(
            "pending",
            "svc",
            "export",
            "2026-01-01T00:01:00Z",
            JobState::Pending,
        );
        for projected in [active, failed, pending] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let filter = JobsWorkbenchFilter {
            sort: JobsWorkbenchSort {
                field: JobsWorkbenchSortField::Runtime,
                descending: true,
            },
            group_by: Some(JobsWorkbenchGroupBy::State),
            after: None,
            limit: 10,
            service: None,
            job_type: None,
            states: None,
            since: None,
            search: None,
            queue_key: None,
            runtime_band: None,
            trigger: None,
        };
        let page = store.query_jobs(&filter).expect("query should succeed");
        let groups = store
            .query_job_groups(&filter)
            .expect("group query should succeed");

        assert_eq!(page.entries[0].job.id, "active");
        assert_eq!(page.entries[0].runtime_band.as_deref(), Some("slow"));
        assert_eq!(page.stats.by_state.get("failed"), Some(&1));
        assert_eq!(
            groups
                .iter()
                .map(|group| group.key.as_str())
                .collect::<Vec<_>>(),
            vec!["active", "failed", "pending"]
        );
    }

    #[test]
    fn query_metrics_groups_by_job_type_and_buckets_events() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut completed = job(
            "completed-import",
            "svc",
            "import",
            "2026-01-01T00:03:00Z",
            JobState::Completed,
        );
        completed.started_at = Some("2026-01-01T00:01:00Z".to_string());
        completed.completed_at = Some("2026-01-01T00:03:00Z".to_string());
        let mut failed = job(
            "failed-import",
            "svc",
            "import",
            "2026-01-01T00:04:00Z",
            JobState::Failed,
        );
        failed.started_at = Some("2026-01-01T00:02:00Z".to_string());
        failed.last_error = Some("boom".to_string());
        let queued = job(
            "queued-export",
            "svc",
            "export",
            "2026-01-01T00:05:00Z",
            JobState::Pending,
        );

        for job in [&completed, &failed, &queued] {
            store.upsert_job(job).expect("job should project");
        }
        store
            .project_timeline_event(
                &event(
                    &completed,
                    JobEventType::Created,
                    JobState::Pending,
                    "2026-01-01T00:00:10Z",
                ),
                &json!({}),
                None,
                None,
            )
            .expect("created event should project");
        store
            .project_timeline_event(
                &event(
                    &completed,
                    JobEventType::Started,
                    JobState::Active,
                    "2026-01-01T00:01:00Z",
                ),
                &json!({}),
                None,
                None,
            )
            .expect("started event should project");
        store
            .project_timeline_event(
                &event(
                    &failed,
                    JobEventType::Failed,
                    JobState::Failed,
                    "2026-01-01T00:04:00Z",
                ),
                &json!({}),
                None,
                None,
            )
            .expect("failed event should project");

        let metrics = store
            .query_metrics(&JobsMetricsFilter {
                service: None,
                job_type: None,
                states: None,
                since: OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339).unwrap(),
                until: OffsetDateTime::parse("2026-01-01T00:10:00Z", &Rfc3339).unwrap(),
                step_nanos: 5 * 60 * 1_000_000_000,
                queue_key: None,
                trigger: None,
                group_by: JobsWorkbenchGroupBy::Type,
            })
            .expect("metrics should query");

        let import = metrics
            .summary
            .iter()
            .find(|group| group.key == "import")
            .expect("import summary group");
        assert_eq!(import.total, 2);
        assert_eq!(import.failed, Some(1));
        assert_eq!(import.runtime.count, 2);
        assert_eq!(import.queue_wait.count, 2);

        let first_bucket_import = metrics.buckets[0]
            .groups
            .iter()
            .find(|group| group.key == "import")
            .expect("import bucket group");
        assert_eq!(first_bucket_import.submitted, 1);
        assert_eq!(first_bucket_import.started, 1);
        assert_eq!(first_bucket_import.failed, 1);
    }

    #[test]
    fn duplicate_global_id_is_rejected() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        store
            .upsert_job(&job(
                "job-1",
                "svc-a",
                "import",
                "2026-01-01T00:00:00Z",
                JobState::Pending,
            ))
            .expect("insert should succeed");
        let error = store
            .upsert_job(&job(
                "job-1",
                "svc-b",
                "import",
                "2026-01-01T00:00:00Z",
                JobState::Pending,
            ))
            .expect_err("duplicate global id should fail");
        assert!(matches!(error, SqliteJobsStoreError::Sqlite(_)));
    }

    #[test]
    fn deadline_scan_returns_expirable_jobs_only() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut expired = job(
            "expired",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Pending,
        );
        expired.deadline = Some("2026-01-01T00:01:00Z".to_string());
        let mut future = job(
            "future",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Pending,
        );
        future.deadline = Some("2026-01-01T00:03:00Z".to_string());
        let mut terminal = job(
            "done",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Completed,
        );
        terminal.deadline = Some("2026-01-01T00:01:00Z".to_string());
        for projected in [expired, future, terminal] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let jobs = store
            .scan_expired_jobs("2026-01-01T00:02:00Z")
            .expect("scan should succeed");
        assert_eq!(
            jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            vec!["expired"]
        );
    }

    #[test]
    fn deadline_scan_compares_equivalent_timestamp_variants() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut at_millis = job(
            "at-millis",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Pending,
        );
        at_millis.deadline = Some("2026-01-01T00:01:00.000Z".to_string());
        let mut at_offset = job(
            "at-offset",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Pending,
        );
        at_offset.deadline = Some("2025-12-31T19:01:00-05:00".to_string());
        let mut future_offset = job(
            "future-offset",
            "svc",
            "import",
            "2026-01-01T00:00:00Z",
            JobState::Pending,
        );
        future_offset.deadline = Some("2025-12-31T19:01:01-05:00".to_string());
        for projected in [at_millis, at_offset, future_offset] {
            store.upsert_job(&projected).expect("insert should succeed");
        }

        let jobs = store
            .scan_expired_jobs("2026-01-01T00:01:00Z")
            .expect("scan should succeed");
        assert_eq!(
            jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            vec!["at-millis", "at-offset"]
        );
    }

    #[test]
    fn projected_key_uses_job_states_and_metadata() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut active = job(
            "active",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Active,
        );
        active.started_at = Some("2026-01-01T00:01:00Z".to_string());
        let queued = job(
            "queued",
            "svc",
            "sync",
            "2026-01-01T00:01:00Z",
            JobState::Pending,
        );
        store.upsert_job(&active).expect("active job should insert");
        store.upsert_job(&queued).expect("queued job should insert");
        store
            .apply_job_metadata_patch(
                "svc",
                "sync",
                "active",
                "2026-01-01T00:02:00Z",
                &JobProjectionMetadataPatch {
                    concurrency: Some(JobConcurrencyMetadata {
                        key: "tenant-1".to_string(),
                        key_hash: "hash-1".to_string(),
                        instance_id: Some("worker-1".to_string()),
                        heartbeat_at: Some("2026-01-01T00:02:00Z".to_string()),
                        lease_expires_at: Some("2026-01-01T00:04:00Z".to_string()),
                        stale_takeover_count: Some(3),
                    }),
                    queue_policy: None,
                },
            )
            .expect("active metadata should insert");
        store
            .apply_job_metadata_patch(
                "svc",
                "sync",
                "queued",
                "2026-01-01T00:03:00Z",
                &JobProjectionMetadataPatch {
                    concurrency: Some(JobConcurrencyMetadata {
                        key: "tenant-1".to_string(),
                        key_hash: "hash-1".to_string(),
                        ..Default::default()
                    }),
                    queue_policy: Some(JobQueuePolicyMetadata {
                        outcome: "rejected".to_string(),
                        reason: Some("queue-depth".to_string()),
                        existing_job_id: None,
                        replaced_job_id: None,
                    }),
                },
            )
            .expect("queued metadata should insert");

        let key = store
            .get_projected_key("svc", "sync", "tenant-1")
            .expect("key query should succeed")
            .expect("key should exist");

        assert_eq!(key.key_hash, "hash-1");
        assert_eq!(key.stale_takeover_count, 3);
        assert_eq!(key.latest_policy_reason.as_deref(), Some("queue-depth"));
        assert_eq!(key.active.len(), 1);
        assert_eq!(key.active[0].job_id, "active");
        assert_eq!(key.active[0].instance_id.as_deref(), Some("worker-1"));
        assert_eq!(key.queued.len(), 1);
        assert_eq!(key.queued[0].job_id, "queued");
    }

    #[test]
    fn list_related_jobs_ignores_same_service_and_type_without_relation() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut current = job(
            "current",
            "svc",
            "sync",
            "2026-01-01T00:03:00Z",
            JobState::Pending,
        );
        let mut peer_one = job(
            "peer-one",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Pending,
        );
        let mut peer_two = job(
            "peer-two",
            "svc",
            "sync",
            "2026-01-01T00:01:00Z",
            JobState::Pending,
        );
        set_trace(&mut current, "00000000000000000000000000000001");
        set_trace(&mut peer_one, "00000000000000000000000000000002");
        set_trace(&mut peer_two, "00000000000000000000000000000003");
        store
            .upsert_job(&current)
            .expect("current job should insert");
        store.upsert_job(&peer_one).expect("peer one should insert");
        store.upsert_job(&peer_two).expect("peer two should insert");

        let related = store
            .list_related_jobs(&current, 10)
            .expect("related query should succeed");

        assert!(related.is_empty());
    }

    #[test]
    fn list_related_jobs_marks_trace_match() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut current = job(
            "current",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Pending,
        );
        let mut peer = job(
            "peer",
            "svc",
            "sync",
            "2026-01-01T00:01:00Z",
            JobState::Pending,
        );
        set_trace(&mut current, "0000000000000000000000000000000a");
        set_trace(&mut peer, "0000000000000000000000000000000a");
        store
            .upsert_job(&current)
            .expect("current job should insert");
        store.upsert_job(&peer).expect("peer job should insert");

        let related = store
            .list_related_jobs(&current, 10)
            .expect("related query should succeed");

        assert_eq!(related.len(), 1);
        assert_eq!(related[0].job.id, "peer");
        assert_eq!(related[0].matched_by.as_deref(), Some("trace"));
    }

    #[test]
    fn list_related_jobs_prefers_trace_over_parent_match() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut current = job(
            "current",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Pending,
        );
        let mut peer = job(
            "peer",
            "svc",
            "sync",
            "2026-01-01T00:01:00Z",
            JobState::Pending,
        );
        set_trace(&mut current, "0000000000000000000000000000000b");
        set_trace(&mut peer, "0000000000000000000000000000000b");
        current.lineage = Some(JobLineage {
            parent_job_id: Some("parent".to_string()),
            root_job_id: None,
            operation_id: None,
            related_keys: None,
        });
        peer.lineage = Some(JobLineage {
            parent_job_id: Some("parent".to_string()),
            root_job_id: None,
            operation_id: None,
            related_keys: None,
        });
        store
            .upsert_job(&current)
            .expect("current job should insert");
        store.upsert_job(&peer).expect("peer job should insert");
        store
            .upsert_job_lineage(&current)
            .expect("current lineage should insert");
        store
            .upsert_job_lineage(&peer)
            .expect("peer lineage should insert");

        let related = store
            .list_related_jobs(&current, 10)
            .expect("related query should succeed");

        assert_eq!(related.len(), 1);
        assert_eq!(related[0].job.id, "peer");
        assert_eq!(related[0].matched_by.as_deref(), Some("trace"));
    }

    #[test]
    fn list_related_jobs_marks_concurrency_match() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut current = job(
            "current",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Pending,
        );
        let mut peer = job(
            "peer",
            "svc",
            "sync",
            "2026-01-01T00:01:00Z",
            JobState::Pending,
        );
        set_trace(&mut current, "0000000000000000000000000000000c");
        set_trace(&mut peer, "0000000000000000000000000000000d");
        store
            .upsert_job(&current)
            .expect("current job should insert");
        store.upsert_job(&peer).expect("peer job should insert");
        for job_id in ["current", "peer"] {
            store
                .apply_job_metadata_patch(
                    "svc",
                    "sync",
                    job_id,
                    "2026-01-01T00:02:00Z",
                    &JobProjectionMetadataPatch {
                        concurrency: Some(JobConcurrencyMetadata {
                            key: "tenant-1".to_string(),
                            key_hash: "hash-1".to_string(),
                            ..Default::default()
                        }),
                        queue_policy: None,
                    },
                )
                .expect("metadata should insert");
        }

        let related = store
            .list_related_jobs(&current, 10)
            .expect("related query should succeed");

        assert_eq!(related.len(), 1);
        assert_eq!(related[0].job.id, "peer");
        assert_eq!(related[0].matched_by.as_deref(), Some("concurrency"));
    }

    #[test]
    fn list_related_jobs_marks_wait_matches() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        let mut current = job(
            "current",
            "svc",
            "sync",
            "2026-01-01T00:02:00Z",
            JobState::Active,
        );
        let mut child = job(
            "child",
            "svc",
            "sync",
            "2026-01-01T00:01:00Z",
            JobState::Active,
        );
        set_trace(&mut current, "0000000000000000000000000000000e");
        set_trace(&mut child, "0000000000000000000000000000000f");
        store
            .upsert_job(&current)
            .expect("current job should insert");
        store.upsert_job(&child).expect("child job should insert");
        let mut waiting = event(
            &current,
            JobEventType::Waiting,
            JobState::Active,
            "2026-01-01T00:01:00Z",
        );
        waiting.wait_edge = Some(wait_edge("wait-1", "child"));
        store
            .project_wait_edge(&waiting)
            .expect("wait should project");

        let related = store
            .list_related_jobs(&current, 10)
            .expect("related query should succeed");

        assert_eq!(related.len(), 1);
        assert_eq!(related[0].job.id, "child");
        assert_eq!(related[0].matched_by.as_deref(), Some("wait"));
    }

    #[test]
    fn fresh_worker_listing_excludes_stale_records() {
        let store = SqliteJobsStore::open_in_memory().expect("store should open");
        store
            .upsert_worker_presence(&worker("svc", "import", "fresh", "2026-01-01T00:00:30Z"))
            .expect("insert should succeed");
        store
            .upsert_worker_presence(&worker("svc", "import", "stale", "2025-12-31T23:58:00Z"))
            .expect("insert should succeed");

        let now = OffsetDateTime::parse("2026-01-01T00:01:00Z", &Rfc3339).expect("valid timestamp");
        let workers = store
            .list_fresh_workers(now, Duration::from_secs(90))
            .expect("fresh listing should succeed");
        assert_eq!(
            workers
                .iter()
                .map(|worker| worker.instance_id.as_str())
                .collect::<Vec<_>>(),
            vec!["fresh"]
        );
    }
}
