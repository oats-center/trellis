use super::*;

fn sqlite_config(path: PathBuf) -> SqliteStorageConfig {
    SqliteStorageConfig {
        path,
        journal_mode: Some("wal".to_owned()),
        busy_timeout_ms: Some(2_500),
        single_writer: Some(true),
    }
}

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
fn subsystem_config(path: PathBuf) -> crate::SubsystemConfig {
    crate::SubsystemConfig {
        storage: Some(crate::StorageConfig {
            kind: "sqlite".to_owned(),
            path: Some(path),
            url: None,
            journal_mode: Some("wal".to_owned()),
            busy_timeout_ms: Some(2_500),
            single_writer: Some(true),
        }),
        history_retention_days: None,
        transport_retention_hours: None,
        transport_max_bytes: None,
        dead_letter_retention_days: None,
    }
}

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
fn invalid_postgres_subsystem_config() -> crate::SubsystemConfig {
    crate::SubsystemConfig {
        storage: Some(crate::StorageConfig {
            kind: "postgres".to_owned(),
            path: None,
            url: Some("postgres://trellis@localhost/trellis".to_owned()),
            journal_mode: None,
            busy_timeout_ms: None,
            single_writer: None,
        }),
        history_retention_days: None,
        transport_retention_hours: None,
        transport_max_bytes: None,
        dead_letter_retention_days: None,
    }
}

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        instance_name: None,
        event_context_digest_file: None,
        paths: None,
        http: None,
        console: None,
        tls: None,
        nats: Some(crate::NatsConfig {
            private_network: false,
            managed: false,
            servers: Some("nats://127.0.0.1:4222".to_owned()),
            ca_certificate: None,
            system_credentials: None,
            operator_signing_key: None,
        }),
        client: None,
        leases: Some(crate::LeasesConfig {
            bucket: None,
            replicas: Some(1),
            ttl_ms: None,
            renew_ms: None,
        }),
        auth: Some(crate::AuthConfig {
            avatar: None,
            administration: Default::default(),
            instance_id: None,
            local_identity: crate::config::RemovedSection,
            authorization: Some(crate::AuthorizationConfig {
                context_lifetime_seconds: 300,
                allowed_clock_skew_seconds: 30,
                oauth_admission_lifetime_seconds: 300,
                issuer_activation_delay_seconds: 60,
            }),
        }),
        platform: None,
        jobs: None,
        health: None,
        events: None,
        secrets: crate::Secrets {
            nats: crate::NatsSecrets {
                users: crate::NatsUsers {
                    trellis: Some(crate::NatsUser {
                        jwt: "trellis.jwt".into(),
                        seed: "trellis.seed".into(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

#[test]
fn sqlite_migration_check_rejects_missing_database_without_creating_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("platform.sqlite");
    let store = SqliteStore::new(SubsystemName::Platform, sqlite_config(path.clone()));

    assert!(matches!(
        store.check_migrations(),
        Err(StoreError::MissingSqlite { .. })
    ));

    assert!(!path.exists());
    Ok(())
}

#[test]
fn sqlite_migration_check_does_not_modify_configured_database(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("platform.sqlite");
    let store = SqliteStore::new(SubsystemName::Platform, sqlite_config(path.clone()));
    store.migrate()?;
    let connection = store.open()?;
    connection.execute_batch("CREATE TABLE migration_probe (value TEXT); INSERT INTO migration_probe VALUES ('retained')")?;

    store.check_migrations()?;

    let value: String =
        connection.query_row("SELECT value FROM migration_probe", [], |row| row.get(0))?;
    assert_eq!(value, "retained");
    connection.execute("INSERT INTO migration_probe VALUES ('still writable')", [])?;
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM migration_probe", [], |row| row.get(0))?;
    assert_eq!(count, 2);
    Ok(())
}

#[test]
fn migration_check_reads_committed_wal_history() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = SqliteStore::new(
        SubsystemName::Platform,
        sqlite_config(directory.path().join("platform.sqlite")),
    );
    store.migrate()?;
    let connection = store.open()?;
    connection.pragma_update(None, "wal_autocheckpoint", 0)?;
    connection.execute("UPDATE refinery_schema_history SET checksum = '0'", [])?;
    assert!(
        matches!(
            store.check_migrations(),
            Err(StoreError::MigrateSqlite { .. })
        ),
        "snapshot must include the committed WAL migration history, not only the main database"
    );
    let checksum: String = connection.query_row(
        "SELECT checksum FROM refinery_schema_history LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(checksum, "0", "check must not repair or migrate the source");
    Ok(())
}

#[test]
fn migration_check_can_run_while_transactions_commit() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let store = SqliteStore::new(
        SubsystemName::Platform,
        sqlite_config(directory.path().join("platform.sqlite")),
    );
    store.migrate()?;
    let mut connection = store.open()?;
    connection.execute_batch("CREATE TABLE migration_probe (value INTEGER)")?;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> rusqlite::Result<Connection> {
            for value in 0..128 {
                let transaction = connection.transaction()?;
                transaction.execute("INSERT INTO migration_probe VALUES (?1)", [value])?;
                transaction.commit()?;
            }
            Ok(connection)
        });
        for _ in 0..4 {
            store.check_migrations()?;
        }
        let connection = writer.join().unwrap()?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM migration_probe", [], |row| row.get(0))?;
        assert_eq!(
            count, 128,
            "checks must not lose or duplicate source writes"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}

/// A source store filled with `rows` rows of one kilobyte on 512-byte pages
/// in `journal_mode`, so copying it one page per step takes many thousands
/// of steps.
fn large_store(
    directory: &Path,
    rows: i64,
    journal_mode: &str,
) -> Result<(SqliteStore, Connection), Box<dyn std::error::Error>> {
    let path = directory.join("platform.sqlite");
    let connection = Connection::open(&path)?;
    connection.pragma_update(None, "page_size", 512)?;
    connection.pragma_update(None, "journal_mode", journal_mode)?;
    connection.execute_batch("CREATE TABLE copy_probe (id INTEGER PRIMARY KEY, value BLOB)")?;
    connection.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
        INSERT INTO copy_probe (value) SELECT randomblob(1024) FROM n",
        [rows],
    )?;
    Ok((
        SqliteStore::new(SubsystemName::Platform, sqlite_config(path)),
        connection,
    ))
}

/// Another connection takes the source's exclusive lock again and again,
/// for a fifth of the stall limit each time, and lets the copy through only
/// until it has written another twentieth of the database to the
/// destination. Every wait is far shorter than the limit and is followed by
/// progress, while together they take several times the limit, however
/// fast the machine copies.
#[test]
fn snapshot_that_keeps_progressing_outlasts_the_stall_limit(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    // A rollback journal, where an exclusive lock keeps readers out. Taking
    // and releasing it without writing leaves the content as it is, so the
    // copy is never restarted.
    let (store, connection) = large_store(directory.path(), 8_192, "delete")?;
    let destination = directory.path().join("copy.sqlite");
    let stall_limit = Duration::from_secs(1);
    let hold = stall_limit / 5;
    let share = std::fs::metadata(directory.path().join("platform.sqlite"))?.len() / 20;
    let finished = std::sync::atomic::AtomicBool::new(false);

    let (copy, elapsed, holds) = std::thread::scope(|scope| {
        let (finished, destination) = (&finished, &destination);
        let holder = scope.spawn(move || -> rusqlite::Result<u32> {
            let written = || std::fs::metadata(destination).map_or(0, |meta| meta.len());
            let mut holds = 0;
            while !finished.load(std::sync::atomic::Ordering::Relaxed) {
                let before = written();
                connection.execute_batch("BEGIN EXCLUSIVE")?;
                std::thread::sleep(hold);
                connection.execute_batch("ROLLBACK")?;
                holds += 1;
                while written() < before + share
                    && !finished.load(std::sync::atomic::Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_micros(100));
                }
            }
            Ok(holds)
        });
        let started = std::time::Instant::now();
        let copy = store.snapshot_into(destination, 1, stall_limit);
        let elapsed = started.elapsed();
        finished.store(true, std::sync::atomic::Ordering::Relaxed);
        let holds = holder.join().expect("lock holder thread")?;
        Ok::<_, rusqlite::Error>((copy, elapsed, holds))
    })?;
    let copy = copy?;

    assert!(
        elapsed > stall_limit * 3,
        "the copy must run well past the stall limit to show that only stalls fail \
        ({elapsed:?} over {holds} holds)"
    );
    let rows: i64 =
        copy.open_read_only()?
            .query_row("SELECT COUNT(*) FROM copy_probe", [], |row| row.get(0))?;
    assert_eq!(rows, 8_192, "the snapshot holds every committed row");
    Ok(())
}

/// Another connection holds the source's exclusive lock before the copy
/// starts and keeps it for many busy timeouts, so the first step gives up
/// with nothing copied. It then lets the copy through a twentieth of the
/// database at a time, holding the lock briefly in between, so the copy
/// still has work long after the stall limit has passed since it started.
/// Contention before the first copied page must not count as the copy's
/// best progress.
#[test]
fn snapshot_that_starts_locked_out_counts_progress_after_the_lock_clears(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let (_, connection) = large_store(directory.path(), 8_192, "delete")?;
    let busy_timeout = Duration::from_millis(20);
    let store = SqliteStore::new(
        SubsystemName::Platform,
        SqliteStorageConfig {
            busy_timeout_ms: Some(20),
            ..sqlite_config(directory.path().join("platform.sqlite"))
        },
    );
    let destination = directory.path().join("copy.sqlite");
    let stall_limit = Duration::from_secs(1);
    let first_hold = stall_limit / 2;
    let hold = stall_limit / 5;
    let share = std::fs::metadata(directory.path().join("platform.sqlite"))?.len() / 20;
    let finished = std::sync::atomic::AtomicBool::new(false);

    connection.execute_batch("BEGIN EXCLUSIVE")?;
    let (copy, started, elapsed, released) = std::thread::scope(|scope| {
        let (finished, destination) = (&finished, &destination);
        let holder = scope.spawn(move || -> rusqlite::Result<std::time::Instant> {
            std::thread::sleep(first_hold);
            let released = std::time::Instant::now();
            connection.execute_batch("ROLLBACK")?;
            let written = || std::fs::metadata(destination).map_or(0, |meta| meta.len());
            let mut before = 0;
            while !finished.load(std::sync::atomic::Ordering::Relaxed) {
                while written() < before + share
                    && !finished.load(std::sync::atomic::Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_micros(100));
                }
                before = written();
                connection.execute_batch("BEGIN EXCLUSIVE")?;
                std::thread::sleep(hold);
                connection.execute_batch("ROLLBACK")?;
            }
            Ok(released)
        });
        let started = std::time::Instant::now();
        let copy = store.snapshot_into(destination, 1, stall_limit);
        let elapsed = started.elapsed();
        finished.store(true, std::sync::atomic::Ordering::Relaxed);
        let released = holder.join().expect("lock holder thread")?;
        Ok::<_, rusqlite::Error>((copy, started, elapsed, released))
    })?;

    assert!(
        released.saturating_duration_since(started) > busy_timeout * 10,
        "the lock must outlast the busy timeout after the copy starts, so its \
        first step returns busy"
    );
    let copy = copy?;
    assert!(
        elapsed > stall_limit * 2,
        "the copy must still be copying well past the stall limit from its start \
        ({elapsed:?})"
    );
    let rows: i64 =
        copy.open_read_only()?
            .query_row("SELECT COUNT(*) FROM copy_probe", [], |row| row.get(0))?;
    assert_eq!(rows, 8_192, "the snapshot holds every committed row");
    Ok(())
}

#[test]
fn snapshot_fails_while_another_connection_keeps_changing_the_source(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let (store, connection) = large_store(directory.path(), 40_960, "wal")?;
    let stall_limit = Duration::from_millis(200);
    let stop = std::sync::atomic::AtomicBool::new(false);

    let result = std::thread::scope(|scope| {
        let stop = &stop;
        let writer = scope.spawn(move || -> rusqlite::Result<()> {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                connection.execute(
                    "UPDATE copy_probe SET value = randomblob(1024) WHERE id = 1",
                    [],
                )?;
            }
            Ok(())
        });
        let result = store.snapshot_into(&directory.path().join("copy.sqlite"), 1, stall_limit);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().expect("writer thread")?;
        Ok::<_, rusqlite::Error>(result)
    })?;

    let error = result.expect_err("a source that changes on every step never finishes copying");
    let StoreError::SqliteSnapshotChanged { stalled_for, .. } = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(stalled_for, stall_limit);
    assert!(
        error.to_string().contains("made no progress"),
        "the message names the stall, not the copy time: {error}"
    );
    Ok(())
}

