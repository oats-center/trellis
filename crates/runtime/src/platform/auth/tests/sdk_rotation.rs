//! Persisted public Node and Chromium custody over the production OAuth HTTP adapter.

use super::super::oauth::{OAuthEngine, OAuthListener, OAuthStorage, OAuthTransport};
use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use trellis_runtime_apis::participants::{trellis_health_runtime, trellis_platform};

async fn sdk_rotation(mode: &str) {
    use trellis_runtime_apis::participants::{
        trellis_events_runtime, trellis_health_runtime, trellis_jobs_runtime, trellis_platform,
    };
    let certificate = super::oauth_transport::certificate();
    let mut f = fixture();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    f.store
        .initialize_builtin_provider::<trellis_platform::Participant>(
            &URL_SAFE_NO_PAD.encode([61; 32]),
            now,
        )
        .unwrap();
    f.store
        .initialize_builtin_provider::<trellis_health_runtime::Participant>(
            &URL_SAFE_NO_PAD.encode([62; 32]),
            now,
        )
        .unwrap();
    f.store
        .initialize_builtin_provider::<trellis_jobs_runtime::Participant>(
            &URL_SAFE_NO_PAD.encode([63; 32]),
            now,
        )
        .unwrap();
    f.store
        .initialize_builtin_provider::<trellis_events_runtime::Participant>(
            &URL_SAFE_NO_PAD.encode([64; 32]),
            now,
        )
        .unwrap();
    // The Node driver binds the public and app ports, and the backend is rebound
    // after a restart. Ports read from `:0` and released come from the ephemeral
    // range, where any outbound connection can take them first, so every port
    // is reserved outside it for the whole test.
    let public = trellis_local_nats::ReservedPort::reserve_on([127, 0, 0, 2].into()).unwrap();
    let app = trellis_local_nats::ReservedPort::reserve().unwrap();
    let backend_port = trellis_local_nats::ReservedPort::reserve().unwrap();
    let origin = format!("https://127.0.0.2:{}", public.port());
    let app_origin = format!("https://127.0.0.1:{}", app.port());
    let backend_address = std::net::SocketAddr::from(([127, 0, 0, 1], backend_port.port()));
    let backend = tokio::net::TcpListener::bind(backend_address)
        .await
        .unwrap();
    let backend_origin = format!("http://{backend_address}");
    Arc::get_mut(&mut f.store).unwrap().settings.oauth_issuer = origin.clone();
    for (client_id, kind, redirect_uris) in [
        ("trellis.cli", "native", Vec::new()),
        (
            "issuer-rotation-browser",
            "browser",
            vec![format!("{app_origin}/callback")],
        ),
    ] {
        let registration = ClientRegistration {
            client_id: client_id.into(),
            kind: kind.into(),
            display_name: "Issuer rotation consumer".into(),
            redirect_uris,
            development: true,
            implied_capabilities: Vec::new(),
            metadata: if kind == "native" {
                serde_json::json!({
                    "grant_types": [
                        "urn:ietf:params:oauth:grant-type:device_code",
                        "refresh_token"
                            ]
                })
            } else {
                serde_json::json!({})
            },
            expected_revision: 0,
        };
        f.store
            .transaction(|connection| {
                f.store.register_client_in_transaction(
                    connection,
                    &mutation(&f.admin, &registration),
                    &registration,
                    now,
                )
            })
            .unwrap();
    }
    if mode == "native" {
        let input = trellis_runtime_apis::types::AuthUserCreateRequest {
            given_name: None,
            family_name: None,
            email: None,
            username: "other-user".to_string().into(),
            display_name: "Other user".to_string().into(),
            password: Some("another strong local password".to_string().into()),
            idempotency_key: ulid::Ulid::new().to_string().into(),
            extra: Default::default(),
        };
        let other = super::super::rpc_users::create_user(
            &f.store,
            &mutation(&f.admin, &input),
            &input,
            now,
        )
        .unwrap();
        policy(
            &f.store,
            &f.admin,
            PolicyMutation::CapabilityGrant {
                principal_id: other.user.principal_id.to_string(),
                capability_id: "trellis.auth.v1:identities.read".into(),
                expected_revision: 0,
                expires_at: None,
            },
            now,
        );
    }
    let mut authority = f.store;
    let engine = Arc::new(
        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap()).unwrap(),
    );
    let listener = OAuthListener::load(
        backend,
        &backend_origin,
        &OAuthTransport::LoopbackDevelopment,
    )
    .await
    .unwrap();
    let mut shutdown = tokio_util::sync::CancellationToken::new();
    let mut server = tokio::spawn(listener.run(
        super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]),
        shutdown.clone(),
    ));
    let mut child = tokio::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "ts/browser/oauth_issuer_rotation_driver.ts",
            mode,
        ])
        .env("TRELLIS_ROTATION_ISSUER", &origin)
        .env("TRELLIS_ROTATION_BACKEND", &backend_origin)
        .env("TRELLIS_ROTATION_APP", &app_origin)
        .env("TRELLIS_ROTATION_CERT", &certificate.certificate)
        .env("TRELLIS_ROTATION_KEY", &certificate.private_key)
        .env(
            "NODE_EXTRA_CA_CERTS",
            certificate.directory.path().join("root.pem"),
        )
        .env("TRELLIS_ROTATION_DIRECTORY", f.directory.path().join(mode))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let outcome = tokio::time::timeout(Duration::from_secs(150), async {
        while let Some(line) = output.next_line().await.unwrap() {
            let event: serde_json::Value =
                serde_json::from_str(&line).expect("rotation runner emits only JSON stages");
            match event["stage"].as_str().unwrap() {
                "rotate" => {
                    shutdown.cancel();
                    (&mut server).await.unwrap().unwrap();
                    for _ in 0..event["steps"].as_u64().unwrap() {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64;
                        crate::platform::auth::tests::rotate_issuer(&authority, &f.admin, now)
                            .unwrap();
                        authority = Arc::new(
                            SqliteAuthorizationStore::from_connection(
                                rusqlite::Connection::open(f.directory.path().join("platform.db"))
                                    .unwrap(),
                                authority.signer.clone(),
                                authority.settings.clone(),
                                authority.custody_directory.clone(),
                            )
                            .unwrap(),
                        );
                    }
                    let socket = tokio::net::TcpListener::bind(backend_address)
                        .await
                        .unwrap();
                    let engine = Arc::new(
                        OAuthEngine::new(OAuthStorage::new(authority.clone(), &[17; 32]).unwrap())
                            .unwrap(),
                    );
                    let listener = OAuthListener::load(
                        socket,
                        &backend_origin,
                        &OAuthTransport::LoopbackDevelopment,
                    )
                    .await
                    .unwrap();
                    shutdown = tokio_util::sync::CancellationToken::new();
                    server = tokio::spawn(listener.run(
                        super::super::oauth::router(engine, vec!["nats://127.0.0.1:4222".into()]),
                        shutdown.clone(),
                    ));
                    let reply = serde_json::json!({"keyId":authority.issuer().unwrap().key_id});
                    input
                        .write_all(format!("{reply}\n").as_bytes())
                        .await
                        .unwrap();
                }
                "done" => {
                    assert_eq!(event["mode"], mode);
                }
                stage => panic!("unexpected rotation stage {stage}"),
            }
        }
        child.wait().await.unwrap()
    })
    .await
    .expect("real SDK issuer rotation deadline");
    shutdown.cancel();
    server.await.unwrap().unwrap();
    assert!(
        outcome.success(),
        "{mode} issuer rotation consumer failed: {outcome}"
    );
}

