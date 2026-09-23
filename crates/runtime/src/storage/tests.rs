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
        retention_days: None,
        ttl_ms: None,
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
        retention_days: None,
        ttl_ms: None,
    }
}

#[cfg(all(feature = "sqlite-storage", feature = "nats-leases"))]
fn runtime_config() -> RuntimeConfig {
    RuntimeConfig {
        instance_name: None,
        event_session_seed_file: Some(PathBuf::from("session.seed")),
        event_context_digest_file: None,
        live_provider_seed_files: None,
        paths: None,
        http: None,
        nats: Some(crate::NatsConfig {
            servers: Some("nats://127.0.0.1:4222".to_owned()),
            runtime: Some(crate::NatsRuntimeConfig {
                auth_creds_path: Some(PathBuf::from("auth.creds")),
                trellis_creds_path: Some(PathBuf::from("trellis.creds")),
                system_creds_path: Some(PathBuf::from("system.creds")),
            }),
            auth_callout: Some(crate::NatsAuthCalloutConfig {
                issuer_signing_seed_file: Some(PathBuf::from("issuer.seed")),
                target_signing_seed_file: Some(PathBuf::from("target.seed")),
                xkey_seed_file: Some(PathBuf::from("xkey.seed")),
            }),
        }),
        client: None,
        leases: Some(crate::LeasesConfig {
            bucket: None,
            replicas: Some(1),
            ttl_ms: None,
            renew_ms: None,
        }),
        auth: Some(crate::AuthConfig {
            local_identity: None,
            authorization: Some(crate::AuthorizationConfig {
                issuer_signing_seed_file: PathBuf::from("authorization-issuer.seed"),
                context_lifetime_seconds: 300,
                refresh_lead_seconds: 60,
                refresh_jitter_seconds: 15,
                minimum_context_lifetime_seconds: 76,
                maximum_bootstrap_jwt_lifetime_seconds: 3_600,
                allowed_clock_skew_seconds: 30,
                maximum_context_bytes: 16_384,
                maximum_permissions: 4_096,
                context_bucket: "trellis_authorization_contexts".to_owned(),
                registry_replicas: 1,
            }),
        }),
        oauth: None,
        platform: None,
        jobs: None,
        health: None,
        events: None,
    }
}

fn assert_marker(path: &Path, table_name: &str) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    let exists: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |row| row.get(0),
    )?;
    assert_eq!(exists, 1);

    let marker_count: i64 =
        connection.query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
            row.get(0)
        })?;
    assert_eq!(marker_count, 1);

    Ok(())
}

fn assert_table(path: &Path, table_name: &str) -> rusqlite::Result<()> {
    let connection = rusqlite::Connection::open(path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table_name],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "missing table {table_name}");
    Ok(())
}

fn assert_index(path: &Path, index_name: &str) -> rusqlite::Result<()> {
    let connection = rusqlite::Connection::open(path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [index_name],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "missing index {index_name}");
    Ok(())
}

fn assert_no_table(path: &Path, table_name: &str) -> rusqlite::Result<()> {
    let connection = rusqlite::Connection::open(path)?;
    assert!(!connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table_name],
        |row| row.get::<_, bool>(0),
    )?);
    Ok(())
}

fn assert_migration(path: &Path, version: i32, name: &str) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    let migration_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM refinery_schema_history WHERE version = ?1 AND name = ?2",
        rusqlite::params![version, name],
        |row| row.get(0),
    )?;
    assert_eq!(migration_count, 1);
    Ok(())
}

fn assert_migration_order(path: &Path, expected_versions: &[i32]) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    let mut statement =
        connection.prepare("SELECT version FROM refinery_schema_history ORDER BY rowid")?;
    let versions = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i32>>>()?;
    assert_eq!(versions, expected_versions);
    Ok(())
}

