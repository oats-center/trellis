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

// ---------------------------------------------------------------------------
// What an import decides, at the Auth transaction an import runs: the
// records an export reads from one installation's database, applied to
// another's.

use super::super::installation_export::{apply, read_records, Outcome, Received};

/// The Auth storage over `store`, sealing secrets with `key`.
fn sealed(store: &Arc<SqliteAuthorizationStore>, key: u8) -> OAuthStorage {
    OAuthStorage::open(Arc::clone(store), &[key; 32]).unwrap()
}

/// Another installation, set up with nothing but its own administrator and
/// the fixture's API, whose `documents.read` capability tells people
/// `read_grants`.
fn destination(read_grants: &str) -> (tempfile::TempDir, OAuthStorage, String) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(open(&directory, SigningKey::from_bytes(&[43; 32])));
    store.initialize_issuer(NOW).unwrap();
    let admin = store
        .initialize_administrator(
            "admin",
            Some("a strong bootstrap password"),
            None,
            None,
            NOW,
        )
        .unwrap()
        .principal_id;
    accept(
        &store,
        &admin,
        &SOURCE.replace("Sees contents.", read_grants),
        false,
        NOW,
    );
    (directory, sealed(&store, 8), admin)
}

/// What an export of `source` carries, as an import receives it.
fn exported_records(source: &OAuthStorage, now: i64) -> Received {
    let records = source
        .authority
        .transaction(|connection| read_records(connection, source, now))
        .unwrap();
    Received {
        meta: serde_json::json!({}),
        records: records
            .into_iter()
            .map(|(kind, records)| (kind.to_owned(), records))
            .collect(),
        bundles: Vec::new(),
    }
}

/// `received` imported into `destination`.
fn imported(destination: &OAuthStorage, received: &Received) -> Outcome {
    destination
        .authority
        .transaction(|connection| apply(connection, destination, received, &[], NOW))
        .unwrap()
}

/// Whether `outcome` skipped a record of `kind` for `id`.
fn skipped(outcome: &Outcome, kind: &str, id: &str) -> bool {
    outcome
        .skipped
        .iter()
        .any(|item| item["kind"] == kind && item["id"] == id)
}

/// A single value `sql` reads from `storage`'s database.
fn read<T: rusqlite::types::FromSql>(
    storage: &OAuthStorage,
    sql: &str,
    values: impl rusqlite::Params,
) -> T {
    storage
        .authority
        .reader()
        .unwrap()
        .query_row(sql, values, |row| row.get(0))
        .unwrap()
}

/// Registers an OIDC provider for `issuer` on `store`, as `admin`.
fn provider(store: &SqliteAuthorizationStore, admin: &str, issuer: &str) -> String {
    let registration = super::super::lifecycle::OidcProviderRegistration {
        provider_id: id(),
        issuer: issuer.into(),
        display_name: "Company identity".into(),
        client_id: "trellis".into(),
        sealed_client_secret: None,
        issuer_certificate: None,
        config: serde_json::json!({ "scopes": [] }),
        claim_freshness_seconds: 300,
        expected_revision: 0,
    };
    store
        .transaction(|connection| {
            store.register_oidc_provider_in_transaction(
                connection,
                &mutation(admin, &registration),
                &registration,
                NOW,
            )
        })
        .unwrap();
    registration.provider_id
}

/// People's OIDC sign-ins, and the role mappings that give roles from a
/// provider's claims, move only through a provider that is active on the
/// new installation. A disabled provider moves with its settings, but a
/// person's sign-in through it is left out and reported, the person moving
/// with the rest, and its role mapping is skipped with a warning; a mapping
/// gives only roles the new installation has.
#[test]
fn sign_ins_and_role_mappings_move_only_through_active_providers() {
    let f = fixture();
    let active = provider(&f.store, &f.admin, "https://active.example.com/");
    let switched_off = provider(&f.store, &f.admin, "https://disabled.example.com/");
    let input: trellis_runtime_apis::__types::trellis::AuthUserCreateRequest =
        serde_json::from_value(serde_json::json!({
            "username": "linked",
            "displayName": "Linked person",
            "password": "a person's own password",
            "identities": [
                { "providerId": active, "subject": "active-subject" },
                { "providerId": switched_off, "subject": "disabled-subject" }
            ],
            "idempotencyKey": id()
        }))
        .unwrap();
    let person =
        super::super::rpc_users::create_user(&f.store, &mutation(&f.admin, &input), &input, NOW)
            .unwrap()
            .user
            .principal_id
            .to_string();
    let role = id();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RolePut {
            role_id: role.clone(),
            expected_revision: 0,
            title: "Readers".into(),
            description: "Read documents.".into(),
            capabilities: vec![f.read.clone()],
        },
        NOW,
    );
    let mappings: Vec<String> = [&active, &switched_off]
        .into_iter()
        .map(|provider| {
            let mapping = id();
            policy(
                &f.store,
                &f.admin,
                PolicyMutation::OidcMappingPut {
                    mapping_id: mapping.clone(),
                    provider_id: provider.clone(),
                    claim_name: "/groups".into(),
                    claim_value: "readers".into(),
                    role_ids: vec![role.clone()],
                    expected_revision: 0,
                },
                NOW,
            );
            mapping
        })
        .collect();
    let disable = trellis_runtime_apis::__types::trellis::AuthOIDCProviderMutationRequest {
        provider_id: switched_off.parse().unwrap(),
        expected_revision: trellis_runtime_apis::__types::trellis::AuthRevision(1.into()),
        idempotency_key: trellis_protocol::model::Ulid::generate().into(),
        extra: Default::default(),
    };
    super::super::rpc_users::disable_provider(
        &f.store,
        &mutation(&f.admin, &disable),
        &disable,
        NOW,
    )
    .unwrap();

    let source = sealed(&f.store, 7);
    let (_directory, target, _) = destination("Sees contents.");
    let outcome = imported(&target, &exported_records(&source, NOW));

    let sign_ins = |provider: &str| -> i64 {
        read(
            &target,
            "SELECT count(*) FROM auth_provider_identities WHERE principal_id=?1 AND provider=?2",
            [&person, provider],
        )
    };
    assert_eq!(sign_ins(&active), 1);
    assert_eq!(sign_ins(&switched_off), 0);
    assert!(
        skipped(&outcome, "userSignIn", &person),
        "{:?}",
        outcome.skipped
    );
    let mapped = |mapping: &str| -> i64 {
        read(
            &target,
            "SELECT count(*) FROM auth_oidc_mapping_roles WHERE mapping_id=?1 AND role_id=?2",
            [mapping, role.as_str()],
        )
    };
    assert_eq!(mapped(&mappings[0]), 1);
    assert_eq!(mapped(&mappings[1]), 0);
    assert!(
        skipped(&outcome, "oidcRoleMapping", &mappings[1]),
        "{:?}",
        outcome.skipped
    );
}

