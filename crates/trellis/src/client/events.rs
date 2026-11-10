use async_nats::header::HeaderMap;
use bytes::Bytes;
#[cfg(feature = "event-store-postgres")]
use postgres::Client as PostgresClient;
#[cfg(feature = "event-store-sqlite")]
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use ulid::Ulid;

use crate::client::{subject::SubjectError, EventDescriptor};
use crate::jobs::{OutboxJobOutcome, PreparedJobSubmission};

const OUTBOX_STATUS_PENDING: &str = "pending";
const OUTBOX_STATUS_IN_FLIGHT: &str = "in_flight";
const OUTBOX_STATUS_PUBLISHED: &str = "published";
const OUTBOX_CLAIM_SECONDS: i64 = 30;
#[cfg(any(feature = "event-store-sqlite", feature = "event-store-postgres"))]
const OUTBOX_KIND_EVENT: &str = "event";
#[cfg(any(feature = "event-store-sqlite", feature = "event-store-postgres"))]
const OUTBOX_KIND_JOB: &str = "job";
pub(crate) const EVENT_ID_HEADER: &str = "Nats-Msg-Id";
pub(crate) const EVENT_TIME_HEADER: &str = "Trellis-Event-Time";
pub(crate) const EVENT_DESCRIPTOR_HEADER: &str = "Trellis-Event-Descriptor";

/// A Trellis event prepared for durable storage or later publishing.
///
/// The prepared form stores the event subject, encoded body payload, transport
/// headers, exact descriptor identity, and event metadata separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedTrellisEvent {
    subject: String,
    payload: Bytes,
    headers: HeaderMap,
    event_id: String,
    event_time: String,
    descriptor_identity: String,
}

impl PreparedTrellisEvent {
    /// Restore a durable outbox publication without replacing its ID or timestamp.
    /// Descriptor identity and payload are checked when the event is published.
    pub fn restore(
        subject: impl Into<String>,
        payload: Bytes,
        descriptor_identity: impl Into<String>,
        event_id: impl Into<String>,
        event_time: impl Into<String>,
    ) -> Result<Self, SubjectError> {
        let subject = subject.into();
        let descriptor_identity = descriptor_identity.into();
        let identity = trellis_protocol::decode_event_descriptor_identity(&descriptor_identity)
            .map_err(|error| SubjectError::InvalidPayload(error.to_string()))?;
        trellis_protocol::validate_event_descriptor_subject(&identity, &subject)
            .map_err(|error| SubjectError::InvalidPayload(error.to_string()))?;
        let event_id = event_id.into();
        let event_time = event_time.into();
        if Ulid::from_string(&event_id).is_err()
            || OffsetDateTime::parse(&event_time, &Rfc3339).is_err()
        {
            return Err(SubjectError::InvalidPayload(
                "invalid persisted event metadata".into(),
            ));
        }
        let mut headers = HeaderMap::new();
        headers.insert(EVENT_DESCRIPTOR_HEADER, descriptor_identity.as_str());
        Ok(Self {
            subject,
            payload,
            headers,
            event_id,
            event_time,
            descriptor_identity,
        })
    }
    pub(crate) fn with_subject(mut self, subject: String) -> Self {
        self.subject = subject;
        self
    }
    /// Build a prepared event from an already encoded JSON body payload.
    ///
    /// The event id and event time are generated as metadata and are not written
    /// into the payload bytes.
    pub fn new(
        subject: impl Into<String>,
        payload: Bytes,
        descriptor_identity: impl Into<String>,
    ) -> Self {
        let descriptor_identity = descriptor_identity.into();
        let mut headers = HeaderMap::new();
        headers.insert(EVENT_DESCRIPTOR_HEADER, descriptor_identity.as_str());
        Self {
            subject: subject.into(),
            payload,
            headers,
            event_id: Ulid::generate().to_string(),
            event_time: now_rfc3339(),
            descriptor_identity,
        }
    }

    #[cfg(any(feature = "event-store-sqlite", feature = "event-store-postgres"))]
    fn from_parts(
        subject: String,
        payload: Bytes,
        headers: HeaderMap,
        event_id: String,
        event_time: String,
    ) -> Self {
        let descriptor_identity = headers
            .get(EVENT_DESCRIPTOR_HEADER)
            .map(|value| value.as_str().to_owned())
            .unwrap_or_default();
        Self {
            subject,
            payload,
            headers,
            event_id,
            event_time,
            descriptor_identity,
        }
    }

    /// Return the concrete NATS subject for this event.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Return the encoded JSON event payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Return a cloned payload suitable for NATS publish calls.
    pub fn payload_bytes(&self) -> Bytes {
        self.payload.clone()
    }

