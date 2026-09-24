use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::Semaphore;

use super::super::AuthorizationStateError;
use super::{SqliteAuthorizationStore, AUTHORIZATION_CONNECTION_POOL_SIZE};
use crate::storage::{SqliteStore, StoreError};
use crate::telemetry::snapshots::{start_bounded_read, OutstandingRead, SourcePoll};

/// Read-only aggregate snapshot for telemetry.
#[derive(Clone, Debug, Default)]
pub(crate) struct AuthTelemetrySnapshot {
    /// Outstanding `auth_post_commit_actions` rows.
    pub(crate) post_commit_pending: u64,
    /// Age in seconds of the oldest outstanding action; zero when empty.
    pub(crate) post_commit_oldest_age_seconds: f64,
    /// Materialized resource bindings by catalog kind and mapped state.
    pub(crate) resources: Vec<(String, &'static str, u64)>,
}

impl SqliteAuthorizationStore {
    /// Starts a bounded read using the persisted reader pool, or the single
    /// connection for an in-memory store.
    pub(crate) async fn start_telemetry_snapshot(
        &self,
        poll: &SourcePoll,
        now_ms: i64,
    ) -> Option<OutstandingRead<Result<AuthTelemetrySnapshot, AuthorizationStateError>>> {
        if let Some(pool) = &self.readers {
            let pool = Arc::clone(pool);
            let permit = poll
                .within(Arc::clone(&pool.permits).acquire_owned())
                .await?
                .ok()?;
            start_bounded_read(poll, move || {
                let connection = match pool.available.lock() {
                    Ok(mut available) => match available.pop() {
                        Some(connection) => connection,
                        None => {
                            return Err(AuthorizationStateError::Storage(
                                "SQLite pool permit without connection".to_owned(),
                            ))
                        }
                    },
                    Err(_) => {
                        return Err(AuthorizationStateError::Storage(
                            "SQLite connection lock poisoned".to_owned(),
                        ))
                    }
                };
                let result = read_auth_telemetry(&connection, now_ms);
                let returned = pool
                    .available
                    .lock()
                    .map(|mut available| available.push(connection));
                drop(permit);
                returned.map_err(|_| {
                    AuthorizationStateError::Storage("SQLite connection lock poisoned".to_owned())
                })?;
                result
            })
            .await
        } else {
            let writer = Arc::clone(&self.writer);
            start_bounded_read(poll, move || {
                let connection = writer.lock().map_err(|_| {
                    AuthorizationStateError::Storage("SQLite connection lock poisoned".to_owned())
                })?;
                read_auth_telemetry(&connection, now_ms)
            })
            .await
        }
    }
}

fn read_auth_telemetry(
    connection: &Connection,
    now_ms: i64,
) -> Result<AuthTelemetrySnapshot, AuthorizationStateError> {
    {
        let (pending, oldest_created_at): (u64, Option<i64>) = connection
            .query_row(
                "SELECT COUNT(*), MIN(created_at) FROM auth_post_commit_actions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_error)?;
        let post_commit_oldest_age_seconds = oldest_created_at
            .map(|created_at| ((now_ms - created_at).max(0) as f64) / 1000.0)
            .unwrap_or(0.0);
        let mut resources = Vec::new();
        let mut statement = connection
            .prepare("SELECT kind, state, COUNT(*) FROM auth_resources GROUP BY kind, state")
            .map_err(sql_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(sql_error)?;
        for row in rows {
            let (kind, state, count) = row.map_err(sql_error)?;
            resources.push((kind, resource_state_label(&state), count.max(0) as u64));
        }
        Ok(AuthTelemetrySnapshot {
            post_commit_pending: pending,
            post_commit_oldest_age_seconds,
            resources,
        })
    }
}

/// Maps one persisted resource state to the bounded catalog state.
fn resource_state_label(state: &str) -> &'static str {
    match state {
        "ready" => "available",
        "pending" => "pending",
        "failed" | "destroying" => "unavailable",
        "detached" => "detached",
        _ => "unavailable",
    }
}

#[derive(Debug)]
pub(super) struct SqliteConnectionPool {
    available: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

impl SqliteAuthorizationStore {
    pub(crate) fn open(store: &SqliteStore) -> Result<Self, StoreError> {
        let connections = (0..AUTHORIZATION_CONNECTION_POOL_SIZE)
            .map(|_| store.open())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_connections(connections))
    }

    pub(crate) fn open_read_only(store: &SqliteStore) -> Result<Self, StoreError> {
        let connections = (0..AUTHORIZATION_CONNECTION_POOL_SIZE)
            .map(|_| store.open_read_only())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_connections(connections))
    }

    /// Create an isolated in-memory store with the current platform schema.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationStateError::Storage`] if SQLite cannot open the
    /// database or apply the platform authorization schema.
    pub fn open_in_memory() -> Result<Self, AuthorizationStateError> {
        let connection = Connection::open_in_memory().map_err(sql_error)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(sql_error)?;
        connection
            .execute_batch(include_str!(
                "../../../storage/sqlite/platform/V1000__platform_init.sql"
            ))
            .map_err(sql_error)?;
        Ok(Self::from_connections(vec![connection]))
    }

    fn from_connections(connections: Vec<Connection>) -> Self {
        let mut connections = connections.into_iter();
        let writer = Arc::new(Mutex::new(
            connections
                .next()
                .expect("sqlite store requires a connection"),
        ));
        let readers = connections.collect::<Vec<_>>();
        Self {
            writer,
            readers: (!readers.is_empty()).then(|| {
                Arc::new(SqliteConnectionPool {
                    permits: Arc::new(Semaphore::new(readers.len())),
                    available: Mutex::new(readers),
                })
            }),
            compiled_evidence: Arc::new(
                super::super::compiled_evidence::CompiledEvidenceCache::new(),
            ),
        }
    }

    pub(in crate::platform::auth) async fn run<T, F>(
        &self,
        operation: F,
    ) -> Result<T, AuthorizationStateError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, AuthorizationStateError> + Send + 'static,
    {
        // ponytail: diagnostic; captures the awaiting caller so the slow write is named.
        let caller = std::backtrace::Backtrace::force_capture();
        let queued_at = Instant::now();
        let writer = Arc::clone(&self.writer);
        tokio::task::spawn_blocking(move || {
            let spawn_delay = queued_at.elapsed();
            let wait_started = Instant::now();
            let mut connection = writer.lock().map_err(|_| {
                AuthorizationStateError::Storage("SQLite connection lock poisoned".to_owned())
            })?;
            let wait_elapsed = wait_started.elapsed();
            let operation_started = Instant::now();
            let result = operation(&mut connection);
            let operation_elapsed = operation_started.elapsed();
            observe_storage(
                "write",
                spawn_delay + wait_elapsed,
                operation_elapsed,
                &result,
            );
            if spawn_delay >= Duration::from_secs(1)
                || wait_elapsed >= Duration::from_secs(1)
                || operation_elapsed >= Duration::from_secs(1)
            {
                tracing::warn!(
                    caller = %caller,
                    spawn_delay_ms = spawn_delay.as_millis(),
                    wait_ms = wait_elapsed.as_millis(),
                    operation_ms = operation_elapsed.as_millis(),
                    "Auth SQLite operation exceeded one second"
                );
            }
            result
        })
        .await
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
    }

    pub(in crate::platform::auth) async fn run_read<T, F>(
        &self,
        operation: F,
    ) -> Result<T, AuthorizationStateError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, AuthorizationStateError> + Send + 'static,
    {
        let Some(pool) = &self.readers else {
            return self.run(operation).await;
        };
        run_on_pool(Arc::clone(pool), operation).await
    }
}

/// Records the bounded storage phases for one Auth SQLite operation.
fn observe_storage<T>(
    operation: &'static str,
    wait: Duration,
    execute: Duration,
    result: &Result<T, AuthorizationStateError>,
) {
    let outcome = if result.is_ok() { "ok" } else { "error" };
    let attributes = |phase: &'static str| {
        vec![
            trellis_rs::telemetry::KeyValue::new("trellis.backend", "auth_sql"),
            trellis_rs::telemetry::KeyValue::new("trellis.operation", operation),
            trellis_rs::telemetry::KeyValue::new("trellis.phase", phase),
            trellis_rs::telemetry::KeyValue::new("trellis.outcome", outcome),
        ]
    };
    let family = trellis_rs::telemetry::instruments::DurationFamily::Storage;
    trellis_rs::telemetry::instruments::record_family_duration(family, wait, &attributes("wait"));
    trellis_rs::telemetry::instruments::record_family_duration(
        family,
        execute,
        &attributes("execute"),
    );
    trellis_rs::telemetry::instruments::record_family_duration(
        family,
        wait + execute,
        &attributes("total"),
    );
}

