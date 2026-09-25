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
