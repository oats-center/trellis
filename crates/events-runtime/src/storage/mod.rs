use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::dead_letters::{dead_letter_id, DeadLetterCause, DeadLetterState, DeadLetterTransition};

/// SQLite-backed Events projection store.
#[derive(Debug, Clone)]
pub struct EventsStore {
    connection: Arc<Mutex<Connection>>,
}

/// One projected Trellis event row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedEvent {
    /// JetStream stream sequence.
    pub stream_sequence: u64,
    /// Trellis event id, normally from `Nats-Msg-Id`.
    pub event_id: Option<String>,
    /// Event timestamp.
    pub event_time: String,
    /// Concrete event subject.
    pub subject: String,
    /// Owner contract id resolved from the catalog, when available.
    pub owner_contract_id: Option<String>,
    /// Owner event name resolved from the catalog, when available.
    pub owner_event_name: Option<String>,
    /// Resolution status.
    pub resolution: String,
    /// Auth proof validation status.
    pub verification_status: String,
    /// Publisher kind when event verification metadata is available.
    pub publisher_kind: Option<String>,
    /// Publisher deployment id when event verification metadata is available.
    pub publisher_deployment_id: Option<String>,
    /// Publisher instance id when event verification metadata is available.
    pub publisher_instance_id: Option<String>,
    /// Publisher participant id proven by the authorization context.
    pub publisher_participant_id: Option<String>,
    /// Stable principal proven by the authorization context.
    pub publisher_principal_id: Option<String>,
    /// Logical runtime connection that owns the proof key.
    pub publisher_connection_id: Option<String>,
    /// Durable user login, when the publisher is a user.
    pub publisher_login_session_id: Option<String>,
    /// Permanent Auth SQL context record referenced by this event.
    pub authorization_context_digest: Option<String>,
    /// W3C trace id, when traceparent is present.
    pub trace_id: Option<String>,
    /// W3C traceparent header.
    pub traceparent: Option<String>,
    /// Raw payload bytes.
    pub payload_bytes: Vec<u8>,
    /// JSON object of message headers.
    pub headers_json: String,
    /// Decoded JSON payload cache.
    pub payload_json: Option<String>,
    /// UTF-8 payload cache.
    pub payload_text: Option<String>,
    /// Decode error for non-text payloads.
    pub decode_error: Option<String>,
    /// Projection timestamp.
    pub projected_at: String,
}

/// A resolved Events event type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventTypeRef {
    /// Owner contract id.
    pub owner_contract_id: String,
    /// Owner event name.
    pub owner_event_name: String,
}

/// Filters supported by `Events.Query`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsFilter {
    /// Free-text search over indexed metadata fields.
    pub search: Option<String>,
    /// Exact event subject.
    pub subject: Option<String>,
    /// Owner contract id.
    pub owner_contract_id: Option<String>,
    /// Owner event name.
    pub owner_event_name: Option<String>,
    /// Event types to include.
    pub include_event_types: Vec<EventTypeRef>,
    /// Event types to exclude.
    pub exclude_event_types: Vec<EventTypeRef>,
    /// Publisher deployment id.
    pub publisher_deployment_id: Option<String>,
    /// Exact publisher participant id.
    pub publisher_participant_id: Option<String>,
    /// Resolution statuses.
    pub resolution: Vec<String>,
    /// Verification statuses.
    pub verification_status: Vec<String>,
    /// Whether to include only events with a resolution or verification exception.
    pub integrity_exception_only: bool,
    /// Lower bound for event time.
    pub since: Option<i64>,
    /// Stable key strictly after which the page starts.
    pub after: Option<(String, u64)>,
    /// Page limit.
    pub limit: u64,
    /// Sort field.
    pub sort_field: String,
    /// Sort direction.
    pub sort_direction: String,
}

/// Errors returned by the SQLite Events projection store.
#[derive(Debug, thiserror::Error)]
pub enum EventsStoreError {
    /// SQLite operation failed.
    #[error("sqlite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// SQLite connection lock was poisoned.
    #[error("sqlite connection lock is poisoned")]
    Poisoned,
    /// JSON encode/decode failed.
    #[error("failed to encode {model}: {details}")]
    EncodeJson {
        /// Model name.
        model: &'static str,
        /// Error details.
        details: String,
    },
    /// Stream sequence cannot be represented in SQLite's signed integer range.
    #[error("stream sequence {0} is outside SQLite integer range")]
    SequenceOutOfRange(u64),
    /// Cursor cannot be decoded.
    #[error("invalid pagination cursor")]
    InvalidCursor,
    /// A persisted timestamp is not RFC3339 or cannot fit SQLite's signed nanoseconds.
    #[error("timestamp cannot be represented as Unix nanoseconds: {0}")]
    InvalidTimestamp(String),
    /// A journal entry contradicts already projected lifecycle identity or ordering.
    #[error("contradictory dead-letter transition for {id}: {details}")]
    ContradictoryDeadLetter {
        /// Dead-letter identity.
        id: String,
        /// Contradiction detail.
        details: String,
    },
}