#[tokio::test]
async fn node_reopen_authenticates_issuer_chain_and_retains_inflight_successor() {
    sdk_rotation("native").await;
}

#[tokio::test]
async fn chromium_reload_authenticates_issuer_chain_before_spending_and_retains_inflight_successor()
{
    sdk_rotation("browser").await;
}

/// Reproduce cold current-pin custody against ordinary, immutable builtin material.
/// No OAuth grant, verified context, catalog, or Auth responder is manufactured.
#[tokio::test]
async fn cold_rotated_owner_resolves_builtin_catalog_signer_over_real_nats() {
    use futures_util::StreamExt as _;
    use trellis_rs::client::{AuthorizationOwner, RuntimeAuthorizationTrust};

    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory, SigningKey::from_bytes(&[42; 32]));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    store.initialize_issuer(now).unwrap();
    let admin = store
        .initialize_administrator(
            "admin",
            Some("a strong bootstrap password"),
            None,
            None,
            now,
        )
        .map(|initialized| initialized.principal_id)
        .unwrap();
    let provisioned = store
        .initialize_builtin_provider::<trellis_platform::Participant>(
            &URL_SAFE_NO_PAD.encode([61; 32]),
            now,
        )
        .unwrap();
    store
        .initialize_builtin_provider::<trellis_health_runtime::Participant>(
            &URL_SAFE_NO_PAD.encode([62; 32]),
            now,
        )
        .unwrap();
    let original = store.issuer().unwrap();
    let catalog = store.current_catalog("trellis.auth@v1").unwrap();
    let health_catalog = store.current_catalog("trellis.health@v1").unwrap();
    catalog
        .verify(
            &original,
            &store.settings.instance_id,
            &store.settings.account,
        )
        .unwrap();
    let rotation = crate::platform::auth::tests::rotate_issuer(&store, &admin, now).unwrap();
    // This is the real forward-verified successor that cold custody retains,
    // not a discovery-selected key or injected authorization context.
    let current = rotation
        .verify_successor(
            &original,
            &store.settings.instance_id,
            &store.settings.account,
            0,
            now,
        )
        .unwrap();
    let store = Arc::new(
        SqliteAuthorizationStore::from_connection(
            rusqlite::Connection::open(directory.path().join("platform.db")).unwrap(),
            store.signer.clone(),
            store.settings.clone(),
            store.custody_directory.clone(),
        )
        .unwrap(),
    );
    assert_eq!(
        store
            .current_catalog("trellis.auth@v1")
            .unwrap()
            .digest()
            .unwrap(),
        catalog.digest().unwrap()
    );

    // Held until the broker's listener is up; see ReservedPort.
    let reservation = trellis_local_nats::ReservedPort::reserve().unwrap();
    let port = reservation.port();
    let binary =
        std::env::var_os("TRELLIS_NATS_SERVER_BIN").unwrap_or_else(|| "nats-server".into());
    let mut broker = tokio::process::Command::new(binary)
        .args(["-js", "-p", &port.to_string(), "-sd"])
        .arg(directory.path().join("nats"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("provide the ordinary NATS executable through TRELLIS_NATS_SERVER_BIN");
    let mut output = BufReader::new(broker.stderr.take().unwrap()).lines();
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
    let transport = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    let jetstream = async_nats::jetstream::new(transport.clone());
    let material = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "TRELLIS_AUTH_MATERIAL".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let revocations = jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "TRELLIS_AUTH_REVOCATIONS".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    // Ordinary startup materialization and a real provisioned-key admission
    // supply the current live caller for authorize_action, the public path into
    // the owner's crate-private immutable material cache.
    super::super::resources::materialize_native_kv(
        &store,
        &transport,
        &provisioned.deployment_id,
        now,
    )
    .await
    .unwrap();
    let ephemeral = nkeys::KeyPair::new_user();
    let runtime_id = URL_SAFE_NO_PAD.encode([7; 16]);
    let (_, ephemeral_public) = nkeys::from_public_key(&ephemeral.public_key()).unwrap();
    let session_public_key = URL_SAFE_NO_PAD.encode(ephemeral_public);
    let credential = trellis_protocol::sign_provisioned_admission(
        trellis_protocol::ProvisionedAdmission {
            format: trellis_protocol::PROVISIONED_ADMISSION_FORMAT_V1.into(),
            trellis_instance_id: store.settings.instance_id.clone(),
            audience_nats_account: store.settings.account.clone(),
            credential_id: provisioned.identity_key_id,
            runtime_id: runtime_id.clone(),
            ephemeral_nkey: ephemeral.public_key(),
            issued_at: now,
            expires_at: now + 60,
        },
        &SigningKey::from_bytes(&[61; 32]),
    )
    .unwrap();
    let admitted = crate::platform::auth::tests::admit_credential(
        &store,
        &serde_json::to_vec(&credential).unwrap(),
        &ephemeral.public_key(),
        now,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while store
            .process_effects(&material, &revocations, &transport, now)
            .await
            .unwrap()
            != 0
        {}
    })
    .await
    .expect("ordinary material publication deadline");
    let bytes = material
        .get(format!("api.{}", catalog.digest().unwrap()))
        .await
        .unwrap()
        .expect("ordinary outbox publishes builtin catalog");
    let published: trellis_protocol::SignedCatalogSnapshot =
        serde_json::from_slice(&bytes).unwrap();
    published
        .verify(
            &original,
            &store.settings.instance_id,
            &store.settings.account,
        )
        .unwrap();
    let owner = AuthorizationOwner::new(
        RuntimeAuthorizationTrust {
            instance_id: store.settings.instance_id.clone(),
            account: store.settings.account.clone(),
            issuer: current.clone(),
            rotation_sequence: rotation.sequence.get(),
            root_certificates: Vec::new(),
        },
        runtime_id,
        session_public_key,
    )
    .unwrap();
    assert_eq!(
        owner.issuer(&transport, &current.key_id).await.unwrap(),
        current
    );
    // Capture a genuine current-key attestation before compromise, to exercise
    // replay against a later owner that already knows this key is revoked.
    let shutdown = tokio_util::sync::CancellationToken::new();
    let service = super::super::own_context::OwnContextService::start(
        transport.clone(),
        material,
        store.clone(),
    )
    .await
    .unwrap();
    let responder = tokio::spawn(service.run(shutdown.clone().cancelled_owned()));
    let request = trellis_protocol::IssuerMetadataRequest {
        trellis_instance_id: store.settings.instance_id.clone(),
        audience_nats_account: store.settings.account.clone(),
        request_nonce: URL_SAFE_NO_PAD.encode([27; 16]),
        query: trellis_protocol::IssuerMetadataQuery::IssuerKey {
            key_id: original.key_id.clone(),
        },
    };
    let reply = format!("{}.request", transport.new_inbox());
    let mut response = transport.subscribe(reply.clone()).await.unwrap();
    transport.flush().await.unwrap();
    transport
        .publish_with_reply(
            trellis_protocol::ISSUER_METADATA_SUBJECT,
            reply.clone(),
            serde_json::to_vec(&request).unwrap().into(),
        )
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), response.next())
        .await
        .unwrap()
        .unwrap();
    let verified = owner
        .verify_metadata(
            &transport,
            &response.payload,
            &request,
            trellis_protocol::ISSUER_METADATA_SUBJECT,
            &reply,
        )
        .await
        .unwrap();
    assert!(matches!(
        verified.result(),
        trellis_protocol::IssuerMetadataResult::Value { .. }
    ));
    let resolved = owner
        .issuer(&transport, &published.issuer_key_id)
        .await
        .unwrap();
    assert_eq!(
        resolved.state,
        trellis_protocol::AuthorityIssuerState::Retired
    );
    published
        .verify(
            &resolved,
            &store.settings.instance_id,
            &store.settings.account,
        )
        .unwrap();
    assert!(owner.assert_issuer_current(&original.key_id).is_err());
    owner.assert_issuer_current(&current.key_id).unwrap();

    let (stop_mirror, mirror_stop) = tokio::sync::watch::channel(false);
    let (ready, coverage) = tokio::sync::oneshot::channel();
    let mirror_owner = owner.clone();
    let mirror_transport = transport.clone();
    let mirror = tokio::spawn(async move {
        mirror_owner
            .run_revocations(&mirror_transport, mirror_stop, ready)
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), coverage)
        .await
        .unwrap()
        .unwrap();
    let authority = owner
        .prepare_own(&transport, &admitted.authority)
        .await
        .unwrap();
    owner.promote_own(authority.clone(), None).unwrap();
    let action = health_catalog
        .capabilities
        .iter()
        .find(|capability| capability.capability_id == "trellis.health.v1:health.report")
        .unwrap()
        .memberships[0]
        .action
        .clone();
    let materialized = owner
        .authorize_action(&transport, &authority, &health_catalog.api_id, &action)
        .await
        .unwrap();
    assert_eq!(materialized.digest(), health_catalog.digest().unwrap());
    assert!(materialized.authorizes(&admitted.authority, &action));

    // The public Node owner consumes the same production-issued root and
    // registry over the real broker, without an injected verified context.
    let registry = store
        .transaction(|connection| {
            super::super::own_context::admission_registry(
                connection,
                &store.current_issuer(connection)?,
            )
        })
        .unwrap();
    let input_path = directory.path().join("cold-catalog.json");
    // Real protocol signatures isolate the escalation: the retired K1 key can
    // sign a structurally valid next link but cannot extend the current K2 pin.
    // Neither link fabricates the catalog, live root, or metadata responder.
    let successor = SigningKey::from_bytes(&[81; 32]);
    let attacker = SigningKey::from_bytes(&[82; 32]);
    let mut historical_rotation = rotation.clone();
    historical_rotation.sequence = trellis_protocol::U64s::new(rotation.sequence.get() + 1);
    historical_rotation.next_public_key =
        URL_SAFE_NO_PAD.encode(attacker.verifying_key().as_bytes());
    historical_rotation.next_key_id =
        URL_SAFE_NO_PAD.encode(Sha256::digest(attacker.verifying_key().as_bytes()));
    let historical_rotation = historical_rotation
        .sign(&SigningKey::from_bytes(&[42; 32]))
        .unwrap();
    // Establish that the attack has a genuine K1 signature and exact next slot;
    // rejection must come from live-pin ownership rather than malformed bytes.
    historical_rotation
        .verify_successor(
            &original,
            &store.settings.instance_id,
            &store.settings.account,
            rotation.sequence.get(),
            now,
        )
        .unwrap();
    assert!(owner
        .accept_issuer_rotation(historical_rotation.clone())
        .is_err());
    owner.assert_issuer_current(&current.key_id).unwrap();
    let mut forward_rotation = historical_rotation.clone();
    forward_rotation.previous_key_id = current.key_id.clone();
    forward_rotation.next_public_key = URL_SAFE_NO_PAD.encode(successor.verifying_key().as_bytes());
    forward_rotation.next_key_id =
        URL_SAFE_NO_PAD.encode(Sha256::digest(successor.verifying_key().as_bytes()));
    let forward_rotation = forward_rotation
        .sign(
            &store
                .transaction(|connection| store.current_signer(connection))
                .unwrap(),
        )
        .unwrap();
    let successor_issuer = forward_rotation
        .verify_successor(
            &current,
            &store.settings.instance_id,
            &store.settings.account,
            rotation.sequence.get(),
            now,
        )
        .unwrap();
    std::fs::write(
        &input_path,
        serde_json::to_vec(&serde_json::json!({
            "servers": [format!("nats://127.0.0.1:{port}")],
            "trust": {
                "instanceId": store.settings.instance_id,
                "account": store.settings.account,
                "issuer": current,
                "rotationSequence": rotation.sequence.get().to_string()
            },
            "registry": registry,
            "authority": serde_json::to_string(&admitted.authority).unwrap(),
            "runtimeId": admitted.authority.unsigned.runtime_id,
            "sessionPublicKey": admitted.authority.unsigned.session_public_key,
            "apiId": health_catalog.api_id,
            "action": action,
            "historicalKeyId": original.key_id,
            "historicalRotation": serde_json::to_string(&historical_rotation).unwrap(),
            "forwardRotation": serde_json::to_string(&forward_rotation).unwrap(),
            "successorTrust": {
                "instanceId": store.settings.instance_id,
                "account": store.settings.account,
                "issuer": successor_issuer,
                "rotationSequence": forward_rotation.sequence.get().to_string()
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let node = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "ts/packages/trellis/native/cold_catalog_worker.ts",
            ])
            .arg(&input_path)
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("Node historical catalog verification deadline")
    .unwrap();
    assert!(
        node.status.success(),
        "Node historical catalog: {}",
        String::from_utf8_lossy(&node.stderr)
    );

    let compromised = EnforcementScope::Issuer(original.key_id.clone());
    crate::platform::auth::tests::revoke_root(
        &store,
        &mutation(&admin, &compromised),
        &compromised,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    )
    .unwrap();
    let revoked_owner = AuthorizationOwner::new(
        RuntimeAuthorizationTrust {
            instance_id: store.settings.instance_id.clone(),
            account: store.settings.account.clone(),
            issuer: current.clone(),
            rotation_sequence: rotation.sequence.get(),
            root_certificates: Vec::new(),
        },
        admitted.authority.unsigned.runtime_id.clone(),
        admitted.authority.unsigned.session_public_key.clone(),
    )
    .unwrap();
    let revoked = revoked_owner
        .issuer(&transport, &original.key_id)
        .await
        .unwrap();
    assert_eq!(
        revoked.state,
        trellis_protocol::AuthorityIssuerState::Revoked
    );
    assert!(published
        .verify(
            &revoked,
            &store.settings.instance_id,
            &store.settings.account
        )
        .is_err());
    assert!(revoked_owner
        .assert_issuer_current(&original.key_id)
        .is_err());
    let (stop_revoked_mirror, revoked_mirror_stop) = tokio::sync::watch::channel(false);
    let (ready, coverage) = tokio::sync::oneshot::channel();
    let mirror_owner = revoked_owner.clone();
    let mirror_transport = transport.clone();
    let revoked_mirror = tokio::spawn(async move {
        mirror_owner
            .run_revocations(&mirror_transport, revoked_mirror_stop, ready)
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), coverage)
        .await
        .unwrap()
        .unwrap();
    let authority = revoked_owner
        .prepare_own(&transport, &admitted.authority)
        .await
        .unwrap();
    revoked_owner.promote_own(authority.clone(), None).unwrap();
    assert!(revoked_owner
        .authorize_action(&transport, &authority, &health_catalog.api_id, &action)
        .await
        .is_err());
    revoked_owner
        .verify_metadata(
            &transport,
            &response.payload,
            &request,
            trellis_protocol::ISSUER_METADATA_SUBJECT,
            &reply,
        )
        .await
        .unwrap();
    assert_eq!(
        revoked_owner
            .issuer(&transport, &original.key_id)
            .await
            .unwrap()
            .state,
        trellis_protocol::AuthorityIssuerState::Revoked
    );
    assert!(revoked_owner
        .authorize_action(&transport, &authority, &health_catalog.api_id, &action)
        .await
        .is_err());

    // Loading a historical key cannot advance the pin or its rotation sequence.
    // A genuine next rotation must still be accepted at the original next slot.
    let next = crate::platform::auth::tests::rotate_issuer(
        &store,
        &admin,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    )
    .unwrap();
    owner.accept_issuer_rotation(next.clone()).unwrap();
    owner.assert_issuer_current(&next.next_key_id).unwrap();
    assert!(owner.assert_issuer_current(&current.key_id).is_err());
    stop_mirror.send(true).unwrap();
    stop_revoked_mirror.send(true).unwrap();
    mirror.await.unwrap().unwrap();
    revoked_mirror.await.unwrap().unwrap();
    shutdown.cancel();
    responder.await.unwrap().unwrap();
    broker.kill().await.unwrap();
    broker.wait().await.unwrap();
}
