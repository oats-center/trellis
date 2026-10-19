//! Browser account acceptance against the ordinary runtime and generated clients.

use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_runtime_apis::apis::trellis_auth_v1::Client;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browser_accounts_survive_tabs_and_restart_without_crossing_principals() {
    let mut runtime = TrellisTestRuntime::builder().start().await.unwrap();
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("browser-admin"),
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
    let device = native.start_device(&serde_json::json!([
        {"type":"trellis_capabilities","required":["trellis.auth@v1::usersCreate", "trellis.auth@v1::capabilityGrantsGrant", "trellis.auth@v1::oauthClientsPut", "trellis.auth@v1::oauthClientsDisable", "trellis.auth@v1::oauthClientsDelete"],"optional":[]},
        {"type":"trellis_platform_privileges","required":["principals.manage", "roles.manage", "clients.manage"]}
    ])).await.unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    let session = device.complete().await.unwrap();
    let admin = Client::from_generated(
        trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers,
            30_000,
            session,
        ))
        .await
        .unwrap(),
    );
    let user = admin.users_create(&serde_json::from_value(serde_json::json!({
        "username": "browser-second-user", "displayName": "Browser second user",
        "password": "a separate browser account password", "idempotencyKey": ulid::Ulid::new().to_string()
    })).unwrap()).await.unwrap();
    admin
        .capability_grants_grant(
            &serde_json::from_value(serde_json::json!({
                "principalId": user.user.principal_id,
                "capabilityId": "trellis.auth@v1::accountRead",
                "expectedRevision": "0", "idempotencyKey": ulid::Ulid::new().to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let result = std::process::Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .args(["--test", "ts/browser/oauth_accounts.browser_driver.ts"])
        .env("TRELLIS_TEST_CONSOLE_URL", runtime.trellis_url())
        .env("TRELLIS_TEST_OAUTH_ORIGIN", runtime.oauth_origin())
        .env("TRELLIS_TEST_ADMIN_USERNAME", runtime.admin_username())
        .env("TRELLIS_TEST_ADMIN_PASSWORD", runtime.admin_password())
        .status()
        .unwrap();
    assert!(result.success(), "live browser account lifecycle failed");
    let client = admin.o_auth_clients_put(&serde_json::from_value(serde_json::json!({
        "clientId": "lifecycle-native", "kind": "native", "title": "Lifecycle native",
        "description": "Lifecycle acceptance", "redirectUris": ["http://127.0.0.1/callback"],
        "development": false, "impliedConsent": [], "expectedRevision": "0",
        "idempotencyKey": ulid::Ulid::new().to_string()
    })).unwrap()).await.unwrap();
    let server = discover_native_oauth(runtime.oauth_origin()).await.unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("disabled-client"),
        NativeCredentialScope {
            issuer: server.issuer,
            instance_id: server.trust.instance_id.clone(),
            account: server.trust.account.clone(),
            client_id: "lifecycle-native".into(),
            credential_handle: ulid::Ulid::new(),
        },
        server.trust.clone(),
    )
    .await
    .unwrap();
    let device = native.start_device(&serde_json::json!([
        {"type":"trellis_capabilities","required":["trellis.auth@v1::accountRead"],"optional":[]}
    ])).await.unwrap();
    runtime
        .approve_device_login("lifecycle-native", device.user_code())
        .await
        .unwrap();
    let mut session = device.complete().await.unwrap();
    session.refresh(server.trust.clone()).await.unwrap();
    let disabled = admin
        .o_auth_clients_disable(
            &serde_json::from_value(serde_json::json!({
                "clientId": "lifecycle-native", "expectedRevision": client.client.revision,
                "idempotencyKey": ulid::Ulid::new().to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            session.refresh(server.trust).await,
            Err(trellis_rs::auth::NativeOAuthError::RecoveryRequired)
        ),
        "disabled client must not rotate its existing grant"
    );
    admin
        .o_auth_clients_delete(
            &serde_json::from_value(serde_json::json!({
                "clientId": "lifecycle-native", "expectedRevision": disabled.revision,
                "idempotencyKey": ulid::Ulid::new().to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    drop(admin);
    runtime.shutdown().await.unwrap();
}
