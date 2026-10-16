//! Real store and broker resource materialization; registered under phase_two_tests.

use super::*;

#[test]
fn builtin_provider_replay_preserves_independent_roles_and_refuses_disabled_or_retired_identity() {
    use trellis_runtime_apis::participants::{
        trellis_events_runtime, trellis_health_runtime, trellis_jobs_runtime, trellis_platform,
    };
    let f = fixture();
    let seeds: Vec<_> = (51..55)
        .map(|byte| URL_SAFE_NO_PAD.encode([byte; 32]))
        .collect();
    let platform = f
        .store
        .initialize_builtin_provider::<trellis_platform::Participant>(&seeds[0], NOW)
        .unwrap();
    let health = f
        .store
        .initialize_builtin_provider::<trellis_health_runtime::Participant>(&seeds[1], NOW)
        .unwrap();
    let jobs = f
        .store
        .initialize_builtin_provider::<trellis_jobs_runtime::Participant>(&seeds[2], NOW)
        .unwrap();
    let events = f
        .store
        .initialize_builtin_provider::<trellis_events_runtime::Participant>(&seeds[3], NOW)
        .unwrap();
    let deployments: std::collections::BTreeSet<_> = [&platform, &health, &jobs, &events]
        .map(|provider| provider.deployment_id.clone())
        .into_iter()
        .collect();
    assert_eq!(
        deployments.len(),
        4,
        "roles must not select the same deployment"
    );
    assert!(matches!(
        f.store
            .initialize_builtin_provider::<trellis_health_runtime::Participant>(&seeds[0], NOW),
        Err(AuthError::Denied)
    ));
    assert!(matches!(
        f.store
            .initialize_builtin_provider::<trellis_health_runtime::Participant>(
                &URL_SAFE_NO_PAD.encode([61; 32]),
                NOW
            ),
        Err(AuthError::Conflict)
    ));
    let disabled = PolicyMutation::PrincipalState {
        principal_id: health.principal_id.clone(),
        state: "disabled".into(),
        expected_revision: 1,
    };
    f.store
        .apply_policy(&mutation(&f.admin, &disabled), &disabled, NOW + 1)
        .unwrap();
    let retired = EnforcementScope::Identity(events.identity_key_id.clone());
    f.store
        .revoke_root(&mutation(&f.admin, &retired), &retired, NOW + 1)
        .unwrap();
    let reopened = open(&f.directory, SigningKey::from_bytes(&[42; 32]));
    let platform_replay = reopened
        .initialize_builtin_provider::<trellis_platform::Participant>(&seeds[0], NOW + 2)
        .unwrap();
    let jobs_replay = reopened
        .initialize_builtin_provider::<trellis_jobs_runtime::Participant>(&seeds[2], NOW + 2)
        .unwrap();
    assert_eq!(platform_replay.deployment_id, platform.deployment_id);
    assert_eq!(platform_replay.instance_id, platform.instance_id);
    assert_eq!(jobs_replay.deployment_id, jobs.deployment_id);
    assert_eq!(jobs_replay.identity_key_id, jobs.identity_key_id);
    assert!(matches!(
        reopened
            .initialize_builtin_provider::<trellis_health_runtime::Participant>(&seeds[1], NOW + 2),
        Err(AuthError::Denied)
    ));
    assert!(matches!(
        reopened
            .initialize_builtin_provider::<trellis_events_runtime::Participant>(&seeds[3], NOW + 2),
        Err(AuthError::Denied)
    ));
}

