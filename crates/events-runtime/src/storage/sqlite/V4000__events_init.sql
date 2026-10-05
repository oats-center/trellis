CREATE TABLE IF NOT EXISTS trellis_events_store_marker (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT OR IGNORE INTO trellis_events_store_marker (id) VALUES (1);

CREATE TABLE IF NOT EXISTS events_projection_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    stream_sequence INTEGER PRIMARY KEY,
    event_id TEXT,
    event_time TEXT NOT NULL,
    event_time_ns INTEGER NOT NULL,
    subject TEXT NOT NULL,
    owner_contract_id TEXT,
    owner_event_name TEXT,
    resolution TEXT NOT NULL,
    verification_status TEXT NOT NULL,
    publisher_kind TEXT,
    publisher_deployment_id TEXT,
    publisher_instance_id TEXT,
    publisher_participant_id TEXT,
    publisher_principal_id TEXT,
    publisher_connection_id TEXT,
    publisher_login_session_id TEXT,
    authorization_context_digest TEXT,
    trace_id TEXT,
    traceparent TEXT,
    payload_size_bytes INTEGER NOT NULL,
    payload_bytes BLOB NOT NULL,
    headers_json TEXT NOT NULL,
    payload_json TEXT,
    payload_text TEXT,
    decode_error TEXT,
    projected_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_event_id ON events (event_id);
CREATE INDEX IF NOT EXISTS idx_events_event_time ON events (event_time_ns DESC, stream_sequence DESC);
CREATE INDEX IF NOT EXISTS idx_events_payload_size ON events (payload_size_bytes DESC, stream_sequence DESC);
CREATE INDEX IF NOT EXISTS idx_events_subject ON events (subject);
CREATE INDEX IF NOT EXISTS idx_events_owner ON events (owner_contract_id, owner_event_name);
CREATE INDEX IF NOT EXISTS idx_events_publisher_deployment ON events (publisher_deployment_id);
CREATE INDEX IF NOT EXISTS idx_events_publisher_participant ON events (publisher_participant_id);
CREATE INDEX IF NOT EXISTS idx_events_trace ON events (trace_id);
CREATE INDEX IF NOT EXISTS idx_events_resolution ON events (resolution);
CREATE INDEX IF NOT EXISTS idx_events_verification ON events (verification_status);
CREATE INDEX IF NOT EXISTS idx_events_authorization_context ON events (authorization_context_digest);
CREATE INDEX IF NOT EXISTS idx_events_publisher_connection ON events (publisher_connection_id);

CREATE TABLE IF NOT EXISTS events_consumer_samples (
    sampled_at TEXT NOT NULL,
    consumer_name TEXT NOT NULL,
    deployment_id TEXT,
    contract_id TEXT,
    group_name TEXT,
    status TEXT NOT NULL,
    pending INTEGER NOT NULL,
    ack_pending INTEGER NOT NULL,
    waiting_pulls INTEGER NOT NULL,
    redelivered INTEGER,
    oldest_pending_at TEXT,
    PRIMARY KEY (sampled_at, consumer_name)
);

CREATE TABLE IF NOT EXISTS consumer_dead_letters (
    id TEXT PRIMARY KEY,
    resource_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_at_ns INTEGER NOT NULL,
    original_stream TEXT NOT NULL,
    original_sequence INTEGER NOT NULL,
    original_record_sequence INTEGER NOT NULL,
    event_id TEXT,
    subject TEXT NOT NULL,
    payload_bytes BLOB NOT NULL,
    headers_json TEXT NOT NULL,
    api_id TEXT,
    event_name TEXT,
    context_digest TEXT,
    verification_status TEXT NOT NULL,
    deliveries INTEGER NOT NULL,
    last_error TEXT,
    replay_stream_sequence INTEGER,
    last_journal_sequence INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_consumer_dead_letters_resource_state
    ON consumer_dead_letters (resource_id, state, updated_at_ns DESC, id);
CREATE INDEX IF NOT EXISTS idx_consumer_dead_letters_original
    ON consumer_dead_letters (original_stream, original_sequence);

CREATE TABLE IF NOT EXISTS consumer_dlq_projection_checkpoint (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    stream_sequence INTEGER NOT NULL
);

INSERT OR IGNORE INTO consumer_dlq_projection_checkpoint (id, stream_sequence)
VALUES (1, 0);

CREATE TABLE IF NOT EXISTS consumer_dlq_commands (
    request_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    dead_letter_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL,
    PRIMARY KEY (dead_letter_id, request_id)
);

CREATE TABLE IF NOT EXISTS consumer_dlq_transitions (
    journal_sequence INTEGER PRIMARY KEY,
    dead_letter_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    occurred_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_consumer_dlq_transitions_entry
    ON consumer_dlq_transitions (dead_letter_id, revision);
