//! Request records in `webhooks.sqlite`, beside the platform database.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::verify::KeyMatch;

mod migrations {
    use refinery::embed_migrations;
    embed_migrations!("src/storage/sqlite/webhooks");
}

/// The record of one request, as the API shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Attempt {
    pub(crate) attempt_id: String,
    pub(crate) service: String,
    pub(crate) endpoint: String,
    pub(crate) received_at: String,
    pub(crate) client_address: String,
    pub(crate) method: String,
    pub(crate) body_size: String,
    pub(crate) body_sha256: String,
    pub(crate) outcome: String,
    pub(crate) reason: Option<String>,
    pub(crate) matched_key: Option<String>,
    pub(crate) verdict: Option<String>,
    pub(crate) status: Option<u32>,
    pub(crate) delivery_id: Option<String>,
    pub(crate) stored: bool,
    #[serde(skip)]
    pub(crate) stream_sequence: Option<u64>,
}

/// What became of a request, written when its outcome is known.
#[derive(Clone, Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) outcome: &'static str,
    pub(crate) reason: Option<String>,
    pub(crate) matched_key: Option<&'static str>,
    pub(crate) verdict: Option<String>,
    pub(crate) status: Option<u16>,
    pub(crate) delivery_id: Option<String>,
    /// The request's verifiers reached a verdict on it. A request refused
    /// before verification, or still waiting for it, has not.
    pub(crate) reached_verification: bool,
}

/// A new record's fixed facts.
pub(crate) struct NewAttempt<'a> {
    pub(crate) attempt_id: &'a str,
    pub(crate) deployment_id: &'a str,
    pub(crate) endpoint: &'a str,
    pub(crate) received_at_ms: i64,
    pub(crate) client_address: &'a str,
    pub(crate) method: &'a str,
    pub(crate) body_size: usize,
    pub(crate) body_sha256: &'a str,
}

/// How long a deployment keeps stored requests waiting for verification,
/// and request records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Retention {
    pub(crate) intake_ms: u64,
    pub(crate) records_ms: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            intake_ms: 7 * 86_400_000,
            records_ms: 30 * 86_400_000,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Records {
    connection: Arc<Mutex<rusqlite::Connection>>,
}

type Result<T> = std::result::Result<T, rusqlite::Error>;

fn now_ms() -> i64 {
    super::now_ms()
}

