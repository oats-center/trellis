use super::*;
use crate::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use futures_util::TryStreamExt;
use serde_json::json;
use trellis_runtime_apis::apis::{trellis_auth_v1, trellis_health_v1::rpc::Query};
use trellis_runtime_apis::types::AuthLoginRevokeRequest;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn own_policy_reduction_rebinds_the_mirror_without_reviving_the_retired_session() {
    use trellis_runtime_apis::types::{
        AuthCapabilityGrantRequest, AuthCapabilityGrantResponse, AuthCapabilityRevokeRequest,
        AuthUserCreateRequest, AuthUserGetResponse,
    };
    // An optional capability the user holds only by a direct grant.
    let inspect = "trellis.health.v1:health.inspect".to_owned();
    let mut runtime = trellis_testkit::TrellisTestRuntime::builder()
        .start()
        .await
        .unwrap();
    let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("credentials/own-reduction-admin"),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    let login = native
        .start_device(&json!([
            {"type":"trellis_capabilities","required":[
                trellis_auth_v1::rpc::UsersCreate::CALLER_CAPABILITIES[0],
                trellis_auth_v1::rpc::CapabilityGrantsGrant::CALLER_CAPABILITIES[0],
                trellis_auth_v1::rpc::SessionsRevoke::CALLER_CAPABILITIES[0],
                // Ending another person's sign-in.
                trellis_auth_v1::rpc::AuthorizationSessionsRevoke::CALLER_CAPABILITIES[0]
            ],"optional":[]}
        ]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", login.user_code())
        .await
        .unwrap();
    let admin = TrellisClient::connect_user(UserConnectOptions::new(
        server.nats_servers.clone(),
        10_000,
        login.complete().await.unwrap(),
    ))
    .await
    .unwrap();
    let user = AuthUserCreateRequest {
        given_name: None,
        family_name: None,
        email: None,
        username: "mirror-user".to_string().into(),
        display_name: "Mirror user".to_string().into(),
        password: Some("ordinary mirror user password".to_string().into()),
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    };
    let user: AuthUserGetResponse = serde_json::from_value(
        admin
            .request_json_value(
                trellis_auth_v1::rpc::UsersCreate::API_ID,
                trellis_protocol::CatalogActionIdentity {
                    kind: trellis_protocol::ApiSurfaceKind::Rpc,
                    name: "Users.Create".into(),
                    direction: trellis_protocol::PermissionAction::Call,
                },
                trellis_auth_v1::rpc::UsersCreate::SUBJECT,
                &serde_json::to_value(user).unwrap(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let grant = AuthCapabilityGrantRequest {
        principal_id: user.user.principal_id,
        capability_id: Query::CALLER_CAPABILITIES[0].to_string().into(),
        expected_revision: 0.into(),
        expires_at: None,
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    };
    admin
        .request_json_value(
            trellis_auth_v1::rpc::CapabilityGrantsGrant::API_ID,
            trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Rpc,
                name: "CapabilityGrants.Grant".into(),
                direction: trellis_protocol::PermissionAction::Call,
            },
            trellis_auth_v1::rpc::CapabilityGrantsGrant::SUBJECT,
            &serde_json::to_value(grant).unwrap(),
        )
        .await
        .unwrap();
    let grant = AuthCapabilityGrantRequest {
        principal_id: user.user.principal_id,
        capability_id: inspect.clone().into(),
        expected_revision: 0.into(),
        expires_at: None,
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    };
    let granted: AuthCapabilityGrantResponse = serde_json::from_value(
        admin
            .request_json_value(
                trellis_auth_v1::rpc::CapabilityGrantsGrant::API_ID,
                trellis_protocol::CatalogActionIdentity {
                    kind: trellis_protocol::ApiSurfaceKind::Rpc,
                    name: "CapabilityGrants.Grant".into(),
                    direction: trellis_protocol::PermissionAction::Call,
                },
                trellis_auth_v1::rpc::CapabilityGrantsGrant::SUBJECT,
                &serde_json::to_value(grant).unwrap(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("credentials/own-reduction-user"),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust,
    )
    .await
    .unwrap();
    let login = native.start_device(&json!([
        {"type":"trellis_capabilities","required":[Query::CALLER_CAPABILITIES[0]],"optional":[inspect]}
    ])).await.unwrap();
    // The ordinary local-user portal establishes this non-administrator's login
    // and consent; no fixture inserts an OAuth grant or a verified context.
    let origin = runtime.oauth_origin();
    let http = runtime
        .https_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let response = http
        .post(format!("{origin}/portal/session"))
        .header("Origin", origin)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let cookies: Vec<_> = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    let csrf = cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("__Host-trellis-portal-csrf="))
        .unwrap()
        .to_owned();
    let response = http
        .post(format!("{origin}/portal/login"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, cookies.join("; "))
        .form(&[
            ("csrf", csrf.clone()),
            ("username", "mirror-user".into()),
            ("password", "ordinary mirror user password".into()),
            ("request_id", ulid::Ulid::new().to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let cookies: Vec<_> = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    let cookie = cookies.join("; ");
    let response = http
        .post(format!("{origin}/portal/device"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, &cookie)
        .form(&[("csrf", csrf.as_str()), ("user_code", login.user_code())])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let consent = url::Url::parse(origin)
        .unwrap()
        .join(
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
    let transaction = consent
        .query_pairs()
        .find(|(key, _)| key == "transaction")
        .unwrap()
        .1
        .into_owned();
    let view: serde_json::Value = http
        .get(format!("{origin}/portal/authorization/{transaction}"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut form = vec![
        ("csrf", csrf.as_str()),
        ("transaction", transaction.as_str()),
        ("presentation", view["presentationId"].as_str().unwrap()),
        ("decision", "approve"),
    ];
    for item in view["capabilities"].as_array().unwrap() {
        if item["available"] == true {
            form.push(("approved", item["capabilityId"].as_str().unwrap()));
        }
    }
    let response = http
        .post(format!("{origin}/portal/approve"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, &cookie)
        .form(&form)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_redirection());
    let provider = TrellisClient::connect_user(UserConnectOptions::new(
        server.nats_servers,
        10_000,
        login.complete().await.unwrap(),
    ))
    .await
    .unwrap();
    let action = trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: "Query".into(),
        direction: trellis_protocol::PermissionAction::Call,
    };
    provider
        .request_json_value(Query::API_ID, action.clone(), Query::SUBJECT, &json!({}))
        .await
        .unwrap();
    let old = provider.authorization_owner().own_authority().unwrap();
    assert!(old
        .authority()
        .capabilities
        .iter()
        .any(|held| held.capability_id == inspect));
    let mut availability = provider
        .authorization_contexts
        .as_ref()
        .unwrap()
        .watch_availability();
    let original = provider
        .generations
        .acquire_published()
        .unwrap()
        .generation_id();
    let mut publication = provider.generations.subscribe_publication();
    let revoke = AuthCapabilityRevokeRequest {
        grant_id: granted.grant.grant_id,
        expected_revision: granted.grant.revision,
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    };
    admin
        .request_json_value(
            trellis_auth_v1::rpc::CapabilityGrantsRevoke::API_ID,
            trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Rpc,
                name: "CapabilityGrants.Revoke".into(),
                direction: trellis_protocol::PermissionAction::Call,
            },
            trellis_auth_v1::rpc::CapabilityGrantsRevoke::SUBJECT,
            &serde_json::to_value(revoke).unwrap(),
        )
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        availability.wait_for(|state| !state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(provider.authorization_owner().assert_current(&old).is_err());
    provider.refresh_authorization_context().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        publication.wait_for(|id| id.is_some_and(|id| id != original)),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        availability.wait_for(|state| state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    let replacement = provider.authorization_owner().own_authority().unwrap();
    assert_ne!(
        replacement.authority().authorization_session_id,
        old.authority().authorization_session_id
    );
    assert!(!replacement
        .authority()
        .capabilities
        .iter()
        .any(|held| held.capability_id == inspect));
    assert!(provider.authorization_owner().assert_current(&old).is_err());
    provider
        .request_json_value(Query::API_ID, action.clone(), Query::SUBJECT, &json!({}))
        .await
        .unwrap();
    // Revoking the actual login removes the credential root, unlike the earlier
    // optional-authority reduction. Fresh admission must not restore this client.
    let revoke = AuthLoginRevokeRequest {
        login_session_id: replacement
            .authority()
            .login_session_id
            .as_ref()
            .unwrap()
            .parse()
            .unwrap(),
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    };
    admin
        .request_json_value(
            trellis_auth_v1::rpc::SessionsRevoke::API_ID,
            trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Rpc,
                name: "Sessions.Revoke".into(),
                direction: trellis_protocol::PermissionAction::Call,
            },
            trellis_auth_v1::rpc::SessionsRevoke::SUBJECT,
            &serde_json::to_value(revoke).unwrap(),
        )
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        availability.wait_for(|state| !state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(provider.refresh_authorization_context().await.is_err());
    assert!(provider
        .request_json_value(Query::API_ID, action, Query::SUBJECT, &json!({}))
        .await
        .is_err());
    provider.shutdown_native_runtime().await.unwrap();
    admin.shutdown_native_runtime().await.unwrap();
    runtime.shutdown().await.unwrap();
}

async fn connect_caller(
    runtime: &mut trellis_testkit::TrellisTestRuntime,
    name: &str,
) -> TrellisClient {
    let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("credentials").join(name),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust,
    )
    .await
    .unwrap();
    let login = native
    .start_device(&json!([{
    "type":"trellis_capabilities","required":[
        Query::CALLER_CAPABILITIES[0], trellis_auth_v1::rpc::SessionsRevoke::CALLER_CAPABILITIES[0]
    ],"optional":[]
    }]))
    .await
    .unwrap();
    runtime
        .approve_device_login("trellis.cli", login.user_code())
        .await
        .unwrap();
    TrellisClient::connect_user(UserConnectOptions::new(
        server.nats_servers,
        1_000,
        login.complete().await.unwrap(),
    ))
    .await
    .unwrap()
}

// Consumer deletion and a real broker capacity limit interrupt only the shared
// retirement watch: the application socket and the production control plane stay up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_mirror_recovers_without_recreating_application_and_preserves_cutoffs() {
    let mut runtime = trellis_testkit::TrellisTestRuntime::builder()
        .start()
        .await
        .unwrap();
    let admin_nats = async_nats::ConnectOptions::new()
        .credentials_file(
            runtime
                .workdir()
                .join("config/nats/creds/trellis-auth.creds"),
        )
        .await
        .unwrap()
        .add_root_certificates(runtime.trust_roots()[0].clone())
        .connect(runtime.nats_url())
        .await
        .unwrap();
    let js = jetstream::new(admin_nats.clone());
    let mut stream = js.get_stream("KV_TRELLIS_AUTH_REVOCATIONS").await.unwrap();
    let admin = connect_caller(&mut runtime, "mirror-controller").await;
    let peer = connect_caller(&mut runtime, "mirror-peer").await;
    let before = stream
        .consumer_names()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let client = connect_caller(&mut runtime, "mirror-application").await;
    let after = stream
        .consumer_names()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let mirror = after
        .iter()
        .find(|name| !before.contains(name))
        .unwrap()
        .clone();
    let action = trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: "Query".into(),
        direction: trellis_protocol::PermissionAction::Call,
    };
    client
        .request_json_value(Query::API_ID, action.clone(), Query::SUBJECT, &json!({}))
        .await
        .unwrap();
    let peer_own = peer.authorization_owner().own_authority().unwrap();
    let api = peer_own
        .authority()
        .apis
        .iter()
        .find(|api| api.api_id == Query::API_ID)
        .unwrap();
    let opening = trellis_protocol::SessionRequest {
        authority_digest: peer_own.digest().into(),
        api_id: Query::API_ID.into(),
        api_generation: api.generation,
        accepted_revision: api.accepted_revision,
        action: action.name.clone(),
        subject: Query::SUBJECT.into(),
        reply_subject: Some(peer.nats().new_inbox()),
        request_id: ulid::Ulid::new().to_string(),
        issued_at: peer.authorization_owner().policy().now_unix_seconds,
    };
    let (proof, _) = peer.auth().create_request_proof(&opening, b"{}").unwrap();
    client
        .authorization_owner()
        .verify_possession(&client.nats(), &opening, b"{}", &proof)
        .await
        .unwrap();
    let contexts = client.authorization_contexts.as_ref().unwrap();
    let mut availability = contexts.watch_availability();
    let original = client
        .generations
        .acquire_published()
        .unwrap()
        .generation_id();
    let mut config = stream.info().await.unwrap().config.clone();
    let previous_limit = config.max_consumers;
    config.max_consumers = after.len().try_into().unwrap();
    js.update_stream(config.clone()).await.unwrap();
    stream.delete_consumer(&mirror).await.unwrap();
    // Occupy the freed slot through the normal JetStream administration API,
    // so retries remain unavailable until the broker permits a new consumer.
    let blocker = stream
        .create_consumer(jetstream::consumer::pull::Config {
            name: Some("mirror-outage-capacity".into()),
            ack_policy: jetstream::consumer::AckPolicy::None,
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        availability.wait_for(|state| !state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    client.nats().flush().await.unwrap();
    assert_eq!(
        client.nats().connection_state(),
        async_nats::connection::State::Connected
    );
    assert!(client
        .request_json_value(Query::API_ID, action.clone(), Query::SUBJECT, &json!({}))
        .await
        .is_err());
    let retirement = AuthLoginRevokeRequest {
        login_session_id: peer_own
            .authority()
            .login_session_id
            .as_ref()
            .unwrap()
            .parse()
            .unwrap(),
        idempotency_key: ulid::Ulid::new().to_string().into(),
        extra: Default::default(),
    };
    admin
        .request_json_value(
            trellis_auth_v1::rpc::SessionsRevoke::API_ID,
            trellis_protocol::CatalogActionIdentity {
                kind: trellis_protocol::ApiSurfaceKind::Rpc,
                name: trellis_auth_v1::rpc::SessionsRevoke::DESCRIPTOR_NAME
                    .strip_prefix("rpc.")
                    .unwrap()
                    .into(),
                direction: trellis_protocol::PermissionAction::Call,
            },
            trellis_auth_v1::rpc::SessionsRevoke::SUBJECT,
            &serde_json::to_value(retirement).unwrap(),
        )
        .await
        .unwrap();
    // Restore broker capacity; the same client must re-read the authentic
    // retirement written while its mirror was unavailable before serving again.
    let retirements = js.get_key_value("TRELLIS_AUTH_REVOCATIONS").await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if retirements
                .get(&peer_own.authority().authorization_session_id)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stream
        .delete_consumer(&blocker.cached_info().name)
        .await
        .unwrap();
    config.max_consumers = previous_limit;
    js.update_stream(config.clone()).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        availability.wait_for(|state| state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        client
            .generations
            .acquire_published()
            .unwrap()
            .generation_id(),
        original
    );
    client
        .request_json_value(Query::API_ID, action.clone(), Query::SUBJECT, &json!({}))
        .await
        .unwrap();
    let fresh = trellis_protocol::SessionRequest {
        request_id: ulid::Ulid::new().to_string(),
        ..opening.clone()
    };
    let (proof, _) = peer.auth().create_request_proof(&fresh, b"{}").unwrap();
    assert!(client
        .authorization_owner()
        .verify_possession(&client.nats(), &fresh, b"{}", &proof)
        .await
        .is_err());

    // A real socket close publishes a new generation. Its replacement mirror
    // must serve the same logical client and retain the already known cutoff.
    let previous_mirrors = stream
        .consumer_names()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let mut publication = client.generations.subscribe_publication();
    client
        .generations
        .acquire_published()
        .unwrap()
        .nats()
        .drain()
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        publication.wait_for(|id| id.is_some_and(|id| id != original)),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        availability.wait_for(|state| state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    let rebound_mirrors = stream
        .consumer_names()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let replacements: Vec<_> = rebound_mirrors
        .iter()
        .filter(|name| !previous_mirrors.contains(name))
        .collect();
    assert_eq!(
        replacements.len(),
        1,
        "one socket replacement must establish one owner mirror, not duplicate consumers"
    );
    client
        .request_json_value(Query::API_ID, action.clone(), Query::SUBJECT, &json!({}))
        .await
        .unwrap();
    let fresh = trellis_protocol::SessionRequest {
        request_id: ulid::Ulid::new().to_string(),
        ..opening
    };
    let (proof, _) = peer.auth().create_request_proof(&fresh, b"{}").unwrap();
    let current = client.generations.acquire_published().unwrap();
    assert!(client
        .authorization_owner()
        .verify_possession(current.nats(), &fresh, b"{}", &proof)
        .await
        .is_err());
    drop(current);

    // Explicit owner shutdown cancels the supervisor even if no retirement
    // update or transport publication will ever arrive to wake its failed retry.
    config.max_consumers = rebound_mirrors.len().try_into().unwrap();
    js.update_stream(config.clone()).await.unwrap();
    stream.delete_consumer(replacements[0]).await.unwrap();
    let close_blocker = stream
        .create_consumer(jetstream::consumer::pull::Config {
            name: Some("mirror-close-capacity".into()),
            ack_policy: jetstream::consumer::AckPolicy::None,
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        availability.wait_for(|state| !state.is_usable()),
    )
    .await
    .unwrap()
    .unwrap();
    client.shutdown_native_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client.authorization_provider_task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stream
        .delete_consumer(&close_blocker.cached_info().name)
        .await
        .unwrap();
    config.max_consumers = previous_limit;
    js.update_stream(config).await.unwrap();
    assert!(client
        .request_json_value(Query::API_ID, action, Query::SUBJECT, &json!({}))
        .await
        .is_err());
    peer.shutdown_native_runtime().await.unwrap();
    admin.shutdown_native_runtime().await.unwrap();
    runtime.shutdown().await.unwrap();
}
