//! Exercise existing SDK consumer fixtures against ordinary OAuth and real NATS.
use runtime_trellis::participants::{runtime_trellis_echo_provider, runtime_trellis_event_service};
use std::time::Duration;
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_runtime_apis::participants::trellis_platform;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sdk_consumers_preserve_original_publication_through_dlq_replay() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    // Own the runtime, not a version-manager launcher that forks it and cannot
    // forward SIGKILL when the consumer is stopped or this test unwinds.
    let node = std::process::Command::new("node")
        .args(["--print", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success(), "Node runtime discovery failed");
    let node = std::path::PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
    for sdk in ["rust", "typescript"] {
        let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
        let identity = runtime
            .register_service::<runtime_trellis_event_service::Participant>("durable events")
            .await
            .unwrap();
        let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
            .await
            .unwrap();
        let native = NativeOAuthClient::open(
            runtime.workdir().join("events-admin"),
            NativeCredentialScope {
                issuer: server.issuer,
                instance_id: server.trust.instance_id.clone(),
                account: server.trust.account.clone(),
                client_id: "trellis.cli".into(),
                credential_handle: ulid::Ulid::new(),
            },
            server.trust.clone(),
        )
        .await
        .unwrap();
        let device = native
            .start_device(&serde_json::json!([
                {
                    "type":"trellis_capabilities",
                    "required": [
                        "trellis.auth.v1:account.read",
                        "trellis.auth.v1:roles.manage",
                        "trellis.auth.v1:users.manage",
                        "trellis.auth.v1:users.manage",
                        "trellis.auth.v1:users.manage"
                                ],
                    "optional": []
                }
            ]))
            .await
            .unwrap();
        runtime
            .approve_device_login("trellis.cli", device.user_code())
            .await
            .unwrap();
        let generated = trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers.clone(),
            1_000,
            device.complete().await.unwrap(),
        ))
        .await
        .unwrap();
        let auth = trellis_runtime_apis::apis::trellis_auth_v1::Client::from_generated(generated);
        let user = auth
            .users_resolve(&serde_json::from_value(serde_json::json!({})).unwrap())
            .await
            .unwrap();
        for capability in [
            "runtime-trellis.events.v1:alpha.publish",
            "runtime-trellis.events.v1:events.inspect",
        ] {
            auth.capability_grants_grant(
                &serde_json::from_value(serde_json::json!({
                    "principalId": user.user.principal_id,
                    "capabilityId":capability,
                    "expectedRevision":"0",
                    "idempotencyKey":ulid::Ulid::new().to_string()
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        }
        let native = NativeOAuthClient::open(
            runtime.workdir().join("events-driver"),
            NativeCredentialScope {
                issuer: runtime.oauth_origin().into(),
                instance_id: server.trust.instance_id.clone(),
                account: server.trust.account.clone(),
                client_id: "trellis.cli".into(),
                credential_handle: ulid::Ulid::new(),
            },
            server.trust,
        )
        .await
        .unwrap();
        let device = native
            .start_device(&serde_json::json!([
                {
                    "type":"trellis_capabilities",
                    "required": [
                        "runtime-trellis.events.v1:alpha.publish",
                        "runtime-trellis.events.v1:events.inspect",
                        "trellis.events.v1:events.query",
                        "trellis.events.v1:consumers.inspect",
                        "trellis.events.v1:deadLetters.inspect",
                        "trellis.events.v1:deadLetters.replay"
                                ],
                    "optional": []
                }
            ]))
            .await
            .unwrap();
        runtime
            .approve_device_login("trellis.cli", device.user_code())
            .await
            .unwrap();
        let generated = trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers,
            1_000,
            device.complete().await.unwrap(),
        ))
        .await
        .unwrap();
        let publisher = runtime_trellis::apis::runtime_trellis_events_v1::Client::from_generated(
            generated.clone(),
        );
        let events =
            trellis_runtime_apis::apis::trellis_events_v1::Client::from_generated(generated);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let plain_identity = runtime
            .register_service::<runtime_trellis_echo_provider::Participant>(
                "service without Consumers",
            )
            .await
            .unwrap();
        let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
            .await
            .unwrap();
        let plain = runtime_trellis_echo_provider::Participant::connect(
            trellis_rs::service::ServiceConnectOptions::new(
                server.nats_servers,
                server.trust,
                plain_identity.seed(),
            ),
        )
        .await
        .unwrap();
        let plain_events = trellis_runtime_apis::apis::trellis_events_v1::Client::from_generated(
            plain.generated_client(),
        );
        // Without Consumer provisioning there is no Events transport authority:
        // the request cannot reach the resource handler (which would return Forbidden).
        let denied = plain_events
            .dead_letters_inspect(
                &serde_json::from_value(serde_json::json!({
                    "resourceId":"unbound-consumer",
                    "deadLetterId":"unbound-event"
                }))
                .unwrap(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                denied,
                trellis_rs::client::CallError::Timeout
                    | trellis_rs::client::CallError::Authentication(_)
            ),
            "unbound service unexpectedly reached Events: {denied}"
        );
        let mut command = if sdk == "typescript" {
            let mut command = std::process::Command::new(&node);
            command.args([
                "--experimental-transform-types",
                "integration/fixtures/runtime/events.ts",
                runtime.oauth_origin(),
                identity.seed(),
            ]);
            command
        } else {
            std::process::Command::new(
                std::env::var("TRELLIS_TEST_EVENTS_BIN")
                    .expect("producer-built events fixture required"),
            )
        };
        command
            .current_dir(&root)
            .env("TRELLIS_URL", runtime.oauth_origin())
            // Each SDK trusts the installation CA through its ordinary
            // configuration, never through system trust.
            .env("TRELLIS_CA_CERTIFICATE", &runtime.trust_roots()[0])
            .env("NODE_EXTRA_CA_CERTS", &runtime.trust_roots()[0])
            .env("TRELLIS_IDENTITY_SEED", identity.seed())
            .env("REVERSE", "false")
            .env("OTEL_SDK_DISABLED", "true")
            .env("CRASH", "false");
        let mut child = tokio::process::Command::from(command)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut phase = "consumer readiness";
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            let empty = runtime_trellis::types::Empty {
                extra: Default::default(),
            };
            let mut tick = tokio::time::interval(Duration::from_millis(100));
            loop {
                tick.tick().await;
                if publisher.delivery_stats(&empty).await.is_ok() {
                    break;
                }
            }
            phase = "original durable delivery";
            publisher
                .publish_alpha(&runtime_trellis::types::Sample {
                    site: sdk.into(),
                    value: "slow".into(),
                    extra: Default::default(),
                })
                .await
                .unwrap();
            loop {
                tick.tick().await;
                let stats = publisher.delivery_stats(&empty).await.unwrap();
                if stats.successes.contains(&"slow".into()) {
                    assert_eq!(stats.attempts.len(), 1);
                    break;
                }
            }
            phase = "DLQ capture";
            publisher
                .publish_alpha(&runtime_trellis::types::Sample {
                    site: sdk.into(),
                    value: "replay".into(),
                    extra: Default::default(),
                })
                .await
                .unwrap();
            let consumers = events
                .consumers_query(&serde_json::from_value(serde_json::json!({})).unwrap())
                .await
                .unwrap();
            let consumer = consumers
                .items
                .iter()
                .find(|item| item.filter_subjects.iter().any(|s| s.ends_with(".Alpha")))
                .unwrap();
            let row = loop {
                tick.tick().await;
                let rows = events
                    .dead_letters_query(
                        &serde_json::from_value(serde_json::json!({
                            "resourceId":consumer.resource_id,
                            "state": ["dead"]
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                if let Some(row) = rows.items.into_iter().next() {
                    break row;
                }
            };
            let inspect = serde_json::from_value(serde_json::json!({
                "resourceId":consumer.resource_id,
                "deadLetterId":row.dead_letter_id
            }))
            .unwrap();
            let original = events
                .dead_letters_inspect(&inspect)
                .await
                .unwrap()
                .dead_letter;
            // A second deployment of the same participant has the runtime capability
            // but cannot inspect or report this Consumer's delivery.
            let other_identity = runtime
                .register_service_sharing_apis::<runtime_trellis_event_service::Participant>(
                    "other Consumer owner",
                )
                .await
                .unwrap();
            let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
                .await
                .unwrap();
            let other = runtime_trellis_event_service::Participant::connect(
                trellis_rs::service::ServiceConnectOptions::new(
                    server.nats_servers.clone(),
                    server.trust.clone(),
                    other_identity.seed(),
                ),
            )
            .await
            .unwrap();
            let other_events =
                trellis_runtime_apis::apis::trellis_events_v1::Client::from_generated(
                    other.generated_client(),
                );
            let denied = other_events
                .dead_letters_inspect(&inspect)
                .await
                .unwrap_err();
            assert!(
                matches!(
                    &denied,
                    trellis_rs::client::CallError::Remote(payload) if payload.raw().contains("Forbidden")
                ),
                "cross-Consumer inspection: {denied}"
            );
            let denied = other_events
                .consumers_report_delivery(
                    &serde_json::from_value(serde_json::json!({
                        "resourceId":consumer.resource_id,
                        "sourceStream":"TRELLIS_EVENT_REPLAYS",
                        "sourceSequence":"1",
                        "deliveryCount":1,
                        "deliveryProof":"$JS.ACK.TRELLIS_EVENT_REPLAYS.foreign.1.1.1.1.0",
                        "replayGeneration":1,
                        "outcome":"succeeded"
                    }))
                    .unwrap(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(
                    &denied,
                    trellis_rs::client::CallError::Remote(payload) if payload.raw().contains("Forbidden")
                ),
                "cross-Consumer terminal report: {denied}"
            );
            let own = runtime_trellis_event_service::Participant::connect(
                trellis_rs::service::ServiceConnectOptions::new(
                    server.nats_servers,
                    server.trust,
                    identity.seed(),
                ),
            )
            .await
            .unwrap();
            let own_events = trellis_runtime_apis::apis::trellis_events_v1::Client::from_generated(
                own.generated_client(),
            );
            assert_eq!(
                own_events
                    .dead_letters_inspect(&inspect)
                    .await
                    .unwrap()
                    .dead_letter
                    .original_payload,
                original.original_payload
            );
            assert!(
                own_events
                    .dead_letters_replay(
                        &serde_json::from_value(serde_json::json!({
                            "resourceId":consumer.resource_id,
                            "deadLetterId":row.dead_letter_id,
                            "expectedRevision":row.revision,
                            "requestId":ulid::Ulid::new().to_string()
                        }))
                        .unwrap()
                    )
                    .await
                    .is_err(),
                "ownership must not authorize Replay"
            );
            assert!(
                own_events
                    .dead_letters_dismiss(
                        &serde_json::from_value(serde_json::json!({
                            "resourceId":consumer.resource_id,
                            "deadLetterId":row.dead_letter_id,
                            "expectedRevision":row.revision,
                            "requestId":ulid::Ulid::new().to_string()
                        }))
                        .unwrap()
                    )
                    .await
                    .is_err(),
                "ownership must not authorize Dismiss"
            );
            assert_eq!(
                events
                    .dead_letters_inspect(&inspect)
                    .await
                    .unwrap()
                    .dead_letter
                    .dead_letter
                    .revision,
                row.revision
            );
            phase = "authenticated consumer replay";
            let before = publisher.delivery_stats(&empty).await.unwrap();
            assert_eq!(
                before.attempts.len(),
                4,
                "{sdk}: initial delivery plus three failed handler attempts"
            );
            events
                .dead_letters_replay(
                    &serde_json::from_value(serde_json::json!({
                        "resourceId":consumer.resource_id,
                        "deadLetterId":row.dead_letter_id,
                        "expectedRevision":row.revision,
                        "requestId":ulid::Ulid::new().to_string()
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            loop {
                tick.tick().await;
                let stats = publisher.delivery_stats(&empty).await.unwrap();
                if stats.successes.contains(&"replay".into()) {
                    assert_eq!(
                        stats
                            .successes
                            .iter()
                            .filter(|s| s.as_str() == "replay")
                            .count(),
                        1
                    );
                    break;
                }
            }
            let replayed = loop {
                tick.tick().await;
                let detail = events
                    .dead_letters_inspect(&inspect)
                    .await
                    .unwrap()
                    .dead_letter;
                if serde_json::to_value(&detail.dead_letter.state).unwrap() == "resolved" {
                    break detail;
                }
            };
            assert_eq!(
                original.original_published_at,
                replayed.original_published_at
            );
            assert_eq!(original.original_payload, replayed.original_payload);
            assert_eq!(
                original.dead_letter.original_sequence,
                replayed.dead_letter.original_sequence
            );
            eprintln!(
                "{sdk}: durable original delivery and authenticated retained DLQ replay \
                        passed"
            );
        }).await;
        assert!(result.is_ok(), "{sdk}: timed out during {phase}");
        if sdk == "rust" {
            let consumers = events
                .consumers_query(
                    &serde_json::from_value(
                        serde_json::json!({"deploymentId":identity.deployment_id()}),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let consumer = consumers
                .items
                .iter()
                .find(|item| item.filter_subjects.iter().any(|s| s.ends_with(".Alpha")))
                .unwrap();
            tokio::time::timeout(Duration::from_secs(30), async {
                let mut tick = tokio::time::interval(Duration::from_millis(100));
                publisher
                    .publish_alpha(&runtime_trellis::types::Sample {
                        site: "rust".into(),
                        value: "fail".into(),
                        extra: Default::default(),
                    })
                    .await
                    .unwrap();
                let row = loop {
                    tick.tick().await;
                    let rows = events
                        .dead_letters_query(
                            &serde_json::from_value(serde_json::json!({
                                "resourceId":consumer.resource_id,
                                "state": ["dead"]
                            }))
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    if let Some(row) = rows.items.into_iter().next() {
                        break row;
                    }
                };
                events
                    .dead_letters_replay(
                        &serde_json::from_value(serde_json::json!({
                            "resourceId":consumer.resource_id,
                            "deadLetterId":row.dead_letter_id,
                            "expectedRevision":row.revision,
                            "requestId":ulid::Ulid::new().to_string()
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                loop {
                    tick.tick().await;
                    let detail = events
                        .dead_letters_inspect(
                            &serde_json::from_value(serde_json::json!({
                                "resourceId":consumer.resource_id,
                                "deadLetterId":row.dead_letter_id
                            }))
                            .unwrap(),
                        )
                        .await
                        .unwrap()
                        .dead_letter;
                    if serde_json::to_value(&detail.dead_letter.state).unwrap() == "dead"
                        && detail.dead_letter.revision != row.revision
                    {
                        assert_eq!(
                            detail.dead_letter.last_error.as_deref(),
                            Some("broker max deliveries exceeded"),
                            "replay exhaustion must be broker captured, not a service report"
                        );
                        assert_eq!(detail.dead_letter.deliveries, 3);
                        break;
                    }
                }
                eprintln!("rust: repeated replay handler exhaustion captured by broker advisory");
            })
            .await
            .expect("replay exhaustion must reach DLQ");
            let malformed = runtime_trellis::types::Sample {
                site: "rust".into(),
                value: "invalid".into(),
                extra: serde_json::Map::from_iter([("value".into(), serde_json::json!(42))]),
            };
            publisher.publish_alpha(&malformed).await.unwrap();
            let row = tokio::time::timeout(Duration::from_secs(30), async {
                let mut tick = tokio::time::interval(Duration::from_millis(100));
                loop {
                    tick.tick().await;
                    let rows = events
                        .dead_letters_query(
                            &serde_json::from_value(serde_json::json!({
                                "resourceId":consumer.resource_id,
                                "state": ["dead"]
                            }))
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    for row in rows.items {
                        let detail = events
                            .dead_letters_inspect(
                                &serde_json::from_value(serde_json::json!({
                                    "resourceId":consumer.resource_id,
                                    "deadLetterId":row.dead_letter_id
                                }))
                                .unwrap(),
                            )
                            .await
                            .unwrap()
                            .dead_letter;
                        use base64::Engine as _;
                        let encoded = serde_json::to_value(&detail.original_payload).unwrap();
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(encoded.as_str().unwrap())
                            .unwrap();
                        if serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["value"]
                            == 42
                        {
                            return row;
                        }
                    }
                }
            })
            .await
            .expect(
                "malformed signed payload must reach DLQ through Rust decoder \
                exhaustion",
            );
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            child = tokio::process::Command::new(&node)
                .args([
                    "--experimental-transform-types",
                    "integration/fixtures/runtime/events.ts",
                    runtime.oauth_origin(),
                    identity.seed(),
                ])
                .current_dir(&root)
                .env("REVERSE", "false")
                .env("CRASH", "false")
                .env("OTEL_SDK_DISABLED", "true")
                .env("NODE_EXTRA_CA_CERTS", &runtime.trust_roots()[0])
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            events
                .dead_letters_replay(
                    &serde_json::from_value(serde_json::json!({
                        "resourceId":consumer.resource_id,
                        "deadLetterId":row.dead_letter_id,
                        "expectedRevision":row.revision,
                        "requestId":ulid::Ulid::new().to_string()
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(30), async {
                let mut tick = tokio::time::interval(Duration::from_millis(100));
                loop {
                    tick.tick().await;
                    let detail = events
                        .dead_letters_inspect(
                            &serde_json::from_value(serde_json::json!({
                                "resourceId":consumer.resource_id,
                                "deadLetterId":row.dead_letter_id
                            }))
                            .unwrap(),
                        )
                        .await
                        .unwrap()
                        .dead_letter;
                    if serde_json::to_value(&detail.dead_letter.state).unwrap() == "dead"
                        && detail.dead_letter.revision != row.revision
                    {
                        assert_eq!(
                            detail.dead_letter.deliveries, 1,
                            "permanent replay rejection must not exhaust broker retries"
                        );
                        assert!(
                            detail
                                .dead_letter
                                .last_error
                                .as_deref()
                                .is_some_and(|error| error.contains("Expected string")
                                    || error.contains("unexpected error")),
                            "terminal validation error: {:?}",
                            detail.dead_letter.last_error
                        );
                        break;
                    }
                }
            })
            .await
            .expect("TS payload validation must durably mark replay unreplayable");
            eprintln!("typescript: signed malformed retained payload durably unreplayable");
            use std::collections::BTreeMap;
            Box::pin(async {
                use trellis_rs::generated::ParticipantDescriptor;
                type P = runtime_trellis_event_service::Participant;
                let manifest: trellis_idl::project::PackageManifest = toml::from_str(include_str!(
                    "../../../integration/fixtures/runtime/trellis.toml"
                ))
                .unwrap();
                let builtin: trellis_idl::PackageEvidence = serde_json::from_value(
                    serde_json::to_value(trellis_platform::Participant::package_evidence())
                        .unwrap(),
                )
                .unwrap();
                let builtin_graph = trellis_idl::compile_evidence(builtin.clone()).unwrap();
                let source = include_str!("../../../integration/fixtures/runtime/contract.trellis");
                let omitted = source.replace(
                    "  consumer events { title \"Events\"; description \"Durable event \
                acceptance values.\"; events [events.Alpha, events.Beta]; concurrency \
                1; replay new; retry { attempts 3; backoff [150ms, 300ms]; } }",
                    "",
                );
                let graph = trellis_idl::compile_project(
                    &manifest,
                    vec![trellis_idl::SourceUnit {
                        alias: "runtime".into(),
                        path: "contract.trellis".into(),
                        source: omitted,
                    }],
                    BTreeMap::from([(
                        "trellis".into(),
                        std::sync::Arc::unwrap_or_clone(builtin_graph),
                    )]),
                )
                .unwrap();
                let mut packages = builtin.packages;
                packages.push(trellis_idl::PackageSourceEvidence {
                    name: graph.root().to_string(),
                    version: graph.root_package().version().clone(),
                    digest: graph.root_digest().into(),
                    source: trellis_idl::canonical_package(
                        &graph,
                        graph.root(),
                        trellis_idl::CanonicalMode::Presentation,
                    )
                    .unwrap(),
                });
                packages.sort_by(|a, b| a.name.cmp(&b.name));
                let omitted_evidence = trellis_idl::PackageEvidence {
                    root_package: graph.root().to_string(),
                    root_digest: graph.root_digest().into(),
                    packages,
                };
                let deployment = auth
                    .deployments_create(
                        &serde_json::from_value(serde_json::json!({
                            "title":"Consumer lifecycle",
                            "kind":"service",
                            "participantId":P::ID,
                            "packageEvidence":omitted_evidence,
                            // Another deployment of this participant already
                            // serves the API in this runtime.
                            "acknowledgeSharedApis":true,
                            "idempotencyKey":ulid::Ulid::new().to_string()
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap()
                    .deployment;
                let (seed, key) = trellis_rs::auth::generate_session_keypair();
                auth.service_instances_provision(
                    &serde_json::from_value(serde_json::json!({
                        "deploymentId":deployment.deployment_id,
                        "title":"consumer lifecycle instance",
                        "identityPublicKey":key,
                        "idempotencyKey":ulid::Ulid::new().to_string()
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
                let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
                    .await
                    .unwrap();
                // This instance runs the revision without the Consumer. Its
                // authority is observed through a plain connection: connecting
                // the current build as a service would submit that build as an
                // update instead.
                use trellis_rs::auth::NativeOAuthServer;
                use trellis_rs::client::{ServiceConnectWithContractOptions, TrellisClient};
                use trellis_runtime_apis::apis::trellis_events_v1 as events_api;
                let pinned = |seed: String, server: NativeOAuthServer| async move {
                    TrellisClient::connect_service_with_contract(
                        ServiceConnectWithContractOptions {
                            nats_servers: server.nats_servers,
                            trust: server.trust,
                            participant_id: P::ID,
                            provisioned_identity_seed_base64url: &seed,
                            name: None,
                            timeout_ms: 10_000,
                        },
                    )
                    .await
                    .map(std::sync::Arc::new)
                    .unwrap()
                };
                let inspect = |client: std::sync::Arc<TrellisClient>,
                               probe: serde_json::Value| {
                    async move {
                        client
                            .request_json_value(
                                events_api::API_ID,
                                trellis_protocol::CatalogActionIdentity {
                                    kind: trellis_protocol::ApiSurfaceKind::Rpc,
                                    name: "DeadLetters.Inspect".into(),
                                    direction: trellis_protocol::PermissionAction::Call,
                                },
                                events_api::rpc::DeadLettersInspect::SUBJECT,
                                &probe,
                            )
                            .await
                    }
                };
                let service = pinned(seed.clone(), server).await;
                let added = auth
                    .deployments_apply(
                        &serde_json::from_value(serde_json::json!({
                            "deploymentId":deployment.deployment_id,
                            "packageEvidence":P::package_evidence(),
                            "expectedRevision":deployment.revision,
                            "idempotencyKey":ulid::Ulid::new().to_string()
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap()
                    .deployment;
                let (current_seed, current_key) = trellis_rs::auth::generate_session_keypair();
                auth.service_instances_provision(
                    &serde_json::from_value(serde_json::json!({
                        "deploymentId":deployment.deployment_id,
                        "title":"current Consumer implementation",
                        "identityPublicKey":current_key,
                        "idempotencyKey":ulid::Ulid::new().to_string()
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
                let added_consumer = tokio::time::timeout(Duration::from_secs(10), async {
                    let mut tick = tokio::time::interval(Duration::from_millis(100));
                    loop {
                        tick.tick().await;
                        let rows = events
                            .consumers_query(
                                &serde_json::from_value(serde_json::json!({
                                    "deploymentId":deployment.deployment_id
                                }))
                                .unwrap(),
                            )
                            .await
                            .unwrap();
                        if let Some(row) = rows.items.into_iter().next() {
                            break row;
                        }
                    }
                })
                .await
                .expect("Apply must materialize Consumer");
                let probe_json = serde_json::json!({
                    "resourceId":added_consumer.resource_id,
                    "deadLetterId":"absent-event"
                });
                let probe = serde_json::from_value(probe_json.clone()).unwrap();
                let denied = inspect(service.clone(), probe_json.clone()).await.unwrap_err();
                assert!(
                    !denied.to_string().contains("not found"),
                    "old pre-Apply authority must not gain access: {denied}"
                );
                let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
                    .await
                    .unwrap();
                let renewed = P::connect(trellis_rs::service::ServiceConnectOptions::new(
                    server.nats_servers,
                    server.trust,
                    &current_seed,
                ))
                .await
                .unwrap();
                let scoped = trellis_runtime_apis::apis::trellis_events_v1::Client::from_generated(
                    renewed.generated_client(),
                );
                let denied = scoped.dead_letters_inspect(&probe).await.unwrap_err();
                assert!(
                    denied.to_string().contains("not found")
                        || denied.to_string().contains("Not found"),
                    "renewed owned inspection must reach retained lookup: {denied}"
                );
                auth.deployments_apply(
                    &serde_json::from_value(serde_json::json!({
                        "deploymentId":deployment.deployment_id,
                        "packageEvidence":omitted_evidence,
                        "expectedRevision":added.revision,
                        "idempotencyKey":ulid::Ulid::new().to_string()
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
                let denied = scoped.dead_letters_inspect(&probe).await.unwrap_err();
                assert!(
                    !denied.to_string().contains("not found"),
                    "omitted Consumer must deny old binding: {denied}"
                );
                let (omitted_seed, omitted_key) = trellis_rs::auth::generate_session_keypair();
                auth.service_instances_provision(
                    &serde_json::from_value(serde_json::json!({
                        "deploymentId":deployment.deployment_id,
                        "title":"current implementation without Consumer",
                        "identityPublicKey":omitted_key,
                        "idempotencyKey":ulid::Ulid::new().to_string()
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
                let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
                    .await
                    .unwrap();
                let omitted = pinned(omitted_seed.clone(), server).await;
                let denied = inspect(omitted.clone(), probe_json.clone()).await.unwrap_err();
                assert!(
                    !denied.to_string().contains("not found"),
                    "fresh omitted implementation must not regain old Consumer access: \
                {denied}"
                );
                eprintln!("Consumer add/current-instance/omit deployment lifecycle passed");
            })
            .await;
        }
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        drop(events);
        drop(publisher);
        runtime.shutdown().await.unwrap();
        assert!(result.is_ok(), "{sdk}: timed out during {phase}");
    }
}
