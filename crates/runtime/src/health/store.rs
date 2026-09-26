use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use trellis_runtime_apis::__types::trellis::HealthHeartbeatSample;
use trellis_runtime_apis::types::{
    HealthStatusChangedEvent, HealthStatusChangedEventheader as HealthStatusChangedEventHeader,
    HealthStatusChangedEventparticipant as HealthStatusChangedEventParticipant,
};
use ulid::Ulid;

const METRIC_BUCKET_NS: i64 = 5 * 60 * 1_000_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HeartbeatIdentity {
    pub(crate) session_key: String,
    pub(crate) participant_kind: String,
    pub(crate) contract_id: String,
    pub(crate) contract_digest: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthChange {
    pub(crate) participant_kind: String,
    pub(crate) contract_id: String,
    pub(crate) deployment_id: String,
    pub(crate) instance_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectionCommit {
    pub(crate) revision: i64,
    pub(crate) changes: Vec<HealthChange>,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingTransition {
    pub(crate) event_id: String,
    pub(crate) created_at_ns: i64,
    pub(crate) payload: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct HealthStore {
    pub(super) connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Error)]
pub(crate) enum HealthStoreError {
    #[error("health SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("health JSON operation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("health store lock is poisoned")]
    Poisoned,
    #[error("health heartbeat deadline overflow")]
    DeadlineOverflow,
    #[error("health timestamp is outside the supported range")]
    TimestampRange,
    #[error("health pagination cursor or limit is invalid")]
    InvalidPagination,
}

#[derive(Debug)]
struct CurrentInstance {
    participant_name: String,
    reported_status: String,
    effective_status: String,
    observed_at_ns: i64,
    heartbeat_deadline_ns: i64,
    checks_json: String,
}

struct IntervalRow<'a> {
    started_at_ns: i64,
    reported_status: &'a str,
    effective_status: &'a str,
    checks_json: &'a str,
    reason: &'a str,
}

struct TransitionRow<'a> {
    participant_name: &'a str,
    previous_status: &'a str,
    status: &'a str,
    reported_status: &'a str,
    reason: &'a str,
    changed_at_ns: i64,
    last_seen_at_ns: i64,
    summary: Option<&'a str>,
}

impl HealthStore {
    pub(crate) fn new(connection: Connection) -> Result<Self, HealthStoreError> {
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
        };
        store.ensure_projection_meta()?;
        Ok(store)
    }

    pub(crate) fn projection_id(&self) -> Result<String, HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        Ok(connection.query_row(
            "SELECT projection_id FROM health_projection_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn project_sample(
        &self,
        identity: &HeartbeatIdentity,
        sample: &HealthHeartbeatSample,
        observed_at_ns: i64,
        projected_at_ns: i64,
        stream_sequence: u64,
    ) -> Result<Option<ProjectionCommit>, HealthStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let last_sequence: i64 = transaction.query_row(
            "SELECT last_stream_sequence FROM health_projection_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let stream_sequence = i64::try_from(stream_sequence).unwrap_or(i64::MAX);
        if stream_sequence <= last_sequence {
            transaction.commit()?;
            return Ok(None);
        }

        let interval_ns = sample
            .participant
            .publish_interval_ms
            .0
             .0
            .checked_mul(2)
            .and_then(|value| value.checked_mul(1_000_000))
            .ok_or(HealthStoreError::DeadlineOverflow)?;
        let deadline_ns = observed_at_ns
            .checked_add(interval_ns)
            .ok_or(HealthStoreError::DeadlineOverflow)?;
        let checks_json = serde_json::to_string(&sample.checks)?;
        let sample_json = serde_json::to_string(sample)?;
        let current = load_current(&transaction, identity)?;

        if let Some(current) = &current {
            if observed_at_ns <= current.observed_at_ns {
                update_meta_sequence(&transaction, last_sequence, stream_sequence, observed_at_ns)?;
                transaction.commit()?;
                return Ok(None);
            }
        }

        if let Some(current) = &current {
            if current.effective_status != "offline"
                && observed_at_ns > current.heartbeat_deadline_ns
            {
                close_interval(&transaction, identity, current.heartbeat_deadline_ns)?;
                insert_interval(
                    &transaction,
                    identity,
                    IntervalRow {
                        started_at_ns: current.heartbeat_deadline_ns,
                        reported_status: &current.reported_status,
                        effective_status: "offline",
                        checks_json: &current.checks_json,
                        reason: "deadline-expired",
                    },
                )?;
                insert_transition(
                    &transaction,
                    identity,
                    TransitionRow {
                        participant_name: &current.participant_name,
                        previous_status: &current.effective_status,
                        status: "offline",
                        reported_status: &current.reported_status,
                        reason: "deadline-expired",
                        changed_at_ns: current.heartbeat_deadline_ns,
                        last_seen_at_ns: current.observed_at_ns,
                        summary: None,
                    },
                )?;
                close_interval(&transaction, identity, observed_at_ns)?;
                insert_interval(
                    &transaction,
                    identity,
                    IntervalRow {
                        started_at_ns: observed_at_ns,
                        reported_status: sample.reported_status.as_str(),
                        effective_status: sample.reported_status.as_str(),
                        checks_json: &checks_json,
                        reason: "heartbeat-resumed",
                    },
                )?;
                insert_transition(
                    &transaction,
                    identity,
                    TransitionRow {
                        participant_name: &sample.participant.name,
                        previous_status: "offline",
                        status: sample.reported_status.as_str(),
                        reported_status: sample.reported_status.as_str(),
                        reason: "heartbeat-resumed",
                        changed_at_ns: observed_at_ns,
                        last_seen_at_ns: observed_at_ns,
                        summary: sample.summary.as_ref().map(AsRef::<str>::as_ref),
                    },
                )?;
            } else if current.effective_status == "offline" {
                close_interval(&transaction, identity, observed_at_ns)?;
                insert_interval(
                    &transaction,
                    identity,
                    IntervalRow {
                        started_at_ns: observed_at_ns,
                        reported_status: sample.reported_status.as_str(),
                        effective_status: sample.reported_status.as_str(),
                        checks_json: &checks_json,
                        reason: "heartbeat-resumed",
                    },
                )?;
                insert_transition(
                    &transaction,
                    identity,
                    TransitionRow {
                        participant_name: &sample.participant.name,
                        previous_status: "offline",
                        status: sample.reported_status.as_str(),
                        reported_status: sample.reported_status.as_str(),
                        reason: "heartbeat-resumed",
                        changed_at_ns: observed_at_ns,
                        last_seen_at_ns: observed_at_ns,
                        summary: sample.summary.as_ref().map(AsRef::<str>::as_ref),
                    },
                )?;
            } else if current.effective_status != sample.reported_status.as_str()
                || current.checks_json != checks_json
            {
                close_interval(&transaction, identity, observed_at_ns)?;
                insert_interval(
                    &transaction,
                    identity,
                    IntervalRow {
                        started_at_ns: observed_at_ns,
                        reported_status: sample.reported_status.as_str(),
                        effective_status: sample.reported_status.as_str(),
                        checks_json: &checks_json,
                        reason: "heartbeat-change",
                    },
                )?;
                if current.effective_status != sample.reported_status.as_str() {
                    insert_transition(
                        &transaction,
                        identity,
                        TransitionRow {
                            participant_name: &sample.participant.name,
                            previous_status: &current.effective_status,
                            status: sample.reported_status.as_str(),
                            reported_status: sample.reported_status.as_str(),
                            reason: "heartbeat-change",
                            changed_at_ns: observed_at_ns,
                            last_seen_at_ns: observed_at_ns,
                            summary: sample.summary.as_ref().map(AsRef::<str>::as_ref),
                        },
                    )?;
                }
            }
        } else {
            insert_interval(
                &transaction,
                identity,
                IntervalRow {
                    started_at_ns: observed_at_ns,
                    reported_status: sample.reported_status.as_str(),
                    effective_status: sample.reported_status.as_str(),
                    checks_json: &checks_json,
                    reason: "first-sample",
                },
            )?;
        }

        transaction.execute(
            "INSERT INTO health_latest (
                participant_kind, contract_id, instance_id, deployment_id, session_key,
                participant_name, contract_digest, reported_status, effective_status,
                observed_at_ns, projected_at_ns, heartbeat_deadline_ns, started_at,
                publish_interval_ms, runtime, runtime_version, version, latest_sample_json,
                stream_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT (participant_kind, contract_id, instance_id) DO UPDATE SET
                deployment_id = excluded.deployment_id,
                session_key = excluded.session_key,
                participant_name = excluded.participant_name,
                contract_digest = excluded.contract_digest,
                reported_status = excluded.reported_status,
                effective_status = excluded.effective_status,
                observed_at_ns = excluded.observed_at_ns,
                projected_at_ns = excluded.projected_at_ns,
                heartbeat_deadline_ns = excluded.heartbeat_deadline_ns,
                started_at = excluded.started_at,
                publish_interval_ms = excluded.publish_interval_ms,
                runtime = excluded.runtime,
                runtime_version = excluded.runtime_version,
                version = excluded.version,
                latest_sample_json = excluded.latest_sample_json,
                stream_sequence = excluded.stream_sequence",
            params![
                identity.participant_kind,
                identity.contract_id,
                identity.instance_id,
                identity.deployment_id,
                identity.session_key,
                sample.participant.name.as_ref(),
                identity.contract_digest,
                sample.reported_status.as_str(),
                observed_at_ns,
                projected_at_ns,
                deadline_ns,
                sample.participant.started_at.as_str(),
                sample.participant.publish_interval_ms.0 .0,
                sample.participant.runtime.as_str(),
                sample
                    .participant
                    .runtime_version
                    .as_ref()
                    .map(AsRef::<str>::as_ref),
                sample
                    .participant
                    .version
                    .as_ref()
                    .map(AsRef::<str>::as_ref),
                sample_json,
                stream_sequence,
            ],
        )?;
        update_metric_buckets(&transaction, identity, sample, observed_at_ns)?;
        update_meta_sequence(&transaction, last_sequence, stream_sequence, observed_at_ns)?;
        transaction.execute(
            "UPDATE health_projection_meta SET revision = revision + 1 WHERE singleton = 1",
            [],
        )?;
        let revision = transaction.query_row(
            "SELECT revision FROM health_projection_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some(ProjectionCommit {
            revision,
            changes: vec![HealthChange {
                participant_kind: identity.participant_kind.clone(),
                contract_id: identity.contract_id.clone(),
                deployment_id: identity.deployment_id.clone(),
                instance_id: identity.instance_id.clone(),
            }],
        }))
    }

    pub(crate) fn expire_due(
        &self,
        now_ns: i64,
    ) -> Result<Option<ProjectionCommit>, HealthStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let due = {
            let mut statement = transaction.prepare(
                "SELECT participant_kind, contract_id, instance_id, deployment_id,
                        participant_name, reported_status, effective_status, observed_at_ns,
                        heartbeat_deadline_ns
                 FROM health_latest
                 WHERE effective_status != 'offline' AND heartbeat_deadline_ns <= ?1
                 ORDER BY heartbeat_deadline_ns, participant_kind, contract_id, instance_id",
            )?;
            let rows = statement
                .query_map([now_ns], |row| {
                    Ok((
                        HeartbeatIdentity {
                            participant_kind: row.get(0)?,
                            contract_id: row.get(1)?,
                            instance_id: row.get(2)?,
                            deployment_id: row.get(3)?,
                            session_key: String::new(),
                            contract_digest: String::new(),
                        },
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        if due.is_empty() {
            transaction.commit()?;
            return Ok(None);
        }

        let mut changes = Vec::with_capacity(due.len());
        for (
            identity,
            participant_name,
            reported_status,
            effective_status,
            observed_at,
            deadline,
        ) in due
        {
            close_interval(&transaction, &identity, deadline)?;
            insert_interval(
                &transaction,
                &identity,
                IntervalRow {
                    started_at_ns: deadline,
                    reported_status: &reported_status,
                    effective_status: "offline",
                    checks_json: "[]",
                    reason: "deadline-expired",
                },
            )?;
            insert_transition(
                &transaction,
                &identity,
                TransitionRow {
                    participant_name: &participant_name,
                    previous_status: &effective_status,
                    status: "offline",
                    reported_status: &reported_status,
                    reason: "deadline-expired",
                    changed_at_ns: deadline,
                    last_seen_at_ns: observed_at,
                    summary: None,
                },
            )?;
            transaction.execute(
                "UPDATE health_latest SET effective_status = 'offline'
                 WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3",
                params![
                    identity.participant_kind,
                    identity.contract_id,
                    identity.instance_id
                ],
            )?;
            changes.push(HealthChange {
                participant_kind: identity.participant_kind,
                contract_id: identity.contract_id,
                deployment_id: identity.deployment_id,
                instance_id: identity.instance_id,
            });
        }
        transaction.execute(
            "UPDATE health_projection_meta SET revision = revision + 1 WHERE singleton = 1",
            [],
        )?;
        let revision = transaction.query_row(
            "SELECT revision FROM health_projection_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some(ProjectionCommit { revision, changes }))
    }

    pub(crate) fn pending_transitions(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingTransition>, HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT event_id, payload_json, created_at_ns FROM health_transition_outbox
             WHERE published_at_ns IS NULL ORDER BY created_at_ns LIMIT ?1",
        )?;
        let transitions = statement
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                let payload: String = row.get(1)?;
                Ok(PendingTransition {
                    event_id: row.get(0)?,
                    created_at_ns: row.get(2)?,
                    payload: payload.into_bytes(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(transitions)
    }

    pub(crate) fn mark_transition_published(
        &self,
        event_id: &str,
        published_at_ns: i64,
    ) -> Result<(), HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        connection.execute(
            "UPDATE health_transition_outbox SET published_at_ns = ?2, attempts = attempts + 1,
                    last_error = NULL WHERE event_id = ?1",
            params![event_id, published_at_ns],
        )?;
        Ok(())
    }

    pub(crate) fn mark_transition_failed(
        &self,
        event_id: &str,
        error: &str,
    ) -> Result<(), HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        connection.execute(
            "UPDATE health_transition_outbox SET attempts = attempts + 1, last_error = ?2
             WHERE event_id = ?1",
            params![event_id, error],
        )?;
        Ok(())
    }

    pub(crate) fn record_rejection(
        &self,
        stream_sequence: u64,
        subject: &str,
        observed_at_ns: i64,
        reason: &str,
    ) -> Result<(), HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        connection.execute(
            "INSERT OR IGNORE INTO health_rejections
             (stream_sequence, subject, observed_at_ns, reason) VALUES (?1, ?2, ?3, ?4)",
            params![
                i64::try_from(stream_sequence).unwrap_or(i64::MAX),
                subject,
                observed_at_ns,
                reason
            ],
        )?;
        Ok(())
    }

    pub(crate) fn cleanup(
        &self,
        cutoff_ns: i64,
    ) -> Result<Option<ProjectionCommit>, HealthStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        let transaction = connection.transaction()?;
        let stale = {
            let mut statement = transaction.prepare(
                "SELECT participant_kind, contract_id, deployment_id, instance_id
                 FROM health_latest WHERE effective_status = 'offline' AND observed_at_ns < ?1",
            )?;
            let rows = statement
                .query_map([cutoff_ns], |row| {
                    Ok(HealthChange {
                        participant_kind: row.get(0)?,
                        contract_id: row.get(1)?,
                        deployment_id: row.get(2)?,
                        instance_id: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for change in &stale {
            transaction.execute(
                "DELETE FROM health_status_intervals
                 WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3",
                params![
                    change.participant_kind,
                    change.contract_id,
                    change.instance_id
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM health_latest WHERE effective_status = 'offline' AND observed_at_ns < ?1",
            [cutoff_ns],
        )?;
        let historical_changes = transaction.execute(
            "DELETE FROM health_status_intervals WHERE ended_at_ns IS NOT NULL AND ended_at_ns < ?1",
            [cutoff_ns],
        )? + transaction.execute(
            "DELETE FROM health_metric_buckets WHERE bucket_start_ns < ?1",
            [cutoff_ns],
        )? + transaction.execute(
            "DELETE FROM health_check_metric_buckets WHERE bucket_start_ns < ?1",
            [cutoff_ns],
        )? + transaction.execute(
            "DELETE FROM health_rejections WHERE observed_at_ns < ?1",
            [cutoff_ns],
        )? + transaction.execute(
            "DELETE FROM health_transition_outbox
             WHERE published_at_ns IS NOT NULL AND published_at_ns < ?1",
            [cutoff_ns],
        )?;
        if stale.is_empty() && historical_changes == 0 {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "UPDATE health_projection_meta SET revision = revision + 1,
                    retained_from_ns = MAX(COALESCE(retained_from_ns, ?1), ?1)
             WHERE singleton = 1",
            [cutoff_ns],
        )?;
        let revision = transaction.query_row(
            "SELECT revision FROM health_projection_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(Some(ProjectionCommit {
            revision,
            changes: stale,
        }))
    }

    fn ensure_projection_meta(&self) -> Result<(), HealthStoreError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| HealthStoreError::Poisoned)?;
        connection.execute(
            "INSERT OR IGNORE INTO health_projection_meta (singleton, projection_id)
             VALUES (1, ?1)",
            [Ulid::new().to_string()],
        )?;
        Ok(())
    }
}

fn load_current(
    transaction: &Transaction<'_>,
    identity: &HeartbeatIdentity,
) -> Result<Option<CurrentInstance>, rusqlite::Error> {
    transaction
        .query_row(
            "SELECT participant_name, reported_status, effective_status,
                    observed_at_ns, heartbeat_deadline_ns,
                    (SELECT checks_json FROM health_status_intervals
                     WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3
                       AND ended_at_ns IS NULL)
             FROM health_latest
             WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3",
            params![
                identity.participant_kind,
                identity.contract_id,
                identity.instance_id
            ],
            |row| {
                Ok(CurrentInstance {
                    participant_name: row.get(0)?,
                    reported_status: row.get(1)?,
                    effective_status: row.get(2)?,
                    observed_at_ns: row.get(3)?,
                    heartbeat_deadline_ns: row.get(4)?,
                    checks_json: row.get(5)?,
                })
            },
        )
        .optional()
}

fn close_interval(
    transaction: &Transaction<'_>,
    identity: &HeartbeatIdentity,
    ended_at_ns: i64,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "UPDATE health_status_intervals SET ended_at_ns = ?4
         WHERE participant_kind = ?1 AND contract_id = ?2 AND instance_id = ?3
           AND ended_at_ns IS NULL",
        params![
            identity.participant_kind,
            identity.contract_id,
            identity.instance_id,
            ended_at_ns
        ],
    )?;
    Ok(())
}

fn insert_interval(
    transaction: &Transaction<'_>,
    identity: &HeartbeatIdentity,
    row: IntervalRow<'_>,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO health_status_intervals
         (participant_kind, contract_id, instance_id, deployment_id, started_at_ns,
          reported_status, effective_status, checks_json, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            identity.participant_kind,
            identity.contract_id,
            identity.instance_id,
            identity.deployment_id,
            row.started_at_ns,
            row.reported_status,
            row.effective_status,
            row.checks_json,
            row.reason
        ],
    )?;
    Ok(())
}

fn insert_transition(
    transaction: &Transaction<'_>,
    identity: &HeartbeatIdentity,
    row: TransitionRow<'_>,
) -> Result<(), HealthStoreError> {
    let event_id = Ulid::new().to_string();
    let changed_at = rfc3339(row.changed_at_ns)?;
    let event = HealthStatusChangedEvent {
        header: HealthStatusChangedEventHeader {
            id: event_id.clone().into(),
            time: changed_at.clone(),
            extra: Default::default(),
        },
        participant: HealthStatusChangedEventParticipant {
            kind: wire(&identity.participant_kind)?,
            contract_id: identity.contract_id.clone(),
            deployment_id: identity.deployment_id.clone(),
            instance_id: identity.instance_id.clone(),
            name: row.participant_name.to_string(),
            extra: Default::default(),
        },
        previous_status: wire(row.previous_status)?,
        status: wire(row.status)?,
        reported_status: wire(row.reported_status)?,
        reason: wire(row.reason)?,
        changed_at,
        last_seen_at: rfc3339(row.last_seen_at_ns)?,
        summary: row.summary.map(|value| value.to_string().into()),
        extra: Default::default(),
    };
    transaction.execute(
        "INSERT INTO health_transition_outbox
         (event_id, participant_kind, contract_id, instance_id, payload_json, created_at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event_id,
            identity.participant_kind,
            identity.contract_id,
            identity.instance_id,
            serde_json::to_string(&event)?,
            row.changed_at_ns
        ],
    )?;
    Ok(())
}

fn wire<T: DeserializeOwned, S: Serialize>(value: S) -> Result<T, HealthStoreError> {
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}

fn update_metric_buckets(
    transaction: &Transaction<'_>,
    identity: &HeartbeatIdentity,
    sample: &HealthHeartbeatSample,
    observed_at_ns: i64,
) -> Result<(), rusqlite::Error> {
    let bucket_start_ns = observed_at_ns - observed_at_ns.rem_euclid(METRIC_BUCKET_NS);
    let (healthy, degraded, unhealthy) = match sample.reported_status.as_str() {
        "healthy" => (1, 0, 0),
        "degraded" => (0, 1, 0),
        "unhealthy" => (0, 0, 1),
        _ => (0, 0, 0),
    };
    transaction.execute(
        "INSERT INTO health_metric_buckets
         (participant_kind, contract_id, instance_id, bucket_start_ns, sample_count,
          healthy_count, degraded_count, unhealthy_count)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)
         ON CONFLICT (participant_kind, contract_id, instance_id, bucket_start_ns)
         DO UPDATE SET sample_count = sample_count + 1,
                       healthy_count = healthy_count + excluded.healthy_count,
                       degraded_count = degraded_count + excluded.degraded_count,
                       unhealthy_count = unhealthy_count + excluded.unhealthy_count",
        params![
            identity.participant_kind,
            identity.contract_id,
            identity.instance_id,
            bucket_start_ns,
            healthy,
            degraded,
            unhealthy
        ],
    )?;
    for check in &sample.checks {
        transaction.execute(
            "INSERT INTO health_check_metric_buckets
             (participant_kind, contract_id, instance_id, bucket_start_ns, check_name,
              sample_count, ok_count, failed_count, latency_sum_ms, latency_max_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?8)
             ON CONFLICT (participant_kind, contract_id, instance_id, bucket_start_ns, check_name)
             DO UPDATE SET sample_count = sample_count + 1,
                           ok_count = ok_count + excluded.ok_count,
                           failed_count = failed_count + excluded.failed_count,
                           latency_sum_ms = latency_sum_ms + excluded.latency_sum_ms,
                           latency_max_ms = MAX(latency_max_ms, excluded.latency_max_ms)",
            params![
                identity.participant_kind,
                identity.contract_id,
                identity.instance_id,
                bucket_start_ns,
                check.name.as_ref(),
                i64::from(check.status.as_str() == "ok"),
                i64::from(check.status.as_str() == "failed"),
                check.latency_ms.0 .0,
            ],
        )?;
    }
    Ok(())
}

fn update_meta_sequence(
    transaction: &Transaction<'_>,
    previous_sequence: i64,
    stream_sequence: i64,
    observed_at_ns: i64,
) -> Result<(), rusqlite::Error> {
    let gap = previous_sequence == 0 && stream_sequence > 1
        || previous_sequence > 0 && stream_sequence > previous_sequence + 1;
    transaction.execute(
        "UPDATE health_projection_meta
         SET last_stream_sequence = ?1,
             gap_detected = CASE WHEN ?2 THEN 1 ELSE gap_detected END,
             retained_from_ns = COALESCE(retained_from_ns, ?3),
             complete_since_ns = COALESCE(complete_since_ns, ?3)
         WHERE singleton = 1",
        params![stream_sequence, gap, observed_at_ns],
    )?;
    Ok(())
}

pub(super) fn rfc3339(timestamp_ns: i64) -> Result<String, HealthStoreError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp_ns))
        .map_err(|_| HealthStoreError::TimestampRange)?
        .format(&Rfc3339)
        .map_err(|_| HealthStoreError::TimestampRange)
}

