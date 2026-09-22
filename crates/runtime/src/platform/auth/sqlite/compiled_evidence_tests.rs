//! Correctness checks for immutable installed-evidence reuse, selected-surface compatibility reuse, and cheap current binding reads.
//! caches and their cheap binding reads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::Digest as _;
use tokio::sync::Notify;
use trellis_idl::project::{GenerateConfig, PackageManifest, PackageMetadata};
use trellis_idl::{
    canonical_package, compile_evidence, compile_project, ActionId, ActionKind, ActionSelection,
    CanonicalMode, CompatibilityReport, InteractionDirection, InteractionSelection,
    PackageEvidence, PackageSourceEvidence, SourceUnit,
};

use super::common::sql_error;
use super::deployments::insert_deployment_profile;
use super::grants::{accept_package_evidence, install_participant};
use super::SqliteAuthorizationStore;
use crate::platform::auth::compiled_evidence::{
    CompiledEvidenceCache, CompiledInstalledEvidence, SemanticJobError,
};
use crate::platform::auth::current_api_bindings;
use crate::platform::auth::evidence::{
    ApiRuntimeProjection, PackageEvidenceInput, ParticipantRuntimeProjection,
    ResourceRuntimeProjection,
};
use crate::platform::auth::{
    resolve_api_bindings, AuthorizationStateError, DeploymentProfileRecord, DeploymentProfileState,
    ParticipantBindingRecord, ParticipantBindingState, PrincipalKind,
};

const TINY_SOURCE: &str = r#"
model Input { value: string; }
model Output { value: string; }
api orders@v1 {
  title "Orders"; description "Orders."; version "1.0.0";
  rpc Required { input Input; output Output; }
  capabilities { public { allows { rpc Required; } } }
}
service Only { implements orders; }
service Other { implements orders; }
"#;

const MULTI_SERVICE_SOURCE: &str = r#"
model Input { value: string; }
model Output { value: string; }
api orders@v1 {
  title "Orders"; description "Orders."; version "1.0.0";
  rpc Required { input Input; output Output; }
  capabilities { public { allows { rpc Required; } } }
}
service First { implements orders; }
service Second { implements orders; }
service Third { implements orders; }
"#;

const CONSUMER_SOURCE: &str = r#"
model Input { value: string; }
model Output { value: string; }
api orders@v1 {
  title "Orders"; description "Orders."; version "1.0.0";
  rpc Required { input Input; output Output; }
  capabilities { public { allows { rpc Required; } } }
}
service Consumer { use orders { rpc Required; } }
"#;

fn package_evidence(source: &str, version: &str) -> PackageEvidence {
    let manifest = PackageManifest {
        package: PackageMetadata {
            name: "cache-test".into(),
            version: version.parse().expect("version"),
        },
        sources: BTreeMap::from([("contract".into(), "contract.trellis".into())]),
        dependencies: BTreeMap::new(),
        generate: GenerateConfig::default(),
        default_registry: None,
        registries: BTreeMap::new(),
    };
    let graph = compile_project(
        &manifest,
        vec![SourceUnit {
            alias: "contract".into(),
            path: PathBuf::from("contract.trellis"),
            source: source.into(),
        }],
        BTreeMap::new(),
    )
    .expect("compile package");
    PackageEvidence {
        root_package: "cache-test".into(),
        root_digest: graph.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: "cache-test".into(),
            version: version.parse().expect("version"),
            digest: graph.root_digest().into(),
            source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation)
                .expect("canonical package"),
        }],
    }
}

fn installed(
    evidence: PackageEvidence,
    participant_path: &str,
) -> (ParticipantBindingRecord, String) {
    ParticipantBindingRecord::from_package_evidence(
        &PackageEvidenceInput {
            package_digest: evidence.root_digest.clone(),
            package_evidence: evidence,
            participant_path: participant_path.to_owned(),
        },
        1,
    )
    .expect("installed participant")
}

async fn install_custom(
    store: &SqliteAuthorizationStore,
    record: &ParticipantBindingRecord,
    evidence_json: &str,
) {
    let record = record.clone();
    let evidence_json = evidence_json.to_owned();
    store
        .run(move |connection| {
            accept_package_evidence(
                connection,
                &record.package_digest,
                "cache-test",
                &evidence_json,
                false,
                None,
                1,
            )?;
            install_participant(connection, &record, None)?;
            Ok(())
        })
        .await
        .expect("install custom participant");
}

