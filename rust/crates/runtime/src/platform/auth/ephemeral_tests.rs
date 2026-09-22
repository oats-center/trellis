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

pub(crate) fn browser_flow() -> AuthBrowserFlow {
    let mut flow = AuthBrowserFlow {
        format: BROWSER_FLOW_FORMAT.to_owned(),
        flow_id: "flow-1".to_owned(),
        kind: AuthBrowserFlowKind::UserAuth,
        state: AuthBrowserFlowState::ChooseProvider,
        request_id: "request-1".to_owned(),
        request_digest: DIGEST.to_owned(),
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
        browser_flow_id: None,
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
    repository.create_browser_flow(flow.clone()).await.unwrap();
    assert_eq!(
        repository.get_browser_flow(&flow.flow_id).await.unwrap(),
        Some(flow.clone())
    );
    assert_eq!(
        repository.create_browser_flow(flow.clone()).await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let mut directly_approved = flow.clone();
    directly_approved.flow_id = "flow-approved".to_owned();
    repository
        .create_browser_flow(directly_approved.clone())
        .await
        .unwrap();
    directly_approved.state = AuthBrowserFlowState::Authenticated;
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
        .replace_browser_flow(1, directly_approved.clone())
        .await
        .unwrap();
    directly_approved.state = AuthBrowserFlowState::Approved;
    directly_approved.durable_result_digest = Some(DIGEST.to_owned());
    directly_approved.completed_at = Some(200);
    directly_approved.version = 3;
    repository
        .replace_browser_flow(2, directly_approved.clone())
        .await
        .unwrap();
    let mut changed_grant_revision = directly_approved;
    changed_grant_revision.target_grant_revision = 8;
    changed_grant_revision.version = 4;
    assert_eq!(
        repository
            .replace_browser_flow(3, changed_grant_revision)
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );

    let mut approval_required = flow.clone();
    approval_required.state = AuthBrowserFlowState::Authenticated;
    approval_required.principal_id = Some("user-1".to_owned());
    approval_required.authenticated_provider_id = Some("local".to_owned());
    approval_required.portal_binding_digest = Some(DIGEST.to_owned());
    approval_required.version = 2;
    repository
        .replace_browser_flow(1, approval_required.clone())
        .await
        .unwrap();
    approval_required.state = AuthBrowserFlowState::ApprovalRequired;
    approval_required.version = 3;
    repository
        .replace_browser_flow(2, approval_required.clone())
        .await
        .unwrap();
    assert_eq!(
        repository
            .replace_browser_flow(1, approval_required.clone())
            .await,
        Err(AuthorizationStateError::StorageConflict)
    );
    for changed_consent in [{
        let mut consent = approval_required.consent.clone();
        consent.decision_digest = DIGEST.replace('A', "B");
        consent
    }] {
        let mut changed = approval_required.clone();
        changed.consent = changed_consent;
        changed.version = 4;
        assert_eq!(
            repository.replace_browser_flow(3, changed).await,
            Err(AuthorizationStateError::StorageConflict)
        );
    }
    let mut changed_transcript = approval_required;
    changed_transcript.request_id = "changed".to_owned();
    changed_transcript.version = 4;
    assert_eq!(
        repository.replace_browser_flow(3, changed_transcript).await,
        Err(AuthorizationStateError::StorageConflict)
    );
    let mut skipped_state = flow.clone();
    skipped_state.flow_id = "flow-skipped".to_owned();
    repository
        .create_browser_flow(skipped_state.clone())
        .await
        .unwrap();
    skipped_state.state = AuthBrowserFlowState::ApprovalDenied;
    skipped_state.principal_id = Some("user-1".to_owned());
    skipped_state.authenticated_provider_id = Some("local".to_owned());
    skipped_state.portal_binding_digest = Some(DIGEST.to_owned());
    skipped_state.completed_at = Some(200);
    skipped_state.version = 2;
    assert_eq!(
        repository.replace_browser_flow(1, skipped_state).await,
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
    let connection = AuthConnectionPresence {
        storage_revision: 0,
        format: "trellis.auth-connection-presence.v1".to_owned(),
        connection_id: DIGEST.to_owned(),
        runtime_connection_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        login_session_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned()),
        principal_id: "usr_01".to_owned(),
        principal_kind: trellis_protocol::AuthorizationPrincipalKind::User,
        participant_id: "app-1".to_owned(),
        deployment_id: None,
        instance_id: None,
        context_digest: DIGEST.to_owned(),
        server_id: "server-1".to_owned(),
        client_id: "42".to_owned(),
        user_nkey: "user-nkey".to_owned(),
        remote_address: Some("127.0.0.1".to_owned()),
        connected_at: now,
        last_seen_at: now,
        version: 1,
    };
    repository
        .put_connection_presence(connection.clone())
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
        .delete_connection_presence(DIGEST, 1)
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
    state.browser_flow_id = Some("browser-flow-1".to_owned());
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

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let directory = tempfile::tempdir().unwrap();
    let binary = trellis_local_nats::NatsServerBinary::resolve(
        &trellis_local_nats::NatsBinarySource::DownloadPinned,
        Some(&directory.path().join("cache")),
    )
    .unwrap();
    let server = Server {
        child: Command::new(binary)
            .args(["-a", "127.0.0.1", "-p", &port.to_string(), "-js"])
            .arg("-sd")
            .arg(directory.path().join("data"))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    };
    let url = format!("nats://127.0.0.1:{port}");
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
    let repository = NatsAuthEphemeralRepository::ensure(
        client.clone(),
        std::time::Duration::from_millis(120_000),
    )
    .await
    .unwrap();
    repository_conformance(repository).await;
    let error =
        NatsAuthEphemeralRepository::ensure(client, std::time::Duration::from_millis(360_000))
            .await
            .expect_err("old connection presence retention must be incompatible");
    let error = error.to_string();
    assert!(error.contains("max_age"), "{error}");
    assert!(error.contains("360000ms"), "{error}");
    assert!(error.contains("120000ms"), "{error}");
    drop(server);
}

#[test]
fn strict_json_keeps_required_nullable_fields() {
    let value = serde_json::to_value(browser_flow()).unwrap();
    assert_eq!(value["principalId"], serde_json::Value::Null);
    assert_eq!(value["claimOwner"], serde_json::Value::Null);
    assert_eq!(value["claimedAt"], serde_json::Value::Null);
    assert_eq!(value["durableResultDigest"], serde_json::Value::Null);
    assert_eq!(value["completedAt"], serde_json::Value::Null);

    let mut value = serde_json::to_value(oauth_state()).unwrap();
    assert_eq!(value["claimOwner"], serde_json::Value::Null);
    assert_eq!(value["resultDigest"], serde_json::Value::Null);
    value["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AuthOAuthState>(value).is_err());
}