/// Only access the old installation still honours moves. A grant or a role
/// assignment that had expired by the time of the export, and a grant or a
/// role's capability that the old catalog no longer has, stay behind, even
/// where the new installation's catalog has that capability: the import
/// cannot bring back what the old installation no longer gives.
#[test]
fn only_access_the_source_still_honours_moves() {
    let f = fixture();
    let later = create_user(&f.store, &f.admin, NOW);
    let member = create_user(&f.store, &f.admin, NOW);
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::CapabilityGrant {
            principal_id: later.clone(),
            capability_id: f.read.clone(),
            expected_revision: 0,
            expires_at: Some(NOW + 10),
        },
        NOW,
    );
    let role = id();
    policy(
        &f.store,
        &f.admin,
        PolicyMutation::RolePut {
            role_id: role.clone(),
            expected_revision: 0,
            title: "Editors".into(),
            description: "Read and write documents.".into(),
            capabilities: vec![f.read.clone(), f.write.clone()],
        },
        NOW,
    );
    for (principal, expires_at) in [(&f.principal, None), (&member, Some(NOW + 10))] {
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::RoleAssign {
                principal_id: principal.clone(),
                role_id: role.clone(),
                expected_revision: 0,
                expires_at,
            },
            NOW,
        );
    }
    // The old installation's next build of the API no longer has
    // `documents.write`; grants and role capabilities that name it remain
    // in its database, but nothing honours them.
    let narrowed = SOURCE
        .replace(
            r#"  capability documents.write { title "Write"; description "Write documents."; grants "Changes contents."; allows { rpc Put; } }
"#,
            "",
        )
        .replace(" capability documents.write;", "");
    assert_ne!(narrowed, SOURCE);
    accept(&f.store, &f.admin, &narrowed, true, NOW + 1);

    let source = sealed(&f.store, 7);
    let (_directory, target, _) = destination("Sees contents.");
    let outcome = imported(&target, &exported_records(&source, NOW + 20));
    assert!(
        outcome
            .created
            .iter()
            .any(|item| item["kind"] == "user" && item["id"] == f.principal.as_str()),
        "{:?}",
        outcome.created
    );
    let granted = |principal: &str, capability: &str| -> bool {
        read(
            &target,
            "SELECT EXISTS(SELECT 1 FROM auth_direct_capabilities \
            WHERE principal_id=?1 AND capability_id=?2)",
            [principal, capability],
        )
    };
    assert!(granted(&f.principal, &f.read));
    assert!(!granted(&f.principal, &f.write));
    assert!(!granted(&later, &f.read));
    let role_gives: Vec<String> = target
        .authority
        .reader()
        .unwrap()
        .prepare(
            "SELECT rc.capability_id FROM auth_role_capabilities rc JOIN auth_roles r \
            ON r.role_id=rc.role_id AND r.identity_generation=rc.role_generation \
            WHERE r.role_id=?1 AND r.state='active' ORDER BY rc.capability_id",
        )
        .unwrap()
        .query_map([&role], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(role_gives, vec![f.read.clone()]);
    let holds_role = |principal: &str| -> bool {
        read(
            &target,
            "SELECT EXISTS(SELECT 1 FROM auth_role_assignments WHERE principal_id=?1 AND role_id=?2)",
            [principal, role.as_str()],
        )
    };
    assert!(holds_role(&f.principal));
    assert!(!holds_role(&member));
}
