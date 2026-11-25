//! `trellis-server export` and `import` against real installations: each
//! has its own NATS broker and platform database, prepared as a first start
//! prepares them, and the commands run as the server runs them.
use std::time::Duration;

use super::runtime_rotation::installation;
use super::*;

/// Ports for `count` installations, four each, held for the whole test.
fn reserve(count: usize) -> Vec<trellis_local_nats::ReservedPort> {
    (0..count * 4)
        .map(|_| trellis_local_nats::ReservedPort::reserve().unwrap())
        .collect()
}

/// An installation at `root` on `ports`, started once so it binds its
/// broker, then stopped with its NATS left running.
async fn started_installation(
    root: &std::path::Path,
    ports: &[trellis_local_nats::ReservedPort],
) -> (crate::RuntimeConfig, trellis_local_nats::LocalNats) {
    let ports: Vec<u16> = ports.iter().map(|reserved| reserved.port()).collect();
    let origin = format!("http://127.0.0.1:{}", ports[0]);
    let (config, broker) = installation(root, &ports, &origin, "loopback_development", None).await;
    let (stop, mut runtime) =
        super::runtime_rotation::start_runtime(&config, crate::RuntimeMode::All, &origin, &[])
            .await;
    stop.stop();
    tokio::time::timeout(Duration::from_secs(30), &mut runtime)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    (config, broker)
}

/// The Auth store of the stopped installation `config`, as its server
/// opens it.
fn store_of(config: &crate::RuntimeConfig) -> SqliteAuthorizationStore {
    let crate::StorageBackend::Sqlite(storage) = config.platform_storage_backend().unwrap();
    let store = crate::storage::SqliteStore::new(crate::SubsystemName::Platform, storage);
    let keys = crate::platform::callout_keys(config).unwrap();
    SqliteAuthorizationStore::open(&store, config, keys.target_account()).unwrap()
}

/// The installation's administrator.
fn administrator_of(store: &SqliteAuthorizationStore) -> String {
    store
        .reader()
        .unwrap()
        .query_row(
            "SELECT principal_id FROM auth_bootstrap_administrator",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Run `work` on the stopped installation `config`'s Auth store, off the
/// async workers as the server runs store work.
async fn with_store<T: Send + 'static>(
    config: &crate::RuntimeConfig,
    work: impl FnOnce(&SqliteAuthorizationStore) -> T + Send + 'static,
) -> T {
    let config = config.clone();
    crate::blocking::spawn(move || work(&store_of(&config)))
        .await
        .unwrap()
}

/// Whether a live user `principal` exists in the installation `config`.
async fn has_user(config: &crate::RuntimeConfig, principal: &str) -> bool {
    let principal = principal.to_owned();
    with_store(config, move |store| {
        store
            .reader()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM auth_principals WHERE principal_id=?1 \
                AND kind='user' AND state!='revoked')",
                [&principal],
                |row| row.get(0),
            )
            .unwrap()
    })
    .await
}

/// `config` pointed at the broker of `other` with `other`'s Trellis NATS
/// user, as an operator who copied the wrong `[nats] servers` would have it.
fn misdirected(
    config: &crate::RuntimeConfig,
    other: &crate::RuntimeConfig,
) -> crate::RuntimeConfig {
    let mut misdirected = config.clone();
    misdirected.nats.as_mut().unwrap().servers = other.nats.as_ref().unwrap().servers.clone();
    misdirected
        .secrets
        .nats
        .users
        .trellis
        .clone_from(&other.secrets.nats.users.trellis);
    misdirected
}

/// The content of an export of the installation `config`.
async fn exported(config: &crate::RuntimeConfig) -> Vec<u8> {
    let mut content = Vec::new();
    crate::platform::export_installation(config, &mut content)
        .await
        .unwrap();
    content
}