#[test]
fn migrated_events_store_orders_events_and_dead_letters_by_timestamp(
) -> Result<(), Box<dyn std::error::Error>> {
    use trellis_events_runtime::dead_letters::{
        dead_letter_id, DeadLetterCause, DeadLetterState, DeadLetterTransition, OriginalEvent,
    };
    use trellis_events_runtime::storage::{EventsFilter, EventsStore, ProjectedEvent};

    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("nested").join("events.sqlite");
    let runtime_store = SqliteStore::new(SubsystemName::Events, sqlite_config(path.clone()));
    runtime_store.migrate()?;
    runtime_store.check_migrations()?;
    let events = EventsStore::open(&path, std::time::Duration::from_secs(5))?;

    for (sequence, event_time) in [(1, "2026-01-01T00:00:00.1Z"), (2, "2026-01-01T00:00:00Z")] {
        events.insert_event(&ProjectedEvent {
            stream_sequence: sequence,
            event_id: Some(format!("event-{sequence}")),
            event_time: event_time.to_owned(),
            subject: "events.v1.Test.Created".to_owned(),
            owner_contract_id: None,
            owner_event_name: None,
            resolution: "resolved".to_owned(),
            verification_status: "verified".to_owned(),
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
            payload_bytes: Vec::new(),
            headers_json: "{}".to_owned(),
            payload_json: None,
            payload_text: None,
            decode_error: None,
            projected_at: "2026-01-01T00:00:01Z".to_owned(),
        })?;
    }
    runtime_store.migrate()?;
    let (event_rows, _) = events.query_events(&EventsFilter {
        limit: 2,
        sort_field: "eventTime".to_owned(),
        sort_direction: "asc".to_owned(),
        ..Default::default()
    })?;
    assert_eq!(event_rows[0]["streamSequence"], 2);
    assert_eq!(event_rows[1]["streamSequence"], 1);

    for (sequence, occurred_at) in [(1, "2026-01-01T00:00:00.1Z"), (2, "2026-01-01T00:00:00Z")] {
        events.project_dead_letter(
            &DeadLetterTransition {
                id: dead_letter_id("consumer-1", "trellis", sequence),
                resource_id: "consumer-1".to_owned(),
                revision: 1,
                previous_subject_sequence: 0,
                generation: 0,
                state: DeadLetterState::Dead,
                occurred_at: occurred_at.to_owned(),
                request_id: None,
                request_digest: None,
                cause: DeadLetterCause::Exhausted,
                original: Some(OriginalEvent {
                    stream: "trellis".to_owned(),
                    sequence,
                    published_at: occurred_at.to_owned(),
                    event_id: Some(format!("event-{sequence}")),
                    subject: "events.v1.Test.Created".to_owned(),
                    payload_bytes: Vec::new(),
                    headers: std::collections::BTreeMap::new(),
                    api_id: Some("test@v1".to_owned()),
                    event_name: Some("Created".to_owned()),
                    context_digest: None,
                    verification_status: "verified".to_owned(),
                }),
                original_record_sequence: 0,
                deliveries: 1,
                last_error: Some("failed".to_owned()),
                replay_stream_sequence: None,
            },
            sequence,
        )?;
    }
    let dead_letters = events.query_dead_letters(Some("consumer-1"), &[], None, 2)?;
    assert_eq!(
        dead_letters["items"][0]["updatedAt"],
        "2026-01-01T00:00:00.1Z"
    );
    assert_eq!(
        dead_letters["items"][1]["updatedAt"],
        "2026-01-01T00:00:00Z"
    );
    Ok(())
}

