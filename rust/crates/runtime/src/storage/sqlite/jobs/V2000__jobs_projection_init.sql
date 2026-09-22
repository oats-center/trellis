CREATE TABLE IF NOT EXISTS trellis_jobs_projection_store_marker (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT OR IGNORE INTO trellis_jobs_projection_store_marker (id) VALUES (1);

CREATE TABLE IF NOT EXISTS jobs_projection (
    service TEXT NOT NULL,
    job_type TEXT NOT NULL,
    id TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_at_nanos INTEGER NOT NULL,
    request_id TEXT,
    trace_id TEXT,
    traceparent TEXT,
    started_at_nanos INTEGER,
    completed_at_nanos INTEGER,
    created_at_nanos INTEGER,
    runtime_ms INTEGER,
    queue_age_anchor_nanos INTEGER,
    last_error_message TEXT,
    last_error_fingerprint TEXT,
    deadline TEXT,
    deadline_nanos INTEGER,
    payload_json TEXT NOT NULL,
    job_json TEXT NOT NULL,
    PRIMARY KEY (service, job_type, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_projection_global_id
    ON jobs_projection (id);
CREATE INDEX IF NOT EXISTS idx_jobs_projection_workbench_updated
    ON jobs_projection (updated_at_nanos DESC, service ASC, job_type ASC, id ASC);
CREATE INDEX IF NOT EXISTS idx_jobs_projection_workbench_service_type_state
    ON jobs_projection (service, job_type, state, updated_at_nanos DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_projection_created
    ON jobs_projection (created_at_nanos ASC, service ASC, job_type ASC, id ASC);
CREATE INDEX IF NOT EXISTS idx_jobs_projection_runtime
    ON jobs_projection (runtime_ms DESC, updated_at_nanos DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_projection_trace
    ON jobs_projection (trace_id, updated_at_nanos DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_projection_error_fingerprint
    ON jobs_projection (last_error_fingerprint, updated_at_nanos DESC);

CREATE TABLE IF NOT EXISTS worker_presence_projection (
    service TEXT NOT NULL,
    job_type TEXT NOT NULL,
    instance_id TEXT NOT NULL,
    concurrency INTEGER,
    version TEXT,
    heartbeat_at TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (service, job_type, instance_id)
);
CREATE INDEX IF NOT EXISTS idx_worker_presence_fresh
    ON worker_presence_projection (heartbeat_at DESC, service ASC, job_type ASC, instance_id ASC);

CREATE TABLE IF NOT EXISTS jobs_metadata_projection (
    service TEXT NOT NULL,
    job_type TEXT NOT NULL,
    id TEXT NOT NULL,
    concurrency_key TEXT,
    concurrency_key_hash TEXT,
    concurrency_instance_id TEXT,
    concurrency_heartbeat_at TEXT,
    concurrency_lease_expires_at TEXT,
    concurrency_stale_takeover_count INTEGER,
    queue_policy_outcome TEXT,
    queue_policy_reason TEXT,
    queue_policy_existing_job_id TEXT,
    queue_policy_replaced_job_id TEXT,
    trigger_kind TEXT,
    trigger_id TEXT,
    parent_job_id TEXT,
    operation_id TEXT,
    updated_at TEXT NOT NULL,
    updated_at_nanos INTEGER NOT NULL,
    PRIMARY KEY (service, job_type, id)
);
CREATE INDEX IF NOT EXISTS idx_jobs_metadata_key
    ON jobs_metadata_projection (service, job_type, concurrency_key);
CREATE INDEX IF NOT EXISTS idx_jobs_metadata_queue_key
    ON jobs_metadata_projection (service, job_type, concurrency_key, updated_at_nanos DESC);

CREATE TABLE IF NOT EXISTS jobs_lineage_projection (
    service TEXT NOT NULL,
    job_type TEXT NOT NULL,
    id TEXT NOT NULL,
    parent_job_id TEXT,
    root_job_id TEXT,
    operation_id TEXT,
    trigger_kind TEXT,
    trigger_id TEXT,
    trace_id TEXT,
    request_id TEXT,
    updated_at_nanos INTEGER NOT NULL,
    PRIMARY KEY (service, job_type, id)
);
CREATE INDEX IF NOT EXISTS idx_jobs_lineage_parent
    ON jobs_lineage_projection (parent_job_id);
CREATE INDEX IF NOT EXISTS idx_jobs_lineage_root
    ON jobs_lineage_projection (root_job_id);
CREATE INDEX IF NOT EXISTS idx_jobs_lineage_operation
    ON jobs_lineage_projection (operation_id);
CREATE INDEX IF NOT EXISTS idx_jobs_lineage_trace
    ON jobs_lineage_projection (trace_id, updated_at_nanos DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_lineage_trigger
    ON jobs_lineage_projection (trigger_kind, updated_at_nanos DESC);

CREATE TABLE IF NOT EXISTS jobs_events_projection (
    service TEXT NOT NULL,
    job_type TEXT NOT NULL,
    id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    state TEXT NOT NULL,
    previous_state TEXT,
    timestamp TEXT NOT NULL,
    timestamp_nanos INTEGER NOT NULL,
    tries INTEGER NOT NULL,
    message TEXT,
    error_message TEXT,
    progress_json TEXT,
    logs_json TEXT,
    worker_instance_id TEXT,
    raw_event_json TEXT NOT NULL,
    projected INTEGER,
    reason TEXT,
    PRIMARY KEY (service, job_type, id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_jobs_events_projection_job_time
    ON jobs_events_projection (service, job_type, id, timestamp_nanos ASC, sequence ASC);
CREATE INDEX IF NOT EXISTS idx_jobs_events_projection_id_time
    ON jobs_events_projection (id, timestamp_nanos ASC, sequence ASC);
CREATE INDEX IF NOT EXISTS idx_jobs_events_projection_type_time
    ON jobs_events_projection (event_type, timestamp_nanos DESC);

CREATE TABLE IF NOT EXISTS jobs_wait_projection (
    service TEXT NOT NULL,
    job_type TEXT NOT NULL,
    id TEXT NOT NULL,
    wait_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id TEXT,
    target_service TEXT,
    target_type TEXT,
    wait_edge_json TEXT NOT NULL,
    PRIMARY KEY (service, job_type, id, wait_id)
);
CREATE INDEX IF NOT EXISTS idx_jobs_wait_projection_job
    ON jobs_wait_projection (id, started_at ASC, wait_id ASC);
CREATE INDEX IF NOT EXISTS idx_jobs_wait_projection_target_job
    ON jobs_wait_projection (target_kind, target_id);

CREATE TABLE IF NOT EXISTS jobs_error_projection (
    fingerprint TEXT PRIMARY KEY,
    message TEXT NOT NULL,
    first_seen TEXT NOT NULL,
    first_seen_nanos INTEGER NOT NULL,
    last_seen TEXT NOT NULL,
    last_seen_nanos INTEGER NOT NULL,
    occurrence_count INTEGER NOT NULL,
    sample_service TEXT NOT NULL,
    sample_job_type TEXT NOT NULL,
    sample_state TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_error_projection_last_seen
    ON jobs_error_projection (last_seen_nanos DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS jobs_search_fts USING fts5(
    service,
    job_type,
    id,
    state,
    request_id,
    trace_id,
    traceparent,
    concurrency_key,
    queue_policy_reason,
    progress_text,
    log_text,
    error_text,
    content=''
);

CREATE TABLE IF NOT EXISTS projection_metadata (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO projection_metadata (name, value)
    VALUES ('last_projected_sequence', '0');