#[test]
fn sqlite_platform_store_creates_complete_fresh_schema() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("platform.sqlite");
    let store = SqliteStore::new(SubsystemName::Platform, sqlite_config(path.clone()));

    store.migrate()?;

    assert!(path.exists());
    assert_marker(&path, "trellis_platform_store_marker")?;
    assert_migration(&path, 1000, "platform_init")?;
    assert_table(&path, "auth_principals")?;
    assert_table(&path, "auth_sessions")?;
    assert_table(&path, "auth_grant_bindings")?;
    assert_table(&path, "auth_installed_participants")?;
    assert_table(&path, "auth_authorization_issuers")?;
    assert_table(&path, "auth_authorization_contexts")?;
    assert_table(&path, "auth_bootstrap_administrator")?;
    assert_table(&path, "auth_package_evidence")?;
    assert_table(&path, "auth_package_evidence_documents")?;
    assert_table(&path, "auth_api_bindings")?;
    assert_table(&path, "auth_resources")?;
    assert_table(&path, "auth_resource_history")?;
    let connection = Connection::open(&path)?;
    let mut statement = connection.prepare("PRAGMA table_info(auth_installed_participants)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        columns
            .iter()
            .filter(|column| column.ends_with("_digest"))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "participant_digest",
            "needs_digest",
            "package_digest",
            "evidence_digest"
        ]
    );
    let columns = connection
        .prepare("PRAGMA table_info(auth_device_delegations)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(columns.contains(&"device_grant_revision".to_owned()));
    assert!(columns.contains(&"child_grant_revision".to_owned()));
    let columns = connection
        .prepare("PRAGMA table_info(auth_resources)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for required in ["commitment_json", "actual_json", "readiness_reason"] {
        assert!(columns.contains(&required.to_owned()));
    }
    for retired in [
        "auth_participant_bindings",
        "auth_identity_authorities",
        "auth_deployment_authorities",
        "auth_session_runtime_bindings",
        "auth_dependency_evidence",
        "auth_materialized_authorities",
        "auth_materialized_dependencies",
        "auth_materialized_resource_bindings",
        "auth_transition_outbox",
        "auth_authority_proposals",
        "auth_authority_decisions",
        "auth_portal_authority_bindings",
    ] {
        assert_no_table(&path, retired)?;
    }
    Ok(())
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
    let before = std::fs::read(&path)?;
    let wal = path.with_file_name("platform.sqlite-wal");
    let shm = path.with_file_name("platform.sqlite-shm");
    let sidecars_before = (wal.exists(), shm.exists());

    store.check_migrations()?;

    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!((wal.exists(), shm.exists()), sidecars_before);
    Ok(())
}

#[test]
fn sqlite_platform_store_creates_fresh_schema_and_reruns_safely(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("platform.sqlite");

    let store = SqliteStore::new(SubsystemName::Platform, sqlite_config(path.clone()));
    store.migrate()?;
    store.migrate()?;

    assert_marker(&path, "trellis_platform_store_marker")?;
    assert_table(&path, "auth_provider_identities")?;
    assert_table(&path, "auth_resource_binding_evidence")?;
    assert_table(&path, "auth_authorization_contexts")?;
    Ok(())
}

#[test]
fn sqlite_jobs_projection_store_migrates_complete_schema() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("jobs.sqlite");
    let store = SqliteStore::new(SubsystemName::Jobs, sqlite_config(path.clone()));

    store.migrate()?;
    store.migrate()?;

    assert!(path.exists());
    assert_marker(&path, "trellis_jobs_projection_store_marker")?;
    assert_migration(&path, 2000, "jobs_projection_init")?;
    assert_migration_order(&path, &[2000])?;
    assert_table(&path, "jobs_projection")?;
    assert_table(&path, "jobs_metadata_projection")?;
    assert_table(&path, "jobs_lineage_projection")?;
    assert_table(&path, "jobs_events_projection")?;
    assert_table(&path, "jobs_wait_projection")?;
    assert_table(&path, "jobs_error_projection")?;
    assert_table(&path, "worker_presence_projection")?;
    assert_table(&path, "projection_metadata")?;
    assert_index(&path, "idx_jobs_projection_workbench_updated")?;
    assert_index(&path, "idx_jobs_metadata_queue_key")?;
    Ok(())
}

#[test]
fn sqlite_health_projection_store_creates_parent_directory_and_migrates(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("nested").join("health.sqlite");
    let store = SqliteStore::new(SubsystemName::Health, sqlite_config(path.clone()));

    store.migrate()?;
    store.migrate()?;

    assert!(path.exists());
    assert_marker(&path, "trellis_health_projection_store_marker")?;
    assert_migration(&path, 3000, "health_projection_init")?;
    assert_migration_order(&path, &[3000])?;
    assert_table(&path, "health_projection_meta")?;
    assert_table(&path, "health_latest")?;
    assert_table(&path, "health_status_intervals")?;
    assert_table(&path, "health_metric_buckets")?;
    assert_table(&path, "health_check_metric_buckets")?;
    assert_table(&path, "health_transition_outbox")?;
    assert_table(&path, "health_rejections")?;
    Ok(())
}