    /// Return transport headers preserved with this prepared event.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Replace the transport headers preserved with this prepared event.
    ///
    /// `publish_headers` overlays Trellis event metadata headers on top of these
    /// values so stale `Nats-Msg-Id` or `Trellis-Event-Time` values cannot
    /// override the prepared event metadata.
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self.headers
            .insert(EVENT_DESCRIPTOR_HEADER, self.descriptor_identity.as_str());
        self
    }

    /// Return the Trellis event id used as the `Nats-Msg-Id` publish header.
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    /// Return the Trellis event timestamp propagated as `Trellis-Event-Time`.
    pub fn event_time(&self) -> &str {
        &self.event_time
    }

    /// Return the canonical generated descriptor identity for this event.
    pub fn descriptor_identity(&self) -> &str {
        &self.descriptor_identity
    }

    /// Return the NATS headers required to publish this prepared event.
    pub fn publish_headers(&self) -> HeaderMap {
        let mut headers = self.headers.clone();
        headers.insert(EVENT_ID_HEADER, self.event_id.as_str());
        headers.insert(EVENT_TIME_HEADER, self.event_time.as_str());
        headers.insert(EVENT_DESCRIPTOR_HEADER, self.descriptor_identity.as_str());
        headers
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Prepare one descriptor-backed typed event without publishing it.
pub fn prepare_event<D>(event: &D::Event) -> Result<PreparedTrellisEvent, SubjectError>
where
    D: EventDescriptor,
{
    let value = crate::generated::Codec::encode(event)
        .map_err(|error| SubjectError::InvalidPayload(error.to_string()))?;
    let subject = D::publish_subject(event)?;
    Ok(PreparedTrellisEvent::new(
        subject,
        Bytes::from(serde_json::to_vec(&value)?),
        D::descriptor_identity()?,
    ))
}

/// Prepare one generic JSON-serializable event for a concrete subject.
pub fn prepare_event_value<T>(
    subject: &str,
    descriptor_identity: &str,
    event: &T,
) -> Result<PreparedTrellisEvent, serde_json::Error>
where
    T: Serialize + ?Sized,
{
    Ok(PreparedTrellisEvent::new(
        subject,
        Bytes::from(serde_json::to_vec(event)?),
        descriptor_identity,
    ))
}

/// Errors returned by Trellis event outbox and inbox stores.
#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    /// A stored event has invalid claim metadata.
    #[error("invalid outbox record: {0}")]
    InvalidRecord(String),
    /// JSON encoding or decoding failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// SQLite storage failed.
    #[cfg(feature = "event-store-sqlite")]
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Postgres storage failed.
    #[cfg(feature = "event-store-postgres")]
    #[error("postgres error: {0}")]
    Postgres(#[from] postgres::Error),
    /// Runtime KV storage failed.
    #[error("runtime kv error: {0}")]
    RuntimeKv(String),
    /// A publisher failed while dispatching an outbox event.
    #[error("publish error: {0}")]
    Publish(String),
}

/// One intent an outbox stores inside the application's transaction: an event
/// to publish or a job to submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutboxIntent {
    /// A prepared event, published as is.
    Event(PreparedTrellisEvent),
    /// A prepared job, submitted through its queue's key policy.
    Job(Box<PreparedJobSubmission>),
}

impl From<PreparedTrellisEvent> for OutboxIntent {
    fn from(event: PreparedTrellisEvent) -> Self {
        Self::Event(event)
    }
}

impl From<&PreparedTrellisEvent> for OutboxIntent {
    fn from(event: &PreparedTrellisEvent) -> Self {
        Self::Event(event.clone())
    }
}

impl From<PreparedJobSubmission> for OutboxIntent {
    fn from(job: PreparedJobSubmission) -> Self {
        Self::Job(Box::new(job))
    }
}

impl From<&PreparedJobSubmission> for OutboxIntent {
    fn from(job: &PreparedJobSubmission) -> Self {
        Self::Job(Box::new(job.clone()))
    }
}

/// The columns one outbox intent is stored as. A job row keeps its queue in
/// `subject`, its submission JSON in `payload`, its job ID in `event_id` and
/// its creation time in `event_time`.
#[cfg(any(feature = "event-store-sqlite", feature = "event-store-postgres"))]
struct OutboxRow {
    kind: &'static str,
    subject: String,
    payload: Vec<u8>,
    headers: String,
    event_id: String,
    event_time: String,
}

#[cfg(any(feature = "event-store-sqlite", feature = "event-store-postgres"))]
impl OutboxRow {
    fn encode(intent: &OutboxIntent) -> Result<Self, EventStoreError> {
        Ok(match intent {
            OutboxIntent::Event(event) => Self {
                kind: OUTBOX_KIND_EVENT,
                subject: event.subject().to_owned(),
                payload: event.payload().to_vec(),
                headers: serde_json::to_string(event.headers())?,
                event_id: event.event_id().to_owned(),
                event_time: event.event_time().to_owned(),
            },
            OutboxIntent::Job(job) => Self {
                kind: OUTBOX_KIND_JOB,
                subject: job.queue().to_owned(),
                payload: serde_json::to_vec(job)?,
                headers: "{}".to_owned(),
                event_id: job.job_id().to_owned(),
                event_time: job.created_at().to_owned(),
            },
        })
    }

    fn decode(
        kind: &str,
        subject: String,
        payload: Vec<u8>,
        headers: &str,
        event_id: String,
        event_time: String,
    ) -> Result<OutboxIntent, EventStoreError> {
        match kind {
            OUTBOX_KIND_EVENT => Ok(OutboxIntent::Event(PreparedTrellisEvent::from_parts(
                subject,
                Bytes::from(payload),
                serde_json::from_str(headers)?,
                event_id,
                event_time,
            ))),
            OUTBOX_KIND_JOB => Ok(OutboxIntent::Job(serde_json::from_slice(&payload)?)),
            other => Err(EventStoreError::InvalidRecord(format!(
                "unknown outbox kind '{other}'"
            ))),
        }
    }
}

