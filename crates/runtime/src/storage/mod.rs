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
        /// Snapshot creation error, including SQLite backup errors.
        #[source]
        error: std::io::Error,
    },
    /// A configured SQLite snapshot stopped making progress because other
    /// connections kept changing or locking the database.
    #[error(
        "could not copy configured sqlite store {path} for a check: the copy made no progress \
        for {} seconds while other connections kept changing or locking it",
        stalled_for.as_secs()
    )]
    SqliteSnapshotChanged {
        /// Configured database path.
        path: PathBuf,
        /// How long the copy went without progress before giving up.
        stalled_for: Duration,
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
    /// A database's schema is not one this release's migrations continue
    /// from: the schema baseline it was created with differs from this
    /// release's, as between release candidates. Nothing was changed.
    #[error(
        "Trellis {release} can't upgrade the installation's {subsystem} database{made_by}: its \
        schema migration {migration} differs from this release's. Start it with {start_with}, \
        or create a new installation",
        made_by = installed
            .as_ref()
            .map_or_else(String::new, |installed| format!(", made by Trellis {installed}")),
        start_with = installed.as_ref().map_or_else(
            || "the server version that created it".to_owned(),
            |installed| format!("Trellis {installed}")
        )
    )]
    IncompatibleSchema {
        /// Subsystem whose database was refused.
        subsystem: &'static str,
        /// The applied migration that differs.
        migration: String,
        /// This server's release.
        release: &'static str,
        /// The release that last started the installation, when known.
        installed: Option<String>,
    },
}

mod sqlite_migrations {
    pub mod platform {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/platform");
    }
    pub use trellis_jobs_runtime::storage::sqlite_migrations as jobs;
    pub mod health {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/health");
    }
    pub use trellis_events_runtime::storage::sqlite_migrations as events;
    pub mod webhooks {
        use refinery::embed_migrations;
        embed_migrations!("src/storage/sqlite/webhooks");
    }
}

/// Webhook request records, kept beside the platform database.
pub(crate) const WEBHOOKS_DATABASE: &str = "webhooks.sqlite";

/// The schema a runtime database holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Schema {
    Subsystem(SubsystemName),
    /// Webhook request records, kept beside the platform database and
    /// migrated with it.
    Webhooks,
}

impl Schema {
    fn name(self) -> &'static str {
        match self {
            Self::Subsystem(subsystem) => subsystem.as_str(),
            Self::Webhooks => "webhooks",
        }
    }

    fn runner(self) -> refinery::Runner {
        match self {
            Self::Subsystem(SubsystemName::Platform) => {
                sqlite_migrations::platform::migrations::runner()
            }
            Self::Subsystem(SubsystemName::Jobs) => sqlite_migrations::jobs::migrations::runner(),
            Self::Subsystem(SubsystemName::Health) => {
                sqlite_migrations::health::migrations::runner()
            }
            Self::Subsystem(SubsystemName::Events) => {
                sqlite_migrations::events::migrations::runner()
            }
            Self::Webhooks => sqlite_migrations::webhooks::migrations::runner(),
        }
    }
}

/// SQLite-backed store for a built-in runtime subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteStore {
    schema: Schema,
    config: SqliteStorageConfig,
}

impl SqliteStore {
    /// Creates a config-only SQLite store for the given subsystem.
    #[must_use]
    pub fn new(subsystem: SubsystemName, config: impl Into<SqliteStorageConfig>) -> Self {
        Self {
            schema: Schema::Subsystem(subsystem),
            config: config.into(),
        }
    }

    /// The webhook request records beside the `platform` database, which
    /// share its settings.
    pub(crate) fn webhooks_beside(platform: &Self) -> Self {
        let mut config = platform.config.clone();
        config.path = config.path.with_file_name(WEBHOOKS_DATABASE);
        Self {
            schema: Schema::Webhooks,
            config,
        }
    }

    /// The database file.
    pub(crate) fn path(&self) -> &Path {
        &self.config.path
    }

    /// How long its connections wait for a lock (see
    /// [`SqliteStorageConfig::busy_timeout`]).
    pub(crate) fn busy_timeout(&self) -> Duration {
        self.config.busy_timeout()
    }

    /// Runs schema migrations for this subsystem.
    pub fn migrate(&self) -> Result<(), StoreError> {
        migrate_sqlite(self.schema, &self.config)
    }

