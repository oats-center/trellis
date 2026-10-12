//! Resource lifecycle at the real SQLite and JetStream boundaries.
use super::super::phase_two_tests::{fixture, mutation, open, NOW};
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::SigningKey;

// Compile a native package input, never a handwritten wire schema or resource DTO.
fn evidence(source: String) -> trellis_idl::PackageEvidence {
    use std::{collections::BTreeMap, path::PathBuf};
    use trellis_idl::{
        project::{GenerateConfig, PackageManifest, PackageMetadata},
        CanonicalMode, SourceUnit,
    };
    let graph = trellis_idl::compile_project(
        &PackageManifest {
            package: PackageMetadata {
                name: "trellis".into(),
                version: "1.0.0".parse().unwrap(),
            },
            sources: BTreeMap::from([("main".into(), "main.trellis".into())]),
            dependencies: BTreeMap::new(),
            generate: GenerateConfig::default(),
            default_registry: None,
            registries: BTreeMap::new(),
        },
        vec![SourceUnit {
            alias: "main".into(),
            path: PathBuf::from("main.trellis"),
            source,
        }],
        BTreeMap::new(),
    )
    .unwrap();
    trellis_idl::PackageEvidence {
        root_package: graph.root().to_string(),
        root_digest: graph.root_digest().into(),
        packages: vec![trellis_idl::PackageSourceEvidence {
            name: graph.root().to_string(),
            version: graph.root_package().version().clone(),
            digest: graph.root_digest().into(),
            source: trellis_idl::canonical_package(
                &graph,
                graph.root(),
                CanonicalMode::Presentation,
            )
            .unwrap(),
        }],
    }
}