impl EventsStore {
    /// Open a store at `path` and initialize the schema if needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EventsStoreError> {
        let store = Self {
            connection: Arc::new(Mutex::new(Connection::open(path)?)),
        };
        store.initialize_schema()?;
        Ok(store)
    }

    /// Open an in-memory store and initialize the schema. Intended for tests.
    pub fn open_in_memory() -> Result<Self, EventsStoreError> {
        let store = Self {
            connection: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        store.initialize_schema()?;
        Ok(store)
    }

    /// Initialize the projection schema. Safe to call more than once.
    pub fn initialize_schema(&self) -> Result<(), EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        connection.execute_batch(include_str!(
            "../../../runtime/src/storage/sqlite/events/V4000__events_init.sql"
        ))?;
        let projection_id = format!("{}-{}", std::process::id(), now_timestamp_string());
        connection.execute(
            "INSERT OR IGNORE INTO events_projection_metadata (key, value) VALUES ('projection_id', ?1)",
            params![projection_id],
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO events_projection_metadata (key, value) VALUES ('last_projected_sequence', '0')",
            [],
        )?;
        for (key, value) in [
            ("retention_gap_sequence", "0"),
            ("complete_since_sequence", "0"),
            ("complete_since", ""),
        ] {
            connection.execute(
                "INSERT OR IGNORE INTO events_projection_metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        Ok(())
    }

    /// Return this projection database's stable id.
    pub fn projection_id(&self) -> Result<String, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        Ok(connection.query_row(
            "SELECT value FROM events_projection_metadata WHERE key = 'projection_id'",
            [],
            |row| row.get(0),
        )?)
    }

    /// Return whether one JetStream sequence has already been projected.
    pub fn contains_stream_sequence(&self, stream_sequence: u64) -> Result<bool, EventsStoreError> {
        let stream_sequence = i64::try_from(stream_sequence)
            .map_err(|_| EventsStoreError::SequenceOutOfRange(stream_sequence))?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE stream_sequence = ?1)",
            [stream_sequence],
            |row| row.get(0),
        )?)
    }

    /// Persist evidence that the stream advanced beyond this projection's checkpoint.
    pub fn record_stream_bounds(&self, first_sequence: u64) -> Result<(), EventsStoreError> {
        if first_sequence == 0 {
            return Ok(());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let metadata_sequence = |key: &str| -> Result<u64, EventsStoreError> {
            Ok(transaction
                .query_row(
                    "SELECT value FROM events_projection_metadata WHERE key=?1",
                    [key],
                    |row| row.get::<_, String>(0),
                )?
                .parse()
                .unwrap_or_default())
        };
        let projected = metadata_sequence("last_projected_sequence")?;
        let previous_gap = metadata_sequence("retention_gap_sequence")?;
        if projected.saturating_add(1) < first_sequence && first_sequence > previous_gap {
            for (key, value) in [
                ("retention_gap_sequence", first_sequence.to_string()),
                ("complete_since_sequence", "0".to_owned()),
                ("complete_since", String::new()),
            ] {
                transaction.execute(
                    "UPDATE events_projection_metadata SET value=?2 WHERE key=?1",
                    params![key, value],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Persist one projected event and advance projection metadata.
    pub fn insert_event(&self, event: &ProjectedEvent) -> Result<(), EventsStoreError> {
        let stream_sequence_i64 = i64::try_from(event.stream_sequence)
            .map_err(|_| EventsStoreError::SequenceOutOfRange(event.stream_sequence))?;
        let event_time_ns = timestamp_nanos(&event.event_time)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let projected = transaction
            .query_row(
                "SELECT value FROM events_projection_metadata WHERE key='last_projected_sequence'",
                [],
                |row| row.get::<_, String>(0),
            )?
            .parse::<u64>()
            .unwrap_or_default();
        if projected.saturating_add(1) < event.stream_sequence {
            for (key, value) in [
                ("retention_gap_sequence", event.stream_sequence.to_string()),
                ("complete_since_sequence", "0".to_owned()),
                ("complete_since", String::new()),
            ] {
                transaction.execute(
                    "UPDATE events_projection_metadata SET value=?2 WHERE key=?1",
                    params![key, value],
                )?;
            }
        }
        transaction.execute(
            r#"
            INSERT OR REPLACE INTO events (
                stream_sequence, event_id, event_time, event_time_ns, subject, owner_contract_id,
                owner_event_name, resolution, verification_status, publisher_kind,
                publisher_deployment_id, publisher_instance_id, publisher_participant_id,
                publisher_principal_id, publisher_connection_id, publisher_login_session_id,
                authorization_context_digest, trace_id, traceparent,
                payload_size_bytes, payload_bytes, headers_json, payload_json, payload_text,
                decode_error, projected_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
            )
            "#,
            params![
                event.stream_sequence,
                event.event_id,
                event.event_time,
                event_time_ns,
                event.subject,
                event.owner_contract_id,
                event.owner_event_name,
                event.resolution,
                event.verification_status,
                event.publisher_kind,
                event.publisher_deployment_id,
                event.publisher_instance_id,
                event.publisher_participant_id,
                event.publisher_principal_id,
                event.publisher_connection_id,
                event.publisher_login_session_id,
                event.authorization_context_digest,
                event.trace_id,
                event.traceparent,
                event.payload_bytes.len() as u64,
                event.payload_bytes,
                event.headers_json,
                event.payload_json,
                event.payload_text,
                event.decode_error,
                event.projected_at,
            ],
        )?;
        transaction.execute(
            r#"
            UPDATE events_projection_metadata
            SET value = CASE
                WHEN CAST(value AS INTEGER) < ?1 THEN ?2
                ELSE value
            END
            WHERE key = 'last_projected_sequence'
            "#,
            params![stream_sequence_i64, event.stream_sequence.to_string()],
        )?;
        let gap_sequence: u64 = transaction
            .query_row(
                "SELECT value FROM events_projection_metadata WHERE key='retention_gap_sequence'",
                [],
                |row| row.get::<_, String>(0),
            )?
            .parse()
            .unwrap_or_default();
        let complete_sequence: u64 = transaction
            .query_row(
                "SELECT value FROM events_projection_metadata WHERE key='complete_since_sequence'",
                [],
                |row| row.get::<_, String>(0),
            )?
            .parse()
            .unwrap_or_default();
        if event.stream_sequence >= gap_sequence
            && gap_sequence > 0
            && (complete_sequence == 0 || event.stream_sequence < complete_sequence)
        {
            transaction.execute(
                "UPDATE events_projection_metadata SET value=?1 WHERE key='complete_since_sequence'",
                [event.stream_sequence.to_string()],
            )?;
            transaction.execute(
                "UPDATE events_projection_metadata SET value=?1 WHERE key='complete_since'",
                [&event.event_time],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Mark source evidence that expired before it could be projected or dead-lettered.
    pub fn record_retention_gap(&self, sequence: u64) -> Result<(), EventsStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE events_projection_metadata
             SET value = CASE WHEN CAST(value AS INTEGER) < ?1 THEN ?2 ELSE value END
             WHERE key = 'retention_gap_sequence'",
            params![sequence, sequence.to_string()],
        )?;
        transaction.execute(
            "UPDATE events_projection_metadata SET value='0' WHERE key='complete_since_sequence'",
            [],
        )?;
        transaction.execute(
            "UPDATE events_projection_metadata SET value='' WHERE key='complete_since'",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Query projected event rows and return `(rows, total)`.
    pub fn query_events(
        &self,
        filter: &EventsFilter,
    ) -> Result<(Vec<Value>, u64), EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let (mut where_sql, mut params) = event_where_clause(filter);
        let total = connection.query_row(
            &format!("SELECT COUNT(*) FROM events {where_sql}"),
            params_from_iter(params.clone()),
            |row| row.get::<_, u64>(0),
        )?;
        let order_field = match filter.sort_field.as_str() {
            "streamSequence" => "stream_sequence",
            "payloadSize" => "payload_size_bytes",
            _ => "event_time_ns",
        };
        let order_direction = if filter.sort_direction == "asc" {
            "ASC"
        } else {
            "DESC"
        };
        if let Some((value, sequence)) = &filter.after {
            let field = order_field;
            let comparison = if order_direction == "ASC" { ">" } else { "<" };
            let predicate = format!(
                "({field} {comparison} ? OR ({field} = ? AND stream_sequence {comparison} ?))"
            );
            where_sql = if where_sql.is_empty() {
                format!("WHERE {predicate}")
            } else {
                format!("{where_sql} AND {predicate}")
            };
            if field == "event_time_ns" {
                let value = timestamp_nanos(value).map_err(|_| EventsStoreError::InvalidCursor)?;
                params.push(SqlValue::Integer(value));
                params.push(SqlValue::Integer(value));
            } else {
                let value = value
                    .parse::<i64>()
                    .map_err(|_| EventsStoreError::InvalidCursor)?;
                params.push(SqlValue::Integer(value));
                params.push(SqlValue::Integer(value));
            }
            params.push(SqlValue::Integer(sql_sequence(*sequence)?));
        }
        params.push(SqlValue::from((filter.limit + 1) as i64));
        let mut statement = connection.prepare(&format!(
            "SELECT stream_sequence, event_id, event_time, subject, owner_contract_id, owner_event_name, resolution, verification_status, publisher_kind, publisher_deployment_id, publisher_instance_id, publisher_participant_id, publisher_principal_id, publisher_connection_id, publisher_login_session_id, authorization_context_digest, trace_id, payload_size_bytes, headers_json FROM events {where_sql} ORDER BY {order_field} {order_direction}, stream_sequence {order_direction} LIMIT ?"
        ))?;
        let rows = statement
            .query_map(params_from_iter(params), row_to_summary_value)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total))
    }

    /// Inspect one event by id or stream sequence.
    pub fn inspect_event(
        &self,
        event_id: Option<&str>,
        stream_sequence: Option<u64>,
    ) -> Result<Option<Value>, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let row = if let Some(sequence) = stream_sequence {
            connection
                .query_row(
                    &inspect_sql("stream_sequence = ?1"),
                    params![sequence],
                    row_to_inspect_value,
                )
                .optional()?
        } else if let Some(id) = event_id {
            connection
                .query_row(
                    &inspect_sql("event_id = ?1"),
                    params![id],
                    row_to_inspect_value,
                )
                .optional()?
        } else {
            None
        };
        Ok(row)
    }

    /// Return simple Events metrics from the projection.
    pub fn metrics(
        &self,
        window: Option<(i64, i64, i64)>,
        resource_id: Option<&str>,
    ) -> Result<Value, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let mut where_sql = String::new();
        let mut params = Vec::new();
        if let Some((since, _, _)) = window {
            where_sql.push_str("WHERE event_time_ns >= ?");
            params.push(SqlValue::Integer(since));
        }
        let total = connection.query_row(
            &format!("SELECT COUNT(*) FROM events {where_sql}"),
            params_from_iter(params.clone()),
            |row| row.get::<_, u64>(0),
        )?;
        let unique_subjects = connection.query_row(
            &format!("SELECT COUNT(DISTINCT subject) FROM events {where_sql}"),
            params_from_iter(params.clone()),
            |row| row.get::<_, u64>(0),
        )?;
        let payload_bytes = connection.query_row(
            &format!("SELECT COALESCE(SUM(payload_size_bytes), 0) FROM events {where_sql}"),
            params_from_iter(params.clone()),
            |row| row.get::<_, u64>(0),
        )?;
        let integrity_exceptions = connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM events {where_sql} {} (resolution != 'resolved' OR verification_status != 'verified')",
                if where_sql.is_empty() { "WHERE" } else { " AND" }
            ),
            params_from_iter(params.clone()),
            |row| row.get::<_, u64>(0),
        )?;
        let event_type_where = if where_sql.is_empty() {
            "WHERE owner_contract_id IS NOT NULL AND owner_event_name IS NOT NULL".to_string()
        } else {
            format!(
                "{where_sql} AND owner_contract_id IS NOT NULL AND owner_event_name IS NOT NULL"
            )
        };
        let mut statement = connection.prepare(&format!(
            "SELECT owner_contract_id, owner_event_name, COUNT(*) FROM events {event_type_where} GROUP BY owner_contract_id, owner_event_name ORDER BY COUNT(*) DESC, owner_contract_id, owner_event_name"
        ))?;
        let event_types = statement
            .query_map(params_from_iter(params.iter().cloned()), |row| {
                Ok(json!({
                    "ownerContractId": row.get::<_, String>(0)?,
                    "ownerEventName": row.get::<_, String>(1)?,
                    "count": row.get::<_, u64>(2)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let buckets = if let Some((since, window_seconds, bucket_seconds)) = window {
            let bucket_ns = bucket_seconds * 1_000_000_000;
            let mut bucket_params =
                vec![SqlValue::Integer(bucket_ns), SqlValue::Integer(bucket_ns)];
            bucket_params.extend(params.iter().cloned());
            let mut statement = connection.prepare(&format!(
                r#"
                SELECT
                    (event_time_ns / ?) * ?,
                    COUNT(*),
                    COALESCE(SUM(payload_size_bytes), 0),
                    SUM(resolution != 'resolved' OR verification_status != 'verified'),
                    SUM(resolution = 'resolved'),
                    SUM(resolution = 'unresolved'),
                    SUM(resolution = 'malformed'),
                    SUM(verification_status = 'verified'),
                    SUM(verification_status = 'missing-proof'),
                    SUM(verification_status = 'invalid-signature'),
                    SUM(verification_status = 'missing-session'),
                    SUM(verification_status = 'subject-denied'),
                    SUM(verification_status = 'outside-session-window'),
                    SUM(verification_status = 'auth-unavailable')
                FROM events {where_sql}
                GROUP BY 1
                ORDER BY 1
                "#
            ))?;
            let buckets = statement
                .query_map(params_from_iter(bucket_params), |row| {
                    Ok(json!({
                        "startNs": row.get::<_, i64>(0)?,
                        "total": row.get::<_, u64>(1)?,
                        "payloadSizeBytes": row.get::<_, u64>(2)?,
                        "integrityExceptions": row.get::<_, u64>(3)?,
                        "byResolution": {
                            "resolved": row.get::<_, u64>(4)?,
                            "unresolved": row.get::<_, u64>(5)?,
                            "malformed": row.get::<_, u64>(6)?,
                        },
                        "byVerificationStatus": {
                            "verified": row.get::<_, u64>(7)?,
                            "missingProof": row.get::<_, u64>(8)?,
                            "invalidSignature": row.get::<_, u64>(9)?,
                            "missingSession": row.get::<_, u64>(10)?,
                            "subjectDenied": row.get::<_, u64>(11)?,
                            "outsideSessionWindow": row.get::<_, u64>(12)?,
                            "authUnavailable": row.get::<_, u64>(13)?,
                        }
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut buckets_by_start = buckets
                .into_iter()
                .filter_map(|mut bucket| {
                    let start_ns = bucket.get("startNs")?.as_i64()?;
                    let start = OffsetDateTime::from_unix_timestamp_nanos(start_ns.into())
                        .ok()?
                        .format(&Rfc3339)
                        .ok()?;
                    bucket.as_object_mut()?.remove("startNs");
                    bucket
                        .as_object_mut()?
                        .insert("start".to_owned(), json!(start));
                    Some((start_ns, bucket))
                })
                .collect::<BTreeMap<_, _>>();
            let start = since.div_euclid(bucket_ns) * bucket_ns;
            let end = (since + window_seconds * 1_000_000_000).div_euclid(bucket_ns) * bucket_ns;
            (start..=end)
                .step_by(bucket_ns as usize)
                .filter_map(|timestamp| {
                    let formatted = OffsetDateTime::from_unix_timestamp_nanos(timestamp.into())
                        .ok()?
                        .format(&Rfc3339)
                        .ok()?;
                    Some(buckets_by_start.remove(&timestamp).unwrap_or_else(|| {
                        json!({
                            "start": formatted,
                            "total": 0,
                            "payloadSizeBytes": 0,
                            "integrityExceptions": 0,
                            "byResolution": {
                                "resolved": 0,
                                "unresolved": 0,
                                "malformed": 0,
                            },
                            "byVerificationStatus": {
                                "verified": 0,
                                "missingProof": 0,
                                "invalidSignature": 0,
                                "missingSession": 0,
                                "subjectDenied": 0,
                                "outsideSessionWindow": 0,
                                "authUnavailable": 0,
                            }
                        })
                    }))
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut dead_letters_by_state = BTreeMap::from([
            ("dead".to_owned(), 0_u64),
            ("dismissed".to_owned(), 0),
            ("replayPending".to_owned(), 0),
            ("replaying".to_owned(), 0),
            ("resolved".to_owned(), 0),
        ]);
        let mut statement = connection.prepare(if resource_id.is_some() {
            "SELECT state,COUNT(*) FROM consumer_dead_letters WHERE resource_id=?1 GROUP BY state"
        } else {
            "SELECT state,COUNT(*) FROM consumer_dead_letters GROUP BY state"
        })?;
        let mut rows = if let Some(resource_id) = resource_id {
            statement.query([resource_id])?
        } else {
            statement.query([])?
        };
        while let Some(row) = rows.next()? {
            dead_letters_by_state.insert(row.get(0)?, row.get(1)?);
        }
        Ok(json!({
            "summary": {
                "total": total,
                "uniqueSubjects": unique_subjects,
                "payloadSizeBytes": payload_bytes,
                "integrityExceptions": integrity_exceptions,
                "byResolution": grouped_counts(&connection, "resolution", &where_sql, &params)?,
                "byVerificationStatus": grouped_counts(&connection, "verification_status", &where_sql, &params)?,
                "eventTypes": event_types,
                "deadLettersByState": dead_letters_by_state,
            },
            "buckets": buckets
        }))
    }

    /// Apply one authoritative dead-letter journal transition and checkpoint atomically.
    pub fn project_dead_letter(
        &self,
        transition: &DeadLetterTransition,
        journal_sequence: u64,
    ) -> Result<(), EventsStoreError> {
        let journal_sequence = sql_sequence(journal_sequence)?;
        let revision = sql_sequence(transition.revision)?;
        let generation = sql_sequence(transition.generation)?;
        let deliveries = sql_sequence(transition.deliveries)?;
        let occurred_at_ns = timestamp_nanos(&transition.occurred_at)?;
        let original_record_sequence = sql_sequence(if transition.original_record_sequence == 0 {
            u64::try_from(journal_sequence).unwrap_or_default()
        } else {
            transition.original_record_sequence
        })?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let checkpoint = dead_letter_checkpoint(&transaction)?;
        if u64::try_from(journal_sequence).unwrap_or_default() <= checkpoint {
            let projected = transaction
                .query_row(
                    "SELECT dead_letter_id, revision, state FROM consumer_dlq_transitions WHERE journal_sequence = ?1",
                    [journal_sequence],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?, row.get::<_, String>(2)?)),
                )
                .optional()?;
            if projected.as_ref().is_some_and(|(id, revision, state)| {
                id == &transition.id
                    && *revision == transition.revision
                    && state == state_token(transition.state)
            }) {
                transaction.commit()?;
                return Ok(());
            }
            return Err(EventsStoreError::ContradictoryDeadLetter {
                id: transition.id.clone(),
                details: format!(
                    "journal sequence {journal_sequence} contradicts its projected checkpoint"
                ),
            });
        }
        let current = transaction
            .query_row(
                "SELECT resource_id, revision, generation, state, original_record_sequence, last_journal_sequence FROM consumer_dead_letters WHERE id = ?1",
                [&transition.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?, row.get::<_, u64>(2)?, row.get::<_, String>(3)?, row.get::<_, u64>(4)?, row.get::<_, u64>(5)?)),
            )
            .optional()?;
        if let Some((
            resource_id,
            current_revision,
            current_generation,
            current_state,
            current_original_sequence,
            current_sequence,
        )) = current
        {
            if current_sequence == u64::try_from(journal_sequence).unwrap_or_default() {
                if resource_id == transition.resource_id
                    && current_revision == transition.revision
                    && current_generation == transition.generation
                    && current_state == state_token(transition.state)
                    && current_original_sequence == original_record_sequence as u64
                {
                    transaction.commit()?;
                    return Ok(());
                }
                return Err(EventsStoreError::ContradictoryDeadLetter {
                    id: transition.id.clone(),
                    details: format!(
                        "journal sequence {current_sequence} conflicts with its projected state"
                    ),
                });
            }
            if resource_id != transition.resource_id
                || transition.revision != current_revision + 1
                || transition.generation < current_generation
                || transition.previous_subject_sequence != current_sequence
                || transition.original_record_sequence != current_original_sequence
                || transition.original.is_some()
                || !legal_dead_letter_successor(&current_state, current_generation, transition)
            {
                return Err(EventsStoreError::ContradictoryDeadLetter {
                    id: transition.id.clone(),
                    details: format!(
                        "current revision/generation/sequence is {current_revision}/{current_generation}/{current_sequence}, incoming is {}/{}/{} after {}",
                        transition.revision,
                        transition.generation,
                        journal_sequence,
                        transition.previous_subject_sequence
                    ),
                });
            }
            transaction.execute(
                "UPDATE consumer_dead_letters SET revision=?2, generation=?3, state=?4, updated_at=?5, updated_at_ns=?6, deliveries=?7, last_error=?8, replay_stream_sequence=?9, last_journal_sequence=?10 WHERE id=?1",
                params![transition.id, revision, generation, state_token(transition.state), transition.occurred_at, occurred_at_ns, deliveries, transition.last_error, transition.replay_stream_sequence.map(sql_sequence).transpose()?, journal_sequence],
            )?;
        } else {
            if transition.revision != 1 || transition.previous_subject_sequence != 0 {
                return Err(EventsStoreError::ContradictoryDeadLetter {
                    id: transition.id.clone(),
                    details: "first projected transition is not revision one".to_owned(),
                });
            }
            let original = transition.original.as_ref().ok_or_else(|| {
                EventsStoreError::ContradictoryDeadLetter {
                    id: transition.id.clone(),
                    details: "revision one has no original evidence".to_owned(),
                }
            })?;
            if transition.resource_id.is_empty()
                || transition.generation != 0
                || transition.state != DeadLetterState::Dead
                || transition.cause != DeadLetterCause::Exhausted
                || transition.request_id.is_some()
                || transition.request_digest.is_some()
                || transition.id
                    != dead_letter_id(&transition.resource_id, &original.stream, original.sequence)
            {
                return Err(EventsStoreError::ContradictoryDeadLetter {
                    id: transition.id.clone(),
                    details: "revision one has invalid identity or lifecycle fields".to_owned(),
                });
            }
            transaction.execute(
                r#"INSERT INTO consumer_dead_letters (
                    id, resource_id, revision, generation, state, created_at, updated_at, updated_at_ns,
                    original_stream, original_sequence, original_record_sequence, event_id,
                    subject, payload_bytes, headers_json, api_id, event_name, context_digest,
                    verification_status, deliveries, last_error, replay_stream_sequence,
                    last_journal_sequence
                ) VALUES (?1,?2,?3,?4,?5,?6,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"#,
                params![
                    transition.id,
                    transition.resource_id,
                    revision,
                    generation,
                    state_token(transition.state),
                    transition.occurred_at,
                    occurred_at_ns,
                    original.stream,
                    sql_sequence(original.sequence)?,
                    original_record_sequence,
                    original.event_id,
                    original.subject,
                    original.payload_bytes,
                    serde_json::to_string(&original.headers).map_err(|error| EventsStoreError::EncodeJson { model: "dead-letter headers", details: error.to_string() })?,
                    original.api_id,
                    original.event_name,
                    original.context_digest,
                    original.verification_status,
                    deliveries,
                    transition.last_error,
                    transition.replay_stream_sequence.map(sql_sequence).transpose()?,
                    journal_sequence,
                ],
            )?;
        }
        if let (Some(request_id), Some(request_digest)) =
            (&transition.request_id, &transition.request_digest)
        {
            transaction.execute(
                "INSERT INTO consumer_dlq_commands (request_id,request_digest,dead_letter_id,revision,generation,state,journal_sequence) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![request_id, request_digest, transition.id, revision, generation, state_token(transition.state), journal_sequence],
            )?;
        }
        transaction.execute(
            "INSERT INTO consumer_dlq_transitions (journal_sequence,dead_letter_id,revision,state,occurred_at) VALUES (?1,?2,?3,?4,?5)",
            params![journal_sequence, transition.id, revision, state_token(transition.state), transition.occurred_at],
        )?;
        transaction.execute(
            "UPDATE consumer_dlq_projection_checkpoint SET stream_sequence=?1 WHERE id=1 AND stream_sequence < ?1",
            [journal_sequence],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads the unresolved dead-letter snapshot without claiming or mutating.
    pub fn telemetry_snapshot(
        &self,
    ) -> Result<crate::telemetry::DlqTelemetrySnapshot, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let mut snapshot = crate::telemetry::DlqTelemetrySnapshot::default();
        let mut statement = connection
            .prepare("SELECT state, COUNT(*) FROM consumer_dead_letters GROUP BY state")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (state, count) = row?;
            let count = count.max(0) as u64;
            match state.as_str() {
                "dead" | "open" => snapshot.open = count,
                "replayPending" | "replay_pending" => snapshot.replay_pending = count,
                "replaying" => snapshot.replaying = count,
                _ => {}
            }
        }
        Ok(snapshot)
    }

    pub(crate) fn dead_letter_projection_checkpoint(&self) -> Result<u64, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        dead_letter_checkpoint(&connection)
    }

    /// Look up the projected result of an idempotent dead-letter command.
    pub fn dead_letter_command(
        &self,
        dead_letter_id: &str,
        request_id: &str,
    ) -> Result<Option<Value>, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT request_digest,revision,generation,state,journal_sequence FROM consumer_dlq_commands WHERE dead_letter_id=?1 AND request_id=?2",
                params![dead_letter_id, request_id],
                |row| {
                    Ok(json!({
                        "requestDigest": row.get::<_, String>(0)?,
                        "revision": row.get::<_, u64>(1)?,
                        "generation": row.get::<_, u64>(2)?,
                        "state": row.get::<_, String>(3)?,
                        "journalSequence": row.get::<_, u64>(4)?,
                    }))
                },
            )
            .optional()
            .map_err(EventsStoreError::from)
    }

    /// Clear only the rebuildable dead-letter projection and checkpoint.
    pub fn reset_dead_letter_projection(&self) -> Result<(), EventsStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM consumer_dlq_commands;
             DELETE FROM consumer_dead_letters;
             DELETE FROM consumer_dlq_transitions;
             UPDATE consumer_dlq_projection_checkpoint SET stream_sequence=0 WHERE id=1;",
        )?;
        transaction.execute(
            "UPDATE events_projection_metadata SET value=?1 WHERE key='projection_id'",
            [format!("{}-{}", std::process::id(), now_timestamp_string())],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Return projection watermarks for diagnostics.
    pub fn diagnostics(&self) -> Result<Value, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let event_sequence: String = connection.query_row(
            "SELECT value FROM events_projection_metadata WHERE key='last_projected_sequence'",
            [],
            |row| row.get(0),
        )?;
        let dead_letter_sequence: u64 = connection.query_row(
            "SELECT stream_sequence FROM consumer_dlq_projection_checkpoint WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        let retention_gap_sequence: u64 = connection
            .query_row(
                "SELECT value FROM events_projection_metadata WHERE key='retention_gap_sequence'",
                [],
                |row| row.get::<_, String>(0),
            )?
            .parse()
            .unwrap_or_default();
        let complete_since: String = connection.query_row(
            "SELECT value FROM events_projection_metadata WHERE key='complete_since'",
            [],
            |row| row.get(0),
        )?;
        let retained_from: Option<String> = connection
            .query_row(
                "SELECT event_time FROM events ORDER BY event_time_ns, stream_sequence LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json!({
            "eventProjectionSequence": event_sequence.parse::<u64>().unwrap_or_default(),
            "deadLetterProjectionSequence": dead_letter_sequence,
            "gapDetected": retention_gap_sequence > 0,
            "completeSince": (!complete_since.is_empty()).then_some(complete_since),
            "retainedFrom": retained_from,
        }))
    }

    /// Query the rebuildable dead-letter projection using a query-bound keyset cursor.
    pub fn query_dead_letters(
        &self,
        resource_id: Option<&str>,
        states: &[String],
        cursor: Option<&str>,
        limit: u64,
    ) -> Result<Value, EventsStoreError> {
        if limit == 0 || limit > 500 {
            return Err(EventsStoreError::InvalidCursor);
        }
        let mut digest_states = states.to_vec();
        digest_states.sort_unstable();
        digest_states.dedup();
        let query_digest = trellis_protocol::pagination_query_digest(
            "events.DeadLetters.Query",
            &json!({
                "resourceId": resource_id,
                "state": digest_states,
                "order": ["updatedAt:desc", "deadLetterId:asc"]
            }),
        )
        .map_err(|_| EventsStoreError::InvalidCursor)?;
        let mut conditions = Vec::new();
        let mut values = Vec::<SqlValue>::new();
        if let Some(resource_id) = resource_id {
            conditions.push("resource_id = ?".to_owned());
            values.push(SqlValue::Text(resource_id.to_owned()));
        }
        if !states.is_empty() {
            conditions.push(format!(
                "state IN ({})",
                std::iter::repeat_n("?", states.len())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            values.extend(states.iter().cloned().map(SqlValue::Text));
        }
        if let Some(cursor) = cursor {
            let (updated_at, id): (String, String) =
                trellis_protocol::decode_pagination_cursor(cursor, &query_digest)
                    .map_err(|_| EventsStoreError::InvalidCursor)?;
            let updated_at_ns =
                timestamp_nanos(&updated_at).map_err(|_| EventsStoreError::InvalidCursor)?;
            conditions.push("(updated_at_ns < ? OR (updated_at_ns = ? AND id > ?))".to_owned());
            values.push(SqlValue::Integer(updated_at_ns));
            values.push(SqlValue::Integer(updated_at_ns));
            values.push(SqlValue::Text(id));
        }
        let where_sql = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        values.push(SqlValue::Integer(sql_sequence(limit + 1)?));
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let mut statement = connection.prepare(&format!(
            "SELECT id,resource_id,revision,generation,state,updated_at,original_stream,original_sequence,deliveries,last_error FROM consumer_dead_letters {where_sql} ORDER BY updated_at_ns DESC, id ASC LIMIT ?"
        ))?;
        let mut rows = statement
            .query_map(params_from_iter(values), dead_letter_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if rows.len() > limit as usize {
            rows.pop();
            rows.last()
                .map(|row| {
                    let updated_at = row
                        .get("updatedAt")
                        .and_then(Value::as_str)
                        .ok_or(EventsStoreError::InvalidCursor)?;
                    let id = row
                        .get("deadLetterId")
                        .and_then(Value::as_str)
                        .ok_or(EventsStoreError::InvalidCursor)?;
                    trellis_protocol::encode_pagination_cursor(&query_digest, &(updated_at, id))
                        .map_err(|_| EventsStoreError::InvalidCursor)
                })
                .transpose()?
        } else {
            None
        };
        Ok(json!({ "items": rows, "page": { "nextCursor": next_cursor } }))
    }

    /// Inspect one projected dead letter with original evidence and journal references.
    pub fn inspect_dead_letter(&self, id: &str) -> Result<Option<Value>, EventsStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| EventsStoreError::Poisoned)?;
        let row = connection
            .query_row(
                "SELECT id,resource_id,revision,generation,state,updated_at,original_stream,original_sequence,deliveries,last_error,subject,payload_bytes,headers_json,verification_status FROM consumer_dead_letters WHERE id=?1",
                [id],
                |row| {
                    let summary = dead_letter_row(row)?;
                    let raw_headers = row.get::<_, String>(12)?;
                    let headers = serde_json::from_str::<BTreeMap<String, Vec<String>>>(&raw_headers)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(name, values)| (name, values.join(",")))
                        .collect::<BTreeMap<_, _>>();
                    Ok((summary, row.get::<_, String>(10)?, row.get::<_, Vec<u8>>(11)?, headers, row.get::<_, String>(13)?))
                },
            )
            .optional()?;
        let Some((summary, subject, payload, headers, verification_status)) = row else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT journal_sequence FROM consumer_dlq_transitions WHERE dead_letter_id=?1 ORDER BY revision",
        )?;
        let references = statement
            .query_map([id], |row| row.get::<_, u64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(json!({
            "deadLetter": summary,
            "originalHeaders": headers,
            "originalPayload": payload,
            "originalSubject": subject,
            "transitionReferences": references,
            "verificationStatus": normalize_verification_status(&verification_status),
        })))
    }
}

fn sql_sequence(value: u64) -> Result<i64, EventsStoreError> {
    i64::try_from(value).map_err(|_| EventsStoreError::SequenceOutOfRange(value))
}

fn timestamp_nanos(value: &str) -> Result<i64, EventsStoreError> {
    let nanos = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|_| EventsStoreError::InvalidTimestamp(value.to_owned()))?
        .unix_timestamp_nanos();
    i64::try_from(nanos).map_err(|_| EventsStoreError::InvalidTimestamp(value.to_owned()))
}

fn dead_letter_checkpoint(connection: &Connection) -> Result<u64, EventsStoreError> {
    connection
        .query_row(
            "SELECT stream_sequence FROM consumer_dlq_projection_checkpoint WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(EventsStoreError::from)
}

fn state_token(state: DeadLetterState) -> &'static str {
    match state {
        DeadLetterState::Dead => "dead",
        DeadLetterState::ReplayPending => "replayPending",
        DeadLetterState::Replaying => "replaying",
        DeadLetterState::Resolved => "resolved",
        DeadLetterState::Dismissed => "dismissed",
    }
}

fn legal_dead_letter_successor(
    current_state: &str,
    current_generation: u64,
    next: &DeadLetterTransition,
) -> bool {
    let is_command = matches!(
        next.cause,
        DeadLetterCause::ReplayRequested | DeadLetterCause::Dismissed
    );
    if is_command != (next.request_id.is_some() && next.request_digest.is_some()) {
        return false;
    }
    match (current_state, next.state, next.cause) {
        (
            "dead" | "dismissed",
            DeadLetterState::ReplayPending,
            DeadLetterCause::ReplayRequested,
        ) => next.generation == current_generation + 1 && next.replay_stream_sequence.is_none(),
        ("dead", DeadLetterState::Dismissed, DeadLetterCause::Dismissed) => {
            next.generation == current_generation
        }
        ("dead", DeadLetterState::Dead, DeadLetterCause::Exhausted) => {
            next.generation == current_generation
        }
        ("replayPending", DeadLetterState::Replaying, DeadLetterCause::ReplayDispatched) => {
            next.generation == current_generation && next.replay_stream_sequence.is_some()
        }
        (
            "replayPending" | "replaying",
            DeadLetterState::Resolved,
            DeadLetterCause::ReplaySucceeded,
        )
        | ("replayPending" | "replaying", DeadLetterState::Dead, DeadLetterCause::Exhausted) => {
            next.generation == current_generation
        }
        _ => false,
    }
}

fn dead_letter_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "deadLetterId": row.get::<_, String>(0)?,
        "resourceId": row.get::<_, String>(1)?,
        "revision": row.get::<_, u64>(2)?,
        "generation": row.get::<_, u64>(3)?,
        "state": row.get::<_, String>(4)?,
        "updatedAt": row.get::<_, String>(5)?,
        "originalStream": row.get::<_, String>(6)?,
        "originalSequence": row.get::<_, u64>(7)?,
        "deliveries": row.get::<_, u64>(8)?,
        "lastError": row.get::<_, Option<String>>(9)?,
    }))
}

