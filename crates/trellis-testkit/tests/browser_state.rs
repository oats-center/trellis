//! Browser State acceptance through production OAuth, generated clients and Chromium.
use std::io::{BufRead, Write};
use trellis_rs::auth::{discover_native_oauth, NativeCredentialScope, NativeOAuthClient};
use trellis_rs::client::UserConnectOptions;
use trellis_runtime_apis::apis::trellis_auth_v1::Client;
use trellis_testkit::TrellisTestRuntime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browser_state_namespace_cas_watch_and_persistence() {
    let listeners: Vec<_> = (0..2)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let origins: Vec<_> = listeners
        .iter()
        .map(|listener| format!("http://{}", listener.local_addr().unwrap()))
        .collect();
    let mut runtime = TrellisTestRuntime::builder()
        .extra_origins(origins.clone())
        .start()
        .await
        .unwrap();
    let server = discover_native_oauth(runtime.oauth_origin(), &runtime.trust_roots())
        .await
        .unwrap();
    let native = NativeOAuthClient::open(
        runtime.workdir().join("state-admin"),
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
    let device = native
        .start_device(&serde_json::json!([
            {
                "type":"trellis_capabilities",
                "required": [
                    "trellis.auth@v1::usersCreate",
                    "trellis.auth@v1::capabilityGrantsGrant",
                    "trellis.auth@v1::oauthClientsPut"
                            ],
                "optional": []
            },
            {
                "type":"trellis_platform_privileges",
                "required": ["principals.manage","roles.manage","clients.manage"]
            }
        ]))
        .await
        .unwrap();
    runtime
        .approve_device_login("trellis.cli", device.user_code())
        .await
        .unwrap();
    let admin = Client::from_generated(
        trellis_rs::generated::Client::connect_user(UserConnectOptions::new(
            server.nats_servers,
            30_000,
            device.complete().await.unwrap(),
        ))
        .await
        .unwrap(),
    );
    let user = admin.users_create(&serde_json::from_value(serde_json::json!({
        "username":"state-second","displayName":"State second","password":"separate state password","idempotencyKey":ulid::Ulid::new().to_string()
    })).unwrap()).await.unwrap();
    for capability in ["read", "write", "delete"] {
        admin.capability_grants_grant(&serde_json::from_value(serde_json::json!({
            "principalId":user.user.principal_id,"capabilityId":format!("trellis.state@v1::{capability}"),
            "expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()
        })).unwrap()).await.unwrap();
    }
    for id in ["state-browser", "state-browser-other"] {
        admin.o_auth_clients_put(&serde_json::from_value(serde_json::json!({
            "clientId":id,"kind":"browser","title":"State browser","description":"Browser State acceptance",
            "redirectUris":origins.iter().map(|origin| format!("{origin}/callback")).collect::<Vec<_>>(),
            "development":true,"impliedConsent":[],"expectedRevision":"0","idempotencyKey":ulid::Ulid::new().to_string()
        })).unwrap()).await.unwrap();
    }
    drop(listeners);
    drop(admin);
    let mut child = std::process::Command::new("node")
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .arg("ts/browser/state.browser_driver.ts")
        .env("TRELLIS_TEST_OAUTH_ORIGIN", runtime.oauth_origin())
        // The browser trusts the installation CA through a private NSS store
        // and Node through its standard extra-CA setting.
        .env("TRELLIS_TEST_CA_CERTIFICATE", &runtime.trust_roots()[0])
        .env("NODE_EXTRA_CA_CERTS", &runtime.trust_roots()[0])
        .env("TRELLIS_TEST_ADMIN_USERNAME", runtime.admin_username())
        .env("TRELLIS_TEST_ADMIN_PASSWORD", runtime.admin_password())
        .env(
            "TRELLIS_STATE_ORIGINS",
            serde_json::to_string(&origins).unwrap(),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut restarted = false;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        println!("{line}");
        if line == "STATE_RESTART" {
            runtime.restart().await.unwrap();
            writeln!(child.stdin.as_mut().unwrap(), "restarted").unwrap();
            restarted = true;
        }
    }
    let result = child.wait().unwrap();
    assert!(result.success(), "live browser State lifecycle failed");
    assert!(restarted, "browser must cross the real runtime restart");
    runtime.shutdown().await.unwrap();
}