/// An export or import pointed at another installation's broker refuses it
/// before it reads or changes anything: the export writes nothing, and the
/// import adds nothing to the database it would have paired with that
/// broker. Pointed at its own broker, the same import adds the export's
/// user.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_and_import_refuse_another_installations_broker() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let ports = reserve(2);
    let (source, _source_broker) =
        started_installation(&directory.path().join("source"), &ports[..4]).await;
    let (target, _target_broker) =
        started_installation(&directory.path().join("target"), &ports[4..]).await;
    let person = with_store(&source, |store| {
        create_user(store, &administrator_of(store), crate::clock::unix_now())
    })
    .await;

    let mut written = Vec::new();
    let refused =
        crate::platform::export_installation(&misdirected(&source, &target), &mut written).await;
    assert!(
        matches!(refused, Err(crate::RuntimeError::BrokerIdentity(_))),
        "{refused:?}"
    );
    assert!(written.is_empty());

    let content = exported(&source).await;
    let refused = crate::platform::import_installation(
        &misdirected(&target, &source),
        &mut std::io::Cursor::new(&content),
        false,
    )
    .await;
    assert!(
        matches!(refused, Err(crate::RuntimeError::BrokerIdentity(_))),
        "{refused:?}"
    );
    assert!(!has_user(&target, &person).await);

    crate::platform::import_installation(&target, &mut std::io::Cursor::new(&content), false)
        .await
        .unwrap();
    assert!(has_user(&target, &person).await);
}

/// Each export and import works in a folder of its own and removes only
/// that: a folder beside the platform database that another run, or
/// anything else, keeps is left as it is, and nothing of the runs stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_and_import_keep_to_their_own_working_folder() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let ports = reserve(1);
    let (config, _broker) = started_installation(directory.path(), &ports).await;
    let custody = with_store(&config, |store| store.custody_directory.clone()).await;
    let data = custody.parent().unwrap().to_owned();
    // The folders there; the database's own files come and go.
    let listing = || {
        let mut names: Vec<_> = std::fs::read_dir(&data)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name())
            .collect();
        names.sort();
        names
    };
    // Where every run used to work, holding someone else's file.
    let kept = custody.with_extension("installation");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join("in-use"), "another run's spool").unwrap();
    let before = listing();

    let (first, second) = tokio::join!(exported(&config), exported(&config));
    crate::platform::import_installation(&config, &mut std::io::Cursor::new(&first), true)
        .await
        .unwrap();
    crate::platform::import_installation(&config, &mut std::io::Cursor::new(&second), true)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(kept.join("in-use")).unwrap(),
        "another run's spool"
    );
    assert_eq!(listing(), before);
}

/// A preview only reads the database it previews against: it migrates and
/// rehearses a copy, so it reports what the import would do while another
/// connection holds the database's write lock, which a migration or any
/// write here would wait for, and leaves the database as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_preview_writes_nothing_to_the_database() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let ports = reserve(2);
    let (source, _source_broker) =
        started_installation(&directory.path().join("source"), &ports[..4]).await;
    let (target, _target_broker) =
        started_installation(&directory.path().join("target"), &ports[4..]).await;
    let person = with_store(&source, |store| {
        create_user(store, &administrator_of(store), crate::clock::unix_now())
    })
    .await;
    let content = exported(&source).await;

    let crate::StorageBackend::Sqlite(storage) = target.platform_storage_backend().unwrap();
    let holder = rusqlite::Connection::open(&storage.path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let report =
        crate::platform::import_installation(&target, &mut std::io::Cursor::new(&content), true)
            .await
            .unwrap();
    holder.execute_batch("ROLLBACK").unwrap();
    drop(holder);
    assert_eq!(report["applied"], false);
    assert!(
        report["created"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "user" && item["id"] == person.as_str()),
        "{report}"
    );
    assert!(!has_user(&target, &person).await);
}

/// JetStream of the installation `config`'s broker, as Trellis's own user.
async fn jetstream_of(config: &crate::RuntimeConfig) -> async_nats::jetstream::Context {
    async_nats::jetstream::new(crate::backup::connect(config).await.unwrap())
}

/// Whether the broker behind `jetstream` has the State bucket's stream.
async fn has_state_bucket(jetstream: &async_nats::jetstream::Context) -> bool {
    jetstream.get_stream("KV_trellis_state").await.is_ok()
}

