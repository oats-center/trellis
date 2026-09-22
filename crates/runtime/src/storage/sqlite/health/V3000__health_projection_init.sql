CREATE TABLE IF NOT EXISTS trellis_health_projection_store_marker (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT OR IGNORE INTO trellis_health_projection_store_marker (id) VALUES (1);

CREATE TABLE health_projection_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  projection_id TEXT NOT NULL,
  last_stream_sequence INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL DEFAULT 0,
  gap_detected INTEGER NOT NULL DEFAULT 0,
  retained_from_ns INTEGER,
  complete_since_ns INTEGER
);

CREATE TABLE health_latest (
  participant_kind TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  instance_id TEXT NOT NULL,
  deployment_id TEXT NOT NULL,
  session_key TEXT NOT NULL,
  participant_name TEXT NOT NULL,
  contract_digest TEXT NOT NULL,
  reported_status TEXT NOT NULL,
  effective_status TEXT NOT NULL,
  observed_at_ns INTEGER NOT NULL,
  projected_at_ns INTEGER NOT NULL,
  heartbeat_deadline_ns INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  publish_interval_ms INTEGER NOT NULL,
  runtime TEXT NOT NULL,
  runtime_version TEXT,
  version TEXT,
  latest_sample_json TEXT NOT NULL,
  stream_sequence INTEGER NOT NULL,
  PRIMARY KEY (participant_kind, contract_id, instance_id)
);

CREATE INDEX health_latest_contract_status_idx
  ON health_latest (participant_kind, contract_id, effective_status);
CREATE INDEX health_latest_deadline_idx
  ON health_latest (heartbeat_deadline_ns)
  WHERE effective_status != 'offline';
CREATE INDEX health_latest_deployment_idx
  ON health_latest (deployment_id);

CREATE TABLE health_status_intervals (
  interval_id INTEGER PRIMARY KEY AUTOINCREMENT,
  participant_kind TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  instance_id TEXT NOT NULL,
  deployment_id TEXT NOT NULL,
  started_at_ns INTEGER NOT NULL,
  ended_at_ns INTEGER,
  reported_status TEXT NOT NULL,
  effective_status TEXT NOT NULL,
  checks_json TEXT NOT NULL,
  reason TEXT NOT NULL
);

CREATE UNIQUE INDEX health_status_intervals_open_idx
  ON health_status_intervals (participant_kind, contract_id, instance_id)
  WHERE ended_at_ns IS NULL;
CREATE INDEX health_status_intervals_window_idx
  ON health_status_intervals (participant_kind, contract_id, started_at_ns, ended_at_ns);

CREATE TABLE health_metric_buckets (
  participant_kind TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  instance_id TEXT NOT NULL,
  bucket_start_ns INTEGER NOT NULL,
  sample_count INTEGER NOT NULL DEFAULT 0,
  healthy_count INTEGER NOT NULL DEFAULT 0,
  degraded_count INTEGER NOT NULL DEFAULT 0,
  unhealthy_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (participant_kind, contract_id, instance_id, bucket_start_ns)
);

CREATE TABLE health_check_metric_buckets (
  participant_kind TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  instance_id TEXT NOT NULL,
  bucket_start_ns INTEGER NOT NULL,
  check_name TEXT NOT NULL,
  sample_count INTEGER NOT NULL DEFAULT 0,
  ok_count INTEGER NOT NULL DEFAULT 0,
  failed_count INTEGER NOT NULL DEFAULT 0,
  latency_sum_ms REAL NOT NULL DEFAULT 0,
  latency_max_ms REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (
    participant_kind,
    contract_id,
    instance_id,
    bucket_start_ns,
    check_name
  )
);

CREATE TABLE health_transition_outbox (
  event_id TEXT PRIMARY KEY,
  participant_kind TEXT NOT NULL,
  contract_id TEXT NOT NULL,
  instance_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at_ns INTEGER NOT NULL,
  published_at_ns INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT
);

CREATE INDEX health_transition_outbox_pending_idx
  ON health_transition_outbox (created_at_ns)
  WHERE published_at_ns IS NULL;

CREATE TABLE health_rejections (
  stream_sequence INTEGER PRIMARY KEY,
  subject TEXT NOT NULL,
  observed_at_ns INTEGER NOT NULL,
  reason TEXT NOT NULL
);