#[test]
fn sqlite_events_store_migrates_complete_schema() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("events.sqlite");
    let store = SqliteStore::new(SubsystemName::Events, sqlite_config(path.clone()));

    store.migrate()?;
    store.migrate()?;

    assert!(path.exists());
    assert_marker(&path, "trellis_events_store_marker")?;
    assert_migration(&path, 4000, "events_init")?;
    assert_migration_order(&path, &[4000])?;
    assert_table(&path, "events")?;
    assert_table(&path, "events_projection_metadata")?;
    assert_table(&path, "events_consumer_samples")?;
    assert_table(&path, "consumer_dead_letters")?;
    assert_table(&path, "consumer_dlq_projection_checkpoint")?;
    assert_table(&path, "consumer_dlq_commands")?;
    assert_table(&path, "consumer_dlq_transitions")?;
    assert_index(&path, "idx_events_payload_size")?;
    assert_index(&path, "idx_consumer_dead_letters_resource_state")?;
    Ok(())
}

#[test]
fn sqlite_events_migration_matches_events_store_timestamp_schema(
) -> Result<(), Box<dyn std::error::Error>> {
    use trellis_events_runtime::dead_letters::{
        dead_letter_id, DeadLetterCause, DeadLetterState, DeadLetterTransition, OriginalEvent,
    };
    use trellis_events_runtime::storage::{EventsFilter, EventsStore, ProjectedEvent};

    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("events.sqlite");
    let runtime_store = SqliteStore::new(SubsystemName::Events, sqlite_config(path.clone()));
    runtime_store.migrate()?;
    runtime_store.check_migrations()?;
    let events = EventsStore::open(&path)?;

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
fn runtime_stores_all_mode_migrates_all_selected_subsystems(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let platform_path = temp_dir.path().join("platform.sqlite");
    let jobs_path = temp_dir.path().join("jobs.sqlite");
    let health_path = temp_dir.path().join("health.sqlite");
    let events_path = temp_dir.path().join("events.sqlite");
    let mut config = runtime_config();
    config.platform = Some(subsystem_config(platform_path.clone()));
    config.jobs = Some(subsystem_config(jobs_path.clone()));
    config.health = Some(subsystem_config(health_path.clone()));
    config.events = Some(subsystem_config(events_path.clone()));

    config.validate_for_mode(RuntimeMode::All)?;
    let stores = RuntimeStores::from_config(&config, RuntimeMode::All)?;
    stores.migrate_all()?;

    assert!(stores.platform.is_some());
    assert!(stores.jobs.is_some());
    assert!(stores.health.is_some());
    assert!(stores.events.is_some());
    assert_marker(&platform_path, "trellis_platform_store_marker")?;
    assert_marker(&jobs_path, "trellis_jobs_projection_store_marker")?;
    assert_marker(&health_path, "trellis_health_projection_store_marker")?;
    assert_marker(&events_path, "trellis_events_store_marker")?;
    assert_migration_order(&platform_path, &[1000])?;
    assert_migration_order(&jobs_path, &[2000])?;
    assert_migration_order(&health_path, &[3000])?;
    assert_migration_order(&events_path, &[4000])?;
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

    assert!(stores.platform.is_none());
    assert!(stores.jobs.is_some());
    assert!(stores.health.is_none());
    assert!(stores.events.is_none());
    assert_marker(&jobs_path, "trellis_jobs_projection_store_marker")?;
    assert_migration(&jobs_path, 2000, "jobs_projection_init")?;
    Ok(())
}

#[test]
fn open_sqlite_applies_configured_pragmas() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.path().join("pragmas.sqlite");
    let config = sqlite_config(path);

    let connection = open_sqlite(&config)?;

    let busy_timeout: u64 = connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    assert_eq!(busy_timeout, 2_500);
    assert_eq!(journal_mode.to_lowercase(), "wal");
    Ok(())
}