    /// Verify migration compatibility on a temporary copy without changing the database.
    pub fn check_migrations(&self) -> Result<(), StoreError> {
        let directory = tempfile::tempdir().map_err(|error| StoreError::CopySqliteSnapshot {
            from: self.config.path.clone(),
            to: std::env::temp_dir(),
            error,
        })?;
        let copy = self.snapshot_into(
            &directory.path().join("store.sqlite"),
            SNAPSHOT_PAGES_PER_STEP,
            SNAPSHOT_STALL_LIMIT,
        )?;
        let mut connection = copy.open()?;
        self.schema
            .runner()
            .set_abort_missing(false)
            .run(&mut connection)
            .map_err(|source| StoreError::MigrateSqlite {
                path: self.config.path.clone(),
                subsystem: self.schema.name(),
                source,
            })?;
        Ok(())
    }

    /// A copy of this database's committed contents at `path`, read without
    /// writing to this database, with this store's schema and settings.
    ///
    /// The copy runs `pages_per_step` pages at a time, so writers are never
    /// held off for long. A commit from another connection restarts it, and a
    /// lock makes a step wait. Neither is a failure while the copy still gets
    /// further than it ever has: only `stall_limit` without such progress
    /// fails, so a large database copies however long that takes, and steady
    /// writes or a held lock cannot keep it waiting forever.
    pub(crate) fn snapshot_into(
        &self,
        path: &Path,
        pages_per_step: std::ffi::c_int,
        stall_limit: Duration,
    ) -> Result<Self, StoreError> {
        if !self.config.path.exists() {
            return Err(StoreError::MissingSqlite {
                path: self.config.path.clone(),
            });
        }
        let source = self.open_read_only()?;
        let mut connection = Connection::open(path).map_err(|source| StoreError::OpenSqlite {
            path: path.to_owned(),
            source,
        })?;
        let snapshot_error = |error| StoreError::CopySqliteSnapshot {
            from: self.config.path.clone(),
            to: path.to_owned(),
            error: std::io::Error::other(error),
        };
        let backup =
            rusqlite::backup::Backup::new(&source, &mut connection).map_err(snapshot_error)?;
        // Progress is a step that copies pages and leaves fewer to copy than
        // any step before it. A restart sends the count back up, so steady
        // commits only count while each pass gets further than the last. The
        // fewest remaining pages only falls, so the copy ends either way. A
        // busy or locked step copies nothing, and SQLite leaves its counters
        // at zero until a step succeeds, so only a step that copied is read.
        let mut fewest_remaining = std::ffi::c_int::MAX;
        let mut progressed_at = std::time::Instant::now();
        loop {
            let progressed = match backup.step(pages_per_step).map_err(snapshot_error)? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => {
                    let remaining = backup.progress().remaining;
                    let progressed = remaining < fewest_remaining;
                    fewest_remaining = fewest_remaining.min(remaining);
                    progressed
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(10));
                    false
                }
            };
            if progressed {
                progressed_at = std::time::Instant::now();
            } else if progressed_at.elapsed() >= stall_limit {
                return Err(StoreError::SqliteSnapshotChanged {
                    path: self.config.path.clone(),
                    stalled_for: stall_limit,
                });
            }
        }
        let mut config = self.config.clone();
        config.path = path.to_owned();
        Ok(Self {
            schema: self.schema,
            config,
        })
    }

    /// The first migration applied to this existing database that differs
    /// from this release's migration of the same version, read without
    /// writing anything. `None` when the database does not exist, has no
    /// migrations applied or only lacks later ones.
    #[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
    fn divergent_migration(&self) -> Result<Option<String>, StoreError> {
        if !self.exists() {
            return Ok(None);
        }
        let mut connection = self.open_read_only()?;
        let migrate_error = |source| StoreError::MigrateSqlite {
            path: self.config.path.clone(),
            subsystem: self.schema.name(),
            source,
        };
        let has_history = connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master \
                WHERE type = 'table' AND name = 'refinery_schema_history')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|source| StoreError::OpenSqlite {
                path: self.config.path.clone(),
                source,
            })?;
        if !has_history {
            return Ok(None);
        }
        let runner = self.schema.runner();
        let applied = runner
            .get_applied_migrations(&mut connection)
            .map_err(migrate_error)?;
        // As in `migrate_sqlite`, an applied migration this schema lacks
        // belongs to another subsystem sharing the history table.
        Ok(applied
            .iter()
            .find(|applied| {
                runner
                    .get_migrations()
                    .iter()
                    .any(|embedded| embedded.version() == applied.version() && embedded != *applied)
            })
            .map(ToString::to_string))
    }

    pub(crate) fn open(&self) -> Result<Connection, StoreError> {
        open_sqlite(&self.config)
    }

    pub(crate) fn open_read_only(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open_with_flags(
            &self.config.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|source| StoreError::OpenSqlite {
            path: self.config.path.clone(),
            source,
        })?;
        connection
            .busy_timeout(self.config.busy_timeout())
            .map_err(|source| StoreError::ConfigureSqlite {
                path: self.config.path.clone(),
                source,
            })?;
        Ok(connection)
    }

    pub(crate) fn exists(&self) -> bool {
        self.config.path.exists()
    }
}