fn normalize_verification_status(status: &str) -> &'static str {
    if status == "verified" {
        "verified"
    } else if status.contains("revoked") {
        "revoked"
    } else if status.contains("unavailable") {
        "unavailable"
    } else {
        "invalid"
    }
}

fn event_where_clause(filter: &EventsFilter) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    if let Some(value) = filter.subject.as_ref() {
        clauses.push("subject = ?".to_string());
        params.push(SqlValue::from(value.clone()));
    }
    if let Some(value) = filter.owner_contract_id.as_ref() {
        clauses.push("owner_contract_id = ?".to_string());
        params.push(SqlValue::from(value.clone()));
    }
    if let Some(value) = filter.owner_event_name.as_ref() {
        clauses.push("owner_event_name = ?".to_string());
        params.push(SqlValue::from(value.clone()));
    }
    if !filter.include_event_types.is_empty() {
        clauses.push(format!(
            "({})",
            vec![
                "(owner_contract_id = ? AND owner_event_name = ?)";
                filter.include_event_types.len()
            ]
            .join(" OR ")
        ));
        for event_type in &filter.include_event_types {
            params.push(SqlValue::from(event_type.owner_contract_id.clone()));
            params.push(SqlValue::from(event_type.owner_event_name.clone()));
        }
    }
    if !filter.exclude_event_types.is_empty() {
        clauses.push(format!(
            "(owner_contract_id IS NULL OR owner_event_name IS NULL OR NOT ({}))",
            vec![
                "(owner_contract_id = ? AND owner_event_name = ?)";
                filter.exclude_event_types.len()
            ]
            .join(" OR ")
        ));
        for event_type in &filter.exclude_event_types {
            params.push(SqlValue::from(event_type.owner_contract_id.clone()));
            params.push(SqlValue::from(event_type.owner_event_name.clone()));
        }
    }
    if let Some(value) = filter.publisher_deployment_id.as_ref() {
        clauses.push("publisher_deployment_id = ?".to_string());
        params.push(SqlValue::from(value.clone()));
    }
    if let Some(value) = filter.publisher_participant_id.as_ref() {
        clauses.push("publisher_participant_id = ?".to_string());
        params.push(SqlValue::from(value.clone()));
    }
    if let Some(value) = filter.since.as_ref() {
        clauses.push("event_time_ns >= ?".to_string());
        params.push(SqlValue::Integer(*value));
    }
    add_in_clause("resolution", &filter.resolution, &mut clauses, &mut params);
    add_in_clause(
        "verification_status",
        &filter.verification_status,
        &mut clauses,
        &mut params,
    );
    if filter.integrity_exception_only {
        clauses.push("(resolution != 'resolved' OR verification_status != 'verified')".to_string());
    }
    if let Some(value) = filter
        .search
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        clauses.push("(event_id LIKE ? ESCAPE '\\' OR subject LIKE ? ESCAPE '\\' OR owner_contract_id LIKE ? ESCAPE '\\' OR owner_event_name LIKE ? ESCAPE '\\' OR publisher_deployment_id LIKE ? ESCAPE '\\' OR publisher_participant_id LIKE ? ESCAPE '\\' OR trace_id LIKE ? ESCAPE '\\')".to_string());
        let pattern = format!(
            "%{}%",
            value
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        for _ in 0..7 {
            params.push(SqlValue::from(pattern.clone()));
        }
    }
    if clauses.is_empty() {
        (String::new(), params)
    } else {
        (format!("WHERE {}", clauses.join(" AND ")), params)
    }
}

