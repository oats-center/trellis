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

/// The content of an export of the installation `config`.
async fn exported(config: &crate::RuntimeConfig) -> Vec<u8> {
    let mut content = Vec::new();
    crate::platform::export_installation(config, &mut content)
        .await
        .unwrap();
    content
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
