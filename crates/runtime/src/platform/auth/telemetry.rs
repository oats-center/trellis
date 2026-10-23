//! Read-only projections of Auth's durable outbox and resource lifecycle.

use rusqlite::TransactionBehavior;

use super::{AuthError, SqliteAuthorizationStore};

/// One consistent snapshot; queued, retrying, and claimed effects remain pending
/// until their production completion transaction removes them.
#[derive(Debug)]
pub(crate) struct AuthorizationTelemetrySnapshot {
    pub(crate) post_commit_pending: u64,
    pub(crate) post_commit_oldest_age_seconds: f64,
    pub(crate) resources: Vec<(String, String, u64)>,
}

impl SqliteAuthorizationStore {
    /// Sample committed state without taking a writer reservation or consulting
    /// the broker. `now` and the persisted creation timestamps are Unix seconds.
    pub(crate) fn telemetry_snapshot(
        &self,
        now: i64,
    ) -> Result<AuthorizationTelemetrySnapshot, AuthError> {
        if now < 0 {
            return Err(AuthError::Invalid("invalid snapshot time".into()));
        }
        let mut connection = self.connection.lock().map_err(|_| AuthError::Unavailable)?;
        // Read the owning Auth store without reserving a writer transaction.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let (post_commit_pending, oldest): (u64, Option<i64>) = transaction.query_row(
            "SELECT count(*), min(created_at) FROM auth_post_commit_actions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let resources = {
            let mut statement = transaction.prepare(
                "SELECT kind, state, count(*) \
                FROM auth_resources \
                GROUP BY kind, state \
                ORDER BY kind, state",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        transaction.commit()?;
        Ok(AuthorizationTelemetrySnapshot {
            post_commit_pending,
            post_commit_oldest_age_seconds: oldest
                .map_or(0.0, |created| now.saturating_sub(created).max(0) as f64),
            resources,
        })
    }
}