impl Records {
    /// Open and migrate the records database.
    pub(crate) fn open(path: &Path) -> std::result::Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut connection = rusqlite::Connection::open(path).map_err(|error| error.to_string())?;
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| error.to_string())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        migrations::migrations::runner()
            .run(&mut connection)
            .map_err(|error| format!("migrate {}: {error}", path.display()))?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    async fn with<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            operation(&connection)
        })
        .await
        .map_err(|_| rusqlite::Error::InvalidQuery)?
    }

    pub(crate) async fn insert(&self, attempt: NewAttempt<'_>, outcome: Outcome) -> Result<()> {
        let values = (
            attempt.attempt_id.to_owned(),
            attempt.deployment_id.to_owned(),
            attempt.endpoint.to_owned(),
            attempt.received_at_ms,
            attempt.client_address.to_owned(),
            attempt.method.to_owned(),
            i64::try_from(attempt.body_size).unwrap_or(i64::MAX),
            attempt.body_sha256.to_owned(),
        );
        self.with(move |connection| {
            connection.execute(
                "INSERT INTO webhook_attempts(attempt_id,deployment_id,endpoint,received_at,\
                client_address,method,body_size,body_sha256,outcome,reason,matched_key,verdict,\
                status,delivery_id,reached_verification,updated_at) \
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) \
                ON CONFLICT(attempt_id) DO NOTHING",
                params![
                    values.0,
                    values.1,
                    values.2,
                    values.3,
                    values.4,
                    values.5,
                    values.6,
                    values.7,
                    outcome.outcome,
                    outcome.reason,
                    outcome.matched_key,
                    outcome.verdict,
                    outcome.status,
                    outcome.delivery_id,
                    outcome.reached_verification,
                    now_ms()
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn set_outcome(&self, attempt_id: &str, outcome: Outcome) -> Result<()> {
        let attempt_id = attempt_id.to_owned();
        self.with(move |connection| {
            connection.execute(
                "UPDATE webhook_attempts SET outcome=?2,reason=?3,matched_key=?4,verdict=?5,\
                status=COALESCE(?6,status),delivery_id=?7,reached_verification=?8,updated_at=?9 \
                WHERE attempt_id=?1",
                params![
                    attempt_id,
                    outcome.outcome,
                    outcome.reason,
                    outcome.matched_key,
                    outcome.verdict,
                    outcome.status,
                    outcome.delivery_id,
                    outcome.reached_verification,
                    now_ms()
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn set_stream_sequence(&self, attempt_id: &str, sequence: u64) -> Result<()> {
        let attempt_id = attempt_id.to_owned();
        self.with(move |connection| {
            connection.execute(
                "UPDATE webhook_attempts SET stream_sequence=?2 WHERE attempt_id=?1",
                params![attempt_id, i64::try_from(sequence).unwrap_or(i64::MAX)],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn get(&self, attempt_id: &str) -> Result<Option<Attempt>> {
        let attempt_id = attempt_id.to_owned();
        self.with(move |connection| {
            connection
                .query_row(
                    &format!("{SELECT} WHERE attempt_id=?1"),
                    [attempt_id],
                    attempt,
                )
                .optional()
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn query(
        &self,
        deployment_id: &str,
        endpoint: Option<String>,
        outcome: Option<String>,
        since_ms: Option<i64>,
        before: Option<String>,
        limit: u32,
    ) -> Result<Vec<Attempt>> {
        let deployment_id = deployment_id.to_owned();
        self.with(move |connection| {
            let mut statement = connection.prepare(&format!(
                "{SELECT} WHERE deployment_id=?1 AND (?2 IS NULL OR endpoint=?2) \
                AND (?3 IS NULL OR outcome=?3) AND (?4 IS NULL OR received_at>=?4) \
                AND (?5 IS NULL OR attempt_id<?5) ORDER BY attempt_id DESC LIMIT ?6"
            ))?;
            let rows = statement
                .query_map(
                    params![deployment_id, endpoint, outcome, since_ms, before, limit],
                    attempt,
                )?
                .collect::<Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Requests per outcome received since `since_ms`.
    pub(crate) async fn counts(
        &self,
        deployment_id: &str,
        endpoint: &str,
        since_ms: i64,
    ) -> Result<Vec<(String, u64)>> {
        let (deployment_id, endpoint) = (deployment_id.to_owned(), endpoint.to_owned());
        self.with(move |connection| {
            let mut statement = connection.prepare(
                "SELECT outcome,count(*) FROM webhook_attempts \
                WHERE deployment_id=?1 AND endpoint=?2 AND received_at>=?3 \
                GROUP BY outcome ORDER BY outcome",
            )?;
            let rows = statement
                .query_map(params![deployment_id, endpoint, since_ms], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// The outcome and reason of the newest request received since
    /// `since_ms` whose verifiers reached a verdict on it. A request refused
    /// before verification, such as one over the size limit, is not one.
    pub(crate) async fn latest_verdict(
        &self,
        deployment_id: &str,
        endpoint: &str,
        since_ms: i64,
    ) -> Result<Option<(String, Option<String>)>> {
        let (deployment_id, endpoint) = (deployment_id.to_owned(), endpoint.to_owned());
        self.with(move |connection| {
            connection
                .query_row(
                    "SELECT outcome,reason FROM webhook_attempts \
                    WHERE deployment_id=?1 AND endpoint=?2 AND received_at>=?3 \
                    AND reached_verification=1 \
                    ORDER BY attempt_id DESC LIMIT 1",
                    params![deployment_id, endpoint, since_ms],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
        })
        .await
    }

    /// Note that a request to `endpoint` received at `matched_at_ms` matched
    /// the `key` value of secret `field`, counted toward the field's
    /// `generation`.
    pub(crate) async fn note_match(
        &self,
        deployment_id: &str,
        endpoint: &str,
        field: &str,
        generation: i64,
        key: KeyMatch,
        matched_at_ms: i64,
    ) -> Result<()> {
        let (deployment_id, endpoint, field) = (
            deployment_id.to_owned(),
            endpoint.to_owned(),
            field.to_owned(),
        );
        self.with(move |connection| {
            connection.execute(
                "INSERT INTO webhook_secret_matches\
                (deployment_id,endpoint,field,generation,matched_key,matched_at) \
                VALUES(?1,?2,?3,?4,?5,?6) \
                ON CONFLICT(deployment_id,endpoint,field,generation,matched_key) \
                DO UPDATE SET matched_at=max(matched_at,excluded.matched_at)",
                params![
                    deployment_id,
                    endpoint,
                    field,
                    generation,
                    key.as_str(),
                    matched_at_ms
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// When a request to `endpoint` last matched each value of its secrets,
    /// by field, generation, and `current` or `previous`.
    pub(crate) async fn matches(
        &self,
        deployment_id: &str,
        endpoint: &str,
    ) -> Result<BTreeMap<(String, i64, String), i64>> {
        let (deployment_id, endpoint) = (deployment_id.to_owned(), endpoint.to_owned());
        self.with(move |connection| {
            let mut statement = connection.prepare(
                "SELECT field,generation,matched_key,matched_at FROM webhook_secret_matches \
                WHERE deployment_id=?1 AND endpoint=?2",
            )?;
            let rows = statement
                .query_map(params![deployment_id, endpoint], |row| {
                    Ok(((row.get(0)?, row.get(1)?, row.get(2)?), row.get(3)?))
                })?
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(rows)
        })
        .await
    }

    /// For every deployment with records, whether or not it still has an
    /// endpoint: pending records older than its intake retention left the
    /// stream unverified, and records older than its record retention go. A
    /// deployment missing from `retention` uses the Trellis defaults.
    pub(crate) async fn sweep(
        &self,
        now_ms: i64,
        retention: BTreeMap<String, Retention>,
    ) -> Result<()> {
        let before = move |retention: u64| {
            now_ms.saturating_sub(i64::try_from(retention).unwrap_or(i64::MAX))
        };
        self.with(move |connection| {
            let deployments = connection
                .prepare("SELECT DISTINCT deployment_id FROM webhook_attempts")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>>>()?;
            for deployment_id in deployments {
                let kept = retention.get(&deployment_id).copied().unwrap_or_default();
                connection.execute(
                    "UPDATE webhook_attempts SET outcome='expired',stream_sequence=NULL,updated_at=?3 \
                    WHERE deployment_id=?1 AND outcome='pending' AND received_at<?2",
                    params![deployment_id, before(kept.intake_ms), now_ms],
                )?;
                connection.execute(
                    "DELETE FROM webhook_attempts WHERE deployment_id=?1 AND received_at<?2",
                    params![deployment_id, before(kept.records_ms)],
                )?;
            }
            Ok(())
        })
        .await
    }

    /// Requests still waiting for verification, for the pending gauge.
    pub(crate) async fn pending(&self) -> Result<u64> {
        self.with(|connection| {
            connection.query_row(
                "SELECT count(*) FROM webhook_attempts WHERE outcome='pending'",
                [],
                |row| row.get(0),
            )
        })
        .await
    }
}

const SELECT: &str = "SELECT attempt_id,deployment_id,endpoint,received_at,client_address,method,\
    body_size,body_sha256,outcome,reason,matched_key,verdict,status,delivery_id,stream_sequence \
    FROM webhook_attempts";

fn attempt(row: &rusqlite::Row<'_>) -> Result<Attempt> {
    let received_at_ms: i64 = row.get(3)?;
    let stream_sequence: Option<i64> = row.get(14)?;
    Ok(Attempt {
        attempt_id: row.get(0)?,
        service: row.get(1)?,
        endpoint: row.get(2)?,
        received_at: super::rfc3339(received_at_ms),
        client_address: row.get(4)?,
        method: row.get(5)?,
        body_size: row.get::<_, i64>(6)?.to_string(),
        body_sha256: row.get(7)?,
        outcome: row.get(8)?,
        reason: row.get(9)?,
        matched_key: row.get(10)?,
        verdict: row.get(11)?,
        status: row.get(12)?,
        delivery_id: row.get(13)?,
        stored: stream_sequence.is_some(),
        stream_sequence: stream_sequence.and_then(|value| u64::try_from(value).ok()),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{NewAttempt, Outcome, Records, Retention};

    const DAY: i64 = 86_400_000;

    async fn record(
        records: &Records,
        id: &str,
        deployment: &str,
        age: i64,
        outcome: &'static str,
    ) {
        records
            .insert(
                NewAttempt {
                    attempt_id: id,
                    deployment_id: deployment,
                    endpoint: "signed",
                    received_at_ms: super::now_ms() - age,
                    client_address: "127.0.0.1",
                    method: "POST",
                    body_size: 2,
                    body_sha256: "",
                },
                Outcome {
                    outcome,
                    ..Outcome::default()
                },
            )
            .await
            .expect("record inserted");
    }

    async fn outcome(records: &Records, id: &str) -> Option<String> {
        records
            .get(id)
            .await
            .expect("record read")
            .map(|attempt| attempt.outcome)
    }

    #[tokio::test]
    async fn records_expire_for_deployments_without_endpoints() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let records = Records::open(&directory.path().join("webhooks.sqlite")).expect("opened");
        // A deployment whose last endpoint is gone keeps the Trellis defaults.
        record(
            &records,
            "01JA0000000000000000000001",
            "gone",
            31 * DAY,
            "accepted",
        )
        .await;
        record(
            &records,
            "01JA0000000000000000000002",
            "gone",
            8 * DAY,
            "pending",
        )
        .await;
        record(
            &records,
            "01JA0000000000000000000003",
            "gone",
            DAY,
            "accepted",
        )
        .await;
        // A deployment that chose a longer record retention keeps it.
        record(
            &records,
            "01JA0000000000000000000004",
            "kept",
            31 * DAY,
            "accepted",
        )
        .await;
        // One that chose a shorter one loses records sooner.
        record(
            &records,
            "01JA0000000000000000000005",
            "short",
            2 * 3_600_000,
            "accepted",
        )
        .await;

        let retention = BTreeMap::from([
            (
                "kept".to_owned(),
                Retention {
                    records_ms: 365 * 86_400_000,
                    ..Retention::default()
                },
            ),
            (
                "short".to_owned(),
                Retention {
                    records_ms: 3_600_000,
                    ..Retention::default()
                },
            ),
        ]);
        records
            .sweep(super::now_ms(), retention)
            .await
            .expect("swept");

        assert_eq!(outcome(&records, "01JA0000000000000000000001").await, None);
        assert_eq!(
            outcome(&records, "01JA0000000000000000000002")
                .await
                .as_deref(),
            Some("expired")
        );
        assert_eq!(
            outcome(&records, "01JA0000000000000000000003")
                .await
                .as_deref(),
            Some("accepted")
        );
        assert_eq!(
            outcome(&records, "01JA0000000000000000000004")
                .await
                .as_deref(),
            Some("accepted")
        );
        assert_eq!(outcome(&records, "01JA0000000000000000000005").await, None);
    }
}