async fn run_on_pool<T, F>(
    pool: Arc<SqliteConnectionPool>,
    operation: F,
) -> Result<T, AuthorizationStateError>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, AuthorizationStateError> + Send + 'static,
{
    let queued_at = Instant::now();
    let permit = Arc::clone(&pool.permits)
        .acquire_owned()
        .await
        .map_err(|_| AuthorizationStateError::Storage("SQLite pool closed".to_owned()))?;
    tokio::task::spawn_blocking(move || {
        let spawn_delay = queued_at.elapsed();
        let wait_started = Instant::now();
        let mut connection = pool
            .available
            .lock()
            .map_err(|_| {
                AuthorizationStateError::Storage("SQLite connection lock poisoned".to_owned())
            })?
            .pop()
            .ok_or_else(|| {
                AuthorizationStateError::Storage("SQLite pool permit without connection".to_owned())
            })?;
        let wait_elapsed = wait_started.elapsed();
        let operation_started = Instant::now();
        let result = operation(&mut connection);
        let operation_elapsed = operation_started.elapsed();
        observe_storage(
            "read",
            spawn_delay + wait_elapsed,
            operation_elapsed,
            &result,
        );
        pool.available
            .lock()
            .map_err(|_| {
                AuthorizationStateError::Storage("SQLite connection lock poisoned".to_owned())
            })?
            .push(connection);
        drop(permit);
        if spawn_delay >= Duration::from_secs(1)
            || wait_elapsed >= Duration::from_secs(1)
            || operation_elapsed >= Duration::from_secs(1)
        {
            tracing::warn!(
                spawn_delay_ms = spawn_delay.as_millis(),
                wait_ms = wait_elapsed.as_millis(),
                operation_ms = operation_elapsed.as_millis(),
                "Auth SQLite read operation exceeded one second"
            );
        }
        result
    })
    .await
    .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?
}