fn add_in_clause(
    field: &str,
    values: &[String],
    clauses: &mut Vec<String>,
    params: &mut Vec<SqlValue>,
) {
    if values.is_empty() {
        return;
    }
    clauses.push(format!(
        "{field} IN ({})",
        vec!["?"; values.len()].join(", ")
    ));
    params.extend(values.iter().cloned().map(SqlValue::from));
}

fn row_to_summary_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let stream_sequence: u64 = row.get(0)?;
    let payload_size_bytes: u64 = row.get(17)?;
    let headers_json: String = row.get(18)?;
    let header_count = serde_json::from_str::<Value>(&headers_json)
        .ok()
        .and_then(|value| value.as_object().map(serde_json::Map::len))
        .unwrap_or(0);
    Ok(json!({
        "eventId": row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        "eventTime": row.get::<_, String>(2)?,
        "streamSequence": stream_sequence,
        "subject": row.get::<_, String>(3)?,
        "ownerContractId": row.get::<_, Option<String>>(4)?,
        "ownerEventName": row.get::<_, Option<String>>(5)?,
        "resolution": row.get::<_, String>(6)?,
        "verificationStatus": row.get::<_, String>(7)?,
        "publisherKind": row.get::<_, Option<String>>(8)?,
        "publisherDeploymentId": row.get::<_, Option<String>>(9)?,
        "publisherInstanceId": row.get::<_, Option<String>>(10)?,
        "publisherParticipantId": row.get::<_, Option<String>>(11)?,
        "publisherPrincipalId": row.get::<_, Option<String>>(12)?,
        "publisherConnectionId": row.get::<_, Option<String>>(13)?,
        "publisherLoginSessionId": row.get::<_, Option<String>>(14)?,
        "authorizationContextDigest": row.get::<_, Option<String>>(15)?,
        "traceId": row.get::<_, Option<String>>(16)?,
        "payloadSizeBytes": payload_size_bytes,
        "headerCount": header_count,
    }))
}

