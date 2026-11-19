//! Auth mutation idempotency keys over the production runtime.
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::{TrellisClient, UserConnectOptions};
use trellis_runtime_apis::apis::trellis_auth_v1::rpc::{UsersCreate, UsersList};
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_ulid_idempotency_key_is_refused_by_name_and_creates_nothing() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let server = discover_native_oauth(runtime.trellis_url(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("idempotency-admin"),
        NativeCredentialScope {
            issuer: server.issuer.clone(),
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "trellis.cli".into(),
            credential_handle: ulid::Ulid::generate(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    let login = native
        .start_device(&serde_json::json!([
            {"type":"trellis_capabilities","required":[
                UsersCreate::CALLER_CAPABILITIES[0],
                UsersList::CALLER_CAPABILITIES[0]
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
    let action = |name: &str| trellis_protocol::CatalogActionIdentity {
        kind: trellis_protocol::ApiSurfaceKind::Rpc,
        name: name.into(),
        direction: trellis_protocol::PermissionAction::Call,
    };

    // The raw request bypasses generated encoding, as a caller in another
    // language or a hand-built payload would.
    let refused = admin
        .request_json_value(
            UsersCreate::API_ID,
            action("Users.Create"),
            UsersCreate::SUBJECT,
            &serde_json::json!({
                "username": "not-a-ulid-user",
                "displayName": "Not a ULID",
                "idempotencyKey": "not-a-ulid",
            }),
        )
        .await
        .expect_err("a non-ULID idempotency key must be refused");
    let refused = format!("{refused:?}");
    assert!(refused.contains("idempotencyKey"), "{refused}");

    let listed = admin
        .request_json_value(
            UsersList::API_ID,
            action("Users.List"),
            UsersList::SUBJECT,
            &serde_json::json!({ "search": "not-a-ulid-user" }),
        )
        .await
        .unwrap();
    assert_eq!(listed["items"], serde_json::json!([]), "{listed}");
    drop(admin);
    runtime.shutdown().await.unwrap();
}