/// One claimed outbox record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRecord {
    pub id: String,
    pub intent: OutboxIntent,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// Storage abstraction for pending outbox intents.
pub trait OutboxStore {
    /// Store a prepared event or job under a stable application-chosen outbox
    /// id. Storing the same id again keeps the first intent.
    fn enqueue(
        &mut self,
        id: &str,
        intent: impl Into<OutboxIntent>,
    ) -> impl std::future::Future<Output = Result<(), EventStoreError>>;

    /// Atomically claim pending work or a claim whose 30-second lease expired.
    /// The returned attempt number fences completion against later claims.
    fn claim_next(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<OutboxRecord>, EventStoreError>>;

    /// Mark a claimed intent as dispatched.
    /// Returns false when the claim was superseded, without changing the newer claim.
    fn mark_published(
        &mut self,
        record: &OutboxRecord,
    ) -> impl std::future::Future<Output = Result<bool, EventStoreError>>;

    /// Return a claimed intent to pending state after a dispatch failure.
    /// Returns false when the claim was superseded, without changing the newer claim.
    fn mark_failed(
        &mut self,
        record: &OutboxRecord,
        error: &str,
    ) -> impl std::future::Future<Output = Result<bool, EventStoreError>>;
}

/// Delivers claimed outbox intents. A connected service's
/// `outbox_dispatcher()` publishes events and submits jobs through its own
/// bindings.
pub trait OutboxDispatcher {
    /// A transient failure; the intent is retried.
    type Error: std::fmt::Display;

    /// Publish a prepared event and wait for the stream to store it.
    fn publish_event(
        &self,
        event: PreparedTrellisEvent,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>>;

    /// Submit a prepared job. Every outcome, including a key policy's refusal,
    /// is final; only an error is retried.
    fn dispatch_job(
        &self,
        job: PreparedJobSubmission,
    ) -> impl std::future::Future<Output = Result<OutboxJobOutcome, Self::Error>>;
}

/// Storage abstraction for consumer-side duplicate suppression.
pub trait InboxStore {
    /// Record an incoming event id and report whether it was newly accepted.
    fn record_received(
        &mut self,
        event_id: &str,
    ) -> impl std::future::Future<Output = Result<InboxReceipt, EventStoreError>>;
}

/// Result of recording an inbox event id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxReceipt {
    /// This message id was not seen before and should be processed.
    Accepted,
    /// This message id was already recorded and should be suppressed.
    Duplicate,
}

/// Result of one outbox dispatch attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutboxDispatchResult {
    /// No pending intent was available.
    Empty,
    /// One event was published and marked complete.
    Published { id: String },
    /// One job was submitted and marked complete with its queue's outcome.
    JobDispatched {
        id: String,
        outcome: OutboxJobOutcome,
    },
    /// One intent failed and was returned to pending state.
    Failed { id: String, error: String },
    /// A later claim or completion superseded this dispatch attempt.
    ClaimLost { id: String },
}

/// Claim and dispatch at most one outbox intent.
pub async fn dispatch_outbox_once<S, D>(
    store: &mut S,
    dispatcher: &D,
) -> Result<OutboxDispatchResult, EventStoreError>
where
    S: OutboxStore,
    D: OutboxDispatcher,
{
    let Some(record) = store.claim_next().await? else {
        return Ok(OutboxDispatchResult::Empty);
    };
    let dispatched = match record.intent.clone() {
        OutboxIntent::Event(event) => dispatcher.publish_event(event).await.map(|()| None),
        OutboxIntent::Job(job) => dispatcher.dispatch_job(*job).await.map(Some),
    };
    match dispatched {
        Ok(outcome) => {
            if !store.mark_published(&record).await? {
                return Ok(OutboxDispatchResult::ClaimLost { id: record.id });
            }
            Ok(match outcome {
                None => OutboxDispatchResult::Published { id: record.id },
                Some(outcome) => OutboxDispatchResult::JobDispatched {
                    id: record.id,
                    outcome,
                },
            })
        }
        Err(error) => {
            let error = error.to_string();
            if !store.mark_failed(&record, &error).await? {
                return Ok(OutboxDispatchResult::ClaimLost { id: record.id });
            }
            Ok(OutboxDispatchResult::Failed {
                id: record.id,
                error,
            })
        }
    }
}

#[derive(Clone, Debug)]
struct MemoryOutboxEntry {
    record: OutboxRecord,
    status: String,
    claimed_until: i64,
}

/// In-memory outbox useful for tests and single-process prototypes.
#[derive(Debug, Default)]
pub struct MemoryOutboxStore {
    records: HashMap<String, MemoryOutboxEntry>,
}

impl MemoryOutboxStore {
    /// Create an empty in-memory outbox store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a stored record by id, including its current attempt count.
    pub fn record(&self, id: &str) -> Option<&OutboxRecord> {
        self.records.get(id).map(|entry| &entry.record)
    }
}

