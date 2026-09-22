//! Runtime SQLite storage for built-in subsystems.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;
use thiserror::Error;

use crate::{SqliteStorageConfig, SubsystemName};

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
use crate::{RuntimeConfig, RuntimeMode, StorageBackend};

/// Runtime storage error.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The selected subsystem has no configured store.
    #[error("runtime store for {subsystem} is not configured")]
    NotConfigured {
        /// Missing subsystem store.
        subsystem: SubsystemName,
    },
    /// SQLite parent directory creation failed.
    #[error("failed to create sqlite parent directory {path}: {source}")]
    CreateDirectory {
        /// Directory path that could not be created.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A configured SQLite database does not exist.
    #[error("configured sqlite store does not exist: {path}")]
    MissingSqlite {
        /// Missing configured database path.
        path: PathBuf,
    },
    /// Copying a configured SQLite database into a read-only check snapshot failed.
    #[error("failed to copy sqlite check snapshot from {from} to {to}: {error}")]
    CopySqliteSnapshot {
        /// Configured source path.
        from: PathBuf,
        /// Temporary destination path.
        to: PathBuf,
        /// Underlying filesystem error.
        #[source]
        error: std::io::Error,
    },
    /// A configured SQLite database changed throughout snapshot retries.
    #[error("configured sqlite store changed while creating a check snapshot: {path}")]
    SqliteSnapshotChanged {
        /// Configured database path.
        path: PathBuf,
    },
    /// SQLite connection opening failed.
    #[error("failed to open sqlite store {path}: {source}")]
    OpenSqlite {
        /// SQLite database path.
        path: PathBuf,
        /// Underlying SQLite error.
        #[source]
        source: rusqlite::Error,
    },
    /// SQLite PRAGMA application failed.
    #[error("failed to configure sqlite store {path}: {source}")]
    ConfigureSqlite {
        /// SQLite database path.
        path: PathBuf,
        /// Underlying SQLite error.
        #[source]
        source: rusqlite::Error,
    },
    /// SQLite migration failed.
    #[error("failed to migrate sqlite store {path} for {subsystem}: {source}")]
    MigrateSqlite {
        /// SQLite database path.
        path: PathBuf,
        /// Subsystem whose skeleton migration failed.
        subsystem: &'static str,
        /// Underlying refinery migration error.
        #[source]
        source: refinery::Error,
    },
}

mod sqlite_migrations {
    pub mod platform {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/platform");
    }
    pub mod jobs {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/jobs");
    }
    pub mod health {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/health");
    }
    pub mod events {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/events");
    }
}

/// SQLite-backed store for a built-in runtime subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteStore {
    subsystem: SubsystemName,
    config: SqliteStorageConfig,
}

impl SqliteStore {
    /// Creates a config-only SQLite store for the given subsystem.
    #[must_use]
    pub fn new(subsystem: SubsystemName, config: impl Into<SqliteStorageConfig>) -> Self {
        Self {
            subsystem,
            config: config.into(),
        }
    }

    /// Runs schema migrations for this subsystem.
    pub fn migrate(&self) -> Result<(), StoreError> {
        migrate_sqlite(self.subsystem, &self.config)
    }