pub(in crate::platform::auth) fn encode_enum<T: Serialize>(
    value: T,
) -> Result<String, AuthorizationStateError> {
    let encoded = serde_json::to_string(&value)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))?;
    Ok(encoded.trim_matches('"').to_owned())
}

pub(in crate::platform::auth) fn decode_enum<T: DeserializeOwned>(
    value: String,
) -> rusqlite::Result<T> {
    // Treat raw SQLite text as a literal JSON string value: interpreting JSON
    // escapes (e.g. `\u0072evoked` -> `revoked`) would silently accept
    // noncanonical persisted corruption as a valid enum variant.
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| decode_failure(&error.to_string()))
}

pub(super) fn encode_json<T: Serialize>(value: &T) -> Result<String, AuthorizationStateError> {
    serde_json::to_string(value)
        .map_err(|error| AuthorizationStateError::Storage(error.to_string()))
}

pub(super) fn decode_json<T: DeserializeOwned>(value: String) -> rusqlite::Result<T> {
    serde_json::from_str(&value).map_err(|error| decode_failure(&error.to_string()))
}

pub(in crate::platform::auth) fn to_sql_version(
    value: u64,
) -> Result<i64, AuthorizationStateError> {
    super::super::domain::require_positive("version", value)?;
    i64::try_from(value).map_err(|_| {
        AuthorizationStateError::InvalidRecord("version exceeds SQLite integer range".to_owned())
    })
}

pub(in crate::platform::auth) fn from_sql_version(value: i64) -> rusqlite::Result<u64> {
    let value = u64::try_from(value).map_err(|_| decode_failure("version must be positive"))?;
    if value == 0 || value > super::super::MAX_PROTOCOL_INTEGER {
        return Err(decode_failure(
            "version exceeds protocol-safe integer range",
        ));
    }
    Ok(value)
}

pub(in crate::platform::auth) fn from_sql_u32(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|_| decode_failure("integer is outside u32 range"))
}

pub(in crate::platform::auth) fn decode_failure(message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.to_owned(),
        )),
    )
}

pub(in crate::platform::auth) fn map_write_error(
    error: rusqlite::Error,
) -> AuthorizationStateError {
    if matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::ConstraintViolation)
    ) {
        AuthorizationStateError::StorageConflict
    } else {
        sql_error(error)
    }
}