impl OutboxStore for MemoryOutboxStore {
    async fn enqueue(
        &mut self,
        id: &str,
        intent: impl Into<OutboxIntent>,
    ) -> Result<(), EventStoreError> {
        let intent = intent.into();
        self.records
            .entry(id.to_string())
            .or_insert_with(|| MemoryOutboxEntry {
                record: OutboxRecord {
                    id: id.to_string(),
                    intent,
                    attempts: 0,
                    last_error: None,
                },
                status: OUTBOX_STATUS_PENDING.to_string(),
                claimed_until: 0,
            });
        Ok(())
    }

    async fn claim_next(&mut self) -> Result<Option<OutboxRecord>, EventStoreError> {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let Some((_, entry)) = self.records.iter_mut().find(|(_, entry)| {
            entry.status == OUTBOX_STATUS_PENDING
                || (entry.status == OUTBOX_STATUS_IN_FLIGHT && entry.claimed_until <= now)
        }) else {
            return Ok(None);
        };
        entry.record.attempts = entry.record.attempts.checked_add(1).ok_or_else(|| {
            EventStoreError::InvalidRecord("attempt counter exhausted".to_string())
        })?;
        entry.status = OUTBOX_STATUS_IN_FLIGHT.to_string();
        entry.claimed_until = now + OUTBOX_CLAIM_SECONDS;
        Ok(Some(entry.record.clone()))
    }

    async fn mark_published(&mut self, record: &OutboxRecord) -> Result<bool, EventStoreError> {
        if let Some(entry) = self.records.get_mut(&record.id).filter(|entry| {
            entry.status == OUTBOX_STATUS_IN_FLIGHT && entry.record.attempts == record.attempts
        }) {
            entry.status = OUTBOX_STATUS_PUBLISHED.to_string();
            return Ok(true);
        }
        Ok(false)
    }

    async fn mark_failed(
        &mut self,
        record: &OutboxRecord,
        error: &str,
    ) -> Result<bool, EventStoreError> {
        if let Some(entry) = self.records.get_mut(&record.id).filter(|entry| {
            entry.status == OUTBOX_STATUS_IN_FLIGHT && entry.record.attempts == record.attempts
        }) {
            entry.status = OUTBOX_STATUS_PENDING.to_string();
            entry.record.last_error = Some(error.to_string());
            return Ok(true);
        }
        Ok(false)
    }
}

/// In-memory inbox useful for tests and single-process prototypes.
#[derive(Debug, Default)]
pub struct MemoryInboxStore {
    seen: HashSet<String>,
}

impl MemoryInboxStore {
    /// Create an empty in-memory inbox store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl InboxStore for MemoryInboxStore {
    async fn record_received(&mut self, event_id: &str) -> Result<InboxReceipt, EventStoreError> {
        if self.seen.insert(event_id.to_string()) {
            Ok(InboxReceipt::Accepted)
        } else {
            Ok(InboxReceipt::Duplicate)
        }
    }
}

/// SQLite-backed outbox adapter. Callers own schema migrations.
#[cfg(feature = "event-store-sqlite")]
#[derive(Debug)]
pub struct SqliteOutboxStore<'a> {
    connection: &'a Connection,
}

#[cfg(feature = "event-store-sqlite")]
impl<'a> SqliteOutboxStore<'a> {
    /// Wrap an existing SQLite connection.
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Create a minimal test schema for this adapter.
    pub fn create_schema(connection: &Connection) -> Result<(), EventStoreError> {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS trellis_outbox_events (\
            id TEXT PRIMARY KEY,\
            kind TEXT NOT NULL DEFAULT 'event',\
            subject TEXT NOT NULL,\
            payload BLOB NOT NULL,\
            headers TEXT NOT NULL,\
            event_id TEXT NOT NULL,\
            event_time TEXT NOT NULL,\
            status TEXT NOT NULL,\
            attempts INTEGER NOT NULL DEFAULT 0,\
            claimed_until BIGINT NOT NULL DEFAULT 0,\
            last_error TEXT\
            );",
        )?;
        Ok(())
    }
}