#[test]
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
fn runtime_stores_split_mode_ignores_unselected_storage() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let jobs_path = temp_dir.path().join("jobs.sqlite");
    let mut config = runtime_config();
    config.platform = Some(invalid_postgres_subsystem_config());
    config.jobs = Some(subsystem_config(jobs_path.clone()));

    config.validate_for_mode(RuntimeMode::Jobs)?;
    let stores = RuntimeStores::from_config(&config, RuntimeMode::Jobs)?;
    stores.migrate_all()?;

    stores.check_all()?;
    Ok(())
}

#[test]
#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
fn a_changed_baseline_in_the_last_database_is_refused_before_any_database_is_touched(
) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = |name: &str| directory.path().join(name);
    let mut config = runtime_config();
    config.platform = Some(subsystem_config(path("platform.sqlite")));
    config.jobs = Some(subsystem_config(path("jobs.sqlite")));
    config.health = Some(subsystem_config(path("health.sqlite")));
    config.events = Some(subsystem_config(path("events.sqlite")));
    let stores = RuntimeStores::from_config(&config, RuntimeMode::All)?;
    stores.migrate_all()?;

    // The events database, migrated last, was made by a release whose
    // baseline differs from this one's. That release had not yet made the
    // jobs database, and the health database exists but is not migrated.
    Connection::open(path("events.sqlite"))?.execute(
        "UPDATE refinery_schema_history SET checksum = substr(checksum, 1, length(checksum) - 1) \
        || CASE substr(checksum, -1) WHEN '0' THEN '1' ELSE '0' END",
        [],
    )?;
    for name in ["jobs.sqlite", "health.sqlite"] {
        for suffix in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{suffix}", path(name).display()));
        }
    }
    Connection::open(path("health.sqlite"))?
        .execute_batch("CREATE TABLE unrelated (id INTEGER)")?;
    let earlier = ["platform.sqlite", WEBHOOKS_DATABASE, "health.sqlite"];
    let before = earlier
        .map(|name| fs::read(path(name)))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    let error = stores.migrate_all().unwrap_err();
    assert!(
        matches!(
            &error,
            StoreError::IncompatibleSchema {
                subsystem: "events",
                release,
                ..
            } if *release == env!("CARGO_PKG_VERSION")
        ),
        "{error}"
    );
    let after = earlier
        .map(|name| fs::read(path(name)))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        before == after,
        "the refused start changed an earlier database"
    );
    assert!(
        !path("jobs.sqlite").exists(),
        "the refused start created the jobs database"
    );
    Ok(())
}