    /// Verify migration compatibility on a temporary copy without changing the database.
    pub fn check_migrations(&self) -> Result<(), StoreError> {
        if !self.config.path.exists() {
            return Err(StoreError::MissingSqlite {
                path: self.config.path.clone(),
            });
        }
        let runner = match self.subsystem {
            SubsystemName::Platform => sqlite_migrations::platform::migrations::runner(),
            SubsystemName::Jobs => sqlite_migrations::jobs::migrations::runner(),
            SubsystemName::Health => sqlite_migrations::health::migrations::runner(),
            SubsystemName::Events => sqlite_migrations::events::migrations::runner(),
        };
        let temporary_directory = std::env::temp_dir().join(format!(
            "trellis-migration-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let temporary_path = temporary_directory.join("store.sqlite");
        let result = (|| {
            fs::create_dir(&temporary_directory).map_err(|source| StoreError::CreateDirectory {
                path: temporary_directory.clone(),
                source,
            })?;
            let source_paths = ["", "-wal", "-shm"].map(|suffix| {
                let mut path = self.config.path.as_os_str().to_owned();
                path.push(suffix);
                PathBuf::from(path)
            });
            let destination_paths = ["", "-wal", "-shm"].map(|suffix| {
                let mut path = temporary_path.as_os_str().to_owned();
                path.push(suffix);
                PathBuf::from(path)
            });
            let fingerprint = |paths: &[PathBuf; 3]| -> std::io::Result<_> {
                paths
                    .iter()
                    .map(|path| match fs::metadata(path) {
                        Ok(metadata) => Ok(Some((metadata.len(), metadata.modified()?))),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        Err(error) => Err(error),
                    })
                    .collect::<Result<Vec<_>, _>>()
            };
            let mut stable = false;
            for _ in 0..3 {
                let before =
                    fingerprint(&source_paths).map_err(|error| StoreError::CopySqliteSnapshot {
                        from: self.config.path.clone(),
                        to: temporary_path.clone(),
                        error,
                    })?;
                for (from, to) in source_paths.iter().zip(&destination_paths) {
                    if from.exists() {
                        fs::copy(from, to).map_err(|error| StoreError::CopySqliteSnapshot {
                            from: from.clone(),
                            to: to.clone(),
                            error,
                        })?;
                    } else {
                        let _ = fs::remove_file(to);
                    }
                }
                if before
                    == fingerprint(&source_paths).map_err(|error| {
                        StoreError::CopySqliteSnapshot {
                            from: self.config.path.clone(),
                            to: temporary_path.clone(),
                            error,
                        }
                    })?
                {
                    stable = true;
                    break;
                }
            }
            if !stable {
                return Err(StoreError::SqliteSnapshotChanged {
                    path: self.config.path.clone(),
                });
            }
            let mut connection =
                Connection::open(&temporary_path).map_err(|source| StoreError::OpenSqlite {
                    path: temporary_path.clone(),
                    source,
                })?;
            runner
                .set_abort_missing(false)
                .run(&mut connection)
                .map_err(|source| StoreError::MigrateSqlite {
                    path: self.config.path.clone(),
                    subsystem: self.subsystem.as_str(),
                    source,
                })?;
            Ok(())
        })();
        let _ = fs::remove_dir_all(temporary_directory);
        result
    }

    pub(crate) fn open(&self) -> Result<Connection, StoreError> {
        open_sqlite(&self.config)
    }

    pub(crate) fn open_read_only(&self) -> Result<Connection, StoreError> {
        Connection::open_with_flags(
            &self.config.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|source| StoreError::OpenSqlite {
            path: self.config.path.clone(),
            source,
        })
    }

    pub(crate) fn exists(&self) -> bool {
        self.config.path.exists()
    }
}

/// Subsystem-scoped collection of open runtime stores.
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
#[derive(Debug)]
pub(crate) struct RuntimeStores {
    platform: Option<SqliteStore>,
    jobs: Option<SqliteStore>,
    health: Option<SqliteStore>,
    events: Option<SqliteStore>,
}

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
impl RuntimeStores {
    pub(crate) fn from_config(
        config: &RuntimeConfig,
        mode: RuntimeMode,
    ) -> Result<Self, crate::ConfigError> {
        let mut stores = Self {
            platform: None,
            jobs: None,
            health: None,
            events: None,
        };

        for subsystem in mode.subsystems() {
            match subsystem {
                SubsystemName::Platform => {
                    let StorageBackend::Sqlite(config) = config.platform_storage_backend()?;
                    stores.platform = Some(SqliteStore::new(SubsystemName::Platform, config));
                }
                SubsystemName::Jobs => {
                    let StorageBackend::Sqlite(config) = config.jobs_storage_backend()?;
                    stores.jobs = Some(SqliteStore::new(SubsystemName::Jobs, config));
                }
                SubsystemName::Health => {
                    let StorageBackend::Sqlite(config) = config.health_storage_backend()?;
                    stores.health = Some(SqliteStore::new(SubsystemName::Health, config));
                }
                SubsystemName::Events => {
                    let StorageBackend::Sqlite(config) = config.events_storage_backend()?;
                    stores.events = Some(SqliteStore::new(SubsystemName::Events, config));
                }
            }
        }

        Ok(stores)
    }

    /// Migrates the selected subsystem stores.
    pub(crate) fn migrate_all(&self) -> Result<(), StoreError> {
        if let Some(store) = &self.platform {
            store.migrate()?;
        }
        if let Some(store) = &self.jobs {
            store.migrate()?;
        }
        if let Some(store) = &self.health {
            store.migrate()?;
        }
        if let Some(store) = &self.events {
            store.migrate()?;
        }
        Ok(())
    }

    pub(crate) fn check_all(&self) -> Result<(), StoreError> {
        for store in [&self.platform, &self.jobs, &self.health, &self.events]
            .into_iter()
            .flatten()
        {
            store.check_migrations()?;
        }
        Ok(())
    }

    pub(crate) fn health(&self) -> Result<&SqliteStore, StoreError> {
        self.health.as_ref().ok_or(StoreError::NotConfigured {
            subsystem: SubsystemName::Health,
        })
    }

    pub(crate) fn platform(&self) -> Result<&SqliteStore, StoreError> {
        self.platform.as_ref().ok_or(StoreError::NotConfigured {
            subsystem: SubsystemName::Platform,
        })
    }
}

fn migrate_sqlite(
    subsystem: SubsystemName,
    config: &SqliteStorageConfig,
) -> Result<(), StoreError> {
    let runner = match subsystem {
        SubsystemName::Platform => sqlite_migrations::platform::migrations::runner(),
        SubsystemName::Jobs => sqlite_migrations::jobs::migrations::runner(),
        SubsystemName::Health => sqlite_migrations::health::migrations::runner(),
        SubsystemName::Events => sqlite_migrations::events::migrations::runner(),
    };
    let mut connection = open_sqlite(config)?;
    // Subsystem runners intentionally see only their own migration directory.
    // Keep divergent-version checks, but do not treat other subsystem versions
    // in the shared refinery history table as missing local files.
    runner
        .set_abort_missing(false)
        .run(&mut connection)
        .map_err(|source| StoreError::MigrateSqlite {
            path: config.path.clone(),
            subsystem: subsystem.as_str(),
            source,
        })?;
    Ok(())
}

fn open_sqlite(config: &SqliteStorageConfig) -> Result<Connection, StoreError> {
    create_parent_dir(&config.path)?;
    let connection = Connection::open(&config.path).map_err(|source| StoreError::OpenSqlite {
        path: config.path.clone(),
        source,
    })?;
    apply_sqlite_pragmas(&connection, config)?;
    Ok(connection)
}

fn create_parent_dir(path: &Path) -> Result<(), StoreError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|source| StoreError::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })
}

fn apply_sqlite_pragmas(
    connection: &Connection,
    config: &SqliteStorageConfig,
) -> Result<(), StoreError> {
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|source| StoreError::ConfigureSqlite {
            path: config.path.clone(),
            source,
        })?;
    if let Some(timeout_ms) = config.busy_timeout_ms {
        connection
            .busy_timeout(Duration::from_millis(timeout_ms))
            .map_err(|source| StoreError::ConfigureSqlite {
                path: config.path.clone(),
                source,
            })?;
    }
    if let Some(journal_mode) = &config.journal_mode {
        connection
            .pragma_update(None, "journal_mode", journal_mode)
            .map_err(|source| StoreError::ConfigureSqlite {
                path: config.path.clone(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