#[cfg(feature = "event-store-sqlite")]
impl OutboxStore for SqliteOutboxStore<'_> {
    async fn enqueue(
        &mut self,
        id: &str,
        intent: impl Into<OutboxIntent>,
    ) -> Result<(), EventStoreError> {
        let row = OutboxRow::encode(&intent.into())?;
        self.connection.execute(
            "INSERT INTO trellis_outbox_events (id, kind, subject, payload, headers, \
            event_id, event_time, status, attempts) \
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0) \
            ON CONFLICT(id) DO NOTHING",
            params![
                id,
                row.kind,
                row.subject,
                row.payload,
                row.headers,
                row.event_id,
                row.event_time,
                OUTBOX_STATUS_PENDING,
            ],
        )?;
        Ok(())
    }

    async fn claim_next(&mut self) -> Result<Option<OutboxRecord>, EventStoreError> {
        let row = self
            .connection
            .query_row(
                "UPDATE trellis_outbox_events \
                SET status = ?1, attempts = attempts + 1, claimed_until = unixepoch() \
                + ?2 \
                WHERE id = (SELECT id \
                FROM trellis_outbox_events \
                WHERE status = ?3 OR (status = ?1 \
                AND claimed_until <= unixepoch()) \
                ORDER BY rowid LIMIT 1) \
                RETURNING id, subject, payload, headers, event_id, event_time, \
                attempts, last_error, kind",
                params![
                    OUTBOX_STATUS_IN_FLIGHT,
                    OUTBOX_CLAIM_SECONDS,
                    OUTBOX_STATUS_PENDING
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, u32>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let (id, subject, payload, headers, event_id, event_time, attempts, last_error, kind) = row;
        Ok(Some(OutboxRecord {
            id,
            intent: OutboxRow::decode(&kind, subject, payload, &headers, event_id, event_time)?,
            attempts,
            last_error,
        }))
    }

    async fn mark_published(&mut self, record: &OutboxRecord) -> Result<bool, EventStoreError> {
        Ok(self.connection.execute(
            "UPDATE trellis_outbox_events \
            SET status = ?1 WHERE id = ?2 \
            AND status = ?3 AND attempts = ?4",
            params![
                OUTBOX_STATUS_PUBLISHED,
                record.id,
                OUTBOX_STATUS_IN_FLIGHT,
                record.attempts
            ],
        )? == 1)
    }

    async fn mark_failed(
        &mut self,
        record: &OutboxRecord,
        error: &str,
    ) -> Result<bool, EventStoreError> {
        Ok(self.connection.execute(
            "UPDATE trellis_outbox_events \
            SET status = ?1, last_error = ?2 \
            WHERE id = ?3 AND status = ?4 \
            AND attempts = ?5",
            params![
                OUTBOX_STATUS_PENDING,
                error,
                record.id,
                OUTBOX_STATUS_IN_FLIGHT,
                record.attempts
            ],
        )? == 1)
    }
}

/// SQLite-backed inbox adapter. Callers own schema migrations.
#[cfg(feature = "event-store-sqlite")]
#[derive(Debug)]
pub struct SqliteInboxStore<'a> {
    connection: &'a Connection,
}

#[cfg(feature = "event-store-sqlite")]
impl<'a> SqliteInboxStore<'a> {
    /// Wrap an existing SQLite connection.
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    /// Create a minimal test schema for this adapter.
    pub fn create_schema(connection: &Connection) -> Result<(), EventStoreError> {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS trellis_inbox_events (\
            event_id TEXT PRIMARY KEY\
            );",
        )?;
        Ok(())
    }
}

#[cfg(feature = "event-store-sqlite")]
impl InboxStore for SqliteInboxStore<'_> {
    async fn record_received(&mut self, event_id: &str) -> Result<InboxReceipt, EventStoreError> {
        let inserted = self.connection.execute(
            "INSERT INTO trellis_inbox_events (event_id) \
            VALUES (?1) ON CONFLICT(event_id) \
            DO NOTHING",
            params![event_id],
        )?;
        if inserted == 0 {
            Ok(InboxReceipt::Duplicate)
        } else {
            Ok(InboxReceipt::Accepted)
        }
    }
}

/// Postgres-backed outbox adapter. Callers own schema migrations.
#[cfg(feature = "event-store-postgres")]
pub struct PostgresOutboxStore<'a> {
    client: &'a mut PostgresClient,
}

#[cfg(feature = "event-store-postgres")]
impl std::fmt::Debug for PostgresOutboxStore<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresOutboxStore")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "event-store-postgres")]
impl<'a> PostgresOutboxStore<'a> {
    /// Wrap an existing Postgres client.
    pub fn new(client: &'a mut PostgresClient) -> Self {
        Self { client }
    }

    /// Create a minimal test schema for this adapter.
    pub fn create_schema(client: &mut PostgresClient) -> Result<(), EventStoreError> {
        client.batch_execute(
            "CREATE TABLE IF NOT EXISTS trellis_outbox_events (\
            id TEXT PRIMARY KEY,\
            kind TEXT NOT NULL DEFAULT 'event',\
            subject TEXT NOT NULL,\
            payload BYTEA NOT NULL,\
            headers TEXT NOT NULL,\
            event_id TEXT NOT NULL,\
            event_time TEXT NOT NULL,\
            status TEXT NOT NULL,\
            attempts INTEGER NOT NULL DEFAULT 0,\
            claimed_until BIGINT NOT NULL DEFAULT 0,\
            last_error TEXT\
            );",
        )?;
        Ok(())
    }
}