/// Pages a check snapshot copies per step, between which writers can commit.
pub(crate) const SNAPSHOT_PAGES_PER_STEP: std::ffi::c_int = 128;

/// How long a check snapshot may go without progress before it fails.
pub(crate) const SNAPSHOT_STALL_LIMIT: Duration = Duration::from_secs(10);

/// Subsystem-scoped collection of open runtime stores.
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
#[derive(Debug)]
pub(crate) struct RuntimeStores {
    platform: Option<SqliteStore>,
    webhooks: Option<SqliteStore>,
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
            webhooks: None,
            jobs: None,
            health: None,
            events: None,
        };

        for subsystem in mode.subsystems() {
            match subsystem {
                SubsystemName::Platform => {
                    let StorageBackend::Sqlite(config) = config.platform_storage_backend()?;
                    let platform = SqliteStore::new(SubsystemName::Platform, config);
                    stores.webhooks = Some(SqliteStore::webhooks_beside(&platform));
                    stores.platform = Some(platform);
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

    /// Migrates the selected subsystem stores, and the webhook records with
    /// the platform's, before any other startup work.
    ///
    /// A database whose applied schema differs from this release's, as one a
    /// different release candidate created, is refused with an error naming
    /// both releases (see [`StoreError::IncompatibleSchema`]). Every existing
    /// database is checked before any is migrated or created, so a refused
    /// start leaves the data directory as it found it.
    pub(crate) fn migrate_all(&self) -> Result<(), StoreError> {
        let stores = [
            &self.platform,
            &self.webhooks,
            &self.jobs,
            &self.health,
            &self.events,
        ]
        .into_iter()
        .flatten();
        for store in stores.clone() {
            if let Some(applied) = store.divergent_migration()? {
                return Err(StoreError::IncompatibleSchema {
                    subsystem: store.schema.name(),
                    migration: applied,
                    release: env!("CARGO_PKG_VERSION"),
                    installed: self.installed_release(),
                });
            }
        }
        for store in stores {
            store.migrate()?;
        }
        Ok(())
    }

    /// The release that last started this installation: the version of the
    /// package its built-in platform participant was last installed from,
    /// since each release installs its own build of it. `None` when the
    /// platform database cannot tell.
    fn installed_release(&self) -> Option<String> {
        use trellis_rs::generated::ParticipantDescriptor as _;
        use trellis_runtime_apis::participants::trellis_platform::Participant;
        self.platform
            .as_ref()?
            .open_read_only()
            .ok()?
            .query_row(
                "SELECT json_extract(package.value,'$.version') \
                FROM auth_installed_participants s \
                JOIN auth_package_evidence_documents e ON e.evidence_digest=s.evidence_digest \
                JOIN json_each(e.evidence_json,'$.packages') package \
                ON json_extract(package.value,'$.name')=\
                json_extract(e.evidence_json,'$.rootPackage') \
                WHERE s.participant_id=?1 ORDER BY s.revision DESC LIMIT 1",
                [Participant::ID],
                |row| row.get(0),
            )
            .ok()
    }

    pub(crate) fn check_all(&self) -> Result<(), StoreError> {
        for store in [
            &self.platform,
            &self.webhooks,
            &self.jobs,
            &self.health,
            &self.events,
        ]
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

    /// The webhook request records, migrated with the platform database.
    pub(crate) fn webhooks(&self) -> Result<&SqliteStore, StoreError> {
        self.webhooks.as_ref().ok_or(StoreError::NotConfigured {
            subsystem: SubsystemName::Platform,
        })
    }
}

fn migrate_sqlite(schema: Schema, config: &SqliteStorageConfig) -> Result<(), StoreError> {
    let runner = schema.runner();
    let mut connection = open_sqlite(config)?;
    // Subsystem runners intentionally see only their own migration directory.
    // Keep divergent-version checks, but do not treat other subsystem versions
    // in the shared refinery history table as missing local files.
    runner
        .set_abort_missing(false)
        .run(&mut connection)
        .map_err(|source| StoreError::MigrateSqlite {
            path: config.path.clone(),
            subsystem: schema.name(),
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

/// What a storage failure in background work calls for: a lock, a busy
/// database, an interrupted or failed I/O or a full disk can pass and is
/// tried again; a constraint violation or a value of the wrong type is set
/// aside; a corrupt file, or one that is not a database, stops its subsystem.
pub(crate) use trellis_rs::__internal::Failure;

/// Classifies a SQLite failure (see [`Failure`]).
pub(crate) fn sqlite_failure(error: &rusqlite::Error) -> Failure {
    use rusqlite::ErrorCode;
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => Failure::Unusable,
        Some(
            ErrorCode::DatabaseBusy
            | ErrorCode::DatabaseLocked
            | ErrorCode::OperationInterrupted
            | ErrorCode::SystemIoFailure
            | ErrorCode::DiskFull
            | ErrorCode::OutOfMemory
            | ErrorCode::CannotOpen
            | ErrorCode::FileLockingProtocolFailed
            | ErrorCode::SchemaChanged
            | ErrorCode::ReadOnly,
        ) => Failure::Transient,
        _ => Failure::Permanent,
    }
}

/// A failure of work on a runtime database that tells what it calls for.
pub(crate) trait ClassifiedFailure: std::fmt::Display {
    /// What this failure calls for (see [`Failure`]).
    fn storage_failure(&self) -> Failure;
}

impl ClassifiedFailure for StoreError {
    /// A migration that fails on the database's own error, such as a lock,
    /// is classified by that error; one refinery refuses, such as a changed
    /// checksum, fails the same way again.
    fn storage_failure(&self) -> Failure {
        match self {
            Self::OpenSqlite { source, .. } | Self::ConfigureSqlite { source, .. } => {
                sqlite_failure(source)
            }
            Self::MigrateSqlite { source, .. } => match source.kind() {
                refinery::error::Kind::Connection(_, cause) => cause
                    .downcast_ref::<rusqlite::Error>()
                    .map_or(Failure::Permanent, sqlite_failure),
                _ => Failure::Permanent,
            },
            _ => Failure::Permanent,
        }
    }
}

impl ClassifiedFailure for trellis_jobs_runtime::SqliteJobsStoreError {
    fn storage_failure(&self) -> Failure {
        match self {
            Self::Sqlite(error) => sqlite_failure(error),
            _ => Failure::Permanent,
        }
    }
}

impl ClassifiedFailure for trellis_events_runtime::storage::EventsStoreError {
    fn storage_failure(&self) -> Failure {
        match self {
            Self::Sqlite(error) => sqlite_failure(error),
            _ => Failure::Permanent,
        }
    }
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
    connection
        .busy_timeout(config.busy_timeout())
        .map_err(|source| StoreError::ConfigureSqlite {
            path: config.path.clone(),
            source,
        })?;
    if let Some(journal_mode) = &config.journal_mode {
        connection
            .pragma_update(None, "journal_mode", journal_mode)
            .map_err(|source| StoreError::ConfigureSqlite {
                path: config.path.clone(),
                source,
            })?;
        // WAL + synchronous=NORMAL stays durable across process crashes and drops
        // the per-commit fsync, which is a multi-second stall on the CI HDD.
        if journal_mode.eq_ignore_ascii_case("wal") {
            connection
                .pragma_update(None, "synchronous", "normal")
                .map_err(|source| StoreError::ConfigureSqlite {
                    path: config.path.clone(),
                    source,
                })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