#[tokio::test]
async fn omission_retains_data_and_manual_removal_only_deletes_selected_detached_owned_kv() {
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    use trellis_runtime_apis::participants::trellis_platform::Participant;
    let f = fixture();
    let provider = f
        .store
        .initialize_builtin_provider::<Participant>(&URL_SAFE_NO_PAD.encode([71; 32]), NOW)
        .unwrap();
    let original: trellis_idl::PackageEvidence =
        serde_json::from_value(serde_json::to_value(Participant::package_evidence()).unwrap())
            .unwrap();
    let source = original
        .packages
        .iter()
        .find(|package| package.name == original.root_package)
        .unwrap()
        .source
        .clone();
    let start = source.find("kv connections {").unwrap();
    let mut depth = 0;
    let mut end = start;
    for (offset, byte) in source.as_bytes()[start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + offset + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let omitted = evidence(format!("{}{}", &source[..start], &source[end..]));
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut server = tokio::process::Command::new(
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into()),
    )
    .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
    .arg(directory.path())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut output = tokio::io::BufReader::new(server.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if line.contains("Server is ready") {
                return;
            }
        }
        panic!("NATS exited before readiness");
    })
    .await
    .unwrap();
    let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    let store = Arc::new(f.store);
    let bindings =
        super::super::resources::materialize_native_kv(&store, &nats, &provider.deployment_id, NOW)
            .await
            .unwrap();
    let jetstream = async_nats::jetstream::new(nats.clone());
    let bucket = jetstream
        .get_key_value(&bindings.kv["connections"].bucket)
        .await
        .unwrap();
    bucket
        .put("retained", b"version-a-data".as_slice().into())
        .await
        .unwrap();
    let unrelated = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "UNRELATED".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    unrelated
        .put("untouched", b"unrelated-data".as_slice().into())
        .await
        .unwrap();
    let query = AuthResourcesListRequest {
        owner_kind: Some(AuthResourceOwnerKind::Deployment),
        owner_id: Some(provider.deployment_id.parse().unwrap()),
        state: None,
        page: None,
        extra: Default::default(),
    };
    let active = store
        .transaction(|connection| Ok(list_resources(connection, &query)?.items.remove(0)))
        .unwrap();
    let detached_query = AuthResourcesListRequest {
        state: Some(AuthResourceState::Detached),
        ..query.clone()
    };
    assert!(store
        .transaction(|connection| list_resources(connection, &detached_query))
        .unwrap()
        .items
        .is_empty());
    let active_remove = AuthResourceRemoveRequest {
        resource_id: active.resource_id.clone(),
        expected_revision: active.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        remove_resource(
            &store,
            &nats,
            &mutation(&f.admin, &active_remove),
            &active_remove,
            NOW
        )
        .await,
        Err(AuthError::Conflict)
    ));
    // Apply a new native participant version which omits the allocation.
    let current = store
        .transaction(|connection| {
            super::super::rpc_deployments::read_deployment(connection, &provider.deployment_id)
        })
        .unwrap();
    let apply = AuthDeploymentApplyRequest {
        deployment_id: current.deployment_id.clone(),
        expected_revision: current.revision,
        package_evidence: serde_json::from_value(serde_json::to_value(&omitted).unwrap()).unwrap(),
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    super::super::rpc_deployments::apply_deployment(
        &store,
        &mutation(&f.admin, &apply),
        &apply,
        NOW + 1,
    )
    .unwrap();
    let detached = store
        .transaction(|connection| Ok(list_resources(connection, &detached_query)?.items.remove(0)))
        .unwrap();
    assert_eq!(detached.state, AuthResourceState::Detached);
    assert_eq!(
        bucket.get("retained").await.unwrap().unwrap().as_ref(),
        b"version-a-data"
    );
    drop(store);
    let store = Arc::new(open(&f.directory, SigningKey::from_bytes(&[42; 32])));
    assert_eq!(
        store
            .transaction(|connection| read_resource(connection, &detached.resource_id.to_string()))
            .unwrap()
            .state,
        AuthResourceState::Detached
    );
    assert_eq!(
        bucket.get("retained").await.unwrap().unwrap().as_ref(),
        b"version-a-data"
    );
    let current = store
        .transaction(|connection| {
            super::super::rpc_deployments::read_deployment(connection, &provider.deployment_id)
        })
        .unwrap();
    let readd = AuthDeploymentApplyRequest {
        expected_revision: current.revision,
        package_evidence: serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap(),
        idempotency_key: super::super::sqlite::id().into(),
        ..apply.clone()
    };
    super::super::rpc_deployments::apply_deployment(
        &store,
        &mutation(&f.admin, &readd),
        &readd,
        NOW + 2,
    )
    .unwrap();
    let provision = AuthInstanceProvisionRequest {
        deployment_id: current.deployment_id,
        identity_public_key: Some(
            URL_SAFE_NO_PAD
                .encode(SigningKey::from_bytes(&[72; 32]).verifying_key().as_bytes())
                .into(),
        ),
        title: "New version process".to_string().into(),
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    super::super::rpc_deployments::provision_instance(
        &store,
        &mutation(&f.admin, &provision),
        &provision,
        "service",
        NOW + 2,
    )
    .unwrap();
    let reattached = super::super::resources::materialize_native_kv(
        &store,
        &nats,
        &provider.deployment_id,
        NOW + 2,
    )
    .await
    .unwrap();
    assert_eq!(
        reattached.kv["connections"].bucket,
        bindings.kv["connections"].bucket
    );
    assert_eq!(
        bucket.get("retained").await.unwrap().unwrap().as_ref(),
        b"version-a-data"
    );
    // A retention reduction must fail rather than discard retained data.
    let ttl_start = start + source[start..end].find("ttl ").unwrap();
    let ttl_end = ttl_start + source[ttl_start..].find(';').unwrap() + 1;
    let reduced = evidence(format!(
        "{}ttl 1m;{}",
        &source[..ttl_start],
        &source[ttl_end..]
    ));
    let current = store
        .transaction(|connection| {
            super::super::rpc_deployments::read_deployment(connection, &provider.deployment_id)
        })
        .unwrap();
    let shrink = AuthDeploymentApplyRequest {
        expected_revision: current.revision,
        package_evidence: serde_json::from_value(serde_json::to_value(&reduced).unwrap()).unwrap(),
        idempotency_key: super::super::sqlite::id().into(),
        ..apply.clone()
    };
    assert!(
        matches!(super::super::rpc_deployments::apply_deployment(&store,&mutation(&f.admin,&shrink),&shrink,NOW+3),Err(AuthError::Invalid(code)) if code=="potential_data_loss")
    );
    let detach = AuthDeploymentMutationRequest {
        deployment_id: provider.deployment_id.parse().unwrap(),
        expected_revision: current.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    super::super::rpc_deployments::change_deployment(
        &store,
        &mutation(&f.admin, &detach),
        &detach,
        "disabled",
        NOW + 3,
    )
    .unwrap();
    let resource = store
        .transaction(|connection| read_resource(connection, &detached.resource_id.to_string()))
        .unwrap();
    let removal = AuthResourceRemoveRequest {
        resource_id: resource.resource_id.clone(),
        expected_revision: resource.revision,
        idempotency_key: super::super::sqlite::id().into(),
        extra: Default::default(),
    };
    assert!(matches!(
        remove_resource(
            &store,
            &nats,
            &mutation(&f.principal, &removal),
            &removal,
            NOW + 3
        )
        .await,
        Err(AuthError::Denied)
    ));
    let stale = AuthResourceRemoveRequest {
        expected_revision: active.revision,
        ..removal.clone()
    };
    assert!(matches!(
        remove_resource(&store, &nats, &mutation(&f.admin, &stale), &stale, NOW + 3).await,
        Err(AuthError::Conflict)
    ));
    // An owned catalog record does not authorize deleting a foreign provider.
    let mut stream = jetstream
        .get_stream(format!("KV_{}", bindings.kv["connections"].bucket))
        .await
        .unwrap();
    let mut config = stream.info().await.unwrap().config.clone();
    let owned_marker = config.description.clone();
    config.description = Some("foreign allocation".into());
    jetstream.update_stream(config.clone()).await.unwrap();
    let manual = mutation(&f.admin, &removal);
    assert!(matches!(
        remove_resource(&store, &nats, &manual, &removal, NOW + 3).await,
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        bucket.get("retained").await.unwrap().unwrap().as_ref(),
        b"version-a-data"
    );
    config.description = owned_marker;
    jetstream.update_stream(config).await.unwrap();
    let removed = remove_resource(&store, &nats, &manual, &removal, NOW + 4)
        .await
        .unwrap();
    assert_eq!(
        remove_resource(&store, &nats, &manual, &removal, NOW + 5)
            .await
            .unwrap(),
        removed
    );
    assert!(jetstream
        .get_key_value(&bindings.kv["connections"].bucket)
        .await
        .is_err());
    assert!(store
        .transaction(|connection| Ok(list_resources(connection, &query)?.items.is_empty()))
        .unwrap());
    assert_eq!(
        unrelated.get("untouched").await.unwrap().unwrap().as_ref(),
        b"unrelated-data"
    );
    server.kill().await.unwrap();
}