#[cfg(feature = "event-store-postgres")]
impl OutboxStore for PostgresOutboxStore<'_> {
    async fn enqueue(
        &mut self,
        id: &str,
        intent: impl Into<OutboxIntent>,
    ) -> Result<(), EventStoreError> {
        let row = OutboxRow::encode(&intent.into())?;
        self.client.execute(
            "INSERT INTO trellis_outbox_events (id, kind, subject, payload, headers, \
            event_id, event_time, status, attempts) \
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0) \
            ON CONFLICT(id) DO NOTHING",
            &[
                &id,
                &row.kind,
                &row.subject,
                &row.payload,
                &row.headers,
                &row.event_id,
                &row.event_time,
                &OUTBOX_STATUS_PENDING,
            ],
        )?;
        Ok(())
    }

    async fn claim_next(&mut self) -> Result<Option<OutboxRecord>, EventStoreError> {
        let row = self.client.query_opt(
            "UPDATE trellis_outbox_events \
            SET status = $1, attempts = attempts + 1, claimed_until = \
            EXTRACT(EPOCH \
            FROM clock_timestamp())::bigint + $2 \
            WHERE id = (SELECT id \
            FROM trellis_outbox_events \
            WHERE status = $3 OR (status = $1 \
            AND claimed_until <= EXTRACT(EPOCH \
            FROM clock_timestamp())::bigint) \
            ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED) \
            RETURNING id, subject, payload, headers, event_id, event_time, \
            attempts, last_error, kind",
            &[
                &OUTBOX_STATUS_IN_FLIGHT,
                &OUTBOX_CLAIM_SECONDS,
                &OUTBOX_STATUS_PENDING,
            ],
        )?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: String = row.get(0);
        let attempts = u32::try_from(row.get::<_, i32>(6))
            .map_err(|error| EventStoreError::InvalidRecord(error.to_string()))?;
        let header_json: String = row.get(3);
        let kind: String = row.get(8);
        Ok(Some(OutboxRecord {
            id,
            intent: OutboxRow::decode(
                &kind,
                row.get(1),
                row.get(2),
                &header_json,
                row.get(4),
                row.get(5),
            )?,
            attempts,
            last_error: row.get(7),
        }))
    }

    async fn mark_published(&mut self, record: &OutboxRecord) -> Result<bool, EventStoreError> {
        Ok(self.client.execute(
            "UPDATE trellis_outbox_events \
            SET status = $1 WHERE id = $2 \
            AND status = $3 AND attempts = $4",
            &[
                &OUTBOX_STATUS_PUBLISHED,
                &record.id,
                &OUTBOX_STATUS_IN_FLIGHT,
                &(record.attempts as i32),
            ],
        )? == 1)
    }

    async fn mark_failed(
        &mut self,
        record: &OutboxRecord,
        error: &str,
    ) -> Result<bool, EventStoreError> {
        Ok(self.client.execute(
            "UPDATE trellis_outbox_events \
            SET status = $1, last_error = $2 \
            WHERE id = $3 AND status = $4 \
            AND attempts = $5",
            &[
                &OUTBOX_STATUS_PENDING,
                &error,
                &record.id,
                &OUTBOX_STATUS_IN_FLIGHT,
                &(record.attempts as i32),
            ],
        )? == 1)
    }
}

/// Postgres-backed inbox adapter. Callers own schema migrations.
#[cfg(feature = "event-store-postgres")]
pub struct PostgresInboxStore<'a> {
    client: &'a mut PostgresClient,
}

#[cfg(feature = "event-store-postgres")]
impl std::fmt::Debug for PostgresInboxStore<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PostgresInboxStore")
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "event-store-postgres")]
impl<'a> PostgresInboxStore<'a> {
    /// Wrap an existing Postgres client.
    pub fn new(client: &'a mut PostgresClient) -> Self {
        Self { client }
    }

    /// Create a minimal test schema for this adapter.
    pub fn create_schema(client: &mut PostgresClient) -> Result<(), EventStoreError> {
        client.batch_execute(
            "CREATE TABLE IF NOT EXISTS trellis_inbox_events (\
            event_id TEXT PRIMARY KEY\
            );",
        )?;
        Ok(())
    }
}