#[cfg(test)]
mod tests {
    use trellis_runtime_apis::__types::CursorQuery;
    use trellis_runtime_apis::types::{
        HealthInspectRequest, HealthMetricsRequest, HealthQueryRequest,
    };

    use super::*;

    fn store() -> HealthStore {
        let connection = Connection::open_in_memory().expect("open health test database");
        connection
            .execute_batch(include_str!(
                "../storage/sqlite/health/V3000__health_projection_init.sql"
            ))
            .expect("apply health schema");
        HealthStore::new(connection).expect("open health store")
    }

    fn identity() -> HeartbeatIdentity {
        HeartbeatIdentity {
            session_key: "session".to_string(),
            participant_kind: "service".to_string(),
            contract_id: "example.worker@v1".to_string(),
            contract_digest: "digest".to_string(),
            deployment_id: "worker.default".to_string(),
            instance_id: "worker-1".to_string(),
        }
    }

    fn sample(id: &str) -> HealthHeartbeatSample {
        serde_json::from_value(serde_json::json!({
            "sample": { "id": id, "time": "2026-01-01T00:00:00Z" },
            "participant": {
                "contractDigest": "digest",
                "contractId": "example.worker@v1",
                "info": {},
                "instanceId": "worker-1",
                "kind": "service",
                "name": "Worker",
                "publishIntervalMs": "30000",
                "runtime": "rust",
                "startedAt": "2026-01-01T00:00:00Z"
            },
            "reportedStatus": "healthy",
            "checks": [{ "latencyMs": 1.0, "name": "nats", "status": "ok" }]
        }))
        .expect("valid heartbeat sample")
    }