async fn install_custom_revision(
    store: &SqliteAuthorizationStore,
    record: &ParticipantBindingRecord,
    evidence_json: &str,
    expected_revision: u64,
) {
    let record = record.clone();
    let evidence_json = evidence_json.to_owned();
    store
        .run(move |connection| {
            accept_package_evidence(
                connection,
                &record.package_digest,
                "cache-test",
                &evidence_json,
                false,
                None,
                1,
            )?;
            install_participant(connection, &record, Some(expected_revision))?;
            Ok(())
        })
        .await
        .expect("install custom revision");
}

fn compiled_fixture(evidence_digest: &str) -> CompiledInstalledEvidence {
    let evidence = package_evidence(TINY_SOURCE, "1.0.0");
    let graph = compile_evidence(evidence.clone()).expect("compile fixture");
    CompiledInstalledEvidence {
        evidence_digest: evidence_digest.to_owned(),
        package_digest: evidence.root_digest,
        graph: Arc::new(graph),
    }
}

fn deployment(
    deployment_id: &str,
    participant_id: &str,
    state: DeploymentProfileState,
) -> DeploymentProfileRecord {
    DeploymentProfileRecord {
        deployment_id: deployment_id.to_owned(),
        kind: PrincipalKind::Service,
        display_name: deployment_id.to_owned(),
        participant_id: Some(participant_id.to_owned()),
        portal_id: None,
        review_mode: None,
        requires_device_delegation: false,
        expires_at: None,
        state,
        created_at: 1,
        updated_at: 1,
        version: 1,
    }
}