/// A person on the stopped installation `config` with a State resource
/// registered through an app and a value kept in it, as the State API keeps
/// one. Returns the person and the value.
async fn person_with_state(config: &crate::RuntimeConfig) -> (String, Vec<u8>) {
    let (person, key) = with_store(config, |store| {
        let now = crate::clock::unix_now();
        let person = create_user(store, &administrator_of(store), now);
        let binding = trellis_protocol::SessionBinding::Native {
            client_id: "moving-app".into(),
            durable_dpop_jkt: "moving-key".into(),
        };
        let input = trellis_runtime_apis::types::StateRegisterRequest {
            resource_name: "settings".to_owned().into(),
            representation: trellis_runtime_apis::types::StateRepresentation {
                version: 1.into(),
                schema: trellis_runtime_apis::types::Bytes(br#"{"type":"object"}"#.to_vec()),
                extra: Default::default(),
            },
            accepts: vec![],
            expected_revision: "0".to_owned().into(),
            idempotency_key: id().parse().unwrap(),
            extra: Default::default(),
        };
        store
            .register_state(&person, &binding, &input, now)
            .unwrap()
            .unwrap();
        let (resource, _) =
            SqliteAuthorizationStore::state_namespace(&person, &binding, "settings").unwrap();
        let key: String = store
            .reader()
            .unwrap()
            .query_row(
                "SELECT physical_id FROM auth_resources WHERE resource_id=?1",
                [&resource],
                |row| row.get(0),
            )
            .unwrap();
        (person, key)
    })
    .await;
    let value = br#"{"value":"kept","representationVersion":1}"#.to_vec();
    crate::platform::state::open_store(&jetstream_of(config).await)
        .await
        .unwrap()
        .put(&key, value.clone().into())
        .await
        .unwrap();
    (person, value)
}

/// State moves only from and into a bucket that keeps it. With State
/// registered and its bucket gone from the broker, an export fails instead
/// of writing the values as missing, and makes no bucket. An import into
/// a bucket that expires values refuses before it changes anything, and
/// into one that keeps them writes the value.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn state_moves_only_from_and_into_a_bucket_that_keeps_it() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let directory = tempfile::tempdir().unwrap();
    let ports = reserve(2);
    let (source, _source_broker) =
        started_installation(&directory.path().join("source"), &ports[..4]).await;
    let (target, _target_broker) =
        started_installation(&directory.path().join("target"), &ports[4..]).await;
    let (person, value) = person_with_state(&source).await;
    let content = exported(&source).await;

    let source_jetstream = jetstream_of(&source).await;
    source_jetstream
        .delete_stream("KV_trellis_state")
        .await
        .unwrap();
    let mut written = Vec::new();
    let refused = crate::platform::export_installation(&source, &mut written).await;
    assert!(
        matches!(refused, Err(crate::RuntimeError::Platform(_))),
        "{refused:?}"
    );
    assert!(!has_state_bucket(&source_jetstream).await);

    let target_jetstream = jetstream_of(&target).await;
    target_jetstream
        .delete_stream("KV_trellis_state")
        .await
        .unwrap();
    let expiring = target_jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "trellis_state".into(),
            max_age: Duration::from_secs(3600),
            ..Default::default()
        })
        .await
        .unwrap();
    let refused =
        crate::platform::import_installation(&target, &mut std::io::Cursor::new(&content), false)
            .await;
    assert!(
        matches!(refused, Err(crate::RuntimeError::Platform(_))),
        "{refused:?}"
    );
    assert!(!has_user(&target, &person).await);
    assert_eq!(expiring.status().await.unwrap().values(), 0);

    target_jetstream
        .delete_stream("KV_trellis_state")
        .await
        .unwrap();
    let report =
        crate::platform::import_installation(&target, &mut std::io::Cursor::new(&content), false)
            .await
            .unwrap();
    assert!(has_user(&target, &person).await);
    let kept = crate::platform::state::existing_store(&target_jetstream)
        .await
        .unwrap()
        .unwrap();
    let mut keys = kept.keys().await.unwrap();
    let key = futures_util::StreamExt::next(&mut keys)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kept.get(&key).await.unwrap().as_deref(),
        Some(value.as_slice()),
        "{report}"
    );
}