pub(in crate::platform::auth) fn sql_error(error: rusqlite::Error) -> AuthorizationStateError {
    AuthorizationStateError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::{SqliteStorageConfig, SubsystemName};

    #[tokio::test]
    async fn telemetry_uses_persisted_readers_without_waiting_for_writer() {
        let directory = tempfile::tempdir().unwrap();
        let storage = SqliteStore::new(
            SubsystemName::Platform,
            SqliteStorageConfig {
                path: directory.path().join("auth.sqlite"),
                journal_mode: Some("wal".to_owned()),
                busy_timeout_ms: Some(2_500),
                single_writer: Some(true),
            },
        );
        storage.migrate().unwrap();
        let store = SqliteAuthorizationStore::open(&storage).unwrap();
        let writer = Arc::clone(&store.writer);
        let (held, ready) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let writer_task = tokio::task::spawn_blocking(move || {
            let _guard = writer.lock().unwrap();
            held.send(()).unwrap();
            released.blocking_recv().unwrap();
        });
        ready.await.unwrap();
        let poll = SourcePoll::start();
        let mut read = store.start_telemetry_snapshot(&poll, 1000).await.unwrap();
        let snapshot = crate::telemetry::snapshots::read_within_poll(&poll, &mut read)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.post_commit_pending, 0);
        assert_eq!(snapshot.post_commit_oldest_age_seconds, 0.0);
        release.send(()).unwrap();
        writer_task.await.unwrap();

        let pool = store.readers.as_ref().unwrap();
        let pool_for_lock = Arc::clone(pool);
        let (held, ready) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let reader_task = tokio::task::spawn_blocking(move || {
            let _guard = pool_for_lock.available.lock().unwrap();
            held.send(()).unwrap();
            released.blocking_recv().unwrap();
        });
        ready.await.unwrap();
        let poll = SourcePoll::start();
        let mut late = store.start_telemetry_snapshot(&poll, 1000).await.unwrap();
        assert!(
            crate::telemetry::snapshots::read_within_poll(&poll, &mut late)
                .await
                .is_none()
        );
        assert!(!late.is_finished());
        assert_eq!(pool.permits.available_permits(), 6);
        release.send(()).unwrap();
        reader_task.await.unwrap();
        assert!(
            crate::telemetry::snapshots::read_within_poll(&SourcePoll::start(), &mut late)
                .await
                .unwrap()
                .is_ok()
        );
        assert_eq!(pool.permits.available_permits(), 7);

        let permits = Arc::clone(&pool.permits)
            .acquire_many_owned(7)
            .await
            .unwrap();
        let poll = SourcePoll::start();
        let blocked = store.start_telemetry_snapshot(&poll, 1000).await;
        assert!(
            blocked.is_none(),
            "pool wait must expire within the source poll"
        );
        drop(permits);
        let poll = SourcePoll::start();
        let mut read = store.start_telemetry_snapshot(&poll, 1000).await.unwrap();
        assert!(
            crate::telemetry::snapshots::read_within_poll(&poll, &mut read)
                .await
                .unwrap()
                .is_ok()
        );
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
    #[serde(rename_all = "snake_case")]
    enum TestState {
        Active,
        Revoked,
    }

    #[test]
    fn decode_enum_accepts_canonical_text() {
        assert_eq!(
            decode_enum::<TestState>("revoked".to_owned()).unwrap(),
            TestState::Revoked
        );
        assert_eq!(
            decode_enum::<TestState>("active".to_owned()).unwrap(),
            TestState::Active
        );
    }

    #[test]
    fn decode_enum_rejects_escaped_alias() {
        // Raw SQLite text `\u0072evoked` must not decode as `revoked`: JSON
        // escape interpretation would silently accept persisted corruption.
        assert!(matches!(
            decode_enum::<TestState>("\\u0072evoked".to_owned()),
            Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                _
            ))
        ));
    }

    #[test]
    fn decode_enum_rejects_quoted_text() {
        // Literal surrounding quotes are not canonical enum text either.
        assert!(decode_enum::<TestState>("\"revoked\"".to_owned()).is_err());
    }

    #[test]
    fn decode_enum_rejects_malformed_and_control_text() {
        assert!(decode_enum::<TestState>("not_a_variant".to_owned()).is_err());
        assert!(decode_enum::<TestState>("\\revoked".to_owned()).is_err());
        assert!(decode_enum::<TestState>("revoked\n".to_owned()).is_err());
    }
}