#[tokio::test]
async fn shared_installed_document_compiles_once_and_is_shared() {
    let store = SqliteAuthorizationStore::open_in_memory().expect("store");
    let evidence = package_evidence(TINY_SOURCE, "1.0.0");
    let (consumer, consumer_json) = installed(evidence.clone(), "Only");
    let (other, other_json) = installed(evidence, "Other");
    assert_eq!(consumer.evidence_digest, other.evidence_digest);
    install_custom(&store, &consumer, &consumer_json).await;
    install_custom(&store, &other, &other_json).await;

    let first = store
        .compiled_installed_evidence(&consumer.evidence_digest)
        .await
        .expect("first compile");
    let clone = store.clone();
    let second = clone
        .compiled_installed_evidence(&other.evidence_digest)
        .await
        .expect("shared compile");
    assert!(Arc::ptr_eq(&first, &second), "graph Arc is shared");
    assert_eq!(first.evidence_digest, consumer.evidence_digest);
    assert_eq!(store.compiled_evidence_counters().0, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cold_lookups_compile_once_and_survive_cancellation() {
    let cache = Arc::new(CompiledEvidenceCache::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let owner_started = Arc::new(Notify::new());
    let release_owner = Arc::new(Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let owner_started = Arc::clone(&owner_started);
        let release_owner = Arc::clone(&release_owner);
        tasks.push(tokio::spawn(async move {
            cache
                .compiled_installed_evidence("shared-document", move || {
                    let calls = Arc::clone(&calls);
                    let owner_started = Arc::clone(&owner_started);
                    let release_owner = Arc::clone(&release_owner);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        owner_started.notify_one();
                        release_owner.notified().await;
                        Ok(compiled_fixture("shared-document"))
                    }
                })
                .await
        }));
    }
    owner_started.notified().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "single-flight owner");
    tasks[0].abort();
    release_owner.notify_one();
    let mut succeeded = 0;
    for task in tasks.into_iter().skip(1) {
        succeeded += usize::from(task.await.expect("job completes").is_ok());
    }
    assert_eq!(succeeded, 7);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no duplicate compute");
    assert_eq!(cache.counters().0, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_key_waiters_reuse_the_single_owner_result() {
    let cache = Arc::new(CompiledEvidenceCache::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let owner_started = Arc::new(Notify::new());
    let release_owner = Arc::new(Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let owner_started = Arc::clone(&owner_started);
        let release_owner = Arc::clone(&release_owner);
        tasks.push(tokio::spawn(async move {
            cache
                .compiled_installed_evidence("shared-document", move || {
                    let calls = Arc::clone(&calls);
                    let owner_started = Arc::clone(&owner_started);
                    let release_owner = Arc::clone(&release_owner);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        owner_started.notify_one();
                        release_owner.notified().await;
                        Ok(compiled_fixture("shared-document"))
                    }
                })
                .await
        }));
    }
    owner_started.notified().await;
    release_owner.notify_one();
    let mut values = Vec::new();
    for task in tasks {
        values.push(task.await.expect("job completes").expect("compiled"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one computation");
    assert_eq!(cache.counters().0, 1);
    assert_eq!(
        cache.attempts().0,
        1,
        "one owned attempt despite concurrent waiters"
    );
    for value in &values[1..] {
        assert!(Arc::ptr_eq(&values[0], value), "waiters share one Arc");
    }
    let resident = cache
        .compiled_installed_evidence("shared-document", || async {
            Ok(compiled_fixture("shared-document"))
        })
        .await
        .expect("resident result");
    assert!(Arc::ptr_eq(&values[0], &resident));
    assert_eq!(cache.attempts().0, 1);
    assert_eq!(cache.counters().0, 1);
    let (peak_in_flight, _, _) = cache.peaks();
    assert_eq!(peak_in_flight, 1, "waiters share the single owner flight");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comparison_waiters_reuse_the_single_owner_result() {
    let cache = Arc::new(CompiledEvidenceCache::new());
    let consumer = compiled_fixture("consumer-document");
    let provider = compiled_fixture("provider-document");
    let selection = InteractionSelection {
        api: consumer
            .graph
            .packages()
            .values()
            .flat_map(|package| package.apis().keys())
            .next()
            .expect("fixture api")
            .clone(),
        actions: BTreeSet::new(),
        optional_capabilities: BTreeSet::new(),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let owner_started = Arc::new(Notify::new());
    let release_owner = Arc::new(Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let owner_started = Arc::clone(&owner_started);
        let release_owner = Arc::clone(&release_owner);
        let consumer = consumer.clone();
        let provider = provider.clone();
        let selection = selection.clone();
        tasks.push(tokio::spawn(async move {
            cache
                .compare_selection(&consumer, &selection, &provider, move || {
                    let calls = Arc::clone(&calls);
                    let owner_started = Arc::clone(&owner_started);
                    let release_owner = Arc::clone(&release_owner);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        owner_started.notify_one();
                        release_owner.notified().await;
                        Ok(CompatibilityReport::default())
                    }
                })
                .await
        }));
    }
    owner_started.notified().await;
    release_owner.notify_one();
    let mut values = Vec::new();
    for task in tasks {
        values.push(task.await.expect("job completes").expect("compared"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.attempts().1, 1);
    assert_eq!(cache.counters().1, 1);
    for value in &values[1..] {
        assert!(Arc::ptr_eq(&values[0], value));
    }
}

#[tokio::test]
async fn document_identity_is_exact_and_failed_loads_are_not_cached() {
    let store = SqliteAuthorizationStore::open_in_memory().expect("store");
    let first_evidence = package_evidence(MULTI_SERVICE_SOURCE, "1.0.0");
    let second_evidence = package_evidence(MULTI_SERVICE_SOURCE, "1.0.1");
    assert_eq!(first_evidence.root_digest, second_evidence.root_digest);
    let (first, first_json) = installed(first_evidence, "First");
    let (second, second_json) = installed(second_evidence, "Second");
    assert_ne!(first.evidence_digest, second.evidence_digest);
    install_custom(&store, &first, &first_json).await;
    install_custom(&store, &second, &second_json).await;

    let first_graph = store
        .compiled_installed_evidence(&first.evidence_digest)
        .await
        .expect("first document");
    let second_graph = store
        .compiled_installed_evidence(&second.evidence_digest)
        .await
        .expect("second document");
    assert_eq!(first_graph.package_digest, second_graph.package_digest);
    assert_ne!(first_graph.evidence_digest, second_graph.evidence_digest);
    assert_eq!(store.compiled_evidence_counters().0, 2);

    let (third, third_json) = installed(package_evidence(MULTI_SERVICE_SOURCE, "1.0.2"), "Third");
    install_custom(&store, &third, &third_json).await;
    // A document whose bytes do not hash to its claimed digest is rejected.
    let forged_json = format!("{third_json} ");
    let forged_digest = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(forged_json.as_bytes()));
    let forged_package_digest = "f".repeat(43);
    let wrong_hash = "w".repeat(43);
    // A document whose bytes hash correctly but claim a different package
    // semantic identity is rejected as well.
    let wrong_hash_job = wrong_hash.clone();
    store
        .run({
            let forged_digest = forged_digest.clone();
            move |connection| {
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence
                         (package_digest, platform_trusted, accepted_at, trusted_at, trusted_by)
                         VALUES (?1, 0, 1, NULL, NULL)",
                        [&forged_package_digest],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence_documents
                         (evidence_digest, package_digest, evidence_json, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![forged_digest, &forged_package_digest, forged_json, 1],
                    )
                    .map_err(sql_error)?;
                connection
                    .execute(
                        "INSERT INTO auth_package_evidence_documents
                         (evidence_digest, package_digest, evidence_json, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![wrong_hash_job.as_str(), &forged_package_digest, "{}", 1],
                    )
                    .map_err(sql_error)?;
                Ok(())
            }
        })
        .await
        .expect("insert forged document");
    for _ in 0..2 {
        assert!(matches!(
            store.compiled_installed_evidence(&forged_digest).await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
        assert!(matches!(
            store.compiled_installed_evidence(&wrong_hash).await,
            Err(AuthorizationStateError::InvalidRecord(_))
        ));
    }
    assert!(matches!(
        store
            .compiled_installed_evidence("not-the-document-hash")
            .await,
        Err(AuthorizationStateError::ParticipantMissing)
    ));
    assert_eq!(
        store.compiled_evidence_counters().0,
        2,
        "failures are not cached"
    );

    assert!(store
        .compiled_installed_evidence(&third.evidence_digest)
        .await
        .is_ok());
    assert_eq!(store.compiled_evidence_counters().0, 3);
    assert!(matches!(
        store.compiled_installed_evidence("missing-document").await,
        Err(AuthorizationStateError::ParticipantMissing)
    ));
}

#[tokio::test]
async fn bounded_ready_entries_evict_while_held_arcs_stay_valid() {
    let cache = CompiledEvidenceCache::new();
    let mut first = None;
    for index in 0..17 {
        let key = format!("document-{index}");
        let job_key = key.clone();
        let value = cache
            .compiled_installed_evidence(
                &key,
                move || async move { Ok(compiled_fixture(&job_key)) },
            )
            .await
            .expect("compiled document");
        if index == 0 {
            first = Some(value);
        }
    }
    assert_eq!(cache.ready_counts().0, 16);
    assert_eq!(cache.counters().0, 17);
    let held = first.expect("first arc retained");
    assert_eq!(held.evidence_digest, "document-0");
    assert_eq!(held.graph.root_digest(), held.package_digest);

    cache
        .compiled_installed_evidence("document-0", || async {
            Ok(compiled_fixture("document-0"))
        })
        .await
        .expect("evicted document recompiles");
    assert_eq!(cache.counters().0, 18);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comparison_reuse_is_exact_and_cpu_jobs_are_bounded() {
    let cache = Arc::new(CompiledEvidenceCache::new());
    let consumer = compiled_fixture("consumer-document");
    let provider = compiled_fixture("provider-document");
    let api = consumer
        .graph
        .packages()
        .values()
        .flat_map(|package| package.apis().keys())
        .next()
        .expect("fixture api")
        .clone();

    let warm_selection = InteractionSelection {
        api: api.clone(),
        actions: BTreeSet::from([ActionSelection {
            action: ActionId {
                kind: ActionKind::Rpc,
                name: "WarmAction".to_owned(),
            },
            direction: InteractionDirection::Call,
        }]),
        optional_capabilities: BTreeSet::new(),
    };
    for _ in 0..2 {
        let cpu = cache.cpu_semaphore();
        cache
            .compare_selection(&consumer, &warm_selection, &provider, move || async move {
                let _permit = cpu.acquire_owned().await.map_err(|_| {
                    SemanticJobError::Storage("semantic CPU pool closed".to_owned())
                })?;
                Ok(CompatibilityReport::default())
            })
            .await
            .expect("warm comparison");
    }
    assert_eq!(cache.counters().1, 1, "exact reuse");
    assert_eq!(cache.ready_counts().1, 1);

    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for index in 0..257 {
        let cache = Arc::clone(&cache);
        let cpu = cache.cpu_semaphore();
        let active = Arc::clone(&active);
        let maximum = Arc::clone(&maximum);
        let consumer = consumer.clone();
        let provider = provider.clone();
        let selection = InteractionSelection {
            api: api.clone(),
            actions: BTreeSet::from([ActionSelection {
                action: ActionId {
                    kind: ActionKind::Rpc,
                    name: format!("Action{index}"),
                },
                direction: InteractionDirection::Call,
            }]),
            optional_capabilities: BTreeSet::new(),
        };
        tasks.push(tokio::spawn(async move {
            cache
                .compare_selection(&consumer, &selection, &provider, move || async move {
                    let _permit = cpu.acquire_owned().await.map_err(|_| {
                        SemanticJobError::Storage("semantic CPU pool closed".to_owned())
                    })?;
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(CompatibilityReport::default())
                })
                .await
        }));
    }
    for task in tasks {
        task.await.expect("comparison task").expect("comparison");
    }
    assert!(maximum.load(Ordering::SeqCst) <= 2, "CPU jobs are bounded");
    assert_eq!(cache.ready_counts().1, 256);
    assert_eq!(cache.counters().1, 258);
    let (peak_in_flight, _, peak_comparisons) = cache.peaks();
    assert!(peak_in_flight <= 8, "in-flight jobs are bounded");
    assert!(peak_comparisons <= 256, "ready comparisons are bounded");
}

#[tokio::test]
async fn warm_caches_keep_provider_selection_current() {
    let store = SqliteAuthorizationStore::open_in_memory().expect("store");
    let (consumer, consumer_json) =
        installed(package_evidence(CONSUMER_SOURCE, "1.0.0"), "Consumer");
    let (provider_a, provider_a_json) = installed(
        package_evidence(&provider_source("ProviderA"), "1.0.0"),
        "ProviderA",
    );
    let (provider_b, provider_b_json) = installed(
        package_evidence(&provider_source("ProviderB"), "1.0.0"),
        "ProviderB",
    );
    install_custom(&store, &consumer, &consumer_json).await;
    install_custom(&store, &provider_a, &provider_a_json).await;
    install_custom(&store, &provider_b, &provider_b_json).await;
    store
        .run({
            let a = deployment("deployment-a", &provider_a.participant_id, DeploymentProfileState::Active);
            let b = deployment("deployment-b", &provider_b.participant_id, DeploymentProfileState::Active);
            move |connection| {
                for id in ["deployment-a", "deployment-b"] {
                    connection
                        .execute(
                            "INSERT INTO auth_principals (principal_id, kind, state, created_at, updated_at, version)
                             VALUES (?1, 'service', 'active', 1, 1, 1)",
                            [id],
                        )
                        .map_err(sql_error)?;
                }
                insert_deployment_profile(connection, &a)?;
                insert_deployment_profile(connection, &b)?;
                Ok(())
            }
        })
        .await
        .expect("provider deployments");

    let api = "cache-test.orders@v1";
    let first = resolve_api_bindings(&store, &consumer, Some("consumer-deployment"))
        .await
        .expect("initial binding");
    assert_eq!(first[api].provider_deployment_id, "deployment-a");
    let compiles = store.compiled_evidence_counters().0;
    let warm = resolve_api_bindings(&store, &consumer, Some("consumer-deployment"))
        .await
        .expect("warm binding");
    assert_eq!(warm[api].provider_deployment_id, "deployment-a");
    assert_eq!(
        store.compiled_evidence_counters().0,
        compiles,
        "warm resolution compiles nothing"
    );

    let (changed, changed_json) = installed(
        package_evidence(&incompatible_provider_source("ProviderA"), "1.0.2"),
        "ProviderA",
    );
    install_custom_revision(&store, &changed, &changed_json, 1).await;
    let reselected = resolve_api_bindings(&store, &consumer, Some("consumer-deployment"))
        .await
        .expect("evidence change reselects");
    assert_eq!(reselected[api].provider_deployment_id, "deployment-b");
    assert!(
        store.compiled_evidence_counters().0 > compiles,
        "changed evidence is compiled"
    );

    store
        .run(|connection| {
            connection
                .execute(
                    "UPDATE auth_deployment_profiles SET state = 'disabled'
                     WHERE deployment_id = 'deployment-b'",
                    [],
                )
                .map_err(sql_error)?;
            Ok(())
        })
        .await
        .expect("disable provider B");
    assert!(matches!(
        resolve_api_bindings(&store, &consumer, Some("consumer-deployment")).await,
        Err(AuthorizationStateError::NotAuthorized)
    ));
}

#[tokio::test]
async fn current_bindings_are_exact_and_compile_nothing() {
    let store = SqliteAuthorizationStore::open_in_memory().expect("store");
    store
        .put_api_binding(
            "consumer-deployment",
            "cache-test.billing@v1",
            "billing-provider",
        )
        .await
        .expect("billing binding");
    store
        .put_api_binding(
            "consumer-deployment",
            "trellis.events@v1",
            "events-provider",
        )
        .await
        .expect("events binding");
    store
        .put_api_binding("consumer-deployment", "unrelated@v1", "unrelated-provider")
        .await
        .expect("unrelated binding");
    let consumer = synthetic_consumer();
    let bindings = current_api_bindings(&store, &consumer, Some("consumer-deployment"))
        .await
        .expect("current bindings");
    assert_eq!(
        bindings["cache-test.orders@v1"].provider_deployment_id,
        "consumer-deployment"
    );
    assert_eq!(
        bindings["cache-test.billing@v1"].provider_deployment_id,
        "billing-provider"
    );
    assert_eq!(
        bindings["trellis.events@v1"].provider_deployment_id, "events-provider",
        "implicit Events binding"
    );
    assert!(!bindings.contains_key("unrelated@v1"));
    assert_eq!(store.compiled_evidence_counters().0, 0);
    assert!(matches!(
        current_api_bindings(&store, &consumer, Some("other-deployment")).await,
        Err(AuthorizationStateError::NotAuthorized)
    ));
    assert!(matches!(
        current_api_bindings(&store, &consumer, None).await,
        Err(AuthorizationStateError::NotAuthorized)
    ));
}

fn provider_source(service: &str) -> String {
    format!(
        r#"
model Input {{ value: string; }}
model Output {{ value: string; }}
api orders@v1 {{
  title "Orders"; description "Orders."; version "1.0.0";
  rpc Required {{ input Input; output Output; }}
  capabilities {{ public {{ allows {{ rpc Required; }} }} }}
}}
service {service} {{ implements orders; }}
"#
    )
}

fn incompatible_provider_source(service: &str) -> String {
    format!(
        r#"
model Input {{ value: string; }}
model Output {{ value: string; }}
api orders@v1 {{
  title "Orders"; description "Orders."; version "1.0.1";
  rpc Other {{ input Input; output Output; }}
  capabilities {{ public {{ allows {{ rpc Other; }} }} }}
}}
service {service} {{ implements orders; }}
"#
    )
}

fn api_projection() -> ApiRuntimeProjection {
    ApiRuntimeProjection {
        digest: "digest".to_owned(),
        major: 1,
        actions: BTreeMap::new(),
        capabilities: BTreeMap::new(),
    }
}

fn consumer_resource() -> ResourceRuntimeProjection {
    ResourceRuntimeProjection {
        kind: trellis_protocol::ParticipantResourceKind::EventConsumer,
        optional: false,
        title: "Deliveries".to_owned(),
        description: String::new(),
        representation: None,
        history: None,
        ttl_ms: None,
        desired_max_value: None,
        desired_max_object: None,
        desired_max_total: None,
        deadline_ms: None,
        payload_schema: None,
        result_schema: None,
        update_schema: None,
        job_key_path: None,
        job_key_policy: None,
        retry_attempts: None,
        retry_backoff_ms: Vec::new(),
        consumer_events: BTreeMap::new(),
        consumer_concurrency: None,
        consumer_replay_all: false,
    }
}

fn synthetic_consumer() -> ParticipantBindingRecord {
    let orders = "cache-test.orders@v1".to_owned();
    let mut implemented_apis = BTreeMap::new();
    implemented_apis.insert(orders.clone(), api_projection());
    let mut referenced_apis = implemented_apis.clone();
    referenced_apis.insert("cache-test.billing@v1".to_owned(), api_projection());
    referenced_apis.insert("trellis.events@v1".to_owned(), api_projection());
    let projection = ParticipantRuntimeProjection {
        participant_id: "cache-test.Consumer".to_owned(),
        participant_kind: trellis_protocol::ParticipantKind::Service,
        display_name: "Consumer".to_owned(),
        implemented_apis,
        referenced_apis,
        resources: BTreeMap::from([("deliveries".to_owned(), consumer_resource())]),
        required_grants: trellis_protocol::GrantSet::new(Vec::new()),
        optional_grant_bundles: BTreeMap::new(),
        required_capabilities: Vec::new(),
        optional_capability_definitions: BTreeMap::new(),
        companion_participant_id: None,
        companion_participant_kind: None,
        companion_required: false,
    };
    ParticipantBindingRecord {
        participant_id: "cache-test.Consumer".to_owned(),
        participant_kind: trellis_protocol::ParticipantKind::Service,
        participant_digest: "a".repeat(43),
        needs_digest: "b".repeat(43),
        package_digest: "p".repeat(43),
        evidence_digest: "e".repeat(43),
        participant_path: "Consumer".to_owned(),
        projection,
        resolved_at: 1,
        state: ParticipantBindingState::Resolved,
        error: None,
    }
}