fn inspect_sql(predicate: &str) -> String {
    format!(
        "SELECT stream_sequence, event_id, event_time, subject, owner_contract_id, owner_event_name, resolution, verification_status, publisher_kind, publisher_deployment_id, publisher_instance_id, publisher_participant_id, publisher_principal_id, publisher_connection_id, publisher_login_session_id, authorization_context_digest, trace_id, traceparent, payload_size_bytes, payload_bytes, headers_json, payload_json, payload_text, decode_error, projected_at FROM events WHERE {predicate} ORDER BY stream_sequence DESC LIMIT 1"
    )
}

fn row_to_inspect_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let event = row_to_summary_from_inspect_row(row)?;
    let headers_json: String = row.get(20)?;
    let headers = serde_json::from_str::<BTreeMap<String, Vec<String>>>(&headers_json)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, values)| (name, values.join(",")))
        .collect::<BTreeMap<_, _>>();
    let payload: Vec<u8> = row.get(19)?;
    Ok(json!({
        "event": {
            "row": event,
            "headers": headers,
            "payload": STANDARD.encode(payload),
        }
    }))
}

fn row_to_summary_from_inspect_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let payload_size_bytes: u64 = row.get(18)?;
    let headers_json: String = row.get(20)?;
    let header_count = serde_json::from_str::<Value>(&headers_json)
        .ok()
        .and_then(|value| value.as_object().map(serde_json::Map::len))
        .unwrap_or(0);
    Ok(json!({
        "eventId": row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        "eventTime": row.get::<_, String>(2)?,
        "streamSequence": row.get::<_, u64>(0)?,
        "subject": row.get::<_, String>(3)?,
        "ownerContractId": row.get::<_, Option<String>>(4)?,
        "ownerEventName": row.get::<_, Option<String>>(5)?,
        "resolution": row.get::<_, String>(6)?,
        "verificationStatus": row.get::<_, String>(7)?,
        "publisherKind": row.get::<_, Option<String>>(8)?,
        "publisherDeploymentId": row.get::<_, Option<String>>(9)?,
        "publisherInstanceId": row.get::<_, Option<String>>(10)?,
        "publisherParticipantId": row.get::<_, Option<String>>(11)?,
        "publisherPrincipalId": row.get::<_, Option<String>>(12)?,
        "publisherConnectionId": row.get::<_, Option<String>>(13)?,
        "publisherLoginSessionId": row.get::<_, Option<String>>(14)?,
        "authorizationContextDigest": row.get::<_, Option<String>>(15)?,
        "traceId": row.get::<_, Option<String>>(16)?,
        "payloadSizeBytes": payload_size_bytes,
        "headerCount": header_count,
    }))
}