    #[test]
    fn projection_materializes_deadline_and_resume_without_raw_sample_rows() {
        let store = store();
        let observed = 1_767_225_600_000_000_000_i64;
        let first = store
            .project_sample(
                &identity(),
                &sample("01J00000000000000000000000"),
                observed,
                observed,
                1,
            )
            .expect("project first sample")
            .expect("first sample changes projection");
        assert_eq!(first.revision, 1);

        let offline = store
            .expire_due(observed + 60_000_000_000)
            .expect("expire deadline")
            .expect("deadline changes projection");
        assert_eq!(offline.revision, 2);

        let resumed = store
            .project_sample(
                &identity(),
                &sample("01J00000000000000000000001"),
                observed + 61_000_000_000,
                observed + 61_000_000_000,
                2,
            )
            .expect("project resumed sample")
            .expect("resumed sample changes projection");
        assert_eq!(resumed.revision, 3);
        assert_eq!(store.pending_transitions(10).expect("read outbox").len(), 2);

        let query = store
            .query(
                &HealthQueryRequest {
                    contract_ids: None,
                    deployment_ids: None,
                    page: Some(CursorQuery {
                        cursor: None,
                        limit: Some(50),
                    }),
                    participant_kinds: None,
                    search: None,
                    statuses: None,
                    extra: Default::default(),
                },
                observed + 62_000_000_000,
            )
            .expect("query health projection");
        assert_eq!(query.items.len(), 1);
        assert_eq!(query.items[0].effective_status.as_str(), "healthy");

        let inspect = store
            .inspect(
                &HealthInspectRequest {
                    contract_id: "example.worker@v1".to_string().into(),
                    history_limit: None,
                    history_since: None,
                    instance_id: None,
                    participant_kind: wire("service").unwrap(),
                    extra: Default::default(),
                },
                observed + 62_000_000_000,
            )
            .expect("inspect health projection")
            .expect("health participant exists");
        assert_eq!(inspect.history.len(), 3);

        let metrics = store
            .metrics(
                &HealthMetricsRequest {
                    check_names: None,
                    contract_id: "example.worker@v1".to_string().into(),
                    end: rfc3339(observed + 120_000_000_000).expect("format end"),
                    instance_ids: None,
                    participant_kind: wire("service").unwrap(),
                    start: rfc3339(observed).expect("format start"),
                    step_ms: wire("300000").unwrap(),
                    extra: Default::default(),
                },
                observed + 120_000_000_000,
            )
            .expect("query health metrics");
        assert_eq!(metrics.series.len(), 1);
        assert_eq!(metrics.summary.sample_count.0, 2);
        assert_eq!(metrics.series[0].buckets[0].offline_ms.0, 1_000);

        let connection = store.connection.lock().expect("lock health store");
        let intervals: i64 = connection
            .query_row("SELECT COUNT(*) FROM health_status_intervals", [], |row| {
                row.get(0)
            })
            .expect("count status intervals");
        assert_eq!(intervals, 3);
        let raw_sample_table: Option<String> = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'health_samples'",
                [],
                |row| row.get(0),
            )
            .optional()
            .expect("inspect schema");
        assert!(raw_sample_table.is_none());
    }
}