#[cfg(feature = "event-store-postgres")]
impl InboxStore for PostgresInboxStore<'_> {
    async fn record_received(&mut self, event_id: &str) -> Result<InboxReceipt, EventStoreError> {
        let inserted = self.client.execute(
            "INSERT INTO trellis_inbox_events (event_id) \
            VALUES ($1) ON CONFLICT(event_id) \
            DO NOTHING",
            &[&event_id],
        )?;
        if inserted == 0 {
            Ok(InboxReceipt::Duplicate)
        } else {
            Ok(InboxReceipt::Accepted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

    #[tokio::test]
    #[cfg(feature = "event-store-sqlite")]
    async fn sqlite_outbox_claims_recover_and_fence_concurrent_workers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("outbox.db");
        let first = Connection::open(&path).unwrap();
        SqliteOutboxStore::create_schema(&first).unwrap();
        let second = Connection::open(&path).unwrap();
        SqliteOutboxStore::new(&first)
            .enqueue(
                "event",
                &PreparedTrellisEvent::new(
                    "events.v1.Created",
                    Bytes::from_static(&[1, 2, 3]),
                    "test-event-descriptor",
                ),
            )
            .await
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let [left, right] = [first, second].map(|connection| {
            let barrier = barrier.clone();
            tokio::task::spawn_blocking(move || {
                let mut store = SqliteOutboxStore::new(&connection);
                barrier.wait();
                tokio::runtime::Handle::current()
                    .block_on(store.claim_next())
                    .unwrap()
            })
        });
        let claims = left
            .await
            .unwrap()
            .into_iter()
            .chain(right.await.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(claims.len(), 1);
        // Reopen persistent storage after abandoning the only acquired claim.
        let connection = Connection::open(&path).unwrap();
        let mut recovered = SqliteOutboxStore::new(&connection);
        assert!(recovered.claim_next().await.unwrap().is_none());
        recovered
            .connection
            .execute("UPDATE trellis_outbox_events SET claimed_until = 0", [])
            .unwrap();
        let current = recovered.claim_next().await.unwrap().unwrap();
        assert!(!recovered.mark_published(&claims[0]).await.unwrap());
        assert!(!recovered
            .mark_failed(&claims[0], "stale worker")
            .await
            .unwrap());
        assert!(recovered.claim_next().await.unwrap().is_none());
        assert!(recovered.mark_published(&current).await.unwrap());
        assert!(recovered.claim_next().await.unwrap().is_none());
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct TestEvent {
        header: String,
        value: String,
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct TestEventWithoutDomainHeader {
        value: String,
    }

    struct TestDescriptorWithoutDomainHeader;

    impl EventDescriptor for TestDescriptorWithoutDomainHeader {
        type Event = TestEventWithoutDomainHeader;

        const API_ID: &'static str = "test@v1";
        const DESCRIPTOR_NAME: &'static str = "event.EventWithoutDomainHeader";
        const KEY: &'static str = "Test.EventWithoutDomainHeader";
        const SUBJECT: &'static str = "events.v1.Test.EventWithoutDomainHeader";
        const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[];
    }

    struct TestDescriptor;

    impl EventDescriptor for TestDescriptor {
        type Event = TestEvent;

        const API_ID: &'static str = "test@v1";
        const DESCRIPTOR_NAME: &'static str = "event.Event";
        const KEY: &'static str = "Test.Event";
        const SUBJECT: &'static str = "events.v1.Test.Event";
        const PUBLISH_CAPABILITIES: &'static [&'static str] = &[];
        const SUBSCRIBE_CAPABILITIES: &'static [&'static str] = &[];
    }

    /// Fails its first `failures` deliveries, then delivers.
    struct FlakyDispatcher {
        failures: std::cell::Cell<u32>,
    }

    impl FlakyDispatcher {
        fn failing(failures: u32) -> Self {
            Self {
                failures: std::cell::Cell::new(failures),
            }
        }

        fn deliver(&self) -> Result<(), &'static str> {
            let remaining = self.failures.get();
            if remaining == 0 {
                return Ok(());
            }
            self.failures.set(remaining - 1);
            Err("temporary")
        }
    }

    impl OutboxDispatcher for FlakyDispatcher {
        type Error = &'static str;

        async fn publish_event(&self, _event: PreparedTrellisEvent) -> Result<(), &'static str> {
            self.deliver()
        }

        async fn dispatch_job(
            &self,
            job: PreparedJobSubmission,
        ) -> Result<OutboxJobOutcome, &'static str> {
            self.deliver()?;
            Ok(OutboxJobOutcome::Accepted {
                job: crate::jobs::JobIdentity {
                    service: "test".into(),
                    job_type: job.queue().into(),
                    id: job.job_id().into(),
                },
                key: None,
            })
        }
    }

    #[cfg(feature = "event-store-sqlite")]
    fn test_job() -> PreparedJobSubmission {
        serde_json::from_value(serde_json::json!({
            "queue": "refresh",
            "jobId": "01JOB0000000000000000000000",
            "payload": { "orderId": "o-1" },
            "createdAt": "2026-11-10T12:00:00Z",
            "context": {
                "requestId": "req-1",
                "traceId": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "spanId": "bbbbbbbbbbbbbbbb",
                "traceparent": "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01"
            }
        }))
        .expect("prepared job decodes")
    }

    fn test_event() -> TestEvent {
        TestEvent {
            header: "domain-header".to_string(),
            value: "payload".to_string(),
        }
    }

    #[test]
    fn prepare_event_preserves_subject_and_body_payload() {
        let prepared = prepare_event::<TestDescriptor>(&test_event()).expect("event prepares");
        assert_eq!(prepared.subject(), "events.v1.Test.Event");
        assert!(!prepared.event_id().is_empty());
        assert!(prepared.event_time().ends_with('Z'));
        assert_eq!(
            serde_json::from_slice::<Value>(prepared.payload()).expect("payload is json"),
            serde_json::json!({
                "header": "domain-header",
                "value": "payload"
            })
        );
        let headers = prepared.publish_headers();
        assert_eq!(
            headers.get(EVENT_ID_HEADER).map(|value| value.as_str()),
            Some(prepared.event_id())
        );
        assert_eq!(
            headers.get(EVENT_TIME_HEADER).map(|value| value.as_str()),
            Some(prepared.event_time())
        );
    }

    #[test]
    fn prepare_event_does_not_add_header_when_missing() {
        let prepared =
            prepare_event::<TestDescriptorWithoutDomainHeader>(&TestEventWithoutDomainHeader {
                value: "payload".to_string(),
            })
            .expect("event prepares");
        let payload = serde_json::from_slice::<Value>(prepared.payload()).expect("payload is json");
        assert!(payload.get("header").is_none());
        assert!(!prepared.event_id().is_empty());
        assert!(prepared.event_time().ends_with('Z'));
    }

    #[test]
    fn publish_headers_preserve_existing_headers_and_overlay_metadata() {
        let mut existing = HeaderMap::new();
        existing.insert(
            "traceparent",
            "00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01",
        );
        existing.insert(EVENT_ID_HEADER, "stale");
        let prepared = prepare_event::<TestDescriptor>(&test_event())
            .expect("event prepares")
            .with_headers(existing);

        let headers = prepared.publish_headers();
        assert_eq!(
            headers.get("traceparent").map(|value| value.as_str()),
            Some("00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-01")
        );
        assert_eq!(
            headers.get(EVENT_ID_HEADER).map(|value| value.as_str()),
            Some(prepared.event_id())
        );
        assert_eq!(
            headers.get(EVENT_TIME_HEADER).map(|value| value.as_str()),
            Some(prepared.event_time())
        );
    }

    #[tokio::test]
    async fn outbox_dispatch_retries_failed_publish() {
        let prepared = prepare_event::<TestDescriptor>(&test_event()).expect("event prepares");
        let mut store = MemoryOutboxStore::new();
        store
            .enqueue("outbox_1", &prepared)
            .await
            .expect("enqueue succeeds");

        let dispatcher = FlakyDispatcher::failing(1);
        let failed = dispatch_outbox_once(&mut store, &dispatcher)
            .await
            .expect("dispatch returns failure result");
        assert_eq!(
            failed,
            OutboxDispatchResult::Failed {
                id: "outbox_1".to_string(),
                error: "temporary".to_string()
            }
        );
        assert_eq!(
            store.record("outbox_1").map(|record| record.attempts),
            Some(1)
        );

        let published = dispatch_outbox_once(&mut store, &dispatcher)
            .await
            .expect("dispatch publishes retry");
        assert_eq!(
            published,
            OutboxDispatchResult::Published {
                id: "outbox_1".to_string()
            }
        );
        assert_eq!(
            store.record("outbox_1").map(|record| record.attempts),
            Some(2)
        );
    }

    #[tokio::test]
    async fn inbox_suppresses_duplicates() {
        let mut inbox = MemoryInboxStore::new();
        assert_eq!(
            inbox.record_received("evt_1").await.expect("first record"),
            InboxReceipt::Accepted
        );
        assert_eq!(
            inbox.record_received("evt_1").await.expect("second record"),
            InboxReceipt::Duplicate
        );
    }

    #[tokio::test]
    #[cfg(feature = "event-store-sqlite")]
    async fn sqlite_outbox_persists_dispatch_and_retry_state() {
        let connection = Connection::open_in_memory().expect("sqlite opens");
        SqliteOutboxStore::create_schema(&connection).expect("schema creates");
        let prepared = prepare_event::<TestDescriptor>(&test_event()).expect("event prepares");

        let mut store = SqliteOutboxStore::new(&connection);
        store
            .enqueue("sqlite-outbox", &prepared)
            .await
            .expect("enqueue succeeds");

        let failed = dispatch_outbox_once(&mut store, &FlakyDispatcher::failing(1))
            .await
            .expect("dispatch returns failure result");
        assert_eq!(
            failed,
            OutboxDispatchResult::Failed {
                id: "sqlite-outbox".to_string(),
                error: "temporary".to_string()
            }
        );

        let retried = store
            .claim_next()
            .await
            .expect("retry can claim")
            .expect("failed message is pending again");
        assert_eq!(retried.id, "sqlite-outbox");
        assert_eq!(retried.attempts, 2);
        assert_eq!(retried.last_error.as_deref(), Some("temporary"));
        assert_eq!(retried.intent, OutboxIntent::Event(prepared));
        store
            .mark_published(&retried)
            .await
            .expect("mark published succeeds");
        assert!(store
            .claim_next()
            .await
            .expect("claim after publish succeeds")
            .is_none());
    }

    #[tokio::test]
    #[cfg(feature = "event-store-sqlite")]
    async fn sqlite_outbox_returns_a_stored_job_intact_and_reports_its_outcome() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("outbox.db");
        let job = test_job();
        {
            let connection = Connection::open(&path).unwrap();
            SqliteOutboxStore::create_schema(&connection).unwrap();
            SqliteOutboxStore::new(&connection)
                .enqueue("order-o-1-refresh", &job)
                .await
                .unwrap();
        }
        // A restarted dispatcher reads back exactly the job the transaction stored.
        let connection = Connection::open(&path).unwrap();
        let mut store = SqliteOutboxStore::new(&connection);
        let claimed = store.claim_next().await.unwrap().unwrap();
        assert_eq!(claimed.intent, OutboxIntent::from(&job));
        assert!(store.mark_failed(&claimed, "temporary").await.unwrap());
        assert_eq!(
            dispatch_outbox_once(&mut store, &FlakyDispatcher::failing(0))
                .await
                .unwrap(),
            OutboxDispatchResult::JobDispatched {
                id: "order-o-1-refresh".into(),
                outcome: OutboxJobOutcome::Accepted {
                    job: crate::jobs::JobIdentity {
                        service: "test".into(),
                        job_type: "refresh".into(),
                        id: job.job_id().into(),
                    },
                    key: None,
                },
            }
        );
        assert!(store.claim_next().await.unwrap().is_none());
    }

    #[tokio::test]
    #[cfg(feature = "event-store-sqlite")]
    async fn sqlite_inbox_suppresses_duplicates() {
        let connection = Connection::open_in_memory().expect("sqlite opens");
        SqliteInboxStore::create_schema(&connection).expect("schema creates");
        let mut inbox = SqliteInboxStore::new(&connection);

        assert_eq!(
            inbox
                .record_received("evt_sqlite")
                .await
                .expect("first record succeeds"),
            InboxReceipt::Accepted
        );
        assert_eq!(
            inbox
                .record_received("evt_sqlite")
                .await
                .expect("duplicate record succeeds"),
            InboxReceipt::Duplicate
        );
    }
}