fn grouped_counts(
    connection: &Connection,
    field: &str,
    where_sql: &str,
    params: &[SqlValue],
) -> Result<Value, EventsStoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {field}, COUNT(*) FROM events {where_sql} GROUP BY {field}"
    ))?;
    let mut object = serde_json::Map::new();
    for row in statement.query_map(params_from_iter(params.iter().cloned()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    })? {
        let (mut key, count) = row?;
        if field == "verification_status" {
            key = match key.as_str() {
                "missing-proof" => "missingProof",
                "invalid-signature" => "invalidSignature",
                "missing-session" => "missingSession",
                "subject-denied" => "subjectDenied",
                "outside-session-window" => "outsideSessionWindow",
                "auth-unavailable" => "authUnavailable",
                _ => &key,
            }
            .to_owned();
        }
        object.insert(key, json!(count));
    }
    Ok(Value::Object(object))
}

/// Return the current UTC timestamp in RFC3339 form.
pub fn now_timestamp_string() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{timestamp_nanos, EventTypeRef, EventsFilter, EventsStore, ProjectedEvent};
    use crate::dead_letters::{
        dead_letter_id, DeadLetterCause, DeadLetterState, DeadLetterTransition, OriginalEvent,
    };

    fn event(stream_sequence: u64) -> ProjectedEvent {
        ProjectedEvent {
            stream_sequence,
            event_id: Some(format!("event-{stream_sequence}")),
            event_time: "2026-01-01T00:00:00Z".to_string(),
            subject: "events.v1.Test.Created".to_string(),
            owner_contract_id: None,
            owner_event_name: None,
            resolution: "unresolved".to_string(),
            verification_status: "verified".to_string(),
            publisher_kind: None,
            publisher_deployment_id: None,
            publisher_instance_id: None,
            publisher_participant_id: None,
            publisher_principal_id: None,
            publisher_connection_id: None,
            publisher_login_session_id: None,
            authorization_context_digest: None,
            trace_id: None,
            traceparent: None,
            payload_bytes: b"{}".to_vec(),
            headers_json: "{}".to_string(),
            payload_json: Some("{}".to_string()),
            payload_text: Some("{}".to_string()),
            decode_error: None,
            projected_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn insert_event_keeps_projection_watermark_monotonic() {
        let store = EventsStore::open_in_memory().expect("open store");

        store
            .insert_event(&event(20))
            .expect("insert high sequence");
        store
            .insert_event(&event(10))
            .expect("insert lower sequence");

        let connection = store.connection.lock().expect("lock store");
        let value: String = connection
            .query_row(
                "SELECT value FROM events_projection_metadata WHERE key = 'last_projected_sequence'",
                [],
                |row| row.get(0),
            )
            .expect("read watermark");
        assert_eq!(value, "20");
    }

    #[test]
    fn retention_gap_evidence_survives_projection_restart() {
        let directory = tempfile::tempdir().expect("temporary projection");
        let path = directory.path().join("events.sqlite");
        let store = EventsStore::open(&path).expect("open store");
        store
            .record_stream_bounds(10)
            .expect("record truncated stream");
        let mut first_retained = event(10);
        first_retained.event_time = "2026-01-02T00:00:00Z".to_owned();
        store
            .insert_event(&first_retained)
            .expect("project retained event");
        drop(store);

        let reopened = EventsStore::open(&path).expect("reopen store");
        let diagnostics = reopened.diagnostics().expect("read diagnostics");
        assert_eq!(diagnostics["gapDetected"], true);
        assert_eq!(diagnostics["retainedFrom"], "2026-01-02T00:00:00Z");
        assert_eq!(diagnostics["completeSince"], "2026-01-02T00:00:00Z");
    }

    #[test]
    fn retention_gap_is_detected_after_projection_has_started() {
        let store = EventsStore::open_in_memory().expect("open store");
        store.insert_event(&event(1)).expect("project first event");

        store.record_stream_bounds(5).expect("record live gap");
        let mut first_after_gap = event(5);
        first_after_gap.event_time = "2026-01-02T00:00:00Z".to_owned();
        store
            .insert_event(&first_after_gap)
            .expect("project first event after gap");

        let diagnostics = store.diagnostics().expect("read diagnostics");
        assert_eq!(diagnostics["gapDetected"], true);
        assert_eq!(diagnostics["completeSince"], "2026-01-02T00:00:00Z");
    }

    #[test]
    fn event_keyset_uses_sequence_tie_breaker_across_mutation() {
        let store = EventsStore::open_in_memory().expect("open store");
        for sequence in [2, 3, 4] {
            store.insert_event(&event(sequence)).unwrap();
        }
        let mut filter = EventsFilter {
            limit: 2,
            sort_field: "eventTime".to_owned(),
            sort_direction: "desc".to_owned(),
            ..Default::default()
        };
        let (mut first, _) = store.query_events(&filter).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|row| row["streamSequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [4, 3, 2]
        );
        first.pop();
        filter.after = Some((first[1]["eventTime"].as_str().unwrap().to_owned(), 3));
        store.insert_event(&event(5)).unwrap();
        let (second, _) = store.query_events(&filter).unwrap();
        assert_eq!(
            second
                .iter()
                .map(|row| row["streamSequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [2]
        );
    }

    #[test]
    fn payload_size_keyset_uses_sequence_tie_breaker() {
        let store = EventsStore::open_in_memory().expect("open store");
        for (sequence, size) in [(1, 2), (2, 4), (3, 4), (4, 1)] {
            let mut event = event(sequence);
            event.payload_bytes = vec![0; size];
            store.insert_event(&event).expect("insert event");
        }
        let mut filter = EventsFilter {
            limit: 2,
            sort_field: "payloadSize".to_owned(),
            sort_direction: "desc".to_owned(),
            ..Default::default()
        };
        let mut first = store.query_events(&filter).expect("first page").0;
        assert_eq!(
            first
                .iter()
                .map(|row| row["streamSequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [3, 2, 1]
        );
        first.pop();
        filter.after = Some(("4".to_owned(), 2));
        let second = store.query_events(&filter).expect("second page").0;
        assert_eq!(
            second
                .iter()
                .map(|row| row["streamSequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 4]
        );
    }

    #[test]
    fn event_search_treats_percent_and_underscore_as_literals() {
        let store = EventsStore::open_in_memory().expect("open store");
        for (sequence, subject) in [
            (1, "events.v1.Test.percent%literal"),
            (2, "events.v1.Test.underscore_literal"),
            (3, "events.v1.Test.percentXliteral"),
            (4, "events.v1.Test.underscoreXliteral"),
        ] {
            let mut projected = event(sequence);
            projected.subject = subject.to_owned();
            store.insert_event(&projected).expect("insert event");
        }

        for (search, sequence) in [("percent%literal", 1), ("underscore_literal", 2)] {
            let filter = EventsFilter {
                search: Some(search.to_owned()),
                limit: 100,
                ..EventsFilter::default()
            };
            let (rows, total) = store.query_events(&filter).expect("search events");
            assert_eq!(total, 1);
            assert_eq!(rows[0]["streamSequence"], sequence);
        }
    }

    #[test]
    fn event_time_keyset_is_chronological_across_variable_precision_pages() {
        for direction in ["asc", "desc"] {
            let store = EventsStore::open_in_memory().expect("open store");
            for (sequence, timestamp) in [
                (1, "2026-01-01T00:00:00.9Z"),
                (2, "2026-01-01T00:00:00Z"),
                (3, "2026-01-01T00:00:00.1Z"),
                (4, "2026-01-01T00:00:00.1Z"),
            ] {
                let mut event = event(sequence);
                event.event_time = timestamp.to_owned();
                store.insert_event(&event).expect("insert event");
            }

            let mut filter = EventsFilter {
                limit: 2,
                sort_field: "eventTime".to_owned(),
                sort_direction: direction.to_owned(),
                ..Default::default()
            };
            let mut sequences = Vec::new();
            loop {
                let (mut page, _) = store.query_events(&filter).expect("query page");
                let has_next = page.len() > filter.limit as usize;
                if has_next {
                    page.pop();
                }
                sequences.extend(
                    page.iter()
                        .map(|row| row["streamSequence"].as_u64().expect("sequence")),
                );
                if !has_next {
                    break;
                }
                let last = page.last().expect("last page item");
                filter.after = Some((
                    last["eventTime"].as_str().expect("timestamp").to_owned(),
                    last["streamSequence"].as_u64().expect("sequence"),
                ));
            }

            let expected = if direction == "asc" {
                vec![2, 3, 4, 1]
            } else {
                vec![1, 4, 3, 2]
            };
            assert_eq!(sequences, expected);
        }
    }

    #[test]
    fn metrics_window_compares_integer_event_times() {
        let store = EventsStore::open_in_memory().expect("open store");
        for (sequence, event_time) in [
            (1, "2026-01-01T00:30:00+01:00"),
            (2, "2026-01-01T00:30:00Z"),
        ] {
            let mut event = event(sequence);
            event.event_time = event_time.to_owned();
            store.insert_event(&event).expect("insert event");
        }

        let metrics = store
            .metrics(
                Some((
                    timestamp_nanos("2026-01-01T00:00:00Z").expect("window timestamp"),
                    60 * 60,
                    5 * 60,
                )),
                None,
            )
            .expect("query metrics");
        assert_eq!(metrics["summary"]["total"], 1);
    }

    #[test]
    fn dead_letter_time_keyset_is_chronological_and_query_bound() {
        let store = EventsStore::open_in_memory().expect("open store");
        for (sequence, timestamp) in [
            (1, "2026-01-01T00:00:00.9Z"),
            (2, "2026-01-01T00:00:00Z"),
            (3, "2026-01-01T00:00:00.1Z"),
            (4, "2026-01-01T00:00:00.1Z"),
        ] {
            let transition = DeadLetterTransition {
                id: dead_letter_id("consumer-1", "trellis", sequence),
                resource_id: "consumer-1".to_owned(),
                revision: 1,
                previous_subject_sequence: 0,
                generation: 0,
                state: DeadLetterState::Dead,
                occurred_at: timestamp.to_owned(),
                request_id: None,
                request_digest: None,
                cause: DeadLetterCause::Exhausted,
                original: Some(OriginalEvent {
                    stream: "trellis".to_owned(),
                    sequence,
                    event_id: Some(format!("event-{sequence}")),
                    subject: "events.v1.orders.Created".to_owned(),
                    payload_bytes: Vec::new(),
                    headers: BTreeMap::new(),
                    api_id: Some("orders@v1".to_owned()),
                    event_name: Some("Created".to_owned()),
                    context_digest: None,
                    verification_status: "verified".to_owned(),
                }),
                original_record_sequence: 0,
                deliveries: 1,
                last_error: Some("failed".to_owned()),
                replay_stream_sequence: None,
            };
            store
                .project_dead_letter(&transition, sequence)
                .expect("project dead letter");
        }

        let first = store
            .query_dead_letters(Some("consumer-1"), &[], None, 2)
            .expect("first page");
        let cursor = first["page"]["nextCursor"].as_str().expect("cursor");
        let second = store
            .query_dead_letters(Some("consumer-1"), &[], Some(cursor), 2)
            .expect("second page");
        let times = first["items"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(second["items"].as_array().into_iter().flatten())
            .map(|row| row["updatedAt"].as_str().expect("timestamp"))
            .collect::<Vec<_>>();
        assert_eq!(
            times,
            [
                "2026-01-01T00:00:00.9Z",
                "2026-01-01T00:00:00.1Z",
                "2026-01-01T00:00:00.1Z",
                "2026-01-01T00:00:00Z"
            ]
        );
        let ids = first["items"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(second["items"].as_array().into_iter().flatten())
            .map(|row| row["deadLetterId"].as_str().expect("id"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 4);
        assert!(store
            .query_dead_letters(Some("consumer-2"), &[], Some(cursor), 2)
            .is_err());
        let digest = trellis_protocol::pagination_query_digest(
            "events.DeadLetters.Query",
            &serde_json::json!({
                "resourceId": "consumer-1",
                "state": [],
                "order": ["updatedAt:desc", "deadLetterId:asc"]
            }),
        )
        .expect("digest");
        let invalid_cursor = trellis_protocol::encode_pagination_cursor(
            &digest,
            &("not-a-timestamp", "dead-letter"),
        )
        .expect("cursor");
        assert!(store
            .query_dead_letters(Some("consumer-1"), &[], Some(&invalid_cursor), 2)
            .is_err());
    }

    #[test]
    fn event_type_filters_and_metrics_use_resolved_type_pairs() {
        let store = EventsStore::open_in_memory().expect("open store");
        for (sequence, event_name) in [(1, Some("Created")), (2, Some("Updated")), (3, None)] {
            let mut projected = event(sequence);
            projected.owner_contract_id = event_name.map(|_| "test.events@v1".to_string());
            projected.owner_event_name = event_name.map(str::to_string);
            projected.resolution = if event_name.is_some() {
                "resolved"
            } else {
                "unresolved"
            }
            .to_string();
            projected.verification_status = if sequence == 2 {
                "invalid-signature"
            } else {
                "verified"
            }
            .to_string();
            store.insert_event(&projected).expect("insert event");
        }

        let event_type = |name: &str| EventTypeRef {
            owner_contract_id: "test.events@v1".to_string(),
            owner_event_name: name.to_string(),
        };
        let filter = EventsFilter {
            include_event_types: vec![event_type("Created"), event_type("Updated")],
            exclude_event_types: vec![event_type("Updated")],
            limit: 100,
            ..EventsFilter::default()
        };
        let (events, total) = store.query_events(&filter).expect("query included events");
        assert_eq!(total, 1);
        assert_eq!(events[0]["ownerEventName"], "Created");

        let filter = EventsFilter {
            exclude_event_types: vec![event_type("Created")],
            limit: 100,
            ..EventsFilter::default()
        };
        let (_, total) = store.query_events(&filter).expect("query excluded events");
        assert_eq!(
            total, 2,
            "excluding a resolved type keeps unresolved events"
        );

        let metrics = store.metrics(None, None).expect("query metrics");
        assert_eq!(
            metrics["summary"]["eventTypes"].as_array().map(Vec::len),
            Some(2)
        );

        let metrics = store
            .metrics(
                Some((
                    timestamp_nanos("2025-12-31T23:00:00Z").expect("window timestamp"),
                    60 * 60,
                    5 * 60,
                )),
                None,
            )
            .expect("query bucketed metrics");
        let event_bucket = metrics["buckets"]
            .as_array()
            .and_then(|buckets| buckets.iter().find(|bucket| bucket["total"] == 3))
            .expect("find populated bucket");
        assert_eq!(metrics["buckets"].as_array().map(Vec::len), Some(13));
        assert_eq!(metrics["summary"]["integrityExceptions"], 2);
        assert_eq!(event_bucket["integrityExceptions"], 2);
        assert_eq!(event_bucket["byResolution"]["unresolved"], 1);
        assert_eq!(event_bucket["byVerificationStatus"]["invalidSignature"], 1);

        let filter = EventsFilter {
            integrity_exception_only: true,
            limit: 100,
            ..EventsFilter::default()
        };
        let (_, total) = store
            .query_events(&filter)
            .expect("query integrity exceptions");
        assert_eq!(total, 2);
    }

    #[test]
    fn dead_letter_projection_is_ordered_idempotent_and_rebuildable() {
        let store = EventsStore::open_in_memory().expect("open store");
        let id = dead_letter_id("consumer-1", "trellis", 42);
        let first = DeadLetterTransition {
            id: id.clone(),
            resource_id: "consumer-1".to_owned(),
            revision: 1,
            previous_subject_sequence: 0,
            generation: 0,
            state: DeadLetterState::Dead,
            occurred_at: "2026-01-01T00:00:00Z".to_owned(),
            request_id: None,
            request_digest: None,
            cause: DeadLetterCause::Exhausted,
            original: Some(OriginalEvent {
                stream: "trellis".to_owned(),
                sequence: 42,
                event_id: Some("event-42".to_owned()),
                subject: "events.v1.orders.Created".to_owned(),
                payload_bytes: vec![0, 1, 255],
                headers: BTreeMap::from([("proof".to_owned(), vec!["signed".to_owned()])]),
                api_id: Some("orders@v1".to_owned()),
                event_name: Some("Created".to_owned()),
                context_digest: Some("context".to_owned()),
                verification_status: "verified".to_owned(),
            }),
            original_record_sequence: 0,
            deliveries: 3,
            last_error: Some("failed".to_owned()),
            replay_stream_sequence: None,
        };
        store
            .project_dead_letter(&first, 10)
            .expect("project first");
        store
            .project_dead_letter(&first, 10)
            .expect("duplicate is idempotent");

        let second = DeadLetterTransition {
            revision: 2,
            previous_subject_sequence: 10,
            generation: 1,
            state: DeadLetterState::ReplayPending,
            occurred_at: "2026-01-01T00:01:00Z".to_owned(),
            request_id: Some("request-1".to_owned()),
            request_digest: Some("digest-1".to_owned()),
            cause: DeadLetterCause::ReplayRequested,
            original: None,
            original_record_sequence: 10,
            deliveries: 0,
            last_error: None,
            ..first.clone()
        };
        store
            .project_dead_letter(&second, 11)
            .expect("project successor");
        let detail = store
            .inspect_dead_letter(&id)
            .expect("inspect")
            .expect("entry");
        assert_eq!(detail["deadLetter"]["state"], "replayPending");
        assert_eq!(detail["transitionReferences"], serde_json::json!([10, 11]));
        assert_eq!(
            store
                .dead_letter_command(&id, "request-1")
                .expect("query command")
                .expect("projected command")["generation"],
            1
        );

        let illegal = DeadLetterTransition {
            revision: 3,
            previous_subject_sequence: 11,
            generation: 1,
            state: DeadLetterState::Dismissed,
            occurred_at: "2026-01-01T00:02:00Z".to_owned(),
            request_id: Some("request-2".to_owned()),
            request_digest: Some("digest-2".to_owned()),
            cause: DeadLetterCause::Dismissed,
            original: None,
            original_record_sequence: 10,
            deliveries: 0,
            last_error: None,
            replay_stream_sequence: None,
            ..first.clone()
        };
        assert!(store.project_dead_letter(&illegal, 12).is_err());

        let contradictory = DeadLetterTransition {
            revision: 4,
            previous_subject_sequence: 11,
            ..second
        };
        assert!(store.project_dead_letter(&contradictory, 12).is_err());

        store
            .reset_dead_letter_projection()
            .expect("reset projection");
        assert!(store
            .inspect_dead_letter(&id)
            .expect("inspect reset")
            .is_none());
    }
}
