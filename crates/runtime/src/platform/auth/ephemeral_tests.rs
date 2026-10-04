use std::process::{Child, Command, Stdio};

use super::*;

const DIGEST: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn connection_kick_response_rejects_system_errors() {
    validate_connection_kick_response(br#"{"server":{"id":"N1"}}"#, "N1")
        .expect("successful response");
    validate_connection_kick_response(
        br#"{"server":{"id":"N1"},"error":{"code":500,"description":"no such client or leafnode id"}}"#,
        "N1",
    )
    .expect("already disconnected response");
    assert!(validate_connection_kick_response(
        br#"{"server":{"id":"N1"},"error":{"code":403,"description":"permission denied"}}"#,
        "N1",
    )
    .is_err());
    assert!(validate_connection_kick_response(br#"{"server":{"id":"N2"}}"#, "N1").is_err());
}

pub(crate) fn browser_flow() -> AuthBrowserTransaction {
    let mut flow = AuthBrowserTransaction {
        format: BROWSER_TRANSACTION_FORMAT.to_owned(),
        transaction_id: "flow-1".to_owned(),
        kind: AuthBrowserTransactionKind::UserAuth,
        state: AuthBrowserTransactionState::ChooseProvider,
        intent_id: "request-1".to_owned(),
        intent_digest: DIGEST.to_owned(),
        participant_id: "app-1".to_owned(),
        installed_revision: 1,
        target_grant_revision: 0,
        consent: ConsentRequest {
            participant_id: "app-1".to_owned(),
            package_digest: DIGEST.to_owned(),
            installed_revision: 1,
            expected_grant_revision: 0,
            capabilities: Vec::new(),
            resources: Vec::new(),
            companion: None,
            decision_digest: DIGEST.to_owned(),
        },
        session_public_key: "session-key".to_owned(),
        portal_id: "builtin".to_owned(),
        redirect_target: Some("https://app.example/callback".to_owned()),
        principal_id: None,
        authenticated_provider_id: None,
        authenticated_roles: Vec::new(),
        portal_binding_digest: None,
        claim_owner: None,
        claimed_at: None,
        durable_result_digest: None,
        completed_at: None,
        created_at: 100,
        expires_at: 1_000,
        version: 1,
    };
    flow.consent.decision_digest = flow.consent.computed_decision_digest().unwrap();
    flow
}

fn oauth_state() -> AuthOAuthState {
    AuthOAuthState {
        format: OAUTH_STATE_FORMAT.to_owned(),
        state_id: "state-1".to_owned(),
        provider_id: "provider-1".to_owned(),
        kind: AuthOAuthKind::Browser,
        flow_id: "flow-1".to_owned(),
        status: AuthOAuthStatus::Pending,
        pkce_verifier: "pkce-verifier".to_owned(),
        nonce: "nonce".to_owned(),
        redirect_uri: "https://auth.example/callback".to_owned(),
        browser_binding_digest: DIGEST.to_owned(),
        portal_binding_digest: Some(DIGEST.to_owned()),
        browser_transaction_id: None,
        portal_id: Some("builtin".to_owned()),
        portal_policy_digest: Some(DIGEST.to_owned()),
        claim_owner: None,
        result_digest: None,
        authenticated_principal_id: None,
        authenticated_provider_subject: None,
        authenticated_email: None,
        authenticated_roles: Vec::new(),
        created_at: 100,
        expires_at: 1_000,
        version: 1,
    }
}

async fn repository_conformance(repository: impl AuthEphemeralRepository + Clone) {
    let flow = browser_flow();
    repository
        .create_browser_transaction(flow.clone())
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_browser_transaction(&flow.transaction_id)
            .await
            .unwrap(),
        Some(flow.clone())
    );
    assert_eq!(
        repository.create_browser_transaction(flow.clone()).await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let mut directly_approved = flow.clone();
    directly_approved.transaction_id = "flow-approved".to_owned();
    directly_approved.intent_id = "request-approved".to_owned();
    directly_approved.intent_digest =
        trellis_protocol::digest_json(&serde_json::json!("request-approved")).unwrap();
    repository
        .create_browser_transaction(directly_approved.clone())
        .await
        .unwrap();
    directly_approved.state = AuthBrowserTransactionState::Authenticated;
    directly_approved.principal_id = Some("user-1".to_owned());
    directly_approved.authenticated_provider_id = Some("local".to_owned());
    directly_approved.portal_binding_digest = Some(DIGEST.to_owned());
    directly_approved.target_grant_revision = 7;
    directly_approved.consent.expected_grant_revision = 7;
    directly_approved.consent.decision_digest = directly_approved
        .consent
        .computed_decision_digest()
        .unwrap();
    directly_approved.version = 2;
    repository
        .replace_browser_transaction(1, directly_approved.clone())
        .await
        .unwrap();
    directly_approved.state = AuthBrowserTransactionState::Approved;
    directly_approved.durable_result_digest = Some(DIGEST.to_owned());
    directly_approved.completed_at = Some(200);
    directly_approved.version = 3;
    repository
        .replace_browser_transaction(2, directly_approved.clone())
        .await
        .unwrap();
    let mut changed_grant_revision = directly_approved;
    changed_grant_revision.target_grant_revision = 8;
    changed_grant_revision.version = 4;
    assert_eq!(
        repository
            .replace_browser_transaction(3, changed_grant_revision)
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let mut approval_required = flow.clone();
    approval_required.state = AuthBrowserTransactionState::Authenticated;
    approval_required.principal_id = Some("user-1".to_owned());
    approval_required.authenticated_provider_id = Some("local".to_owned());
    approval_required.portal_binding_digest = Some(DIGEST.to_owned());
    approval_required.version = 2;
    repository
        .replace_browser_transaction(1, approval_required.clone())
        .await
        .unwrap();
    approval_required.state = AuthBrowserTransactionState::ApprovalRequired;
    approval_required.version = 3;
    repository
        .replace_browser_transaction(2, approval_required.clone())
        .await
        .unwrap();
    assert_eq!(
        repository
            .replace_browser_transaction(1, approval_required.clone())
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    let mut refreshed = approval_required.clone();
    refreshed.installed_revision = 2;
    refreshed.consent.installed_revision = 2;
    refreshed.target_grant_revision = 8;
    refreshed.consent.expected_grant_revision = 8;
    refreshed.consent.decision_digest = refreshed.consent.computed_decision_digest().unwrap();
    refreshed.version = 4;
    repository
        .replace_browser_transaction(3, refreshed)
        .await
        .unwrap();
    let refreshed = repository
        .get_browser_transaction(&flow.transaction_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.consent.installed_revision, 2);
    assert_eq!(refreshed.principal_id.as_deref(), Some("user-1"));
    let mut changed_transcript = refreshed;
    changed_transcript.intent_id = "changed".to_owned();
    changed_transcript.version = 5;
    assert_eq!(
        repository
            .replace_browser_transaction(4, changed_transcript)
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    let mut skipped_state = flow.clone();
    skipped_state.transaction_id = "flow-skipped".to_owned();
    skipped_state.intent_id = "request-skipped".to_owned();
    skipped_state.intent_digest =
        trellis_protocol::digest_json(&serde_json::json!("request-skipped")).unwrap();
    repository
        .create_browser_transaction(skipped_state.clone())
        .await
        .unwrap();
    skipped_state.state = AuthBrowserTransactionState::ApprovalDenied;
    skipped_state.principal_id = Some("user-1".to_owned());
    skipped_state.authenticated_provider_id = Some("local".to_owned());
    skipped_state.portal_binding_digest = Some(DIGEST.to_owned());
    skipped_state.completed_at = Some(200);
    skipped_state.version = 2;
    assert_eq!(
        repository
            .replace_browser_transaction(1, skipped_state)
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let oauth = oauth_state();
    repository.create_oauth_state(oauth.clone()).await.unwrap();
    assert_eq!(
        repository.get_oauth_state(&oauth.state_id).await.unwrap(),
        Some(oauth)
    );
    let mut skipped_exchange = oauth_state();
    skipped_exchange.status = AuthOAuthStatus::ExchangeStarted;
    skipped_exchange.claim_owner = Some("owner-1".to_owned());
    skipped_exchange.version = 2;
    assert_eq!(
        repository.replace_oauth_state(1, skipped_exchange).await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let first = repository.clone();
    let second = repository.clone();
    let (left, right) = tokio::join!(
        claim_oauth_state(&first, "state-1", "owner-1"),
        claim_oauth_state(&second, "state-1", "owner-2")
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert!(matches!(
        left.as_ref().err().or(right.as_ref().err()),
        Some(AuthorizationStateError::StorageConflict)
    ));

    let mut current = repository
        .get_oauth_state("state-1")
        .await
        .unwrap()
        .unwrap();
    current.status = AuthOAuthStatus::ExchangeStarted;
    current.version += 1;
    repository
        .replace_oauth_state(current.version - 1, current.clone())
        .await
        .unwrap();

    let stale = current.clone();
    current.status = AuthOAuthStatus::RestartRequired;
    current.version += 1;
    repository
        .replace_oauth_state(current.version - 1, current.clone())
        .await
        .unwrap();
    assert_eq!(
        repository.replace_oauth_state(stale.version, stale).await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let mut completed = oauth_state();
    completed.state_id = "state-2".to_owned();
    repository
        .create_oauth_state(completed.clone())
        .await
        .unwrap();
    completed = claim_oauth_state(&repository, "state-2", "owner-1")
        .await
        .unwrap();
    completed.status = AuthOAuthStatus::ExchangeStarted;
    completed.authenticated_principal_id = Some("user-1".to_owned());
    completed.version += 1;
    repository
        .replace_oauth_state(completed.version - 1, completed.clone())
        .await
        .unwrap();
    completed.status = AuthOAuthStatus::Completed;
    completed.result_digest = Some(DIGEST.to_owned());
    completed.version += 1;
    repository
        .replace_oauth_state(completed.version - 1, completed.clone())
        .await
        .unwrap();
    assert_eq!(
        repository.get_oauth_state("state-2").await.unwrap(),
        Some(completed)
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let transport_authorization = trellis_protocol::TransportAuthorizationV1 {
        format: trellis_protocol::TRANSPORT_AUTHORIZATION_FORMAT_V1.to_owned(),
        account: nkeys::KeyPair::new_account().public_key(),
        publish_allow: vec!["rpc.v1.Example".to_owned()],
        subscribe_allow: vec!["_INBOX.>".to_owned()],
        response: None,
        hard_expires_at: None,
    };
    let transport_authorization_digest = transport_authorization.digest().unwrap();
    let session_key = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32])
            .verifying_key()
            .to_bytes(),
    );
    let connection = AuthConnectionPresence {
        storage_revision: 0,
        format: "trellis.auth-connection-presence.v2".to_owned(),
        connection_id: DIGEST.to_owned(),
        runtime_connection_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        session_key,
        login_session_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned()),
        principal_id: "usr_01".to_owned(),
        principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
        participant_id: "app-1".to_owned(),
        deployment_id: None,
        instance_id: None,
        context_digest: DIGEST.to_owned(),
        transport_authorization_digest,
        attachment_state: AuthAttachmentState::Pending,
        pending_deadline: Some(now + 60_000),
        server_id: "server-1".to_owned(),
        client_id: "42".to_owned(),
        user_nkey: "user-nkey".to_owned(),
        remote_address: Some("127.0.0.1".to_owned()),
        connected_at: now,
        last_seen_at: now,
        version: 2,
    };
    let revision = repository
        .put_connection_presence(connection.clone())
        .await
        .unwrap();
    // Confirmation is a compare-and-swap: a stale revision must not overwrite
    // the retained attachment.
    let mut confirmed = connection.clone();
    confirmed.attachment_state = AuthAttachmentState::Confirmed;
    confirmed.pending_deadline = None;
    assert!(repository
        .replace_connection_presence(revision + 1, confirmed.clone())
        .await
        .is_err());
    let confirmed_revision = repository
        .replace_connection_presence(revision, confirmed)
        .await
        .unwrap();
    let mut second_connection = connection;
    let second_connection_id =
        trellis_protocol::digest_json(&serde_json::json!("second connection")).unwrap();
    second_connection.connection_id = second_connection_id.clone();
    second_connection.client_id = "43".to_owned();
    repository
        .put_connection_presence(second_connection)
        .await
        .unwrap();
    assert_eq!(
        repository
            .list_connection_presence(Some("01ARZ3NDEKTSV4RRFFQ69G5FAW"))
            .await
            .unwrap()
            .len(),
        2
    );
    repository
        .delete_connection_presence(DIGEST, confirmed_revision)
        .await
        .unwrap();
    let remaining = repository
        .list_connection_presence(Some("01ARZ3NDEKTSV4RRFFQ69G5FAW"))
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].connection_id, second_connection_id);
}

#[tokio::test]
async fn in_memory_repository_conforms() {
    repository_conformance(InMemoryAuthEphemeralRepository::default()).await;
}

#[tokio::test]
async fn account_flow_oauth_accepts_browser_continuation_binding() {
    let repository = InMemoryAuthEphemeralRepository::default();
    let mut state = oauth_state();
    state.kind = AuthOAuthKind::AccountFlow;
    state.status = AuthOAuthStatus::ExchangeStarted;
    state.browser_transaction_id = Some("browser-transaction-1".to_owned());
    state.claim_owner = Some("claim-owner".to_owned());
    state.authenticated_provider_subject = Some("provider-subject".to_owned());
    state.authenticated_roles = vec!["administrator".to_owned()];
    repository.create_oauth_state(state.clone()).await.unwrap();
    state.authenticated_principal_id = Some("principal-1".to_owned());
    state.version += 1;
    repository
        .replace_oauth_state(1, state.clone())
        .await
        .unwrap();
    state.status = AuthOAuthStatus::Expired;
    state.version += 1;
    repository.replace_oauth_state(2, state).await.unwrap();
}

#[tokio::test]
async fn nats_kv_repository_conforms() {
    struct Server {
        child: Child,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    let listeners: Vec<_> = (0..9)
        .map(|_| std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap())
        .collect();
    let ports: Vec<_> = listeners
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let cache = std::env::var_os("TRELLIS_CACHE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| directory.path().join("cache"));
    let binary = trellis_local_nats::NatsServerBinary::resolve(
        &trellis_local_nats::NatsBinarySource::DownloadPinned,
        Some(&cache),
    )
    .unwrap();
    drop(listeners);
    let mut servers = Vec::new();
    for index in 0..3 {
        let name = format!("ephemeral-{index}");
        let routes = (0..3)
            .filter(|peer| *peer != index)
            .map(|peer| format!("\"nats://127.0.0.1:{}\"", ports[peer * 3 + 1]))
            .collect::<Vec<_>>()
            .join(",");
        let config = directory.path().join(format!("{name}.conf"));
        std::fs::write(&config, format!(
            "server_name: {name}\nlisten: 127.0.0.1:{}\nhttp: 127.0.0.1:{}\njetstream {{ store_dir: '{}' }}\ncluster {{ name: ephemeral, listen: 127.0.0.1:{}, routes: [{routes}] }}\n",
            ports[index * 3], ports[index * 3 + 2], directory.path().join(&name).display(), ports[index * 3 + 1],
        )).unwrap();
        let server = Server {
            child: Command::new(&binary)
                .arg("-c")
                .arg(&config)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        };
        servers.push((name, server));
    }
    let mut last_status = serde_json::Value::Null;
    let readiness = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let mut ready = true;
            let mut leader: Option<String> = None;
            let mut leader_current = false;
            for index in 0..3 {
                let status = async {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut connection =
                        tokio::net::TcpStream::connect(("127.0.0.1", ports[index * 3 + 2]))
                            .await
                            .ok()?;
                    connection
                        .write_all(
                            b"GET /jsz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .ok()?;
                    let mut response = Vec::new();
                    connection.read_to_end(&mut response).await.ok()?;
                    let start = response.windows(4).position(|bytes| bytes == b"\r\n\r\n")? + 4;
                    serde_json::from_slice::<serde_json::Value>(&response[start..]).ok()
                }
                .await;
                last_status = status.unwrap_or(serde_json::Value::Null);
                let cluster = &last_status["meta_cluster"];
                let current_leader = cluster["leader"].as_str().unwrap_or_default();
                ready &= !current_leader.is_empty()
                    && cluster["cluster_size"].as_u64() == Some(3)
                    && leader
                        .as_deref()
                        .is_none_or(|leader| leader == current_leader);
                leader = Some(current_leader.to_owned());
                if current_leader == servers[index].0 {
                    leader_current = cluster["replicas"].as_array().is_some_and(|replicas| {
                        replicas.len() == 2
                            && replicas.iter().all(|replica| {
                                replica["current"].as_bool() == Some(true)
                                    && replica["offline"].as_bool() != Some(true)
                            })
                    });
                }
            }
            if ready && leader_current {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        readiness.is_ok(),
        "NATS cluster did not become ready: {last_status}"
    );
    let url = format!("nats://127.0.0.1:{}", ports[0]);
    let mut client = None;
    for _ in 0..100 {
        match async_nats::connect(&url).await {
            Ok(connected) => {
                client = Some(connected);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    }
    let client = client.expect("NATS did not start");
    let repository = NatsAuthEphemeralRepository::ensure(client.clone())
        .await
        .unwrap();
    let jetstream = async_nats::jetstream::new(client.clone());
    let mut transactions = jetstream
        .get_key_value("trellis_auth_browser_transactions")
        .await
        .unwrap();
    let mut config = transactions.stream.info().await.unwrap().config.clone();
    config.num_replicas = 3;
    jetstream.update_stream(config).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if transactions
                .stream
                .info()
                .await
                .unwrap()
                .cluster
                .as_ref()
                .is_some_and(|cluster| {
                    cluster.replicas.len() == 2
                        && cluster.replicas.iter().all(|replica| replica.current)
                })
                // Stream metadata can become current before its KV read
                // subscription is available after the replication change.
                && transactions.entry("readiness").await.is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("transaction replicas did not become current");
    repository_conformance(repository.clone()).await;
    // A presence bucket created with a TTL would silently evict active
    // attachments, so a TTL-bearing bucket must be rejected rather than adopted.
    jetstream
        .delete_key_value("trellis_auth_connections")
        .await
        .unwrap();
    jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: "trellis_auth_connections".to_owned(),
            history: 1,
            max_age: std::time::Duration::from_millis(120_000),
            max_value_size: 16_384,
            ..Default::default()
        })
        .await
        .unwrap();
    let error = NatsAuthEphemeralRepository::check(client.clone())
        .await
        .expect_err("a TTL-bearing presence bucket must be rejected");
    let error = error.to_string();
    assert!(error.contains("max_age"), "{error}");
    assert!(error.contains("120000ms"), "{error}");
    let mut active = browser_flow();
    active.created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    active.expires_at = active.created_at + 60_000;
    active.transaction_id = "replicated-active".to_owned();
    active.intent_digest =
        trellis_protocol::digest_json(&serde_json::json!("replicated-intent")).unwrap();
    active.portal_binding_digest = Some(DIGEST.to_owned());
    repository
        .create_browser_transaction(active.clone())
        .await
        .unwrap();
    let leader = transactions
        .stream
        .info()
        .await
        .unwrap()
        .cluster
        .as_ref()
        .unwrap()
        .leader
        .clone()
        .expect("transaction stream has a leader");
    let leader_index = servers
        .iter()
        .position(|(name, _)| name == &leader)
        .unwrap();
    let (_, server) = servers.remove(leader_index);
    drop(server);
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if let Ok(info) = transactions.stream.info().await {
                if info.cluster.as_ref().is_some_and(|cluster| {
                    cluster
                        .leader
                        .as_ref()
                        .is_some_and(|current| !current.is_empty() && current != &leader)
                }) {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("transaction stream did not elect a surviving leader");
    let mut retry = active.clone();
    retry.transaction_id = "replicated-retry".to_owned();
    retry.created_at += 100;
    retry.expires_at += 100;
    assert_eq!(
        repository
            .create_browser_transaction(retry.clone())
            .await
            .unwrap(),
        active
    );
    retry.portal_binding_digest =
        Some(trellis_protocol::digest_json(&serde_json::json!("other-binding")).unwrap());
    assert_eq!(
        repository.create_browser_transaction(retry.clone()).await,
        Err(AuthorizationStateError::StorageConflict)
    );
    transactions.delete(&active.transaction_id).await.unwrap();
    assert!(repository
        .create_browser_transaction(retry.clone())
        .await
        .is_err());
    assert_eq!(
        repository
            .transaction_for_intent(&active.intent_digest)
            .await
            .unwrap(),
        Some(active.transaction_id.clone())
    );
    transactions
        .delete(format!("intent.{}", active.intent_digest))
        .await
        .unwrap();
    assert_eq!(
        repository
            .create_browser_transaction(retry.clone())
            .await
            .unwrap(),
        retry
    );
    drop(servers);
}

#[test]
fn oauth_state_rejects_unknown_fields() {
    let mut value = serde_json::to_value(oauth_state()).unwrap();
    value["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AuthOAuthState>(value).is_err());
}