#[tokio::test]
async fn native_resource_materialization_preserves_data_delivery_and_rejects_foreign_ownership() {
    use std::{sync::Arc, time::Duration};
    use tokio::io::AsyncBufReadExt;
    let f = fixture();
    let seed = URL_SAFE_NO_PAD.encode([41; 32]);
    let deployment = f.store.initialize_builtin_provider::<trellis_runtime_apis::participants::trellis_platform::Participant>(&seed, NOW).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let binary =
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
    let mut server = tokio::process::Command::new(binary)
        .args(["-js", "-a", "127.0.0.1", "-p", &port.to_string(), "-sd"])
        .arg(directory.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("producer-resolved NATS executable");
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
    .expect("NATS readiness deadline");
    let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    let store = Arc::new(f.store);
    // No declarations really means no resources; do not invent a shared bucket
    // or skip the same installed-declaration materialization path.
    let health_seed = URL_SAFE_NO_PAD.encode([42; 32]);
    let health = store.initialize_builtin_provider::<trellis_runtime_apis::participants::trellis_health_runtime::Participant>(&health_seed,NOW).unwrap();
    let health_bindings =
        super::super::resources::materialize_native_kv(&store, &nats, &health.deployment_id, NOW)
            .await
            .unwrap();
    let mut admission = f.admission.clone();
    admission.credential = Credential::ProvisionedIdentity(health.identity_key_id.clone());
    admission.refresh_family_id = None;
    let issued = store.issue_authority(&admission, None, NOW).unwrap();
    assert_eq!(
        issued.authority.unsigned.binding,
        trellis_protocol::SessionBinding::Service {
            deployment_id: health.deployment_id,
            instance_id: health.instance_id,
            participant_id: health.participant_id
        }
    );
    assert!(health_bindings.kv.is_empty());
    let interrupted = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    interrupted.drain().await.unwrap();
    assert!(matches!(
        super::super::resources::materialize_native_kv(
            &store,
            &interrupted,
            &deployment.deployment_id,
            NOW,
        )
        .await,
        Err(AuthError::Broker(_))
    ));
    // A failed first allocation has never admitted a provider; it may finish on
    // the ordinary retry, unlike loss of previously committed storage below.
    let binding = super::super::resources::materialize_native_kv(
        &store,
        &nats,
        &deployment.deployment_id,
        NOW,
    )
    .await
    .unwrap();
    let jetstream = async_nats::jetstream::new(nats.clone());
    let bucket = jetstream
        .get_key_value(&binding.kv["connections"].bucket)
        .await
        .unwrap();
    // The declaration's desired 16KiB capacity must not become a hard limit.
    let payload = vec![7; 20 * 1024];
    bucket
        .put("presence", payload.clone().into())
        .await
        .unwrap();
    let replay = super::super::resources::materialize_native_kv(
        &store,
        &nats,
        &deployment.deployment_id,
        NOW + 1,
    )
    .await
    .unwrap();
    assert_eq!(
        jetstream
            .get_key_value(&replay.kv["connections"].bucket)
            .await
            .unwrap()
            .get("presence")
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        payload
    );
    assert_eq!(
        bucket.status().await.unwrap().max_age(),
        Duration::from_secs(120)
    );
    let mut stream = jetstream
        .get_stream(format!("KV_{}", binding.kv["connections"].bucket))
        .await
        .unwrap();
    let mut config = stream.info().await.unwrap().config.clone();
    config.description = Some("foreign allocation".into());
    jetstream.update_stream(config).await.unwrap();
    assert!(matches!(
        super::super::resources::materialize_native_kv(
            &store,
            &nats,
            &deployment.deployment_id,
            NOW + 2
        )
        .await,
        Err(AuthError::Conflict)
    ));
    assert_eq!(
        bucket.get("presence").await.unwrap().unwrap().as_ref(),
        payload
    );
    // Exercise the same operational provisioning path for stores and durable
    // delivery, including real stream sourcing and replaying an allocation.
    use futures_util::StreamExt;
    use trellis_runtime_apis::types::{AuthDeploymentCreateRequest, AuthInstanceProvisionRequest};
    crate::resources::ExpectedRuntimeResources::for_mode(
        crate::RuntimeMode::All,
        &crate::RuntimeConfig::from_toml_str("").unwrap(),
        async_nats::jetstream::stream::StorageType::File,
    )
    .converge_streams(nats.clone())
    .await
    .unwrap();
    let resource_source = r#"
        model Payload { value: string; }
        api resource@v1 {
          title "Resource"; description "Resource delivery";
          event Changed { payload Payload; }
          capabilities { capability read { title "Read"; description "Read events"; consequence "Read events"; consent_revision 1; allows { subscribe event Changed; } } }
        }
        service Owner {
          store files { title "Files"; description "Retained bytes"; ttl 0; desired_max_object 1KiB; }
          job work { title "Work"; description "Private work"; payload Payload; key_concurrency { path value; policy queue; } }
          job other { title "Other work"; description "Retained queue"; payload Payload; key_concurrency { path value; policy queue; } }
          consumer changes { title "Changes"; description "Durable changes"; events [resource.Changed]; concurrency 1; replay all; }
        }
    "#;
    use trellis_rs::generated::ParticipantDescriptor;
    let builtin =
        trellis_runtime_apis::participants::trellis_platform::Participant::package_evidence();
    let builtin: PackageEvidence =
        serde_json::from_value(serde_json::to_value(builtin).unwrap()).unwrap();
    let graph = compile_project(
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
            source: format!("{}\n{resource_source}", builtin.packages[0].source),
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let package = PackageEvidence {
        root_package: graph.root().to_string(),
        root_digest: graph.root_digest().into(),
        packages: vec![PackageSourceEvidence {
            name: graph.root().to_string(),
            version: graph.root_package().version().clone(),
            digest: graph.root_digest().into(),
            source: canonical_package(&graph, graph.root(), CanonicalMode::Presentation).unwrap(),
        }],
    };
    let owner = graph
        .root_package()
        .participants()
        .values()
        .find(|participant| participant.name() == "Owner")
        .unwrap();
    let create: AuthDeploymentCreateRequest = serde_json::from_value(serde_json::json!({
        "packageEvidence": package, "participantId":owner.identity().as_str(), "kind":"service", "title":"Resources", "idempotencyKey":ulid::Ulid::new().to_string()
    })).unwrap();
    let deployed = super::super::rpc_deployments::create_deployment(
        &store,
        &mutation(&f.admin, &create),
        &create,
        NOW,
    )
    .unwrap();
    let provision: AuthInstanceProvisionRequest = serde_json::from_value(serde_json::json!({
        "deploymentId":deployed.deployment.deployment_id, "identityPublicKey":URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&[83;32]).verifying_key().as_bytes()), "title":"Resource owner", "idempotencyKey":ulid::Ulid::new().to_string()
    })).unwrap();
    super::super::rpc_deployments::provision_service_instance(
        &store,
        &nats,
        &mutation(&f.admin, &provision),
        &provision,
        NOW,
    )
    .await
    .unwrap();
    let deployment = deployed.deployment.deployment_id.to_string();
    let bindings =
        super::super::resources::materialize_native_kv(&store, &nats, &deployment, NOW + 1)
            .await
            .unwrap();
    let objects = jetstream
        .get_object_store(&bindings.store["files"].name)
        .await
        .unwrap();
    let bytes = vec![9u8; 2048];
    objects
        .put("retained", &mut bytes.as_slice())
        .await
        .unwrap();
    let event = &bindings.event_consumers["changes"];
    jetstream
        .publish(event.filter_subjects[0].clone(), b"event".as_slice().into())
        .await
        .unwrap()
        .await
        .unwrap();
    let queue = &bindings.jobs.as_ref().unwrap().queues["work"];
    jetstream
        .publish(
            format!("{}.one.created", queue.publish_prefix),
            b"job".as_slice().into(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    let replayed =
        super::super::resources::materialize_native_kv(&store, &nats, &deployment, NOW + 2)
            .await
            .unwrap();
    let mut object = jetstream
        .get_object_store(&replayed.store["files"].name)
        .await
        .unwrap()
        .get("retained")
        .await
        .unwrap();
    let mut actual = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut object, &mut actual)
        .await
        .unwrap();
    assert_eq!(
        actual, bytes,
        "reconciliation retains bytes and desires are not hard quotas"
    );
    for (stream, consumer, expected) in [
        (&event.stream, &event.consumer_name, b"event".as_slice()),
        (
            replayed
                .jobs
                .as_ref()
                .unwrap()
                .work_stream
                .as_ref()
                .unwrap(),
            &queue.consumer_name,
            b"job".as_slice(),
        ),
    ] {
        let consumer: async_nats::jetstream::consumer::PullConsumer = jetstream
            .get_consumer_from_stream(consumer, stream)
            .await
            .unwrap();
        let mut messages = consumer.messages().await.unwrap();
        let message = tokio::time::timeout(Duration::from_secs(5), messages.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(message.payload.as_ref(), expected);
        message.double_ack().await.unwrap();
    }
    let jobs = replayed.jobs.as_ref().unwrap();
    let coordinator =
        trellis_rs::jobs::keys::NatsKeyCoordinator::open_for_service(nats.clone(), &jobs.namespace)
            .await
            .unwrap();
    let manager = trellis_rs::jobs::manager::JobManager::new_with_key_coordinator(
        trellis_rs::jobs::TrellisJobEventPublisher::new(nats.clone()),
        trellis_rs::jobs::bindings::JobsRuntimeBinding::try_from(&replayed)
            .unwrap()
            .jobs,
        trellis_rs::jobs::TrellisJobMetaSource,
        Arc::new(coordinator),
    );
    let first = manager
        .create("work", serde_json::json!({"value":"shared"}))
        .await
        .unwrap();
    let second = manager
        .create("work", serde_json::json!({"value":"shared"}))
        .await
        .unwrap();
    assert_ne!(
        first.id, second.id,
        "queue policy admits successive work for the same key"
    );
    let mut stream = jetstream
        .get_stream(format!("OBJ_{}", bindings.store["files"].name))
        .await
        .unwrap();
    let mut config = stream.info().await.unwrap().config.clone();
    let owned_config = config.clone();
    config.description = Some("foreign store".into());
    jetstream.update_stream(config).await.unwrap();
    assert!(matches!(
        super::super::resources::materialize_native_kv(&store, &nats, &deployment, NOW + 3).await,
        Err(AuthError::Conflict)
    ));
    assert!(
        objects.get("retained").await.is_ok(),
        "foreign allocations are not overwritten or deleted"
    );
    jetstream.update_stream(owned_config).await.unwrap();
    super::super::resources::materialize_native_kv(&store, &nats, &deployment, NOW + 4)
        .await
        .unwrap();
    let other = manager
        .create("other", serde_json::json!({"value":"shared"}))
        .await
        .unwrap();
    store
        .transaction(|connection| {
            super::super::resources::detach_deployment_resources(
                connection,
                &deployment,
                &std::collections::BTreeSet::from([("state".into(), "other".into())]),
                NOW + 5,
            )
        })
        .unwrap();
    assert!(
        objects.get("retained").await.is_ok(),
        "detach retains stored bytes"
    );
    let keys = jetstream
        .get_key_value(format!("JOBS_KEYS_{}", jobs.namespace))
        .await
        .unwrap();
    for name in ["files", "changes", "work"] {
        let resource = store
            .transaction(|connection| {
                let id: String = connection.query_row(
                    "SELECT resource_id FROM auth_resources WHERE owner_id=?1 AND local_name=?2",
                    rusqlite::params![deployment, name],
                    |row| row.get(0),
                )?;
                super::super::rpc_resources::read_resource(connection, &id)
            })
            .unwrap();
        let input:trellis_runtime_apis::types::AuthResourceRemoveRequest=serde_json::from_value(serde_json::json!({"resourceId":resource.resource_id,"expectedRevision":resource.revision,"idempotencyKey":ulid::Ulid::new().to_string()})).unwrap();
        let identity = mutation(&f.admin, &input);
        if name == "files" {
            let original = jetstream
                .get_stream(format!("OBJ_{}", bindings.store["files"].name))
                .await
                .unwrap()
                .cached_info()
                .config
                .clone();
            let mut foreign = original.clone();
            foreign.description = Some("foreign ownership".into());
            jetstream.update_stream(foreign).await.unwrap();
            assert!(matches!(
                super::super::rpc_resources::remove_resource(
                    &store,
                    &nats,
                    &identity,
                    &input,
                    NOW + 6
                )
                .await,
                Err(AuthError::Conflict)
            ));
            assert!(objects.get("retained").await.is_ok());
            jetstream.update_stream(original).await.unwrap();
        }
        super::super::rpc_resources::remove_resource(&store, &nats, &identity, &input, NOW + 6)
            .await
            .unwrap();
        super::super::rpc_resources::remove_resource(&store, &nats, &identity, &input, NOW + 7)
            .await
            .unwrap();
    }
    assert!(jetstream
        .get_object_store(&bindings.store["files"].name)
        .await
        .is_err());
    let events = jetstream.get_stream(&event.stream).await.unwrap();
    assert!(events.consumer_info(&event.consumer_name).await.is_err());
    assert!(
        events
            .get_first_raw_message_by_subject(&event.filter_subjects[0], 0)
            .await
            .is_ok(),
        "deleting a durable retains the shared event data"
    );
    assert!(jetstream
        .get_stream(&event.replay_binding.stream)
        .await
        .unwrap()
        .consumer_info(&event.replay_binding.consumer_name)
        .await
        .is_err());
    let work = jetstream
        .get_stream(crate::resources::JOBS_WORK_STREAM)
        .await
        .unwrap();
    assert!(work.consumer_info(&queue.consumer_name).await.is_err());
    assert!(work
        .consumer_info(&jobs.queues["other"].consumer_name)
        .await
        .is_ok());
    let lifecycle = jetstream
        .get_stream(crate::resources::JOBS_STREAM)
        .await
        .unwrap();
    assert!(lifecycle
        .get_first_raw_message_by_subject(&format!("{}.{}.*", queue.publish_prefix, first.id), 0)
        .await
        .is_err());
    assert!(lifecycle
        .get_first_raw_message_by_subject(
            &format!("{}.{}.*", jobs.queues["other"].publish_prefix, other.id),
            0
        )
        .await
        .is_ok());
    let remaining: Vec<_> = keys.keys().await.unwrap().collect().await;
    assert!(!remaining.is_empty());
    assert!(remaining.iter().all(|key| key
        .as_ref()
        .unwrap()
        .starts_with(&format!("{}.other.", jobs.namespace))));
    let saved_purges:serde_json::Value=store.transaction(|connection| {
        let encoded:String=connection.query_row("SELECT physical_id FROM auth_resource_removals WHERE json_valid(physical_id) AND json_extract(physical_id,'$.owned.namespace')=?1",[&jobs.namespace],|row|row.get(0))?;
        Ok(serde_json::from_str(&encoded)?)
    }).unwrap();
    let replacement =
        super::super::resources::materialize_native_kv(&store, &nats, &deployment, NOW + 8)
            .await
            .unwrap();
    let replacement_manager = trellis_rs::jobs::manager::JobManager::new_with_key_coordinator(
        trellis_rs::jobs::TrellisJobEventPublisher::new(nats.clone()),
        trellis_rs::jobs::bindings::JobsRuntimeBinding::try_from(&replacement)
            .unwrap()
            .jobs,
        trellis_rs::jobs::TrellisJobMetaSource,
        Arc::new(
            trellis_rs::jobs::keys::NatsKeyCoordinator::open_for_service(
                nats.clone(),
                &jobs.namespace,
            )
            .await
            .unwrap(),
        ),
    );
    let next = replacement_manager
        .create("work", serde_json::json!({"value":"replacement"}))
        .await
        .unwrap();
    // Replay the exact broker requests from the completed command after the
    // new allocation has written data, as a delayed earlier execution can do.
    for purge in saved_purges["purges"].as_array().unwrap() {
        jetstream
            .get_stream(purge["stream"].as_str().unwrap())
            .await
            .unwrap()
            .purge()
            .filter(purge["filter"].as_str().unwrap())
            .sequence(purge["before"].as_u64().unwrap())
            .await
            .unwrap();
    }
    assert!(
        lifecycle
            .get_first_raw_message_by_subject(
                &format!(
                    "{}.{}.*",
                    replacement.jobs.as_ref().unwrap().queues["work"].publish_prefix,
                    next.id
                ),
                0
            )
            .await
            .is_ok(),
        "delayed old purge preserves replacement lifecycle"
    );
    assert!(
        work.get_first_raw_message_by_subject(
            &replacement.jobs.as_ref().unwrap().queues["work"].work_subject,
            0
        )
        .await
        .is_ok(),
        "delayed old purge preserves replacement queued work"
    );
    let remaining: Vec<_> = keys.keys().await.unwrap().collect().await;
    assert!(
        remaining.iter().any(|key| key
            .as_ref()
            .unwrap()
            .starts_with(&format!("{}.work.", jobs.namespace))),
        "delayed old purge preserves replacement keyed coordination"
    );
    // Losing coordinator ownership must not turn a provisioning retry into an
    // empty coordinator that admits overlapping work for an already active key.
    jetstream
        .delete_key_value(format!("JOBS_KEYS_{}", jobs.namespace))
        .await
        .unwrap();
    for now in [NOW + 9, NOW + 10] {
        let outcome =
            super::super::resources::materialize_native_kv(&store, &nats, &deployment, now).await;
        assert!(matches!(outcome, Err(AuthError::Broker(_))), "{outcome:?}");
        assert!(
            trellis_rs::jobs::keys::NatsKeyCoordinator::open_for_service(
                nats.clone(),
                &jobs.namespace,
            )
            .await
            .is_err()
        );
    }
    assert!(work
        .get_first_raw_message_by_subject(
            &replacement.jobs.as_ref().unwrap().queues["work"].work_subject,
            0,
        )
        .await
        .is_ok());
    server.kill().await.unwrap();
}

#[test]
fn typed_status_reports_committed_retirement_and_keeps_issued_authority() {
    let f = fixture();
    let issued = f.store.issue_authority(&f.admission, None, NOW).unwrap();
    let session = issued
        .authority
        .unsigned
        .authorization_session_id
        .parse()
        .unwrap();
    let before = f.store.authorization_session_status(session).unwrap();
    assert_eq!(
        before.state,
        trellis_runtime_apis::types::AuthAuthorizationSessionState::Active
    );
    let scope = super::super::EnforcementScope::Grant(f.grant.clone());
    f.store
        .revoke_root(&super::mutation(&f.admin, &scope), &scope, NOW + 1)
        .unwrap();
    while f.store.enforce_next_page(NOW + 1).unwrap().is_some() {}
    let status = f
        .store
        .authorization_session_status(
            issued
                .authority
                .unsigned
                .authorization_session_id
                .parse()
                .unwrap(),
        )
        .unwrap();
    let evidence = status.revocation.unwrap();
    let cutoff: trellis_protocol::SignedSessionRevocation =
        serde_json::from_slice(&evidence.0 .0).unwrap();
    assert_eq!(cutoff.effective_cutoff, NOW + 1);
    assert_eq!(
        status.state,
        trellis_runtime_apis::types::AuthAuthorizationSessionState::Revoked
    );
    assert_eq!(
        f.store.resolve_authority(&issued.digest).unwrap(),
        issued.authority
    );
}
