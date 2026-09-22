use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::Semaphore;

use super::super::AuthorizationStateError;
use super::{SqliteAuthorizationStore, AUTHORIZATION_CONNECTION_POOL_SIZE};
use crate::storage::{SqliteStore, StoreError};

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
            if spawn_delay >= Duration::from_secs(1)
                || wait_elapsed >= Duration::from_secs(1)
                || operation_elapsed >= Duration::from_secs(1)
            {
                tracing::warn!(
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
